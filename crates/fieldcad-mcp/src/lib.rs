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
//! mutation, experiment (field system) configuration, subscriptions, the
//! latest snapshot, and undo/redo. Left for later, because the underlying
//! capability doesn't exist in the model yet or needs its own design: scene
//! lifecycle (create/open/save), particle templates, rename, probe history
//! and trajectories as retained server-side series, diagnostics as a
//! dedicated read (today folded into the snapshot), run comparison,
//! record/replay, export, and push notifications for snapshot/status/
//! diagnostic events (`watch_session` — everything here is pull, via a tool
//! call, not yet a resource subscription).
//!
//! World commands too varied to give a typed MCP schema in this slice
//! (`commit_world`) are accepted as a JSON-encoded string of
//! [`fieldcad_core::WorldCommand`] values rather than a native MCP input
//! schema: those types are not `schemars::JsonSchema`, and deriving it across
//! all of `fieldcad-core` is bigger than this slice. Every other tool takes
//! plain primitives so its schema is exact.

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, ChannelId, Domain, DomainBounds, PluginId, Precision,
    Resolution, SnapshotIdentity, SolverDiagnostic, TimeStep, WorldCommand,
};
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{
    CommandEvent, CommandPayload, CommandReceipt, FieldDataSource, PlaybackSpeed,
    SimulationStatus, SourceError, Subscription,
};
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

mod transport;
pub use transport::{McpConnections, bind_http, bind_unix, generate_token, run_stdio, serve_http};
#[cfg(unix)]
pub use transport::serve_unix;

/// How long a tool call waits for its own command to complete before giving
/// up. Generous: this is a ceiling against something never completing (a
/// wedged solver, a lost worker), not a latency budget — a healthy command
/// resolves in well under a second.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

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
/// mistaken for this one's. The periodic `tick` below exists only for a
/// *standalone* `fieldcad-mcp` with no other transport attached: nothing
/// else would ever call `advance`/`drain_events` to let a worker's result
/// become visible at all. When embedded, the desktop's own per-frame pump
/// usually resolves the waiter first and the tick is a harmless no-op.
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
                let mut server = lock(model);
                server.advance(Duration::ZERO)?;
                server.drain_events();
            }
        }
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

    #[tool(description = "What the model currently samples when it publishes a snapshot.")]
    async fn get_subscription(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&lock(&self.model).subscription())
    }

    #[tool(
        description = "The latest complete field snapshot: channel batches, revision, tick, and diagnostics."
    )]
    async fn get_latest_snapshot(&self) -> Result<CallToolResult, ErrorData> {
        match lock(&self.model).latest_snapshot() {
            Some(snapshot) => ok_json(snapshot.as_ref()),
            None => tool_error("no snapshot has been published yet"),
        }
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

    #[tool(
        description = "Forces the dynamics system produced on its most recent tick, in SI newtons. Bodies without an entry were not force-integrated (for example pinned or solver-owned bodies), or no tick has run yet."
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
        description = "Apply one atomic, revisioned transaction of world commands (create/edit/remove objects, planes, probes)."
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
        match submit_and_wait(&self.model, CommandPayload::CommitWorld(commands)).await {
            Ok(receipt) => ok_json(&receipt),
            Err(error) => tool_error(error.to_string()),
        }
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
        let receipt = match submit_and_wait(&self.model, CommandPayload::ReconfigureDomain(domain)).await {
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
        description = "Change what the model samples when it publishes a snapshot. A presentation setting only; never changes the physics."
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
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Field CAD simulation model. Tools mirror the desktop app's own command surface: \
             read the world/status/field-systems/snapshot, author the scene through commit_world, \
             and control the run through play/pause/step/set_time_step/undo/redo.",
        )
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectShape, ObjectSpec, ProbeSpec, Transform, WorldCommand};
    use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
    use fieldcad_electrostatics::{electric_field_channel_id, plugin_id as electrostatics_plugin_id};
    use rmcp::model::ContentBlock;
    use serde_json::Value;

    use super::*;

    fn server() -> McpServer {
        let source = fieldcad_server::default_session().expect("default session builds");
        McpServer::new(Arc::new(Mutex::new(HeadlessServer::new(source))))
    }

    /// Every tool here answers with exactly one text content block containing
    /// JSON — this pulls it out and parses it, the way a real MCP client
    /// would read a tool result.
    fn json_of(result: &CallToolResult) -> Value {
        assert_ne!(result.is_error, Some(true), "unexpected tool error: {result:?}");
        let [ContentBlock::Text(text)] = result.content.as_slice() else {
            panic!("expected exactly one text content block, got {:?}", result.content);
        };
        serde_json::from_str(&text.text).expect("tool content is valid JSON")
    }

    fn charge_and_probe_commands() -> Vec<Value> {
        let commands = vec![
            WorldCommand::CreateObject(
                ObjectSpec::new("Point charge")
                    .with_transform(Transform::at(glam::DVec3::ZERO).unwrap())
                    .with_shape(ObjectShape::point(0.1).unwrap())
                    .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
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

        let snapshot = json_of(&server.get_latest_snapshot().await.unwrap());
        assert!(snapshot.get("identity").is_some());
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
        assert_eq!(result["domain"]["resolution"]["cells"], serde_json::json!([8, 12, 16]));
        assert_eq!(result["domain"]["precision"], "F64");
        assert_eq!(result["simulation"]["clock"]["mode"], "Paused");
        assert_eq!(result["simulation"]["clock"]["step"]["tick"], 0);
        assert_eq!(result["simulation"]["run_generation"], 1);
    }
}
