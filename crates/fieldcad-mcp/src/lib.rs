//! MCP transport onto [`fieldcad_server::HeadlessServer`].
//!
//! This crate is deliberately thin: it does not own the model, only a shared
//! handle to it. Every tool here is a direct translation of one
//! [`fieldcad_simulation::CommandPayload`] variant or one read already exposed
//! through [`fieldcad_simulation::FieldDataSource`] — the same commands the
//! desktop UI issues. MCP is one command source among others (see
//! `docs/mcp-plan.md`); this module is where that source's shape lives.
//!
//! Scope of this first slice (mapped from the "Suggested MCP surface" table in
//! `docs/user-stories/README.md`): simulation control, world inventory and
//! mutation, experiment (field system) configuration, subscriptions through
//! four `fieldcad://session/{status,snapshot,diagnostics,queue}` resources with
//! push notifications via `subscriptions/listen`, the latest snapshot, and
//! undo/redo. Left for later, because the underlying capability doesn't exist
//! in the model yet or needs its own design: scene lifecycle (create/open/save),
//! particle templates, rename, probe history and trajectories as retained
//! server-side series, diagnostics as a dedicated read (today folded into the
//! snapshot), run comparison, record/replay, and export.
//!
//! World commands too varied to give a typed MCP schema in this slice
//! (`edit_world` and `commit_world`) are accepted as a `Vec<serde_json::Value>`
//! array of [`fieldcad_core::WorldCommand`] values rather than a native MCP
//! input schema per variant: those types are not `schemars::JsonSchema`, and
//! deriving it across all of `fieldcad-core` is bigger than this slice. Every
//! other tool takes plain primitives so its schema is exact.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, ChannelId, ChannelSnapshot, Domain, DomainBounds,
    PluginId, PluginProvenance, Precision, Resolution, SceneScale, SnapshotCompleteness,
    SnapshotIdentity, SolverDiagnostic, TimeStep, WorldCommand,
};
use fieldcad_server::{HeadlessServer, SessionEvent, WatchEvent};
use fieldcad_simulation::{
    CommandEvent, CommandId, CommandPayload, CommandReceipt, DataSourceStatus, FieldDataSource,
    PlaybackSpeed, QueueStatus, SimulationStatus, SourceError, Subscription,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ContentBlock, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
        ResourceContents, ServerCapabilities, ServerInfo, SubscriptionFilter,
    },
    schemars,
    service::{RequestContext, SubscriptionContext},
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

mod transport;
mod typed_world;
pub use transport::{McpConnections, bind_http, bind_unix, generate_token, run_stdio, serve_http};
#[cfg(unix)]
pub use transport::{UnixSocketLock, serve_unix};
use typed_world::into_world_command;
pub use typed_world::{ChannelRefParam, EditWorldParams, WorldEditParam};

/// How long a tool call waits for its own command to complete before giving
/// up. Generous: this is a ceiling against something never completing (a
/// wedged solver, a lost worker), not a latency budget — a healthy command
/// resolves in well under a second.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Default cap on `get_latest_snapshot`'s total sample count.
///
/// A dense subscription (a fine plane/domain/box/sphere sampling) can make
/// one snapshot read tens of thousands of characters of JSON — large enough
/// to exceed a typical MCP client's own response-size budget outright, which
/// surfaces as an opaque transport failure rather than an actionable error.
/// This default is conservative on purpose (observed: a subscription of one
/// 33x33 plane plus a coarse whole-domain sample, two channels, already
/// produced ~117,000 characters, well past this many samples); a caller that
/// knows its client tolerates more can raise `max_samples` explicitly.
const DEFAULT_MAX_SNAPSHOT_SAMPLES: usize = 2_000;

/// `std::sync::Mutex` (not `tokio::sync::Mutex`) is what lets this be shared
/// with a synchronous caller — the embedded desktop app's winit frame loop,
/// which cannot `.await` a lock without its own runtime handle. Sound
/// because nothing in `HeadlessServer`'s methods blocks or awaits while
/// holding the lock. `unwrap_or_else(PoisonError::into_inner)` rather than a
/// bare `.unwrap()`: unlike `tokio::sync::Mutex`, this one *does* poison on
/// a panic-while-held, and a panic reachable from any one MCP tool call must
/// not crash every other caller of the shared model (including, once
/// embedded, the desktop UI's next frame) on their next lock attempt.
fn lock(model: &Mutex<HeadlessServer>) -> MutexGuard<'_, HeadlessServer> {
    model.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ok_json<T: Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string(value)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn tool_error(message: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(
        message.into(),
    )]))
}

/// Submit a command and, if the model accepted it non-blockingly (ADR 0011:
/// running world edits are queued to the next tick boundary), wait for its
/// completion before answering. A tool call is a single request/response; a
/// client should not have to poll separately to learn whether its own edit
/// landed.
///
/// Registers a waiter with `HeadlessServer` under the same lock as
/// submission (`submit_and_await`) rather than scanning `drain_events()`'s
/// returned list for a matching id: once more than one transport can share
/// one `HeadlessServer` (an embedded desktop UI's own per-frame drain, for
/// instance), whichever caller drains first would otherwise remove the
/// event before this call ever saw it — a hang, since nothing here timed
/// out before that fix, or worse, a different in-flight command's receipt
/// mistaken for this one's.
///
/// The periodic `tick` below exists only to notice a worker reply that
/// arrived while nobody was otherwise pumping the model — it is *not* this
/// call's wall-clock driver. That driver is always someone else: the
/// embedded desktop app's per-frame pump, or (for a standalone
/// `fieldcad-mcp`) the session-driving task `main.rs` spawns alongside its
/// transports (`docs/mcp-plan.md`). Feeding the tick's own real interval
/// into `advance` here, as an earlier version of this function did, fed a
/// *second* stream of real elapsed time into the same shared `TickPacer`
/// whenever that other driver was also running concurrently — a session
/// under a pending tool call ticked at roughly twice wall-clock speed. Using
/// `Duration::ZERO` costs nothing (a queued mutation's own completion still
/// arrives from `AsyncLocalDataSource`'s worker thread the moment it
/// finishes, independent of `advance`'s elapsed argument) and never competes
/// with the session's one real clock.
async fn submit_and_wait(
    model: &Arc<Mutex<HeadlessServer>>,
    payload: CommandPayload,
) -> Result<CommandReceipt, SourceError> {
    let (receipt, waiter) = { lock(model).submit_and_await(payload)? };
    let Some(waiter) = waiter else {
        return Ok(receipt);
    };
    let mut tick = tokio::time::interval(Duration::from_millis(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline = tokio::time::sleep(COMMAND_TIMEOUT);
    tokio::pin!(waiter, deadline);
    loop {
        tokio::select! {
            result = &mut waiter => {
                return match result {
                    Ok(CommandEvent::Completed(completed)) => Ok(completed),
                    Ok(CommandEvent::Failed { error, .. }) => Err(error),
                    // Cancelled by a `cancel_queued_command` call from
                    // elsewhere while this tool call's own command was still
                    // queued -- a legitimate outcome, not a transport error.
                    Ok(CommandEvent::Cancelled(command)) => Err(SourceError::Solver {
                        code: "command-cancelled".to_owned(),
                        message: format!("command {command:?} was cancelled before it applied"),
                    }),
                    Err(_dropped) => Err(SourceError::Disconnected),
                };
            }
            () = &mut deadline => {
                return Err(SourceError::Solver {
                    code: "command-timeout".to_owned(),
                    message: format!(
                        "command {:?} did not complete within {COMMAND_TIMEOUT:?}",
                        receipt.command
                    ),
                });
            }
            _ = tick.tick() => {
                // `Duration::ZERO`, not this tick's own interval: see the
                // doc comment above. And no `drain_events()` here — that
                // buffer is shared with every other transport observing
                // this session (the desktop UI's own per-frame drain,
                // notably), and this loop has nothing to do with its
                // contents; `publish()`, called inside `advance` itself,
                // already resolves this call's own waiter independently of
                // whether anyone ever drains the shared buffer.
                lock(model).advance(Duration::ZERO)?;
            }
        }
    }
}

/// Submit an already-parsed batch as one atomic `CommitWorld` transaction
/// and report the outcome — the tail `edit_world` and `commit_world` share;
/// the two tools differ only in how each turns its own request shape into
/// `Vec<WorldCommand>` before reaching here.
async fn submit_world_commands(
    model: &Arc<Mutex<HeadlessServer>>,
    commands: Vec<WorldCommand>,
) -> Result<CallToolResult, ErrorData> {
    match submit_and_wait(model, CommandPayload::CommitWorld(commands)).await {
        Ok(receipt) => ok_json(&receipt),
        Err(error) => tool_error(error.to_string()),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CommitWorldParams {
    /// Native JSON command objects, applied as one atomic transaction. This
    /// is deliberately an array parameter rather than JSON embedded in a
    /// string: MCP clients can construct and inspect the transaction as JSON.
    /// Component-property values are plugin-defined; use `get_world` and
    /// `list_field_systems` to discover their schemas before authoring them.
    commands: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetTimeStepParams {
    /// The fixed numerical time step, in seconds. Must be finite and positive;
    /// the model rejects a step an active solver reports as unstable (for
    /// example a Maxwell Courant violation).
    seconds: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetPlaybackSpeedParams {
    /// Wall-clock speed multiplier. Never changes the numerical `dt`.
    multiplier: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetSceneScaleParams {
    /// How many metres one render/camera unit represents. Must be finite and
    /// positive. Defaults to 1.0 (metre scale); use e.g. 1e-9 for a
    /// nanometre-scale scene or 1.495978707e11 for an astronomical-unit
    /// scale.
    metres_per_unit: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetSubscriptionParams {
    /// Sample every probe that requested each channel.
    probes: bool,
    /// Sample each visible slice plane at this many points per axis. Omit to
    /// stop sampling planes.
    plane_samples_per_axis: Option<u32>,
    /// Sample the whole domain on a lattice decimated by this stride. Omit to
    /// stop sampling the domain.
    domain_stride: Option<u32>,
    /// Sample each visible field box at this many points per axis. Omit to
    /// stop sampling boxes.
    box_samples_per_axis: Option<u32>,
    /// Sample each visible field sphere's bounding cube at this many points
    /// per axis. Omit to stop sampling spheres.
    sphere_samples_per_axis: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetFieldSystemEnabledParams {
    /// The plugin identifier, as reported by `list_field_systems`.
    plugin: String,
    enabled: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetFieldSystemRealtimeParams {
    /// The plugin identifier, as reported by `list_field_systems`.
    plugin: String,
    /// `true` recomputes during every intermediate UI edit; `false` defers
    /// recomputation until the edit commits. This changes responsiveness, not
    /// the result of the committed scene.
    realtime: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetFieldModelParams {
    /// The plugin that declares the shared channel being modelled.
    channel_plugin: String,
    /// The channel name within that plugin.
    channel_name: String,
    /// The plugin that should compute this field, or omit to stop computing
    /// it. Must be an active field system that declares this channel.
    provider_plugin: Option<String>,
}

/// The five boundary treatments recorded by a numerical domain. Kept as an
/// MCP enum rather than an arbitrary string so an agent can discover valid
/// choices directly from the tool schema.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BoundaryConditionParam {
    Periodic,
    Dirichlet,
    Neumann,
    Absorbing,
    Open,
}

impl From<BoundaryConditionParam> for BoundaryCondition {
    fn from(value: BoundaryConditionParam) -> Self {
        match value {
            BoundaryConditionParam::Periodic => Self::Periodic,
            BoundaryConditionParam::Dirichlet => Self::Dirichlet,
            BoundaryConditionParam::Neumann => Self::Neumann,
            BoundaryConditionParam::Absorbing => Self::Absorbing,
            BoundaryConditionParam::Open => Self::Open,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PrecisionParam {
    F32,
    F64,
}

impl From<PrecisionParam> for Precision {
    fn from(value: PrecisionParam) -> Self {
        match value {
            PrecisionParam::F32 => Self::F32,
            PrecisionParam::F64 => Self::F64,
        }
    }
}

/// A complete numerical domain. Bounds are in metres and cells are solver
/// cells, not visualization sample counts.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReconfigureDomainParams {
    min_x_m: f64,
    min_y_m: f64,
    min_z_m: f64,
    max_x_m: f64,
    max_y_m: f64,
    max_z_m: f64,
    cells_x: u32,
    cells_y: u32,
    cells_z: u32,
    boundary_x: BoundaryConditionParam,
    boundary_y: BoundaryConditionParam,
    boundary_z: BoundaryConditionParam,
    precision: PrecisionParam,
}

impl ReconfigureDomainParams {
    fn domain(self) -> Result<Domain, fieldcad_core::DomainError> {
        Ok(Domain::new(
            DomainBounds::new(
                glam::DVec3::new(self.min_x_m, self.min_y_m, self.min_z_m),
                glam::DVec3::new(self.max_x_m, self.max_y_m, self.max_z_m),
            )?,
            Resolution::new(self.cells_x, self.cells_y, self.cells_z)?,
            BoundaryConditions {
                x: self.boundary_x.into(),
                y: self.boundary_y.into(),
                z: self.boundary_z.into(),
            },
            self.precision.into(),
        ))
    }
}

#[derive(Serialize)]
struct ReconfigureDomainResult {
    receipt: CommandReceipt,
    domain: Domain,
    simulation: SimulationStatus,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetBodyForcesParams {
    /// Restrict the result to one object ID. Omit to retrieve every force the
    /// dynamics system produced on its most recent tick.
    object_id: Option<u64>,
}

#[derive(Serialize)]
struct BodyForceResult {
    object_id: u64,
    /// SI newtons, ordered x/y/z.
    force_newtons: [f64; 3],
}

#[derive(Serialize)]
struct DiagnosticsResult {
    snapshot: SnapshotIdentity,
    diagnostics: Vec<SolverDiagnostic>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CancelQueuedCommandParams {
    /// The command id to cancel, as reported by get_queue's pending list or
    /// a prior tool call's receipt.
    command_id: u64,
}

/// The two status categories not already covered by their own resource
/// (`fieldcad://session/snapshot`, `fieldcad://session/diagnostics`).
#[derive(Serialize)]
struct SessionStatusResource {
    source: DataSourceStatus,
    simulation: SimulationStatus,
    domain: Domain,
    /// How many metres one render/camera unit represents. A presentation
    /// setting for a viewport driving this session — never affects the
    /// authored world or solver results. See `set_scene_scale`.
    scene_scale: SceneScale,
}

const SESSION_STATUS_URI: &str = "fieldcad://session/status";
const SESSION_SNAPSHOT_URI: &str = "fieldcad://session/snapshot";
const SESSION_DIAGNOSTICS_URI: &str = "fieldcad://session/diagnostics";
const SESSION_QUEUE_URI: &str = "fieldcad://session/queue";

const SESSION_RESOURCE_URIS: [&str; 4] = [
    SESSION_STATUS_URI,
    SESSION_SNAPSHOT_URI,
    SESSION_DIAGNOSTICS_URI,
    SESSION_QUEUE_URI,
];

fn session_resources() -> Vec<Resource> {
    vec![
        Resource::new(SESSION_STATUS_URI, "session-status").with_description(
            "Authoritative source/simulation status: connecting/ready/disconnected/failed, \
             run mode/tick/time/time-step/world-revision, the numerical domain, and the scene \
             scale (metres per render/camera unit).",
        ),
        Resource::new(SESSION_SNAPSHOT_URI, "session-snapshot").with_description(
            "The latest complete field snapshot, unfiltered: every published channel, revision, \
             tick, and diagnostics. Prefer get_latest_snapshot for a channel/max_samples-bounded read.",
        ),
        Resource::new(SESSION_DIAGNOSTICS_URI, "session-diagnostics").with_description(
            "Structured diagnostics produced for the latest complete snapshot.",
        ),
        Resource::new(SESSION_QUEUE_URI, "session-queue").with_description(
            "Authoritative queue state: paused flag, ordered pending commands, and recent \
             terminal history (capped at 256).",
        ),
    ]
}

/// Every event notification is an invalidation/summary signal, never the
/// payload itself — a subscriber re-reads the named resource for the full
/// authoritative value.
fn resource_text(
    uri: impl Into<String>,
    value: &impl Serialize,
) -> Result<ReadResourceResponse, ErrorData> {
    let text = serde_json::to_string(value)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
        vec![ResourceContents::text(text, uri)],
    )))
}

/// Which resources a session event invalidates.
fn affected_resource_uris(event: &WatchEvent) -> &'static [&'static str] {
    match event {
        WatchEvent::Lagged => &SESSION_RESOURCE_URIS,
        WatchEvent::Closed => &[],
        WatchEvent::Session(SessionEvent::SnapshotUpdated(_)) => &[SESSION_SNAPSHOT_URI],
        WatchEvent::Session(SessionEvent::DiagnosticsUpdated(_)) => &[SESSION_DIAGNOSTICS_URI],
        WatchEvent::Session(
            SessionEvent::StatusUpdated(_) | SessionEvent::SourceStatusUpdated(_),
        ) => &[SESSION_STATUS_URI],
        WatchEvent::Session(SessionEvent::QueueUpdated(_) | SessionEvent::CommandTerminal(_)) => {
            &[SESSION_QUEUE_URI]
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetLatestSnapshotParams {
    /// Only include these channels' batches. Omit to include every published
    /// channel, subject to `max_samples`. A requested channel with no
    /// published data is simply absent from the result, not an error.
    #[serde(default)]
    channels: Option<Vec<ChannelRefParam>>,
    /// Refuse the read and report a per-channel sample-count breakdown
    /// instead of a giant payload if the (possibly `channels`-filtered)
    /// result would exceed this many total samples. Omit for the default
    /// (see `DEFAULT_MAX_SNAPSHOT_SAMPLES`), which is deliberately
    /// conservative; raise it if your client tolerates a larger tool result.
    #[serde(default)]
    max_samples: Option<usize>,
}

/// `FieldSnapshot`, minus whichever channels a caller's `channels` filter
/// excluded. A borrowing view rather than a clone of the snapshot: a
/// dense subscription's batches are the expensive part of this response, and
/// filtering must not copy what it is about to either serialize or discard.
#[derive(Serialize)]
struct SnapshotView<'a> {
    identity: SnapshotIdentity,
    completeness: SnapshotCompleteness,
    domain: Domain,
    plugins: &'a [PluginProvenance],
    channels: BTreeMap<ChannelId, &'a ChannelSnapshot>,
    diagnostics: &'a [SolverDiagnostic],
}

/// The MCP-facing handle onto one session's model.
///
/// Cloning shares the model: every clone locks the same
/// `Arc<Mutex<HeadlessServer>>`. `StreamableHttpService`'s session factory
/// constructs a new `McpServer` per session/request (per its own
/// documentation, it holds no per-session state itself), so the shared state
/// has to live behind the `Arc` a factory closure captures, not on `Self`.
#[derive(Clone)]
pub struct McpServer {
    model: Arc<Mutex<HeadlessServer>>,
}

impl McpServer {
    pub fn new(model: Arc<Mutex<HeadlessServer>>) -> Self {
        Self { model }
    }
}

#[tool_router]
impl McpServer {
    #[tool(description = "The full authored world: objects, planes, probes, and its revision.")]
    async fn get_world(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).world())
    }

    #[tool(
        description = "Authoritative run state: mode, tick, simulation time, time step, and world revision."
    )]
    async fn get_simulation_status(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).simulation_status())
    }

    #[tool(description = "Whether the model is connecting, ready, disconnected, or failed.")]
    async fn get_source_status(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).status())
    }

    #[tool(
        description = "Every equation system composed into the scene, its channels, configuration, and enabled/realtime state."
    )]
    async fn list_field_systems(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).field_systems())
    }

    #[tool(description = "Whether undo/redo are available and what change each would restore.")]
    async fn get_edit_history(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).edit_history())
    }

    #[tool(
        description = "What the model currently samples when it publishes a snapshot. This is what get_latest_snapshot's response size scales with: roughly (probes) + (plane_samples_per_axis^2 x visible planes) + (domain cells / domain_stride^3) + (box_samples_per_axis^3 x visible boxes) + (sphere_samples_per_axis^3 x visible spheres), all multiplied by the number of published channels. Keep these low for routine MCP polling; get_latest_snapshot also accepts its own channels/max_samples to bound one read without changing this durable subscription."
    )]
    async fn get_subscription(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).subscription())
    }

    #[tool(
        description = "The latest complete field snapshot: channel batches, revision, tick, and diagnostics. A dense subscription (see get_subscription/set_subscription) can make the full snapshot very large — large enough to exceed a typical MCP client's own response-size limit, which otherwise surfaces as an opaque transport failure. Pass `channels` to fetch only specific channels; the read is refused with a structured per-channel sample-count breakdown (not a raw size failure) if the result would still exceed `max_samples` (default 2000 total samples)."
    )]
    async fn get_latest_snapshot(
        &self,
        Parameters(params): Parameters<GetLatestSnapshotParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(snapshot) = lock(&self.model).latest_snapshot() else {
            return tool_error("no snapshot has been published yet");
        };

        // A set, not a `Vec`: `contains` below runs once per published
        // channel, and a linear scan there is O(channels x requested).
        let wanted: Option<BTreeSet<ChannelId>> = match params.channels {
            Some(refs) => match refs.into_iter().map(ChannelRefParam::resolve).collect() {
                Ok(ids) => Some(ids),
                Err(error) => return tool_error(error),
            },
            None => None,
        };

        let mut channels: BTreeMap<ChannelId, &ChannelSnapshot> = BTreeMap::new();
        for (id, channel) in &snapshot.channels {
            if wanted.as_ref().is_none_or(|ids| ids.contains(id)) {
                channels.insert(id.clone(), channel);
            }
        }

        let total_samples: usize = channels
            .values()
            .map(|channel| channel.sample_count())
            .sum();
        let limit = params.max_samples.unwrap_or(DEFAULT_MAX_SNAPSHOT_SAMPLES);
        if total_samples > limit {
            let mut breakdown: Vec<(String, usize)> = channels
                .iter()
                .map(|(id, channel)| (id.to_string(), channel.sample_count()))
                .collect();
            breakdown.sort_by_key(|right| std::cmp::Reverse(right.1));
            let listed = breakdown
                .iter()
                .map(|(id, count)| format!("{id}: {count}"))
                .collect::<Vec<_>>()
                .join(", ");
            return tool_error(format!(
                "snapshot has {total_samples} total samples across {} channel(s), exceeding \
                 max_samples={limit}. Largest channels: [{listed}]. Narrow the read with \
                 `channels`, raise `max_samples`, or call set_subscription with a lower \
                 plane_samples_per_axis/domain_stride/box_samples_per_axis/sphere_samples_per_axis \
                 to reduce sampling density.",
                channels.len()
            ));
        }

        ok_json(&SnapshotView {
            identity: snapshot.identity,
            completeness: snapshot.completeness,
            domain: snapshot.domain,
            plugins: &snapshot.plugins,
            channels,
            diagnostics: &snapshot.diagnostics,
        })
    }

    #[tool(
        description = "Structured diagnostics produced for the latest complete snapshot. The snapshot identity supplies the run, world revision, and time this assessment describes."
    )]
    async fn get_diagnostics(&self) -> Result<CallToolResult, ErrorData> {
        let Some(snapshot) = lock(&self.model).latest_snapshot() else {
            return tool_error("no snapshot has been published yet");
        };
        ok_json(&DiagnosticsResult {
            snapshot: snapshot.identity,
            diagnostics: snapshot.diagnostics.as_ref().to_vec(),
        })
    }

    // Description intentionally mirrors `FieldDataSource::body_forces`'s own
    // doc comment (fieldcad-simulation/src/source.rs) — keep the two in sync.
    #[tool(
        description = "Forces the dynamics system produced on its most recent tick, in SI newtons. A body with no entry covers every reason there is nothing to show for it: no mass component attached, pinned (motion is authored, not solver-integrated), kinematically owned by a solver's own pusher rather than the shared dynamics system, or no tick has run yet. Attaching a charge/mass component alone does not guarantee an entry — mass specifically is required for force integration."
    )]
    async fn get_body_forces(
        &self,
        Parameters(params): Parameters<GetBodyForcesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let forces = lock(&self.model).body_forces();
        let values = forces
            .into_iter()
            .filter(|(object, _)| params.object_id.is_none_or(|wanted| object.get() == wanted))
            .map(|(object, force)| BodyForceResult {
                object_id: object.get(),
                force_newtons: [force.x, force.y, force.z],
            })
            .collect::<Vec<_>>();
        ok_json(&values)
    }

    #[tool(
        description = "Every component schema registered by an active plugin: property IDs, display names, dimensioned kind, required flag, relevance condition, and default value. Discover what an edit_world component attach/AttachComponent payload must contain before authoring one; the same schemas are also inline in get_world's component_schemas."
    )]
    async fn list_component_schemas(&self) -> Result<CallToolResult, ErrorData> {
        let world = lock(&self.model).world();
        ok_json(&world.component_schemas().values().collect::<Vec<_>>())
    }

    #[tool(
        description = "Apply one atomic, revisioned transaction of typed world-mutation commands: create/edit/remove objects, slice planes, field boxes, field spheres, and probes, and attach/detach/edit object components. Entity references are stable numeric IDs (from get_world, or a previous call's receipt.created); component/channel references are {plugin, name} pairs; component-property values are validated against the schema discovered through list_component_schemas before submission, so a mismatched value is rejected with the property's expected kind rather than a raw deserialization error. Prefer this over commit_world, which takes untyped native WorldCommand JSON with no schema discovery and only reports the authority's raw rejection."
    )]
    async fn edit_world(
        &self,
        Parameters(params): Parameters<EditWorldParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let world = lock(&self.model).world();
        let schemas = world.component_schemas();
        let commands: Vec<WorldCommand> = match params
            .commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                into_world_command(schemas, command)
                    .map_err(|error| format!("invalid command at index {index}: {error}"))
            })
            .collect()
        {
            Ok(commands) => commands,
            Err(error) => return tool_error(error),
        };
        drop(world);
        submit_world_commands(&self.model, commands).await
    }

    #[tool(
        description = "Apply one atomic, revisioned transaction of raw WorldCommand JSON (create/edit/remove objects, planes, probes). Legacy/compatibility path kept while edit_world's typed coverage settles: edit_world discovers its schema from tool definitions and validates component-property values before submission, which this cannot."
    )]
    async fn commit_world(
        &self,
        Parameters(params): Parameters<CommitWorldParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let commands: Vec<WorldCommand> = match params
            .commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                serde_json::from_value(command)
                    .map_err(|error| format!("invalid command at index {index}: {error}"))
            })
            .collect()
        {
            Ok(commands) => commands,
            Err(error) => return tool_error(error),
        };
        submit_world_commands(&self.model, commands).await
    }

    #[tool(description = "Start the run: fixed simulation ticks advance until paused.")]
    async fn play(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::Play).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(description = "Pause the run.")]
    async fn pause(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::Pause).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(description = "Advance exactly one fixed time step while paused.")]
    async fn step(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::Step).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(description = "Set the fixed numerical time step, in seconds.")]
    async fn set_time_step(
        &self,
        Parameters(params): Parameters<SetTimeStepParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Ok(step) = TimeStep::from_seconds(params.seconds) else {
            return tool_error("time step must be finite and greater than zero");
        };
        match submit_and_wait(&self.model, CommandPayload::SetTimeStep(step)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Set how many metres one render/camera unit represents. Purely a presentation setting for a viewport's camera range and gizmo/proxy sizing — never changes any stored object position, size, or physical constant. Defaults to 1.0 (metre scale). Use e.g. 1e-9 for a nanometre-scale scene or 1.495978707e11 for an astronomical-unit scale. Read back via the session-status resource."
    )]
    async fn set_scene_scale(
        &self,
        Parameters(params): Parameters<SetSceneScaleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Ok(scale) = SceneScale::from_metres(params.metres_per_unit) else {
            return tool_error("scene scale must be finite and greater than zero");
        };
        match submit_and_wait(&self.model, CommandPayload::SetSceneScale(scale)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Replace the numerical domain: bounds in metres, solver-cell resolution, one boundary condition per axis, and precision. The authority validates the full candidate, queues it at the next tick boundary if running, rebuilds solver state, and resets to paused t=0. If the existing dt is unsafe for the new lattice, it chooses 80% of the strictest active solver limit."
    )]
    async fn reconfigure_domain(
        &self,
        Parameters(params): Parameters<ReconfigureDomainParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let domain = match params.domain() {
            Ok(domain) => domain,
            Err(error) => return tool_error(error.to_string()),
        };
        let receipt =
            match submit_and_wait(&self.model, CommandPayload::ReconfigureDomain(domain)).await {
                Ok(receipt) => receipt,
                Err(error) => return tool_error(error.to_string()),
            };
        let server = lock(&self.model);
        ok_json(&ReconfigureDomainResult {
            receipt,
            domain: server.domain(),
            simulation: server.simulation_status(),
        })
    }

    #[tool(description = "Set the wall-clock playback speed multiplier. Never changes dt.")]
    async fn set_playback_speed(
        &self,
        Parameters(params): Parameters<SetPlaybackSpeedParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Ok(speed) = PlaybackSpeed::from_multiplier(params.multiplier) else {
            return tool_error("playback speed must be finite and greater than zero");
        };
        match submit_and_wait(&self.model, CommandPayload::SetPlaybackSpeed(speed)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Change what the model samples when it publishes a snapshot. A presentation setting only; never changes the physics. A denser sampling here makes every future get_latest_snapshot response larger — a fine plane_samples_per_axis, a small domain_stride, or many visible boxes/spheres can each multiply the total sample count. Prefer the lowest density your workflow needs; use get_latest_snapshot's own channels/max_samples for a one-off bounded read instead of changing this durable subscription."
    )]
    async fn set_subscription(
        &self,
        Parameters(params): Parameters<SetSubscriptionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let subscription = Subscription {
            probes: params.probes,
            planes: params.plane_samples_per_axis.map(glam::UVec2::splat),
            domain_stride: params.domain_stride,
            boxes: params.box_samples_per_axis.map(glam::UVec3::splat),
            spheres: params.sphere_samples_per_axis,
        };
        match submit_and_wait(&self.model, CommandPayload::SetSubscription(subscription)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(description = "Activate or deactivate one equation system in the scene.")]
    async fn set_field_system_enabled(
        &self,
        Parameters(params): Parameters<SetFieldSystemEnabledParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Ok(plugin) = PluginId::new(params.plugin) else {
            return tool_error("plugin is not a valid identifier");
        };
        let payload = CommandPayload::SetFieldSystemEnabled {
            plugin,
            enabled: params.enabled,
        };
        match submit_and_wait(&self.model, payload).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Choose whether one field system recomputes during intermediate interactive edits or defers until the edit commits. This is a performance/latency setting and does not change the committed physics."
    )]
    async fn set_field_system_realtime(
        &self,
        Parameters(params): Parameters<SetFieldSystemRealtimeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let plugin = match PluginId::new(params.plugin) {
            Ok(plugin) => plugin,
            Err(error) => return tool_error(error.to_string()),
        };
        match submit_and_wait(
            &self.model,
            CommandPayload::SetFieldSystemRealtime {
                plugin,
                realtime: params.realtime,
            },
        )
        .await
        {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Begin a streamed interactive scene edit. Use only when sending intermediate mutations (for example a drag); ordinary agents should submit one final atomic commit_world transaction instead."
    )]
    async fn begin_interactive_edit(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::SetInteractiveEdit(true)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "End a streamed interactive scene edit. This commits the gesture boundary, recomputing systems that deferred intermediate updates."
    )]
    async fn end_interactive_edit(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::SetInteractiveEdit(false)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Choose which active equation system computes a shared field, or none. A field has one model at a time."
    )]
    async fn set_field_model(
        &self,
        Parameters(params): Parameters<SetFieldModelParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Ok(channel_plugin) = PluginId::new(params.channel_plugin) else {
            return tool_error("channel_plugin is not a valid identifier");
        };
        let channel = match ChannelId::new(channel_plugin, params.channel_name) {
            Ok(channel) => channel,
            Err(error) => return tool_error(error.to_string()),
        };
        let provider = match params.provider_plugin {
            Some(id) => match PluginId::new(id) {
                Ok(id) => Some(id),
                Err(error) => return tool_error(error.to_string()),
            },
            None => None,
        };
        match submit_and_wait(
            &self.model,
            CommandPayload::SetFieldModel { channel, provider },
        )
        .await
        {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Restore the scene as it stood before the most recent authored edit. Requires the run to be paused."
    )]
    async fn undo(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::Undo).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(description = "Reapply the edit most recently undone.")]
    async fn redo(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::Redo).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Authoritative queue state: paused flag, ordered pending commands, and recent terminal history (capped at 256). The same data as the fieldcad://session/queue resource."
    )]
    async fn get_queue(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).get_queue())
    }

    #[tool(
        description = "Pause the mutation queue: queued scene/domain edits are held at their tick boundary until resumed. Simulation ticks continue; new eligible mutations are still accepted and appended. Idempotent."
    )]
    async fn pause_queue(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::PauseQueue).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Resume a paused mutation queue: held mutations apply at the next eligible tick boundary, in submission order. Idempotent."
    )]
    async fn resume_queue(&self) -> Result<CallToolResult, ErrorData> {
        match submit_and_wait(&self.model, CommandPayload::ResumeQueue).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }

    #[tool(
        description = "Cancel one command still waiting for a tick boundary. A command already submitted to the compute worker (in flight) cannot be cancelled; an already-applied, rejected, cancelled, unknown, or in-flight id is refused."
    )]
    async fn cancel_queued_command(
        &self,
        Parameters(params): Parameters<CancelQueuedCommandParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = CommandId::new(params.command_id);
        match submit_and_wait(&self.model, CommandPayload::CancelQueuedCommand(target)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
    }
}

impl McpServer {
    /// The dispatch behind [`ServerHandler::read_resource`], factored out as
    /// a plain synchronous method — nothing here needs the request context,
    /// and this shape is directly callable from a test without fabricating
    /// one.
    fn read_resource_content(&self, uri: &str) -> Result<ReadResourceResponse, ErrorData> {
        match uri {
            SESSION_STATUS_URI => {
                let server = lock(&self.model);
                let resource = SessionStatusResource {
                    source: server.status(),
                    simulation: server.simulation_status(),
                    domain: server.domain(),
                    scene_scale: server.scene_scale(),
                };
                drop(server);
                resource_text(uri, &resource)
            }
            SESSION_SNAPSHOT_URI => match lock(&self.model).latest_snapshot() {
                Some(snapshot) => resource_text(
                    uri,
                    &SnapshotView {
                        identity: snapshot.identity,
                        completeness: snapshot.completeness,
                        domain: snapshot.domain,
                        plugins: &snapshot.plugins,
                        channels: snapshot
                            .channels
                            .iter()
                            .map(|(id, channel)| (id.clone(), channel))
                            .collect(),
                        diagnostics: &snapshot.diagnostics,
                    },
                ),
                None => resource_text(uri, &Option::<()>::None),
            },
            SESSION_DIAGNOSTICS_URI => match lock(&self.model).latest_snapshot() {
                Some(snapshot) => resource_text(
                    uri,
                    &DiagnosticsResult {
                        snapshot: snapshot.identity,
                        diagnostics: snapshot.diagnostics.as_ref().to_vec(),
                    },
                ),
                None => resource_text(uri, &Option::<()>::None),
            },
            SESSION_QUEUE_URI => {
                let queue: QueueStatus = lock(&self.model).get_queue();
                resource_text(uri, &queue)
            }
            other => Err(ErrorData::resource_not_found(
                format!("unknown resource: {other}"),
                None,
            )),
        }
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_instructions(
            "Field CAD simulation model. Tools mirror the desktop app's own command surface: \
             read the world/status/field-systems/snapshot, author the scene through commit_world, \
             and control the run through play/pause/step/set_time_step/undo/redo/pause_queue/ \
             resume_queue/cancel_queued_command. Resources fieldcad://session/{status,snapshot,\
             diagnostics,queue} mirror those reads; subscribe via subscriptions/listen for \
             notifications/resources/updated push notifications rather than polling.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(session_resources()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.read_resource_content(&request.uri)
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        // The SDK intersects this with both `requested` and this server's own
        // declared capabilities (`enable_resources_subscribe`) before
        // acknowledging, so accepting everything requested here is safe: a
        // client asking for a URI we don't know about simply never receives
        // a notification for it (see `affected_resource_uris`).
        Some(requested.clone())
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let mut watcher = lock(&self.model).subscribe_events();
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                event = watcher.recv() => {
                    let Some(event) = event else { return Ok(()) };
                    if matches!(event, WatchEvent::Closed) {
                        return Ok(());
                    }
                    for &uri in affected_resource_uris(&event) {
                        let accepted = context
                            .accepted()
                            .resource_subscriptions
                            .as_ref()
                            .is_some_and(|uris| uris.iter().any(|accepted| accepted == uri));
                        if accepted {
                            let _ = context.sink().notify_resource_updated(uri).await;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fieldcad_core::{
        FieldSnapshot, ObjectShape, ObjectSpec, ProbeSpec, Transform, WorldCommand, WorldSnapshot,
        quantities::{ChargeCoulombs, coulomb},
    };
    use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
    use fieldcad_electrostatics::{
        electric_field_channel_id, plugin_id as electrostatics_plugin_id,
    };
    use fieldcad_simulation::{CommandId, CommandSequencer, QueueSummary};
    use rmcp::model::ContentBlock;
    use serde_json::Value;

    use super::*;
    use crate::typed_world::{
        ComponentAttachParam, ComponentRefParam, ObjectShapeParam, ProbePositionParam,
        PropertyValueParam, QuatParam, TransformParam, Vec3Param,
    };

    fn server() -> McpServer {
        let source = fieldcad_server::default_session().expect("default session builds");
        McpServer::new(Arc::new(Mutex::new(HeadlessServer::new(source))))
    }

    /// A tool call that submits a running edit (`commit_world` while
    /// `Play`-ing) always awaits its own *terminal* completion, per this
    /// file's `submit_and_wait` — a `Queued` disposition is never returned to
    /// a synchronous MCP caller, only observable meanwhile through
    /// `get_queue()` from a second, concurrent caller. Every queue-control
    /// test below therefore spawns the edit concurrently rather than
    /// awaiting it inline, and polls `get_queue()` until the edit is visibly
    /// pending before acting on it.
    async fn wait_for_one_pending_command_id(server: &McpServer) -> u64 {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let queue = json_of(&server.get_queue().await.unwrap());
            if let Some(record) = queue["pending"].as_array().unwrap().first() {
                return record["command"].as_u64().expect("command id is a number");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the edit never reached the queue"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Every tool here answers with exactly one text content block containing
    /// JSON — this pulls it out and parses it, the way a real MCP client
    /// would read a tool result.
    fn json_of(result: &CallToolResult) -> Value {
        assert_ne!(
            result.is_error,
            Some(true),
            "unexpected tool error: {result:?}"
        );
        let [ContentBlock::Text(text)] = result.content.as_slice() else {
            panic!(
                "expected exactly one text content block, got {:?}",
                result.content
            );
        };
        serde_json::from_str(&text.text).expect("tool content is valid JSON")
    }

    fn charge_and_probe_commands() -> Vec<Value> {
        let commands = vec![
            WorldCommand::CreateObject(
                ObjectSpec::new("Point charge")
                    .with_transform(Transform::at(glam::DVec3::ZERO).unwrap())
                    .with_shape(ObjectShape::point(0.1).unwrap())
                    .with_component(
                        charge_component_id(),
                        charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                    ),
            ),
            WorldCommand::CreateProbe(ProbeSpec::at(
                "Field probe",
                glam::DVec3::new(1.0, 0.0, 0.0),
                vec![electric_field_channel_id()],
            )),
        ];
        commands
            .into_iter()
            .map(|command| serde_json::to_value(command).expect("WorldCommand is serializable"))
            .collect()
    }

    #[tokio::test]
    async fn a_client_authors_a_scene_and_steps_it_through_tools_only() {
        let server = server();

        let world = json_of(&server.get_world().await.unwrap());
        assert_eq!(world["objects"], serde_json::json!({}));

        let status = json_of(&server.get_simulation_status().await.unwrap());
        assert_eq!(status["clock"]["step"]["tick"], 0);

        let authored = json_of(
            &server
                .commit_world(Parameters(CommitWorldParams {
                    commands: charge_and_probe_commands(),
                }))
                .await
                .unwrap(),
        );
        assert_eq!(authored["disposition"], "Applied");

        let world_after = json_of(&server.get_world().await.unwrap());
        assert_eq!(world_after["objects"].as_object().unwrap().len(), 1);

        let stepped = json_of(&server.step().await.unwrap());
        assert_eq!(stepped["disposition"], "Applied");

        let status_after = json_of(&server.get_simulation_status().await.unwrap());
        assert_eq!(status_after["clock"]["step"]["tick"], 1);

        let snapshot = json_of(
            &server
                .get_latest_snapshot(Parameters(GetLatestSnapshotParams {
                    channels: None,
                    max_samples: None,
                }))
                .await
                .unwrap(),
        );
        assert!(snapshot.get("identity").is_some());
    }

    /// BE-3 regression: `submit_and_wait`'s own tick loop must never call
    /// `HeadlessServer::drain_events` — that buffer is shared with every
    /// other transport observing this session (the embedded desktop UI's
    /// own per-frame drain, notably), and an MCP tool call awaiting its own
    /// command has no business discarding a completely unrelated command's
    /// completion out from under whoever else was going to read it.
    #[tokio::test]
    async fn submit_and_wait_never_drains_the_shared_events_buffer_other_transports_read() {
        let model = Arc::new(Mutex::new(HeadlessServer::new(
            fieldcad_server::default_session().unwrap(),
        )));

        // A second, independent transport (the desktop UI, say) submits its
        // own command directly on the shared model and registers no waiter
        // for it — its only record of completion is the shared,
        // passively-observed events log every transport reads via
        // `drain_events`.
        let other_receipt = {
            let mut server = model.lock().unwrap();
            server.submit(CommandPayload::Play).unwrap()
        };

        // An MCP tool call for a *different* command, dispatched to the same
        // worker thread strictly after the one above: by the time this
        // call's own waiter resolves, the worker has already replied to
        // both, in submission order.
        submit_and_wait(
            &model,
            CommandPayload::SetPlaybackSpeed(PlaybackSpeed::from_multiplier(2.0).unwrap()),
        )
        .await
        .expect("the MCP call's own command must succeed");

        let events = model.lock().unwrap().drain_events();
        assert!(
            events
                .iter()
                .any(|event| event.command_id() == other_receipt.command),
            "the other transport's own completion was lost from the shared \
             events buffer: {events:?}"
        );
    }

    async fn scene_with_a_published_snapshot(server: &McpServer) {
        json_of(
            &server
                .commit_world(Parameters(CommitWorldParams {
                    commands: charge_and_probe_commands(),
                }))
                .await
                .unwrap(),
        );
        json_of(&server.step().await.unwrap());
    }

    fn electric_field_channel_ref() -> ChannelRefParam {
        ChannelRefParam {
            plugin: "fieldcad.electromagnetic-field".to_owned(),
            name: "electric-field".to_owned(),
        }
    }

    #[tokio::test]
    async fn get_latest_snapshot_refuses_an_oversized_read_with_a_structured_breakdown() {
        let server = server();
        scene_with_a_published_snapshot(&server).await;

        let result = server
            .get_latest_snapshot(Parameters(GetLatestSnapshotParams {
                channels: None,
                max_samples: Some(0),
            }))
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        let [ContentBlock::Text(text)] = result.content.as_slice() else {
            panic!("expected one text content block, got {:?}", result.content);
        };
        assert!(text.text.contains("max_samples=0"), "{}", text.text);
        assert!(text.text.contains("electric-field"), "{}", text.text);
    }

    #[tokio::test]
    async fn get_latest_snapshot_can_be_narrowed_to_specific_channels() {
        let server = server();
        scene_with_a_published_snapshot(&server).await;

        let narrowed = json_of(
            &server
                .get_latest_snapshot(Parameters(GetLatestSnapshotParams {
                    channels: Some(vec![electric_field_channel_ref()]),
                    max_samples: None,
                }))
                .await
                .unwrap(),
        );
        let channels = narrowed["channels"].as_object().unwrap();
        assert_eq!(channels.len(), 1);
        assert!(channels.contains_key("fieldcad.electromagnetic-field:electric-field"));

        let empty = json_of(
            &server
                .get_latest_snapshot(Parameters(GetLatestSnapshotParams {
                    channels: Some(vec![ChannelRefParam {
                        plugin: "fieldcad.nonexistent".to_owned(),
                        name: "made-up".to_owned(),
                    }]),
                    max_samples: None,
                }))
                .await
                .unwrap(),
        );
        assert_eq!(empty["channels"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn an_invalid_structured_command_is_reported_as_a_tool_error_not_a_protocol_error() {
        let server = server();
        let result = server
            .commit_world(Parameters(CommitWorldParams {
                commands: vec![serde_json::json!({"not": "a world command"})],
            }))
            .await
            .expect("the call itself succeeds; the failure is inside the result");
        assert_eq!(result.is_error, Some(true));
    }

    fn charge_component_ref() -> ComponentRefParam {
        ComponentRefParam {
            plugin: "fieldcad.electromagnetic-sources".to_owned(),
            name: "charge-source".to_owned(),
        }
    }

    #[tokio::test]
    async fn edit_world_creates_a_negatively_charged_object_and_reports_its_allocated_id() {
        let server = server();

        let created = json_of(
            &server
                .edit_world(Parameters(EditWorldParams {
                    commands: vec![WorldEditParam::CreateObject {
                        name: "Negative point charge".to_owned(),
                        transform: Some(TransformParam {
                            translation: Vec3Param {
                                x: 0.0,
                                y: 0.0,
                                z: -0.6,
                            },
                            rotation: QuatParam::default(),
                        }),
                        velocity: None,
                        shape: Some(ObjectShapeParam::Point { radius_m: 0.15 }),
                        visible: true,
                        pinned: false,
                        components: vec![ComponentAttachParam {
                            component: charge_component_ref(),
                            properties: [(
                                "charge".to_owned(),
                                PropertyValueParam::Scalar { si_value: -1.0e-9 },
                            )]
                            .into_iter()
                            .collect(),
                        }],
                    }],
                }))
                .await
                .unwrap(),
        );
        assert_eq!(created["disposition"], "Applied");
        let object_ids = created["created"]["created_objects"].as_array().unwrap();
        assert_eq!(object_ids.len(), 1);
        let object_id = object_ids[0].as_u64().unwrap();

        let world = json_of(&server.get_world().await.unwrap());
        let object = &world["objects"][object_id.to_string()];
        assert_eq!(object["name"], "Negative point charge");
        assert_eq!(
            object["components"]["fieldcad.electromagnetic-sources:charge-source"]["charge"]["Scalar"]
                ["si_value"],
            -1.0e-9
        );
    }

    #[tokio::test]
    async fn edit_world_rejects_a_property_value_of_the_wrong_kind_with_the_expected_kind() {
        let server = server();

        let result = server
            .edit_world(Parameters(EditWorldParams {
                commands: vec![WorldEditParam::CreateObject {
                    name: "Bad charge".to_owned(),
                    transform: None,
                    velocity: None,
                    shape: None,
                    visible: true,
                    pinned: false,
                    components: vec![ComponentAttachParam {
                        component: charge_component_ref(),
                        properties: [(
                            "charge".to_owned(),
                            PropertyValueParam::Boolean { value: true },
                        )]
                        .into_iter()
                        .collect(),
                    }],
                }],
            }))
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        let [ContentBlock::Text(text)] = result.content.as_slice() else {
            panic!("expected one text content block, got {:?}", result.content);
        };
        assert!(text.text.contains("expected scalar"), "{}", text.text);
        assert!(text.text.contains("charge"), "{}", text.text);
    }

    #[tokio::test]
    async fn edit_world_rejects_an_unregistered_component_and_points_at_discovery() {
        let server = server();

        let result = server
            .edit_world(Parameters(EditWorldParams {
                commands: vec![WorldEditParam::CreateObject {
                    name: "Unknown component".to_owned(),
                    transform: None,
                    velocity: None,
                    shape: None,
                    visible: true,
                    pinned: false,
                    components: vec![ComponentAttachParam {
                        component: ComponentRefParam {
                            plugin: "fieldcad.nonexistent".to_owned(),
                            name: "made-up".to_owned(),
                        },
                        properties: BTreeMap::new(),
                    }],
                }],
            }))
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        let [ContentBlock::Text(text)] = result.content.as_slice() else {
            panic!("expected one text content block, got {:?}", result.content);
        };
        assert!(
            text.text.contains("list_component_schemas"),
            "{}",
            text.text
        );
    }

    #[tokio::test]
    async fn list_component_schemas_matches_get_world_and_a_typed_transaction_can_reference_a_new_object()
     {
        let server = server();

        let schemas = json_of(&server.list_component_schemas().await.unwrap());
        let schema_ids: Vec<&str> = schemas
            .as_array()
            .unwrap()
            .iter()
            .map(|schema| schema["id"].as_str().unwrap())
            .collect();
        assert!(
            schema_ids.contains(&"fieldcad.electromagnetic-sources:charge-source"),
            "{schema_ids:?}"
        );

        // A mixed transaction: create an object, then attach a probe to it by
        // ID, then move it — exercising object/probe creation and reference
        // by allocated ID plus an edit, all in the DSL rather than raw JSON.
        let created = json_of(
            &server
                .edit_world(Parameters(EditWorldParams {
                    commands: vec![
                        WorldEditParam::CreateObject {
                            name: "source".to_owned(),
                            transform: None,
                            velocity: None,
                            shape: None,
                            visible: true,
                            pinned: false,
                            components: Vec::new(),
                        },
                        WorldEditParam::CreatePlane {
                            name: "slice".to_owned(),
                            origin: Vec3Param::default(),
                            normal: Vec3Param {
                                x: 0.0,
                                y: 0.0,
                                z: 1.0,
                            },
                            half_extent: None,
                            u_axis: None,
                            visible: true,
                        },
                    ],
                }))
                .await
                .unwrap(),
        );
        let object_id = created["created"]["created_objects"][0].as_u64().unwrap();
        let plane_id = created["created"]["created_planes"][0].as_u64().unwrap();

        let follow_up = json_of(
            &server
                .edit_world(Parameters(EditWorldParams {
                    commands: vec![
                        WorldEditParam::CreateProbe {
                            name: "attached".to_owned(),
                            position: ProbePositionParam::Attached {
                                object: object_id,
                                offset: Vec3Param::default(),
                            },
                            channels: Vec::new(),
                            visible: true,
                            history_capacity: None,
                        },
                        WorldEditParam::SetTransform {
                            object: object_id,
                            transform: TransformParam {
                                translation: Vec3Param {
                                    x: 1.0,
                                    y: 2.0,
                                    z: 3.0,
                                },
                                rotation: QuatParam::default(),
                            },
                        },
                        WorldEditParam::SetPlaneVisible {
                            plane: plane_id,
                            visible: false,
                        },
                    ],
                }))
                .await
                .unwrap(),
        );
        assert_eq!(follow_up["disposition"], "Applied");

        let world = json_of(&server.get_world().await.unwrap());
        let object = &world["objects"][object_id.to_string()];
        assert_eq!(
            object["transform"]["translation"],
            serde_json::json!([1.0, 2.0, 3.0])
        );
        assert_eq!(world["planes"][plane_id.to_string()]["visible"], false);
        assert_eq!(world["probes"].as_object().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn field_systems_and_edit_history_are_readable() {
        let server = server();

        let systems = json_of(&server.list_field_systems().await.unwrap());
        assert!(systems.as_array().unwrap().len() >= 2, "{systems:?}");

        let history = json_of(&server.get_edit_history().await.unwrap());
        assert_eq!(history["undo"], Value::Null);
    }

    #[tokio::test]
    async fn field_system_realtime_mode_is_set_through_the_tool() {
        let server = server();
        let receipt = json_of(
            &server
                .set_field_system_realtime(Parameters(SetFieldSystemRealtimeParams {
                    plugin: electrostatics_plugin_id().to_string(),
                    realtime: false,
                }))
                .await
                .unwrap(),
        );
        assert_eq!(receipt["disposition"], "Applied");

        let systems = json_of(&server.list_field_systems().await.unwrap());
        let electrostatics = systems
            .as_array()
            .unwrap()
            .iter()
            .find(|system| system["plugin"]["id"] == electrostatics_plugin_id().to_string())
            .expect("electrostatics is listed");
        assert_eq!(electrostatics["realtime"], false);
    }

    #[tokio::test]
    async fn diagnostics_and_body_forces_are_readable_as_dedicated_observations() {
        let server = server();

        let diagnostics = json_of(&server.get_diagnostics().await.unwrap());
        assert!(diagnostics["snapshot"].is_object());
        assert!(diagnostics["diagnostics"].is_array());

        let forces = json_of(
            &server
                .get_body_forces(Parameters(GetBodyForcesParams { object_id: None }))
                .await
                .unwrap(),
        );
        assert_eq!(forces, serde_json::json!([]));
    }

    #[tokio::test]
    async fn queue_tools_pause_hold_and_resume_a_running_edit() {
        let server = server();
        // Pausing before submitting the edit (rather than racing to pause
        // after) is what makes this deterministic: nothing can flush a
        // command appended to an already-paused queue, no matter how many
        // real ticks a concurrent `submit_and_wait` pump advances meanwhile.
        server.pause_queue().await.unwrap();
        server.play().await.unwrap();

        let commit_server = server.clone();
        let commit = tokio::spawn(async move {
            commit_server
                .commit_world(Parameters(CommitWorldParams {
                    commands: charge_and_probe_commands(),
                }))
                .await
        });
        wait_for_one_pending_command_id(&server).await;

        let queue = json_of(&server.get_queue().await.unwrap());
        assert_eq!(queue["paused"], true);
        assert_eq!(queue["pending"].as_array().unwrap().len(), 1);
        assert!(!commit.is_finished(), "a paused queue must hold the edit");

        let resumed = json_of(&server.resume_queue().await.unwrap());
        assert_eq!(resumed["disposition"], "Applied");

        // `Pause` flushes the queue unconditionally once it's not paused --
        // `Step` would do the same, but only while already paused, and this
        // session is still `Play`-ing.
        let paused_run = json_of(&server.pause().await.unwrap());
        assert_eq!(paused_run["disposition"], "Applied");

        let committed = json_of(&commit.await.unwrap().unwrap());
        assert_eq!(committed["disposition"], "Applied");

        let queue_after = json_of(&server.get_queue().await.unwrap());
        assert!(queue_after["pending"].as_array().unwrap().is_empty());
        assert_eq!(queue_after["history"].as_array().unwrap().len(), 1);

        let world = json_of(&server.get_world().await.unwrap());
        assert_eq!(world["objects"].as_object().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancel_queued_command_prevents_its_application() {
        let server = server();
        server.pause_queue().await.unwrap();
        server.play().await.unwrap();

        let commit_server = server.clone();
        let commit = tokio::spawn(async move {
            commit_server
                .commit_world(Parameters(CommitWorldParams {
                    commands: charge_and_probe_commands(),
                }))
                .await
        });
        let queued_command_id = wait_for_one_pending_command_id(&server).await;

        let cancelled = json_of(
            &server
                .cancel_queued_command(Parameters(CancelQueuedCommandParams {
                    command_id: queued_command_id,
                }))
                .await
                .unwrap(),
        );
        assert_eq!(cancelled["disposition"], "Applied");

        // The cancelled command's own waiter resolves with a rejection, not
        // a hang.
        let commit_result = commit.await.unwrap().unwrap();
        assert_eq!(commit_result.is_error, Some(true));
        let [ContentBlock::Text(text)] = commit_result.content.as_slice() else {
            panic!(
                "expected one text content block, got {:?}",
                commit_result.content
            );
        };
        assert!(
            text.text.contains("cancelled"),
            "the cancelled command's own waiter should report cancellation: {}",
            text.text
        );

        let queue = json_of(&server.get_queue().await.unwrap());
        assert!(queue["pending"].as_array().unwrap().is_empty());
        let history = queue["history"].as_array().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["state"], "cancelled");

        // Nothing left to flush: resuming and forcing a boundary must not
        // resurrect the cancelled edit.
        server.resume_queue().await.unwrap();
        server.pause().await.unwrap();
        let world = json_of(&server.get_world().await.unwrap());
        assert!(world["objects"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn pause_step_redo_are_refused_while_the_queue_is_paused_with_pending_work() {
        for tool in ["pause", "step", "redo"] {
            let server = server();
            server.pause_queue().await.unwrap();
            server.play().await.unwrap();

            let commit_server = server.clone();
            let commit = tokio::spawn(async move {
                commit_server
                    .commit_world(Parameters(CommitWorldParams {
                        commands: charge_and_probe_commands(),
                    }))
                    .await
            });
            wait_for_one_pending_command_id(&server).await;

            let result = match tool {
                "pause" => server.pause().await.unwrap(),
                "step" => server.step().await.unwrap(),
                "redo" => server.redo().await.unwrap(),
                _ => unreachable!(),
            };
            assert_eq!(
                result.is_error,
                Some(true),
                "{tool} should be refused: {result:?}"
            );
            let [ContentBlock::Text(text)] = result.content.as_slice() else {
                panic!("expected one text content block, got {:?}", result.content);
            };
            assert!(
                text.text.contains("queue is paused"),
                "{tool}'s rejection should name the paused queue: {}",
                text.text
            );

            // Still hanging on its own never-to-arrive terminal completion:
            // nothing in this iteration resumes or cancels it.
            commit.abort();
        }
    }

    /// Unlike `pause`/`step`/`redo`, `undo` does not reject a still-pending
    /// edit — it cancels it, since the edit was never recorded in history to
    /// begin with. Mirrors `cancel_queued_command_prevents_its_application`.
    #[tokio::test]
    async fn undo_cancels_a_queued_command_instead_of_being_refused() {
        let server = server();
        server.pause_queue().await.unwrap();
        server.play().await.unwrap();

        let commit_server = server.clone();
        let commit = tokio::spawn(async move {
            commit_server
                .commit_world(Parameters(CommitWorldParams {
                    commands: charge_and_probe_commands(),
                }))
                .await
        });
        wait_for_one_pending_command_id(&server).await;

        let undone = json_of(&server.undo().await.unwrap());
        assert_eq!(undone["disposition"], "Applied");

        // The cancelled command's own waiter resolves with a rejection, not
        // a hang.
        let commit_result = commit.await.unwrap().unwrap();
        assert_eq!(commit_result.is_error, Some(true));
        let [ContentBlock::Text(text)] = commit_result.content.as_slice() else {
            panic!(
                "expected one text content block, got {:?}",
                commit_result.content
            );
        };
        assert!(
            text.text.contains("cancelled"),
            "the undone command's own waiter should report cancellation: {}",
            text.text
        );

        let queue = json_of(&server.get_queue().await.unwrap());
        assert!(queue["pending"].as_array().unwrap().is_empty());
        let history = queue["history"].as_array().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["state"], "cancelled");

        // Nothing left to flush: resuming and forcing a boundary must not
        // resurrect the cancelled edit.
        server.resume_queue().await.unwrap();
        server.pause().await.unwrap();
        let world = json_of(&server.get_world().await.unwrap());
        assert!(world["objects"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn session_resources_lists_exactly_the_four_stable_uris() {
        let resources = session_resources();
        let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                "fieldcad://session/status",
                "fieldcad://session/snapshot",
                "fieldcad://session/diagnostics",
                "fieldcad://session/queue",
            ]
        );
    }

    #[tokio::test]
    async fn read_resource_matches_the_equivalent_tool_reads() {
        let server = server();

        let ReadResourceResponse::Complete(status) =
            server.read_resource_content(SESSION_STATUS_URI).unwrap()
        else {
            panic!("expected a complete resource read");
        };
        let ResourceContents::TextResourceContents { text, .. } = &status.contents[0] else {
            panic!("expected text resource contents");
        };
        let status_json: Value = serde_json::from_str(text).unwrap();
        assert!(status_json["simulation"]["clock"].is_object());

        let ReadResourceResponse::Complete(queue) =
            server.read_resource_content(SESSION_QUEUE_URI).unwrap()
        else {
            panic!("expected a complete resource read");
        };
        let ResourceContents::TextResourceContents { text, .. } = &queue.contents[0] else {
            panic!("expected text resource contents");
        };
        let queue_json: Value = serde_json::from_str(text).unwrap();
        let tool_queue = json_of(&server.get_queue().await.unwrap());
        assert_eq!(queue_json, tool_queue);

        let unknown = server.read_resource_content("fieldcad://session/nonexistent");
        assert!(unknown.is_err(), "an unknown resource URI must be refused");
    }

    #[tokio::test]
    async fn set_scene_scale_is_reflected_in_session_status() {
        let server = server();

        let ReadResourceResponse::Complete(before) =
            server.read_resource_content(SESSION_STATUS_URI).unwrap()
        else {
            panic!("expected a complete resource read");
        };
        let ResourceContents::TextResourceContents { text, .. } = &before.contents[0] else {
            panic!("expected text resource contents");
        };
        let before_json: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            before_json["scene_scale"], 1.0,
            "default scale is 1 metre per unit"
        );

        let result = server
            .set_scene_scale(Parameters(SetSceneScaleParams {
                metres_per_unit: 1.0e-9,
            }))
            .await
            .unwrap();
        json_of(&result);

        let ReadResourceResponse::Complete(after) =
            server.read_resource_content(SESSION_STATUS_URI).unwrap()
        else {
            panic!("expected a complete resource read");
        };
        let ResourceContents::TextResourceContents { text, .. } = &after.contents[0] else {
            panic!("expected text resource contents");
        };
        let after_json: Value = serde_json::from_str(text).unwrap();
        assert_eq!(after_json["scene_scale"], 1.0e-9);

        let rejected = server
            .set_scene_scale(Parameters(SetSceneScaleParams {
                metres_per_unit: 0.0,
            }))
            .await
            .unwrap();
        assert_eq!(rejected.is_error, Some(true));
    }

    #[test]
    fn a_lag_marker_invalidates_every_stable_resource() {
        let affected = affected_resource_uris(&WatchEvent::Lagged);
        assert_eq!(affected, SESSION_RESOURCE_URIS);
    }

    #[test]
    fn a_closed_hub_invalidates_no_resources() {
        assert_eq!(
            affected_resource_uris(&WatchEvent::Closed),
            &[] as &[&str],
            "a closed hub has no resources to invalidate"
        );
    }

    #[test]
    fn a_queue_event_invalidates_only_the_queue_resource() {
        let event = WatchEvent::Session(SessionEvent::QueueUpdated(QueueSummary {
            paused: false,
            pending_len: 0,
            history_len: 0,
            newest_history: None,
        }));
        assert_eq!(affected_resource_uris(&event), &[SESSION_QUEUE_URI]);
    }

    #[tokio::test]
    async fn streamed_interactive_edits_have_explicit_begin_and_end_tools() {
        let server = server();
        let began = json_of(&server.begin_interactive_edit().await.unwrap());
        assert_eq!(began["disposition"], "Applied");

        let ended = json_of(&server.end_interactive_edit().await.unwrap());
        assert_eq!(ended["disposition"], "Applied");
    }

    #[tokio::test]
    async fn reconfiguring_the_domain_reports_the_reset_authoritative_state() {
        let server = server();
        let result = json_of(
            &server
                .reconfigure_domain(Parameters(ReconfigureDomainParams {
                    min_x_m: -2.0,
                    min_y_m: -3.0,
                    min_z_m: -4.0,
                    max_x_m: 2.0,
                    max_y_m: 3.0,
                    max_z_m: 4.0,
                    cells_x: 8,
                    cells_y: 12,
                    cells_z: 16,
                    boundary_x: BoundaryConditionParam::Periodic,
                    boundary_y: BoundaryConditionParam::Absorbing,
                    boundary_z: BoundaryConditionParam::Open,
                    precision: PrecisionParam::F64,
                }))
                .await
                .unwrap(),
        );

        assert_eq!(result["receipt"]["disposition"], "Applied");
        assert_eq!(
            result["domain"]["resolution"]["cells"],
            serde_json::json!([8, 12, 16])
        );
        assert_eq!(result["domain"]["precision"], "F64");
        assert_eq!(result["simulation"]["clock"]["mode"], "Paused");
        assert_eq!(result["simulation"]["clock"]["step"]["tick"], 0);
        assert_eq!(result["simulation"]["run_generation"], 1);
    }

    /// The same log line format `fieldcad_simulation`'s own
    /// `observed_script` uses (ADR 0001's local/loopback parity test) —
    /// computed from real deserialized types on either side, so the direct
    /// and MCP scripts below share the exact same formatting logic and
    /// cannot drift apart from each other.
    fn parity_log_line(
        snapshot: Option<&FieldSnapshot>,
        status: &SimulationStatus,
        world: &WorldSnapshot,
    ) -> String {
        match snapshot {
            Some(snapshot) => format!(
                "seq={} rev={} tick={} mode={} samples={} freshness={} objects={} world_rev={}",
                snapshot.identity.sequence,
                snapshot.identity.world_revision,
                snapshot.identity.tick,
                status.mode().label(),
                snapshot.total_samples(),
                snapshot.freshness_against(status.world_revision).label(),
                world.objects().len(),
                world.revision(),
            ),
            None => "no data".to_owned(),
        }
    }

    /// Poll until nothing further arrives from `AsyncLocalDataSource`'s
    /// background worker thread (ADR 0011). `observed_script`
    /// (`fieldcad_simulation`'s local/loopback parity test) settles with a
    /// fixed handful of zero-duration polls, which is enough for
    /// `LocalDataSource`/`LoopbackDataSource` — both synchronous, no worker
    /// thread involved. `HeadlessServer` wraps a real worker thread, so a
    /// fixed count of immediately-back-to-back polls isn't a wait at all: in
    /// practice the worker never got scheduled in that window and every
    /// poll below observed the same unfinished state (this was this test's
    /// first failure — the direct-side log never advanced past its first
    /// entry). "Settled" has to be detected — quiet for a few consecutive
    /// polls, with `thread::yield_now()` between them to actually give the
    /// worker a turn — not assumed after N iterations.
    fn settle(source: &mut dyn FieldDataSource) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut quiet_polls = 0;
        loop {
            let outcome = source.poll(Duration::ZERO).unwrap();
            let quiet = !outcome.snapshot_updated
                && outcome.ticks_advanced == 0
                && outcome.commands_applied == 0
                && source.pending_command_count() == 0;
            quiet_polls = if quiet { quiet_polls + 1 } else { 0 };
            if quiet_polls >= 3 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not settle in time"
            );
            std::thread::yield_now();
        }
    }

    /// Drive a session directly through `FieldDataSource`, mirroring
    /// `fieldcad_simulation`'s own `observed_script`
    /// (`local_and_loopback_sources_are_interchangeable_for_consumers`) —
    /// settling manually between commands, since `AsyncLocalDataSource`
    /// resolves non-blocking submissions on a background worker thread.
    /// Adapted from that script: this one drops its final probe-history
    /// line, since there is no MCP tool for probe history yet (deliberately
    /// deferred — see this crate's module docs) — dropping the line keeps
    /// this comparing only what both sides can actually observe, rather
    /// than adding a probe-history tool just to make the two sides
    /// comparable.
    fn direct_script(source: &mut dyn FieldDataSource) -> Vec<String> {
        let mut sequencer = CommandSequencer::default();
        let mut log = Vec::new();

        let record = |source: &mut dyn FieldDataSource, log: &mut Vec<String>| {
            log.push(parity_log_line(
                source.latest_snapshot().as_deref(),
                &source.simulation_status(),
                &source.world(),
            ));
        };

        settle(source);
        record(source, &mut log);

        let receipt = source
            .execute(sequencer.issue(CommandPayload::Step))
            .unwrap();
        assert_eq!(receipt.command, CommandId::new(0));
        settle(source);
        record(source, &mut log);

        source
            .execute(sequencer.issue(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("added")),
            ])))
            .unwrap();
        settle(source);
        record(source, &mut log);

        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();
        source.poll(Duration::from_millis(250)).unwrap();
        settle(source);
        record(source, &mut log);

        source
            .execute(sequencer.issue(CommandPayload::Pause))
            .unwrap();
        settle(source);
        record(source, &mut log);

        log
    }

    fn parsed<T: serde::de::DeserializeOwned>(result: &CallToolResult) -> T {
        serde_json::from_value(json_of(result)).expect("tool content matches the expected type")
    }

    /// Drive an equivalent session purely through MCP tool calls. Unlike
    /// `direct_script`, no manual settling is needed between commands: every
    /// tool call already awaits its own command's completion via
    /// `submit_and_wait` before returning.
    ///
    /// `model` is the same `Arc<Mutex<HeadlessServer>>` `mcp` wraps. Letting
    /// simulated time actually advance while `Play` is active is not
    /// something any MCP tool exposes — in production that role belongs to
    /// the standalone binary's poll loop or the embedded desktop app's
    /// per-frame pump, and neither exists in this test, so the test plays
    /// that role directly here, the same way `direct_script`'s
    /// `source.poll(Duration::from_millis(250))` (plus its trailing
    /// `settle`) does on the other side.
    async fn mcp_script(mcp: &McpServer, model: &Arc<Mutex<HeadlessServer>>) -> Vec<String> {
        let mut log = Vec::new();

        async fn record(mcp: &McpServer, log: &mut Vec<String>) {
            let snapshot_result = mcp
                .get_latest_snapshot(Parameters(GetLatestSnapshotParams {
                    channels: None,
                    max_samples: None,
                }))
                .await
                .unwrap();
            let snapshot: Option<FieldSnapshot> = if snapshot_result.is_error == Some(true) {
                None
            } else {
                Some(parsed(&snapshot_result))
            };
            let status: SimulationStatus = parsed(&mcp.get_simulation_status().await.unwrap());
            let world: WorldSnapshot = parsed(&mcp.get_world().await.unwrap());
            log.push(parity_log_line(snapshot.as_ref(), &status, &world));
        }

        record(mcp, &mut log).await;

        let stepped = json_of(&mcp.step().await.unwrap());
        assert_eq!(stepped["disposition"], "Applied");
        record(mcp, &mut log).await;

        let authored = json_of(
            &mcp.commit_world(Parameters(CommitWorldParams {
                commands: vec![
                    serde_json::to_value(WorldCommand::CreateObject(ObjectSpec::new("added")))
                        .unwrap(),
                ],
            }))
            .await
            .unwrap(),
        );
        assert_eq!(authored["disposition"], "Applied");
        record(mcp, &mut log).await;

        mcp.play().await.unwrap();
        {
            let mut server = model.lock().unwrap();
            server.advance(Duration::from_millis(250)).unwrap();
            settle(&mut *server);
        }
        record(mcp, &mut log).await;

        mcp.pause().await.unwrap();
        record(mcp, &mut log).await;

        log
    }

    /// Test it the way ADR 0001 tests locality: drive one session entirely
    /// through the MCP surface and assert the resulting observable state is
    /// identical to the same script of commands submitted directly through
    /// `FieldDataSource`. This is what makes "MCP is just another
    /// transport" a checked property instead of a claim, exactly the way
    /// ADR 0001's `local_and_loopback_sources_are_interchangeable_for_consumers`
    /// already is for the local/loopback boundary. See docs/mcp-plan.md
    /// phase 7.
    #[tokio::test]
    async fn mcp_and_direct_sources_are_interchangeable_for_consumers() {
        let mut direct = HeadlessServer::new(
            fieldcad_server::default_session().expect("default session builds"),
        );

        let model = Arc::new(Mutex::new(HeadlessServer::new(
            fieldcad_server::default_session().expect("default session builds"),
        )));
        let mcp = McpServer::new(Arc::clone(&model));

        let direct_log = direct_script(&mut direct);
        let mcp_log = mcp_script(&mcp, &model).await;

        assert_eq!(direct_log, mcp_log);
    }
}
