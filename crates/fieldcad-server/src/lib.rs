//! The headless owner of the simulation model.
//!
//! Field CAD follows an Elm-style split: one authoritative model, and
//! commands that mutate it. The desktop UI is one source of commands; this
//! crate is where that boundary stops being desktop-shaped. [`HeadlessServer`]
//! owns the model — a [`SimulationRuntime`] behind an [`AsyncLocalDataSource`]
//! — with no window, no GPU device, nothing that requires a display. Any
//! transport (an embedded UI, MCP, or another network surface) drives it
//! through the same [`fieldcad_simulation::FieldDataSource`] contract ADR
//! 0001 already defines, so "remote and local sources behave identically" is a
//! property of this crate rather than a promise a transport has to keep.
//!
//! `fieldcad-mcp` is a working transport built on this crate, both as its
//! own standalone binary and embedded inside the desktop app, sharing one
//! session with the desktop UI's own commands.

use std::{collections::BTreeMap, collections::HashMap, sync::Arc, time::Duration};

use fieldcad_core::{
    Domain, FieldSnapshot, ObjectId, SceneScale, SessionId, TimeStep, TimeStepError, WorldSnapshot,
};
use fieldcad_electromagnetism::{ElectromagnetismPlugin, courant_limit};
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_simulation::{
    AsyncLocalDataSource, BodySample, Command, CommandDisposition, CommandEvent, CommandId,
    CommandPayload, CommandReceipt, CommandSequencer, DataSourceStatus, EditHistoryStatus,
    FieldDataSource, FieldSystemStatus, IntegrationScheme, LocalDataSource, PlaybackSpeed,
    PluginRegistration, PollOutcome, QueueStatus, QueueSummary, RuntimeConfig, RuntimeError,
    SimulationRuntime, SimulationStatus, SourceError, Subscription,
};
use glam::DVec3;
use tokio::sync::oneshot;

mod event_hub;
pub use event_hub::{EventHub, EventWatcher, SessionEvent, WatchEvent};

/// Builds the default session: a numerical domain and the same solver
/// composition rule the desktop app uses (one electric field, two candidate
/// models — electrostatics active, Maxwell composed but inactive), starting
/// from an empty world.
///
/// Deliberately empty rather than pre-populated: what scene to author is a
/// client decision (desktop's demo scene is a UI convenience, not part of the
/// server's contract), and a remote client must be able to build up a scene
/// through the same commands a local one would use.
pub fn default_session() -> Result<AsyncLocalDataSource, SessionError> {
    let domain = Domain::centred_cube(5.0, 32)?;
    let time_step = TimeStep::from_seconds(courant_limit(&domain) * 0.8)?;
    let config = server_plugin_catalog().into_iter().fold(
        RuntimeConfig::new(domain, time_step, SessionId::from_u128(1)),
        |config, registration| config.with_plugin_registration(registration),
    );
    let runtime = SimulationRuntime::new(config)?;
    Ok(AsyncLocalDataSource::new(LocalDataSource::new(runtime)))
}

/// The headless server's CPU-only plugin composition: one electric field,
/// two candidate models — electrostatics active, Maxwell composed but
/// inactive. Factored out of [`default_session`] so a scene-lifecycle
/// loader (new/save/load, `fieldcad-scene-document`) can rebuild a session
/// from this same composition rather than a hardcoded one, the way the
/// desktop app's GPU-backed catalog does for its own host.
pub fn server_plugin_catalog() -> Vec<PluginRegistration> {
    vec![
        PluginRegistration::with_default_configuration(Box::new(ElectrostaticsPlugin::new())),
        PluginRegistration::with_default_configuration(Box::new(ElectromagnetismPlugin::new()))
            .with_enabled(false),
    ]
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Domain(#[from] fieldcad_core::DomainError),
    #[error(transparent)]
    TimeStep(#[from] TimeStepError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// The model plus the bookkeeping any command source needs: a place to mint
/// [`CommandId`](fieldcad_simulation::CommandId)s and a wall-clock pacer.
///
/// One `HeadlessServer` is one session. Multiple transports driving the same
/// session share one `HeadlessServer`, the way the desktop app's UI is the
/// sole caller of its own `AsyncLocalDataSource` today — and, once more than
/// one transport shares a session (an embedded UI plus MCP, say), it is the
/// reason two transports can safely mint commands and learn of their
/// completion without racing each other: there is exactly one
/// `CommandSequencer` and exactly one place that drains
/// [`AsyncLocalDataSource::drain_command_events`], no matter how many
/// transports are attached.
pub struct HeadlessServer {
    source: AsyncLocalDataSource,
    sequencer: CommandSequencer,
    /// Registered by [`submit_and_await`](Self::submit_and_await), fulfilled
    /// by [`publish`](Self::publish) the moment a command actually goes
    /// terminal — not by whichever transport next happens to call
    /// [`drain_events`](Self::drain_events), which is what used to make two
    /// concurrent transports race for the same waiter.
    waiters: HashMap<CommandId, oneshot::Sender<CommandEvent>>,
    /// The broadcast hub every transport subscribes to independently. See
    /// `docs/tasks/session-events-and-queue-control.md`.
    hub: EventHub,
    /// Buffered for [`drain_events`](Self::drain_events), refilled by
    /// [`publish`](Self::publish) — the sole call site of
    /// [`AsyncLocalDataSource::drain_command_events`] in this crate, keeping
    /// this crate's one-canonical-drain discipline for the *inner* source
    /// even though publication now also happens here.
    events: Vec<CommandEvent>,
}

impl HeadlessServer {
    pub fn new(source: AsyncLocalDataSource) -> Self {
        Self {
            source,
            sequencer: CommandSequencer::default(),
            waiters: HashMap::new(),
            hub: EventHub::default(),
            events: Vec::new(),
        }
    }

    /// Mint a command identity and submit it. See
    /// [`FieldDataSource::execute`] for what the receipt's disposition means.
    pub fn submit(&mut self, payload: CommandPayload) -> Result<CommandReceipt, SourceError> {
        let command = self.sequencer.issue(payload);
        self.execute(command)
    }

    /// Submit a command whose identity was already minted by the caller —
    /// for a transport that tracks its own client-issued ids rather than
    /// this server's sequencer.
    pub fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        let receipt = self.source.execute(command)?;
        self.publish();
        Ok(receipt)
    }

    /// Mint a command identity, submit it, and register interest in its
    /// completion — atomically, under one call, so no [`publish`](Self::publish)
    /// can land between submission and registration and fulfill the waiter
    /// before anyone is listening for it.
    ///
    /// Returns `None` in the receiver position when the command was applied
    /// or queued rather than submitted non-blockingly: there is nothing to
    /// wait for, the receipt already says everything the caller needs.
    pub fn submit_and_await(
        &mut self,
        payload: CommandPayload,
    ) -> Result<(CommandReceipt, Option<oneshot::Receiver<CommandEvent>>), SourceError> {
        let receipt = self.submit(payload)?;
        if receipt.disposition != CommandDisposition::Submitted {
            return Ok((receipt, None));
        }
        let (tx, rx) = oneshot::channel();
        self.waiters.insert(receipt.command, tx);
        Ok((receipt, Some(rx)))
    }

    /// Advance the model by wall-clock time. Call this on a fixed cadence
    /// (a run loop, a timer) — the numerical `dt` is the model's own
    /// business and never changes to compensate for a slow caller.
    pub fn advance(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        let outcome = self.source.poll(elapsed)?;
        self.publish();
        Ok(outcome)
    }

    /// The one place events leave the inner source and fan out: to whichever
    /// `submit_and_await` waiter is registered for that id, to the broadcast
    /// hub, and into `self.events` for `drain_events()`. Waiter resolution no
    /// longer depends on anyone calling `drain_events()`, which is what
    /// removes "whichever transport calls it next completes every pending
    /// waiter" as a race entirely — both [`execute`](Self::execute) and
    /// [`advance`](Self::advance) fold through here, whether the caller
    /// reached them through this type's own methods or through its
    /// [`FieldDataSource`] impl.
    fn publish(&mut self) {
        for event in self.source.drain_command_events() {
            if let Some(waiter) = self.waiters.remove(&event.command_id()) {
                let _ = waiter.send(event.clone());
            }
            self.hub.publish_command_event(&event);
            self.events.push(event);
        }
        // Prune waiters whose receiver was dropped (caller timed out,
        // disconnected, etc.) — the only removal path was the
        // `waiters.remove` above, which only fires for a completed
        // command.  Without this, an MCP 30 s timeout that drops the
        // receiver leaves the sender in the map forever (BE-8).
        self.waiters.retain(|_id, sender| !sender.is_closed());
        self.hub.publish_state(&self.source);
    }

    /// Completion/rejection/cancellation events for commands submitted
    /// non-blockingly.
    ///
    /// Every transport's completion events, and every registered
    /// [`submit_and_await`](Self::submit_and_await) waiter, are resolved by
    /// [`publish`](Self::publish), not by this call — draining here only
    /// hands back what already accumulated. A transport that only wants "did
    /// my command finish" does not need to call this at all; a transport
    /// that wants a running log of everything (the desktop UI's per-frame
    /// diagnostics) still gets the full list unchanged.
    pub fn drain_events(&mut self) -> Vec<CommandEvent> {
        std::mem::take(&mut self.events)
    }

    /// An independent, non-destructive subscription to this session's
    /// events — any number of callers may hold one at once without
    /// competing with [`drain_events`](Self::drain_events) or with each
    /// other.
    pub fn subscribe_events(&self) -> EventWatcher {
        self.hub.subscribe()
    }

    /// Authoritative queue state: paused flag, ordered pending commands, and
    /// recent terminal history.
    pub fn get_queue(&self) -> QueueStatus {
        self.source.get_queue()
    }

    /// The number of unresolved [`submit_and_await`](Self::submit_and_await)
    /// waiters.
    pub fn waiter_count(&self) -> usize {
        self.waiters.len()
    }

    /// The queue's shape without its contents — see
    /// [`FieldDataSource::queue_summary`]. Delegates to
    /// `AsyncLocalDataSource`'s own cheap implementation rather than the
    /// trait's default (which would derive this from [`Self::get_queue`],
    /// defeating the point).
    pub fn queue_summary(&self) -> QueueSummary {
        self.source.queue_summary()
    }

    pub fn status(&self) -> DataSourceStatus {
        self.source.status()
    }

    pub fn simulation_status(&self) -> SimulationStatus {
        self.source.simulation_status()
    }

    pub fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.source.latest_snapshot()
    }

    pub fn world(&self) -> WorldSnapshot {
        self.source.world()
    }

    pub fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.source.field_systems()
    }

    pub fn edit_history(&self) -> EditHistoryStatus {
        self.source.edit_history()
    }

    pub fn subscription(&self) -> Subscription {
        self.source.subscription()
    }

    pub fn scene_scale(&self) -> SceneScale {
        self.source.scene_scale()
    }

    /// The current authoritative numerical time step.
    pub fn time_step(&self) -> TimeStep {
        self.source.simulation_status().time_step()
    }

    /// Synchronously read the current session's full world contents (with
    /// identifier counters) and pending-queue contents, for durable
    /// storage — see [`fieldcad_core::WorldDocument`] and
    /// [`fieldcad_simulation::QueueDocument`]. A rare, explicit, one-shot
    /// save action; blocking on the compute worker is deliberate, the same
    /// way `AsyncLocalDataSource::capture_document` documents.
    pub fn capture_document(
        &mut self,
    ) -> Result<
        (
            fieldcad_core::WorldDocument,
            fieldcad_simulation::QueueDocument,
        ),
        SourceError,
    > {
        self.source.capture_document()
    }

    /// Ask the worker for `object`'s current recorded kinematics history —
    /// see `AsyncLocalDataSource::request_body_history`. Not part of
    /// `FieldDataSource`: it queues a fetch rather than reading anything,
    /// so it does not belong next to that trait's other read-only
    /// accessors (`body_history` among them).
    pub fn request_body_history(&mut self, object: ObjectId) {
        self.source.request_body_history(object);
    }

    /// Override how many samples `object`'s recorded history keeps — see
    /// `AsyncLocalDataSource::set_body_history_capacity`. Same "not part of
    /// `FieldDataSource`" reasoning as `request_body_history` above: this
    /// sets something rather than reading it.
    pub fn set_body_history_capacity(&mut self, object: ObjectId, capacity: usize) {
        self.source.set_body_history_capacity(object, capacity);
    }

    /// Replace the inner session in place — a new/loaded scene replaces the
    /// world, domain, and field-system composition without disturbing the
    /// `Arc<Mutex<HeadlessServer>>` every attached transport (desktop UI,
    /// embedded MCP) already holds a clone of.
    ///
    /// Every waiter registered by [`submit_and_await`](Self::submit_and_await)
    /// against the *old* session can never resolve — drop their senders so
    /// an awaiting caller gets a clean disconnect instead of hanging
    /// forever. Reset the event hub's change-detection cache: the new
    /// session's first `SnapshotIdentity`/`SimulationStatus` can
    /// coincidentally equal cached values from the old session (both
    /// commonly start at sequence 0 / tick 0), which would otherwise
    /// suppress the first post-replace publish and leave subscribers on
    /// stale state.
    pub fn replace_source(&mut self, source: AsyncLocalDataSource) {
        self.source = source;
        self.waiters.clear();
        self.events.clear();
        self.hub.reset();
        self.publish();
    }
}

impl FieldDataSource for HeadlessServer {
    fn description(&self) -> &str {
        self.source.description()
    }

    fn status(&self) -> DataSourceStatus {
        self.source.status()
    }

    fn simulation_status(&self) -> SimulationStatus {
        self.source.simulation_status()
    }

    fn domain(&self) -> Domain {
        self.source.domain()
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.source.playback_speed()
    }

    fn pending_command_count(&self) -> usize {
        self.source.pending_command_count()
    }

    fn get_queue(&self) -> QueueStatus {
        HeadlessServer::get_queue(self)
    }

    fn queue_summary(&self) -> QueueSummary {
        HeadlessServer::queue_summary(self)
    }

    fn subscription(&self) -> Subscription {
        self.source.subscription()
    }

    fn scene_scale(&self) -> SceneScale {
        self.source.scene_scale()
    }

    fn integration_scheme(&self) -> IntegrationScheme {
        self.source.integration_scheme()
    }

    fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.source.field_systems()
    }

    fn edit_history(&self) -> EditHistoryStatus {
        self.source.edit_history()
    }

    fn world(&self) -> WorldSnapshot {
        self.source.world()
    }

    // Not the trait default: a source that actually holds body forces must
    // say so, or an inspector reading this through `&dyn FieldDataSource`
    // (as the desktop UI does) sees an empty map forever.
    fn body_forces(&self) -> BTreeMap<ObjectId, DVec3> {
        self.source.body_forces()
    }

    // Not the trait default, for the same reason as `body_forces` above.
    fn step_compute_ms(&self) -> f32 {
        self.source.step_compute_ms()
    }

    // Not the trait default, for the same reason as `body_forces` above.
    fn body_history(&self, object: ObjectId) -> Vec<BodySample> {
        self.source.body_history(object)
    }

    // Not `self.source.execute(command)` directly: this must go through
    // `Self::execute` above, which also calls `publish()` — the desktop's
    // per-frame pump reaches this crate only through this trait, and
    // publication (waiter resolution, the broadcast hub) must not be
    // bypassable by that path.
    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        HeadlessServer::execute(self, command)
    }

    // Not `self.source.poll(elapsed)` directly, for the same reason as
    // `execute` above.
    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        HeadlessServer::advance(self, elapsed)
    }

    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.source.latest_snapshot()
    }

    // Not the trait default, and not `self.source.drain_command_events()`
    // either: this must go through `Self::drain_events` above, the one
    // canonical drain point that also resolves `submit_and_await` waiters.
    // Calling the inner source's drain directly here would split the drain
    // across two code paths again — the exact race this type exists to
    // prevent.
    fn drain_command_events(&mut self) -> Vec<CommandEvent> {
        self.drain_events()
    }
}
