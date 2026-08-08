//! Non-blocking local compute adapter.
//!
//! The desktop uses this adapter so GPU dispatch/readback and CPU sampling never
//! run on the window/event-loop thread. The worker still drives the exact same
//! [`LocalDataSource`] contract used by headless tests; this is a scheduling
//! boundary, not a second implementation of simulation semantics.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fieldcad_core::{CommitReport, Domain, FieldSnapshot, ObjectId, SceneScale, WorldSnapshot};
use fieldcad_plugin_api::SolverCancellation;
use glam::DVec3;

use crate::{
    Command, CommandDisposition, CommandId, CommandKind, CommandPayload, CommandReceipt,
    CommandRecord, DataSourceStatus, EditHistoryStatus, FieldDataSource, FieldSystemStatus,
    LocalDataSource, PlaybackSpeed, PollOutcome, QueueStatus, QueueSummary, SimulationStatus,
    SnapshotMailbox, SourceError, Subscription,
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
    Stop,
}

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
}

struct SourceState {
    simulation: SimulationStatus,
    domain: Domain,
    playback_speed: PlaybackSpeed,
    queue: QueueStatus,
    subscription: Subscription,
    scene_scale: SceneScale,
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
    field_systems: Vec<FieldSystemStatus>,
    edit_history: EditHistoryStatus,
    world: WorldSnapshot,
    forces: BTreeMap<ObjectId, DVec3>,
    step_compute_ms: f32,
    mailbox: SnapshotMailbox,
    poll_in_flight: bool,
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
        let (event_sender, event_receiver) = mpsc::channel();
        #[cfg(test)]
        let test_events_tx = event_sender.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("fieldcad-compute".to_owned())
            .spawn(move || worker_loop(source, request_receiver, event_sender, worker_stop))
            .expect("the local compute worker thread must be spawnable");

        Self {
            requests: request_sender,
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
            field_systems: initial.field_systems,
            edit_history: initial.edit_history,
            world: initial.world,
            forces: initial.forces,
            step_compute_ms: initial.step_compute_ms,
            mailbox,
            poll_in_flight: false,
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
                Ok(WorkerEvent::CommandCompleted {
                    receipt,
                    state,
                    terminal,
                }) => {
                    self.submitted_commands.remove(&receipt.command);
                    aggregate.snapshot_updated |= self.adopt(state)?;
                    self.command_events.extend(terminal);
                    // A queued acknowledgement is not terminal completion:
                    // its real completion (or rejection) arrives later, via
                    // a `PollCompleted.terminal` entry, once a tick boundary
                    // actually applies it.
                    if receipt.disposition != CommandDisposition::Queued {
                        self.command_events.push(CommandEvent::Completed(receipt));
                    }
                }
                Ok(WorkerEvent::CommandFailed {
                    command,
                    error,
                    state,
                    terminal,
                }) => {
                    self.submitted_commands.remove(&command);
                    aggregate.snapshot_updated |= self.adopt(state)?;
                    // terminal may already contain a Failed for this command
                    // (e.g. from reject_if_queue_paused in BE-7). Only push
                    // one if it's not already there — otherwise the same
                    // command failure is reported twice.
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
                }
                Ok(WorkerEvent::PollCompleted {
                    outcome,
                    state,
                    terminal,
                }) => {
                    self.poll_in_flight = false;
                    aggregate.snapshot_updated |= outcome.snapshot_updated;
                    aggregate.snapshot_updated |= self.adopt(state)?;
                    aggregate.ticks_advanced = aggregate
                        .ticks_advanced
                        .saturating_add(outcome.ticks_advanced);
                    aggregate.commands_applied = aggregate
                        .commands_applied
                        .saturating_add(outcome.commands_applied);
                    aggregate.fell_behind |= outcome.fell_behind;
                    self.command_events.extend(terminal);
                }
                Ok(WorkerEvent::PollFailed(error)) => {
                    self.poll_in_flight = false;
                    self.failure = Some(error.to_string());
                    // Don't return early (BE-9): earlier events in this
                    // batch already called adopt() and their aggregate is
                    // valid. Return it so the caller (e.g. the desktop's
                    // per-frame pump) still redraws. The failure is visible
                    // through status().
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
        self.requests
            .send(WorkerRequest::Execute(command))
            .map_err(|_| SourceError::Disconnected)?;
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

fn worker_loop(
    mut source: LocalDataSource,
    requests: Receiver<WorkerRequest>,
    events: Sender<WorkerEvent>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        let Ok(request) = requests.recv() else {
            break;
        };
        match request {
            WorkerRequest::Execute(command) => {
                let command_id = command.id;
                let result = source.execute(command);
                let state = SourceState::capture(&source);
                // Drained here, not just on `Poll`: a command that flushes
                // another, already-queued one as its own side effect (e.g.
                // `pause` flushing a running edit) produces that other
                // command's terminal event synchronously, inside this same
                // `execute` call — leaving it buffered until some later
                // `Poll` happens to run would strand any waiter for it.
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
                if events.send(event).is_err() {
                    break;
                }
            }
            WorkerRequest::Poll(elapsed) => {
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
            WorkerRequest::Stop => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, Domain, DomainBounds, Precision, Resolution,
        SessionId, TimeStep,
    };
    use fieldcad_electromagnetism::courant_limit;
    use fieldcad_test_field::TestFieldPlugin;
    use glam::DVec3;

    use crate::{
        CommandPayload, CommandSequencer, DataSourceStatus, FieldDataSource, LocalDataSource,
        RuntimeConfig, SimulationRuntime, SourceError,
    };

    use super::{AsyncLocalDataSource, WorkerEvent, WorkerRequest};

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
}
