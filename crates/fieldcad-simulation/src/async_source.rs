//! Non-blocking local compute adapter.
//!
//! The desktop uses this adapter so GPU dispatch/readback and CPU sampling never
//! run on the window/event-loop thread. The worker still drives the exact same
//! [`LocalDataSource`] contract used by headless tests; this is a scheduling
//! boundary, not a second implementation of simulation semantics.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fieldcad_core::{
    CommitReport, Domain, FieldSnapshot, ObjectId, PluginId, PropertyBag, SceneScale, WorldCommand,
    WorldSnapshot,
};
use fieldcad_dynamics::IntegrationScheme;
use fieldcad_plugin_api::SolverCancellation;
use glam::DVec3;

use crate::{
    BodySample, Command, CommandDisposition, CommandId, CommandKind, CommandPayload,
    CommandReceipt, CommandRecord, DataSourceStatus, EditHistoryStatus, FieldDataSource,
    FieldSystemStatus, LocalDataSource, PlaybackSpeed, PollOutcome, QueueDocument, QueueStatus,
    QueueSummary, SimulationStatus, SnapshotMailbox, SourceError, Subscription,
};

#[derive(Clone, Debug, PartialEq)]
pub enum CommandEvent {
    Completed(CommandReceipt),
    Failed {
        command: CommandId,
        error: SourceError,
    },
    Cancelled(CommandId),
}

impl CommandEvent {
    pub fn command_id(&self) -> CommandId {
        match self {
            Self::Completed(receipt) => receipt.command,
            Self::Failed { command, .. } | Self::Cancelled(command) => *command,
        }
    }
}

enum WorkerRequest {
    Execute(Command),
    Poll(Duration),
    /// A synchronous, save-only read of the full world/queue contents — see
    /// [`AsyncLocalDataSource::capture_document`]. Sent on the ordinary
    /// `requests` channel, not `priority_requests`: a save should reflect
    /// state as of everything already submitted before it, not jump ahead of
    /// a backlog the way a queue-control command does.
    CaptureDocument,
    /// A one-off fetch of one body's recorded kinematics history — see
    /// [`AsyncLocalDataSource::request_body_history`]. Unlike `Poll`, this
    /// does not carry a fresh [`SourceState`]: it exists precisely to keep
    /// history off that always-synced path (see `body_history`'s doc
    /// comment on [`crate::FieldDataSource`]).
    BodyHistory(ObjectId),
    /// Fire-and-forget: override one body's recorded-history depth — see
    /// [`AsyncLocalDataSource::set_body_history_capacity`]. No answering
    /// event; the next `BodyHistory`/`Poll` a caller happens to make will
    /// simply reflect it.
    SetBodyHistoryCapacity(ObjectId, usize),
    /// Advisory, non-mutating check — see
    /// [`AsyncLocalDataSource::validate_world_commands`]. Sent on the
    /// ordinary `requests` channel: a preflight answer that jumped a
    /// backlog of real mutations would describe a world that may no longer
    /// exist by the time the caller acts on it.
    ValidateWorldCommands(Vec<WorldCommand>),
    /// Advisory, non-mutating check — see
    /// [`AsyncLocalDataSource::validate_field_system_configuration`].
    ValidateFieldSystemConfiguration {
        plugin: PluginId,
        configuration: PropertyBag,
    },
    Stop,
}

#[derive(Debug)]
enum WorkerEvent {
    CommandCompleted {
        receipt: CommandReceipt,
        state: SourceState,
        /// Terminal events this command's own execution produced as a side
        /// effect (a running edit flushed by `pause`/`step`/`undo`/`redo`, a
        /// target cancelled by `cancel_queued_command`) — drained from the
        /// worker-side source right away rather than left to accumulate
        /// until some later `Poll` happens to drain them.
        terminal: Vec<CommandEvent>,
    },
    CommandFailed {
        command: CommandId,
        error: SourceError,
        state: SourceState,
        /// Side-effect terminal events, as with `CommandCompleted::terminal`.
        terminal: Vec<CommandEvent>,
    },
    PollCompleted {
        outcome: PollOutcome,
        state: SourceState,
        /// Terminal events the poll's own tick-boundary flush produced —
        /// drained from the worker-side source immediately after `poll`
        /// succeeds, so a command that was `Queued` at submission gets its
        /// completion reported once it actually applies, not before.
        terminal: Vec<CommandEvent>,
    },
    PollFailed(SourceError),
    /// Answer to [`WorkerRequest::CaptureDocument`] — carries no `state`,
    /// since a save-only read changes nothing about the running session.
    DocumentCaptured {
        world: fieldcad_core::WorldDocument,
        queue: QueueDocument,
    },
    /// Answer to [`WorkerRequest::BodyHistory`].
    BodyHistoryCaptured {
        object: ObjectId,
        samples: Vec<BodySample>,
    },
    /// Answer to [`WorkerRequest::ValidateWorldCommands`].
    WorldCommandsValidated(Result<CommitReport, SourceError>),
    /// Answer to [`WorkerRequest::ValidateFieldSystemConfiguration`].
    FieldSystemConfigurationValidated(Result<(), SourceError>),
}

#[derive(Debug)]
struct SourceState {
    simulation: SimulationStatus,
    domain: Domain,
    playback_speed: PlaybackSpeed,
    queue: QueueStatus,
    subscription: Subscription,
    scene_scale: SceneScale,
    integration_scheme: IntegrationScheme,
    field_systems: Vec<FieldSystemStatus>,
    edit_history: EditHistoryStatus,
    world: WorldSnapshot,
    snapshot: Option<Arc<FieldSnapshot>>,
    forces: BTreeMap<ObjectId, DVec3>,
    step_compute_ms: f32,
}

impl SourceState {
    fn capture(source: &LocalDataSource) -> Self {
        Self {
            simulation: source.simulation_status(),
            domain: source.domain(),
            playback_speed: source.playback_speed(),
            queue: source.get_queue(),
            subscription: source.subscription(),
            scene_scale: source.scene_scale(),
            integration_scheme: source.integration_scheme(),
            field_systems: source.field_systems(),
            edit_history: source.edit_history(),
            world: source.world(),
            snapshot: source.latest_snapshot(),
            forces: source.runtime().body_forces(),
            step_compute_ms: source.runtime().last_tick_compute_ms(),
        }
    }
}

/// A local runtime driven on a dedicated compute thread.
pub struct AsyncLocalDataSource {
    requests: Sender<WorkerRequest>,
    /// Queue-control commands (`PauseQueue`/`ResumeQueue`/
    /// `CancelQueuedCommand`) bypass `requests` and go here instead, so they
    /// only ever wait behind whatever the worker is already mid-executing,
    /// never behind a backlog of heavy `CommitWorld` solves — see
    /// `worker_loop`.
    priority_requests: Sender<Command>,
    events: Receiver<WorkerEvent>,
    stop: Arc<AtomicBool>,
    cancellation: SolverCancellation,
    worker: Option<JoinHandle<()>>,
    simulation: SimulationStatus,
    domain: Domain,
    playback_speed: PlaybackSpeed,
    worker_queue: QueueStatus,
    /// Commands sent to the worker but not yet executed there, with the
    /// kind and submission-order sequence needed to synthesize a
    /// `Submitted`-state display record for `get_queue()`.
    submitted_commands: BTreeMap<CommandId, (CommandKind, u64)>,
    subscription: Subscription,
    scene_scale: SceneScale,
    integration_scheme: IntegrationScheme,
    field_systems: Vec<FieldSystemStatus>,
    edit_history: EditHistoryStatus,
    world: WorldSnapshot,
    forces: BTreeMap<ObjectId, DVec3>,
    step_compute_ms: f32,
    mailbox: SnapshotMailbox,
    poll_in_flight: bool,
    /// Latest fetched history per object, populated on demand by
    /// [`Self::request_body_history`] — never by [`Self::adopt`], since
    /// history is deliberately not part of the per-poll `SourceState` sync
    /// (see `FieldDataSource::body_history`'s doc comment).
    body_history_cache: BTreeMap<ObjectId, Vec<BodySample>>,
    /// Objects with a `WorkerRequest::BodyHistory` already sent but not yet
    /// answered, so [`Self::request_body_history`] doesn't flood the worker
    /// channel with a duplicate request every frame while one is in flight.
    body_history_in_flight: BTreeSet<ObjectId>,
    /// The capacity most recently sent to the worker for each object via
    /// [`Self::set_body_history_capacity`], so a caller that recomputes and
    /// re-requests it every frame (as the desktop's trajectory display does,
    /// sized to `trail_seconds / dt`) only actually sends a
    /// `WorkerRequest::SetBodyHistoryCapacity` when the value has changed.
    body_history_capacity: BTreeMap<ObjectId, usize>,
    accumulated_elapsed: Duration,
    /// Monotonically increasing counter assigned as `sequence` on synthetic
    /// `Submitted` records so external sorters (MCP clients) see them in
    /// submission order, not all at sequence 0 (BE-15).
    submission_counter: u64,
    command_events: Vec<CommandEvent>,
    failure: Option<String>,
    #[cfg(test)]
    test_events_tx: mpsc::Sender<WorkerEvent>,
}

impl AsyncLocalDataSource {
    pub fn new(source: LocalDataSource) -> Self {
        let initial = SourceState::capture(&source);
        let cancellation = source.cancellation();
        let mut mailbox = SnapshotMailbox::default();
        if let Some(snapshot) = &initial.snapshot {
            let _ = mailbox.offer(Arc::clone(snapshot));
        }
        let (request_sender, request_receiver) = mpsc::channel();
        let (priority_sender, priority_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        #[cfg(test)]
        let test_events_tx = event_sender.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("fieldcad-compute".to_owned())
            .spawn(move || {
                worker_loop(
                    source,
                    request_receiver,
                    priority_receiver,
                    event_sender,
                    worker_stop,
                )
            })
            .expect("the local compute worker thread must be spawnable");

        Self {
            requests: request_sender,
            priority_requests: priority_sender,
            events: event_receiver,
            stop,
            cancellation,
            worker: Some(worker),
            simulation: initial.simulation,
            domain: initial.domain,
            playback_speed: initial.playback_speed,
            worker_queue: initial.queue,
            submitted_commands: BTreeMap::new(),
            submission_counter: 0,
            subscription: initial.subscription,
            scene_scale: initial.scene_scale,
            integration_scheme: initial.integration_scheme,
            field_systems: initial.field_systems,
            edit_history: initial.edit_history,
            world: initial.world,
            forces: initial.forces,
            step_compute_ms: initial.step_compute_ms,
            mailbox,
            poll_in_flight: false,
            body_history_cache: BTreeMap::new(),
            body_history_in_flight: BTreeSet::new(),
            body_history_capacity: BTreeMap::new(),
            accumulated_elapsed: Duration::ZERO,
            command_events: Vec::new(),
            failure: None,
            #[cfg(test)]
            test_events_tx,
        }
    }

    fn adopt(&mut self, state: SourceState) -> Result<bool, SourceError> {
        self.simulation = state.simulation;
        self.domain = state.domain;
        self.playback_speed = state.playback_speed;
        self.worker_queue = state.queue;
        self.subscription = state.subscription;
        self.scene_scale = state.scene_scale;
        self.integration_scheme = state.integration_scheme;
        self.field_systems = state.field_systems;
        self.edit_history = state.edit_history;
        self.world = state.world;
        self.forces = state.forces;
        self.step_compute_ms = state.step_compute_ms;
        match state.snapshot {
            Some(snapshot) => Ok(self.mailbox.offer(snapshot)?),
            None => Ok(false),
        }
    }

    fn drain_worker_events(&mut self) -> Result<PollOutcome, SourceError> {
        let mut aggregate = PollOutcome::default();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    if let Some(outcome) = self.handle_worker_event(event)? {
                        aggregate.snapshot_updated |= outcome.snapshot_updated;
                        aggregate.ticks_advanced = aggregate
                            .ticks_advanced
                            .saturating_add(outcome.ticks_advanced);
                        aggregate.commands_applied = aggregate
                            .commands_applied
                            .saturating_add(outcome.commands_applied);
                        aggregate.fell_behind |= outcome.fell_behind;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.poll_in_flight = false;
                    self.failure = Some("local compute worker stopped".to_owned());
                    break;
                }
            }
        }
        Ok(aggregate)
    }

    /// One worker event's effect on `self` — shared between the
    /// non-blocking [`Self::drain_worker_events`] loop and the blocking wait
    /// in [`Self::capture_document`], which must apply any ordinary event it
    /// happens to receive while waiting for its own answer, exactly as
    /// `drain_worker_events` would have. Returns the poll-shaped part of the
    /// outcome (`None` for anything that isn't a `PollCompleted`) so the
    /// non-blocking caller can still aggregate it; `capture_document` ignores
    /// it and matches `WorkerEvent::DocumentCaptured` directly instead of
    /// routing it through here.
    fn handle_worker_event(
        &mut self,
        event: WorkerEvent,
    ) -> Result<Option<PollOutcome>, SourceError> {
        match event {
            WorkerEvent::CommandCompleted {
                receipt,
                state,
                terminal,
            } => {
                self.submitted_commands.remove(&receipt.command);
                let snapshot_updated = self.adopt(state)?;
                self.command_events.extend(terminal);
                // A queued acknowledgement is not terminal completion: its
                // real completion (or rejection) arrives later, via a
                // `PollCompleted.terminal` entry, once a tick boundary
                // actually applies it.
                if receipt.disposition != CommandDisposition::Queued {
                    self.command_events.push(CommandEvent::Completed(receipt));
                }
                Ok(Some(PollOutcome {
                    snapshot_updated,
                    ..Default::default()
                }))
            }
            WorkerEvent::CommandFailed {
                command,
                error,
                state,
                terminal,
            } => {
                self.submitted_commands.remove(&command);
                let snapshot_updated = self.adopt(state)?;
                // terminal may already contain a Failed for this command
                // (e.g. from reject_if_queue_paused in BE-7). Only push one
                // if it's not already there — otherwise the same command
                // failure is reported twice.
                let already_reported = terminal.iter().any(|event| {
                    matches!(
                        event,
                        CommandEvent::Failed { command: id, .. } if *id == command
                    )
                });
                self.command_events.extend(terminal);
                if !already_reported {
                    self.command_events
                        .push(CommandEvent::Failed { command, error });
                }
                Ok(Some(PollOutcome {
                    snapshot_updated,
                    ..Default::default()
                }))
            }
            WorkerEvent::PollCompleted {
                outcome,
                state,
                terminal,
            } => {
                self.poll_in_flight = false;
                let snapshot_updated = outcome.snapshot_updated | self.adopt(state)?;
                self.command_events.extend(terminal);
                Ok(Some(PollOutcome {
                    snapshot_updated,
                    ..outcome
                }))
            }
            WorkerEvent::PollFailed(error) => {
                self.poll_in_flight = false;
                self.failure = Some(error.to_string());
                // Don't return early (BE-9): earlier events in this batch
                // already called adopt() and their aggregate is valid.
                // Return it so the caller (e.g. the desktop's per-frame
                // pump) still redraws. The failure is visible through
                // status().
                Ok(None)
            }
            WorkerEvent::DocumentCaptured { .. } => {
                // Only meaningful to `capture_document`'s own blocking wait,
                // which matches this variant directly before routing
                // anything else through here.
                Ok(None)
            }
            WorkerEvent::WorldCommandsValidated(_) => {
                // Only meaningful to `validate_world_commands`'s own
                // blocking wait, same reasoning as `DocumentCaptured`.
                Ok(None)
            }
            WorkerEvent::FieldSystemConfigurationValidated(_) => {
                // Only meaningful to `validate_field_system_configuration`'s
                // own blocking wait, same reasoning as `DocumentCaptured`.
                Ok(None)
            }
            WorkerEvent::BodyHistoryCaptured { object, samples } => {
                self.body_history_in_flight.remove(&object);
                self.body_history_cache.insert(object, samples);
                Ok(None)
            }
        }
    }

    /// Synchronously read the current session's full world contents (with
    /// identifier counters) and pending-queue contents, for durable storage.
    /// See [`fieldcad_core::WorldDocument`] and [`QueueDocument`].
    ///
    /// Blocking is deliberate: this is a rare, explicit, one-shot save
    /// action, not part of the per-frame poll loop. Any other worker event
    /// received while waiting is applied exactly as `drain_worker_events`
    /// would have, so a save never silently drops an unrelated command's
    /// completion that happened to land first.
    pub fn capture_document(
        &mut self,
    ) -> Result<(fieldcad_core::WorldDocument, QueueDocument), SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        self.requests
            .send(WorkerRequest::CaptureDocument)
            .map_err(|_| SourceError::Disconnected)?;
        loop {
            match self.events.recv().map_err(|_| SourceError::Disconnected)? {
                WorkerEvent::DocumentCaptured { world, queue } => return Ok((world, queue)),
                other => {
                    self.handle_worker_event(other)?;
                }
            }
        }
    }

    /// Advisory, non-mutating: would this transaction be adopted if
    /// committed right now? Blocking, like [`Self::capture_document`] — a
    /// caller wants a definitive answer, not a value to poll for. See
    /// [`crate::runtime::SimulationRuntime::validate_world_commands`]: the
    /// runtime remains the sole authority at actual commit time, so a
    /// same-shaped rejection can still occur at `execute` if state changed
    /// between preflight and commit.
    pub fn validate_world_commands(
        &mut self,
        commands: Vec<WorldCommand>,
    ) -> Result<CommitReport, SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        self.requests
            .send(WorkerRequest::ValidateWorldCommands(commands))
            .map_err(|_| SourceError::Disconnected)?;
        loop {
            match self.events.recv().map_err(|_| SourceError::Disconnected)? {
                WorkerEvent::WorldCommandsValidated(result) => return result,
                other => {
                    self.handle_worker_event(other)?;
                }
            }
        }
    }

    /// Advisory, non-mutating counterpart to `validate_world_commands` for
    /// one field system's proposed configuration.
    pub fn validate_field_system_configuration(
        &mut self,
        plugin: PluginId,
        configuration: PropertyBag,
    ) -> Result<(), SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        self.requests
            .send(WorkerRequest::ValidateFieldSystemConfiguration {
                plugin,
                configuration,
            })
            .map_err(|_| SourceError::Disconnected)?;
        loop {
            match self.events.recv().map_err(|_| SourceError::Disconnected)? {
                WorkerEvent::FieldSystemConfigurationValidated(result) => return result,
                other => {
                    self.handle_worker_event(other)?;
                }
            }
        }
    }

    /// Submit `command` and block until *its own* terminal outcome is known
    /// — [`CommandEvent::Completed`]/`Failed`/`Cancelled` — rather than the
    /// immediate `Submitted` [`Self::execute`] itself always returns.
    ///
    /// Blocking is deliberate, like [`Self::capture_document`]: session
    /// replay ([`crate::recording::SessionRecording`], driven by
    /// `fieldcad_server::HeadlessServer::replay_recording`) must fully
    /// settle one recorded command before issuing the next or capturing an
    /// observation, or two replays of the same recording could race against
    /// worker-thread timing and disagree. Any other worker event received
    /// while waiting (an unrelated command's own completion, say) is left in
    /// place for the ordinary [`Self::drain_command_events`] to pick up —
    /// this never removes anyone else's event, only reads and returns a copy
    /// of the one this call is waiting for.
    pub fn execute_blocking(&mut self, command: Command) -> Result<CommandEvent, SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        let command_id = command.id;
        let initial = self.execute(command)?;
        if initial.disposition != CommandDisposition::Submitted {
            return Ok(CommandEvent::Completed(initial));
        }
        loop {
            if let Some(event) = self
                .command_events
                .iter()
                .find(|event| event.command_id() == command_id)
            {
                return Ok(event.clone());
            }
            let event = self.events.recv().map_err(|_| SourceError::Disconnected)?;
            self.handle_worker_event(event)?;
        }
    }

    /// Block until a poll of `elapsed` wall-clock time is fully processed by
    /// the worker, rather than [`Self::poll`]'s non-blocking "drain whatever
    /// already arrived, submit a new poll if idle." Same reasoning and same
    /// caller as [`Self::execute_blocking`]: session replay needs each
    /// recorded poll settled before the next event.
    pub fn poll_blocking(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        let mut aggregate = self.drain_worker_events()?;
        self.accumulated_elapsed = self.accumulated_elapsed.saturating_add(elapsed);
        self.submit_poll_if_idle()?;
        while self.poll_in_flight {
            let event = self.events.recv().map_err(|_| SourceError::Disconnected)?;
            if let Some(outcome) = self.handle_worker_event(event)? {
                aggregate.snapshot_updated |= outcome.snapshot_updated;
                aggregate.ticks_advanced = aggregate
                    .ticks_advanced
                    .saturating_add(outcome.ticks_advanced);
                aggregate.commands_applied = aggregate
                    .commands_applied
                    .saturating_add(outcome.commands_applied);
                aggregate.fell_behind |= outcome.fell_behind;
            }
        }
        Ok(aggregate)
    }

    /// Ask the worker for `object`'s current recorded history, non-blocking
    /// — the answer arrives as a `WorkerEvent::BodyHistoryCaptured`, drained
    /// on a later `drain_worker_events`/`poll` call same as any other event,
    /// and read back through [`FieldDataSource::body_history`]. Safe (and
    /// cheap) to call every frame for every object a trajectory display is
    /// currently on for: a repeat call while a request is already in
    /// flight for the same object is a no-op rather than a second send.
    pub fn request_body_history(&mut self, object: ObjectId) {
        if self.failure.is_some() || !self.body_history_in_flight.insert(object) {
            return;
        }
        if self
            .requests
            .send(WorkerRequest::BodyHistory(object))
            .is_err()
        {
            self.body_history_in_flight.remove(&object);
        }
    }

    /// Override how many samples `object`'s recorded history keeps — see
    /// [`crate::body_history::BodyHistory::set_capacity`]. Safe (and cheap)
    /// to call every frame with a freshly recomputed value, the way the
    /// desktop's trajectory display does (sized to `trail_seconds / dt`):
    /// a call that repeats the capacity already sent is a no-op rather than
    /// a second send.
    pub fn set_body_history_capacity(&mut self, object: ObjectId, capacity: usize) {
        if self.failure.is_some() || self.body_history_capacity.get(&object) == Some(&capacity) {
            return;
        }
        if self
            .requests
            .send(WorkerRequest::SetBodyHistoryCapacity(object, capacity))
            .is_err()
        {
            return;
        }
        self.body_history_capacity.insert(object, capacity);
    }

    fn submit_poll_if_idle(&mut self) -> Result<(), SourceError> {
        if self.poll_in_flight || self.accumulated_elapsed == Duration::ZERO {
            return Ok(());
        }
        let elapsed = std::mem::take(&mut self.accumulated_elapsed);
        self.requests
            .send(WorkerRequest::Poll(elapsed))
            .map_err(|_| SourceError::Disconnected)?;
        self.poll_in_flight = true;
        Ok(())
    }

    pub fn drain_command_events(&mut self) -> Vec<CommandEvent> {
        std::mem::take(&mut self.command_events)
    }
}

impl FieldDataSource for AsyncLocalDataSource {
    fn description(&self) -> &str {
        "Asynchronous local compute worker"
    }

    fn status(&self) -> DataSourceStatus {
        self.failure
            .as_ref()
            .map_or(DataSourceStatus::Ready, |error| {
                DataSourceStatus::Failed(error.clone())
            })
    }

    fn simulation_status(&self) -> SimulationStatus {
        self.simulation
    }

    fn domain(&self) -> Domain {
        self.domain
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.worker_queue.pending.len() + self.submitted_commands.len()
    }

    fn get_queue(&self) -> QueueStatus {
        let mut status = self.worker_queue.clone();
        // Commands sent to the worker but not yet executed there have no
        // record in `worker_queue` yet — synthesize a `Submitted` display
        // entry for each so a caller sees them immediately, before the
        // worker even reports back.
        for (&command, &(kind, seq)) in &self.submitted_commands {
            status
                .pending
                .push(CommandRecord::submitted(command, kind, seq));
        }
        status
    }

    /// `pending_len` folds in `submitted_commands` the same way
    /// [`Self::pending_command_count`] and `get_queue`'s synthesized
    /// `Submitted` entries do, so the two stay consistent.
    fn queue_summary(&self) -> QueueSummary {
        QueueSummary {
            paused: self.worker_queue.paused,
            pending_len: self.worker_queue.pending.len() + self.submitted_commands.len(),
            history_len: self.worker_queue.history.len(),
            newest_history: self
                .worker_queue
                .history
                .last()
                .map(|record| record.command),
        }
    }

    fn subscription(&self) -> Subscription {
        self.subscription
    }

    fn scene_scale(&self) -> SceneScale {
        self.scene_scale
    }

    fn integration_scheme(&self) -> IntegrationScheme {
        self.integration_scheme
    }

    fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.field_systems.clone()
    }

    fn edit_history(&self) -> EditHistoryStatus {
        self.edit_history.clone()
    }

    fn world(&self) -> WorldSnapshot {
        self.world.clone()
    }

    fn body_forces(&self) -> BTreeMap<ObjectId, DVec3> {
        self.forces.clone()
    }

    fn body_history(&self, object: ObjectId) -> Vec<BodySample> {
        self.body_history_cache
            .get(&object)
            .cloned()
            .unwrap_or_default()
    }

    fn step_compute_ms(&self) -> f32 {
        self.step_compute_ms
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        // BE-6: a command that is still in flight (in the mpsc channel, not
        // yet acknowledged by the worker) cannot be cancelled — there is no
        // way to pull it from the FIFO. Return a clear error rather than
        // sending the cancel to the worker where it would fail with a
        // misleading "not found" against pending_mutations.
        if let CommandPayload::CancelQueuedCommand(target) = &command.payload
            && self.submitted_commands.contains_key(target)
        {
            return Err(SourceError::CommandInFlight(*target));
        }
        let command_id = command.id;
        let kind = command.payload.kind();
        // Queue-control commands are never a world-simulation event, so
        // they must not wait behind a backlog of heavy `CommitWorld`
        // solves already sitting in `requests` — send them on the
        // priority channel `worker_loop` drains first instead.
        let is_queue_control = matches!(
            command.payload,
            CommandPayload::PauseQueue
                | CommandPayload::ResumeQueue
                | CommandPayload::CancelQueuedCommand(_)
        );
        if is_queue_control {
            self.priority_requests
                .send(command)
                .map_err(|_| SourceError::Disconnected)?;
        } else {
            self.requests
                .send(WorkerRequest::Execute(command))
                .map_err(|_| SourceError::Disconnected)?;
        }
        let seq = self.submission_counter;
        self.submission_counter += 1;
        self.submitted_commands.insert(command_id, (kind, seq));
        Ok(CommandReceipt {
            command: command_id,
            world_revision: self.simulation.world_revision,
            tick: self.simulation.tick(),
            snapshot_sequence: None,
            disposition: CommandDisposition::Submitted,
            created: CommitReport::empty(self.simulation.world_revision),
        })
    }

    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        self.accumulated_elapsed = self.accumulated_elapsed.saturating_add(elapsed);
        let outcome = self.drain_worker_events()?;
        self.submit_poll_if_idle()?;
        Ok(outcome)
    }

    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.mailbox.latest()
    }

    fn drain_command_events(&mut self) -> Vec<CommandEvent> {
        AsyncLocalDataSource::drain_command_events(self)
    }
}

impl Drop for AsyncLocalDataSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancellation.cancel();
        let _ = self.requests.send(WorkerRequest::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Runs one command against `source` and turns the outcome into the
/// `WorkerEvent` the main thread expects, draining any terminal side
/// effects the command produced synchronously (see
/// `WorkerEvent::CommandCompleted::terminal`) — not just on `Poll`, since a
/// command that flushes another, already-queued one as its own side effect
/// (e.g. `pause` flushing a running edit) produces that other command's
/// terminal event synchronously, inside this same call. Returns `Err(())`
/// if `events` is disconnected, the caller's cue to stop the worker loop.
fn run_command(
    source: &mut LocalDataSource,
    command: Command,
    events: &Sender<WorkerEvent>,
) -> Result<(), ()> {
    let command_id = command.id;
    let result = source.execute(command);
    let state = SourceState::capture(source);
    let terminal = source.drain_command_events();
    let event = match result {
        Ok(receipt) => WorkerEvent::CommandCompleted {
            receipt,
            state,
            terminal,
        },
        Err(error) => WorkerEvent::CommandFailed {
            command: command_id,
            error,
            state,
            terminal,
        },
    };
    events.send(event).map_err(|_| ())
}

/// A backlog of already-submitted `Execute` requests must never delay a
/// queue-control command (`PauseQueue`/`ResumeQueue`/
/// `CancelQueuedCommand`, sent on `priority_requests` instead of
/// `requests` — see `AsyncLocalDataSource::execute`) by more than the one
/// request already in flight when it arrives.
const PRIORITY_POLL_INTERVAL: Duration = Duration::from_millis(20);

fn worker_loop(
    mut source: LocalDataSource,
    requests: Receiver<WorkerRequest>,
    priority_requests: Receiver<Command>,
    events: Sender<WorkerEvent>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        // Drained unconditionally at the top of every iteration — i.e.
        // right after whatever `Execute` just finished, before the next
        // backlog item is even looked at — so a queue-control command
        // waits behind at most one already-in-flight command, never the
        // whole backlog.
        while let Ok(command) = priority_requests.try_recv() {
            if run_command(&mut source, command, &events).is_err() {
                return;
            }
        }

        // `recv_timeout`, not a blocking `recv`: with an empty backlog the
        // worker would otherwise block indefinitely and only notice a
        // priority arrival once some other request happened to wake it.
        match requests.recv_timeout(PRIORITY_POLL_INTERVAL) {
            Ok(WorkerRequest::Execute(command)) => {
                if run_command(&mut source, command, &events).is_err() {
                    return;
                }
            }
            Ok(WorkerRequest::Poll(elapsed)) => {
                let event = match source.poll(elapsed) {
                    Ok(outcome) => WorkerEvent::PollCompleted {
                        outcome,
                        state: SourceState::capture(&source),
                        terminal: source.drain_command_events(),
                    },
                    Err(error) => WorkerEvent::PollFailed(error),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            Ok(WorkerRequest::CaptureDocument) => {
                let event = WorkerEvent::DocumentCaptured {
                    world: source.runtime().world_document(),
                    queue: source.queue_document(),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            Ok(WorkerRequest::BodyHistory(object)) => {
                let event = WorkerEvent::BodyHistoryCaptured {
                    object,
                    samples: source.runtime().body_history(object).copied().collect(),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            Ok(WorkerRequest::SetBodyHistoryCapacity(object, capacity)) => {
                source
                    .runtime_mut()
                    .set_body_history_capacity(object, capacity);
            }
            Ok(WorkerRequest::ValidateWorldCommands(commands)) => {
                let result = source
                    .runtime()
                    .validate_world_commands(&commands)
                    .map_err(SourceError::from);
                if events
                    .send(WorkerEvent::WorldCommandsValidated(result))
                    .is_err()
                {
                    break;
                }
            }
            Ok(WorkerRequest::ValidateFieldSystemConfiguration {
                plugin,
                configuration,
            }) => {
                let result = source
                    .runtime()
                    .validate_field_system_configuration(&plugin, &configuration)
                    .map_err(SourceError::from);
                if events
                    .send(WorkerEvent::FieldSystemConfigurationValidated(result))
                    .is_err()
                {
                    break;
                }
            }
            Ok(WorkerRequest::Stop) => break,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, Domain, DomainBounds, ObjectId, Precision,
        Resolution, SessionId, TimeStep, WorldRevision,
    };
    use fieldcad_electromagnetism::courant_limit;
    use fieldcad_test_field::TestFieldPlugin;
    use glam::DVec3;

    use crate::{
        BodySample, CommandPayload, CommandSequencer, DataSourceStatus, FieldDataSource,
        LocalDataSource, RuntimeConfig, SimulationRuntime, SourceError,
    };

    use super::{AsyncLocalDataSource, WorkerEvent, WorkerRequest, worker_loop};

    fn runtime() -> SimulationRuntime {
        let domain = Domain::new(
            DomainBounds::new(DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0)).unwrap(),
            Resolution::new(8, 8, 8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.5).unwrap();
        SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(99))
                .with_plugin(Box::new(TestFieldPlugin)),
        )
        .unwrap()
    }

    #[test]
    fn requesting_body_history_dedupes_in_flight_and_populates_the_cache_once_answered() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        let object = ObjectId::new(7);
        assert!(source.body_history(object).is_empty());

        source.request_body_history(object);
        assert!(source.body_history_in_flight.contains(&object));

        // A second request for the same object while one is already in
        // flight must not queue a duplicate — the worker channel would
        // otherwise be spammed once per frame for every trajectory-enabled
        // object.
        source.request_body_history(object);
        assert_eq!(source.body_history_in_flight.len(), 1);

        // Wait for the worker's real answer (an empty history — object 7
        // doesn't exist) to drain before injecting the synthetic answer
        // below: both travel the same event channel, and if the real answer
        // landed *after* the synthetic one it would overwrite the cache
        // with `[]`, making this test's outcome depend on thread scheduling.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while source.body_history_in_flight.contains(&object) {
            source.drain_worker_events().unwrap();
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never answered the in-flight history request"
            );
            std::thread::yield_now();
        }

        let sample = BodySample {
            tick: 3,
            time_seconds: 0.75,
            world_revision: WorldRevision::INITIAL,
            position: DVec3::new(1.0, 2.0, 3.0),
            velocity: DVec3::new(0.1, 0.0, 0.0),
            force: DVec3::ZERO,
        };
        source
            .test_events_tx
            .send(WorkerEvent::BodyHistoryCaptured {
                object,
                samples: vec![sample],
            })
            .unwrap();
        source.drain_worker_events().unwrap();

        assert!(!source.body_history_in_flight.contains(&object));
        assert_eq!(source.body_history(object), vec![sample]);
    }

    /// End-to-end regression for the desktop's per-frame trajectory-trail
    /// path (`app.rs`'s `request_body_history` + `body_history` pair): a
    /// real ticking body's recorded history must actually round-trip
    /// through the worker, the same way `app.rs` reads it every frame.
    #[test]
    fn a_moving_body_s_recorded_history_round_trips_through_the_worker() {
        use fieldcad_core::{ObjectShape, ObjectSpec, Transform, Velocity, WorldCommand};
        use fieldcad_sources::{inertial_mass_component_id, mass_component_schemas};

        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));

        let mut sequencer = CommandSequencer::default();
        let object = ObjectId::new(0);
        let mut commands: Vec<WorldCommand> = mass_component_schemas()
            .into_iter()
            .map(WorldCommand::RegisterComponentSchema)
            .collect();
        commands.push(
            WorldCommand::CreateObject(
                ObjectSpec::new("free")
                    .with_transform(Transform::at(DVec3::ZERO).unwrap())
                    .with_shape(ObjectShape::point(0.05).unwrap())
                    .with_component(
                        inertial_mass_component_id(),
                        fieldcad_sources::inertial_mass_properties(
                            fieldcad_core::quantities::MassKg::new::<
                                fieldcad_core::quantities::kilogram,
                            >(1.0),
                        )
                        .unwrap(),
                    ),
            ),
        );
        commands.push(WorldCommand::SetVelocity {
            object,
            velocity: Velocity::new(DVec3::new(1.0, 0.0, 0.0), DVec3::ZERO).unwrap(),
        });
        source
            .execute(sequencer.issue(CommandPayload::CommitWorld(commands)))
            .unwrap();
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        // Advance real ticks, the same way the desktop's per-frame `poll`
        // does — not a single zero-elapsed poll, which would never cross a
        // tick boundary.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            source.poll(Duration::from_millis(50)).unwrap();
            source.request_body_history(object);
            source.poll(Duration::ZERO).unwrap();
            if source.body_history(object).len() >= 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a moving, unpinned, mass-bearing body never accumulated recorded history"
            );
            std::thread::yield_now();
        }

        let history = source.body_history(object);
        assert!(
            history
                .windows(2)
                .all(|pair| pair[0].time_seconds < pair[1].time_seconds),
            "oldest sample first, strictly increasing time"
        );
        assert!(
            history.last().unwrap().position.x > history.first().unwrap().position.x,
            "a body moving at +x should show increasing x across recorded samples: {history:?}"
        );
    }

    #[test]
    fn poll_failed_does_not_discard_aggregate_from_earlier_events() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));

        // Play the simulation: submit Play, then poll with real elapsed
        // to let the worker tick at least once.
        source
            .execute(CommandSequencer::default().issue(CommandPayload::Play))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            source.poll(Duration::ZERO).unwrap();
            if source.simulation_status().tick() > 0 {
                break;
            }
            source.poll(Duration::from_millis(100)).unwrap();
            assert!(
                std::time::Instant::now() < deadline,
                "worker never advanced a tick"
            );
            std::thread::yield_now();
        }

        // Inject a PollFailed via the test sender (the cloned mpsc sender
        // that shares the same receiver as the real worker). This simulates
        // a transient poll failure arriving *after* real worker events.
        source
            .test_events_tx
            .send(WorkerEvent::PollFailed(SourceError::Solver {
                code: "test-error".to_owned(),
                message: "simulated transient failure".to_owned(),
            }))
            .unwrap();

        // drain_worker_events must NOT propagate the PollFailed as Err —
        // events processed before it already called adopt() and their aggregate is valid.
        let _ = source.drain_worker_events().unwrap();

        // The failure is recorded in status() for diagnostics.
        assert!(
            matches!(source.status(), DataSourceStatus::Failed(_)),
            "the failure must be visible through status()"
        );
    }

    #[test]
    fn disconnected_clears_poll_in_flight_and_returns_error() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));

        // Give the worker a chance to send initial events.
        source.poll(Duration::ZERO).unwrap();

        // Stop the worker thread gracefully.
        source
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
        source.cancellation.cancel();
        let _ = source.requests.send(WorkerRequest::Stop);
        if let Some(handle) = source.worker.take() {
            handle.join().unwrap();
        }

        // Replace the request channel with a dead one (receiver dropped
        // immediately) so submit_poll_if_idle fails on the next call.
        let (dead_tx, _dead_rx) = std::sync::mpsc::channel();
        drop(_dead_rx);
        let _old = std::mem::replace(&mut source.requests, dead_tx);

        // drain_worker_events hits Disconnected. With the fix it clears
        // poll_in_flight before breaking.
        let _outcome = source.drain_worker_events().unwrap();

        // poll_in_flight was cleared. submit_poll_if_idle now attempts to
        // send to the dead channel and returns Err(Disconnected).
        let err = source.poll(Duration::from_millis(1)).unwrap_err();
        assert_eq!(err, SourceError::Disconnected);
    }

    /// A queue-control command sent on the priority channel must not wait
    /// behind a backlog already sitting in the normal `requests` channel —
    /// regression test for the pause/resume-gets-stuck-behind-a-backlog
    /// bug. Deterministic: the whole backlog is enqueued before the worker
    /// thread is even spawned, so there is no timing race to get right.
    #[test]
    fn priority_channel_commands_are_observed_before_the_backlog_drains() {
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let (priority_tx, priority_rx) = std::sync::mpsc::channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut sequencer = CommandSequencer::default();

        let mut backlog_ids = Vec::new();
        for _ in 0..5 {
            let command = sequencer.issue(CommandPayload::Play);
            backlog_ids.push(command.id);
            request_tx.send(WorkerRequest::Execute(command)).unwrap();
        }

        let pause = sequencer.issue(CommandPayload::PauseQueue);
        let pause_id = pause.id;
        priority_tx.send(pause).unwrap();

        let source = LocalDataSource::new(runtime());
        let worker = std::thread::spawn(move || {
            worker_loop(source, request_rx, priority_rx, event_tx, stop);
        });

        match event_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            WorkerEvent::CommandCompleted { receipt, .. } => {
                assert_eq!(
                    receipt.command, pause_id,
                    "the priority command must complete first"
                );
            }
            other => panic!("expected the priority command first, got {other:?}"),
        }

        let mut remaining = Vec::new();
        for _ in 0..backlog_ids.len() {
            match event_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                WorkerEvent::CommandCompleted { receipt, .. } => remaining.push(receipt.command),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(
            remaining, backlog_ids,
            "backlog still completes, in submission order"
        );

        request_tx.send(WorkerRequest::Stop).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn execute_blocking_returns_the_command_s_own_terminal_outcome() {
        use fieldcad_core::SimulationMode;

        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        let mut sequencer = CommandSequencer::default();

        let event = source
            .execute_blocking(sequencer.issue(CommandPayload::Play))
            .unwrap();

        assert!(
            matches!(event, super::CommandEvent::Completed(_)),
            "expected a completed command, got {event:?}"
        );
        assert_eq!(source.simulation_status().mode(), SimulationMode::Running);
    }

    #[test]
    fn execute_blocking_leaves_unrelated_events_for_the_ordinary_drain() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        let mut sequencer = CommandSequencer::default();

        source
            .execute_blocking(sequencer.issue(CommandPayload::Play))
            .unwrap();
        // A second command, executed non-blockingly, then waited for via
        // `execute_blocking`'s peek-and-leave contract: its own completion
        // must still show up through the ordinary drain afterwards, proving
        // `execute_blocking` never removed anyone else's event.
        let command = sequencer.issue(CommandPayload::Pause);
        let command_id = command.id;
        source.execute_blocking(command).unwrap();

        let drained = source.drain_command_events();
        assert!(
            drained.iter().any(|event| event.command_id() == command_id),
            "the command's own completion must still reach the ordinary drain: {drained:?}"
        );
    }

    #[test]
    fn poll_blocking_settles_before_returning() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        let mut sequencer = CommandSequencer::default();
        source
            .execute_blocking(sequencer.issue(CommandPayload::Play))
            .unwrap();

        let outcome = source.poll_blocking(Duration::from_millis(50)).unwrap();

        assert!(outcome.ticks_advanced > 0, "a real elapsed poll must tick");
        assert!(
            !source.poll_in_flight,
            "poll_blocking must not return while a poll is still outstanding"
        );
    }

    #[test]
    fn replaying_a_recording_through_the_blocking_api_is_deterministic() {
        use crate::recording::SessionRecording;

        let recording = SessionRecording::new()
            .with_command(CommandPayload::Play)
            .with_poll(Duration::ZERO)
            .with_command(CommandPayload::Pause)
            .with_command(CommandPayload::Step)
            .with_poll(Duration::ZERO);

        let replay_once = |recording: &SessionRecording| {
            let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
            let mut sequencer = CommandSequencer::default();
            for event in recording.events() {
                match event {
                    crate::recording::RecordedEvent::Command(payload) => {
                        source
                            .execute_blocking(sequencer.issue(payload.clone()))
                            .unwrap();
                    }
                    crate::recording::RecordedEvent::Poll(elapsed) => {
                        source.poll_blocking(*elapsed).unwrap();
                    }
                }
            }
            source.simulation_status()
        };

        let first = replay_once(&recording);
        let second = replay_once(&recording);

        assert_eq!(first, second);
        assert_eq!(first.tick(), 1);
    }
}
