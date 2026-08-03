//! Non-blocking local compute adapter.
//!
//! The desktop uses this adapter so GPU dispatch/readback and CPU sampling never
//! run on the window/event-loop thread. The worker still drives the exact same
//! [`LocalDataSource`] contract used by headless tests; this is a scheduling
//! boundary, not a second implementation of simulation semantics.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fieldcad_core::{FieldSnapshot, WorldSnapshot};
use fieldcad_plugin_api::SolverCancellation;

use crate::{
    Command, CommandDisposition, CommandId, CommandReceipt, DataSourceStatus, FieldDataSource,
    FieldSystemStatus, LocalDataSource, PlaybackSpeed, PollOutcome, SimulationStatus,
    SnapshotMailbox, SourceError, Subscription,
};

#[derive(Clone, Debug, PartialEq)]
pub enum CommandEvent {
    Completed(CommandReceipt),
    Failed {
        command: CommandId,
        error: SourceError,
    },
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
    },
    CommandFailed {
        command: CommandId,
        error: SourceError,
        state: SourceState,
    },
    PollCompleted {
        outcome: PollOutcome,
        state: SourceState,
    },
    PollFailed(SourceError),
}

struct SourceState {
    simulation: SimulationStatus,
    playback_speed: PlaybackSpeed,
    pending_commands: usize,
    subscription: Subscription,
    field_systems: Vec<FieldSystemStatus>,
    world: WorldSnapshot,
    snapshot: Option<Arc<FieldSnapshot>>,
}

impl SourceState {
    fn capture(source: &LocalDataSource) -> Self {
        Self {
            simulation: source.simulation_status(),
            playback_speed: source.playback_speed(),
            pending_commands: source.pending_command_count(),
            subscription: source.subscription(),
            field_systems: source.field_systems(),
            world: source.world(),
            snapshot: source.latest_snapshot(),
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
    playback_speed: PlaybackSpeed,
    worker_pending_commands: usize,
    submitted_commands: BTreeSet<CommandId>,
    subscription: Subscription,
    field_systems: Vec<FieldSystemStatus>,
    world: WorldSnapshot,
    mailbox: SnapshotMailbox,
    poll_in_flight: bool,
    accumulated_elapsed: Duration,
    command_events: Vec<CommandEvent>,
    failure: Option<String>,
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
            playback_speed: initial.playback_speed,
            worker_pending_commands: initial.pending_commands,
            submitted_commands: BTreeSet::new(),
            subscription: initial.subscription,
            field_systems: initial.field_systems,
            world: initial.world,
            mailbox,
            poll_in_flight: false,
            accumulated_elapsed: Duration::ZERO,
            command_events: Vec::new(),
            failure: None,
        }
    }

    fn adopt(&mut self, state: SourceState) -> Result<bool, SourceError> {
        self.simulation = state.simulation;
        self.playback_speed = state.playback_speed;
        self.worker_pending_commands = state.pending_commands;
        self.subscription = state.subscription;
        self.field_systems = state.field_systems;
        self.world = state.world;
        match state.snapshot {
            Some(snapshot) => Ok(self.mailbox.offer(snapshot)?),
            None => Ok(false),
        }
    }

    fn drain_worker_events(&mut self) -> Result<PollOutcome, SourceError> {
        let mut aggregate = PollOutcome::default();
        loop {
            match self.events.try_recv() {
                Ok(WorkerEvent::CommandCompleted { receipt, state }) => {
                    self.submitted_commands.remove(&receipt.command);
                    aggregate.snapshot_updated |= self.adopt(state)?;
                    self.command_events.push(CommandEvent::Completed(receipt));
                }
                Ok(WorkerEvent::CommandFailed {
                    command,
                    error,
                    state,
                }) => {
                    self.submitted_commands.remove(&command);
                    aggregate.snapshot_updated |= self.adopt(state)?;
                    self.command_events
                        .push(CommandEvent::Failed { command, error });
                }
                Ok(WorkerEvent::PollCompleted { outcome, state }) => {
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
                }
                Ok(WorkerEvent::PollFailed(error)) => {
                    self.poll_in_flight = false;
                    self.failure = Some(error.to_string());
                    return Err(error);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
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

    fn playback_speed(&self) -> PlaybackSpeed {
        self.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.worker_pending_commands + self.submitted_commands.len()
    }

    fn subscription(&self) -> Subscription {
        self.subscription
    }

    fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.field_systems.clone()
    }

    fn world(&self) -> WorldSnapshot {
        self.world.clone()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        if self.failure.is_some() {
            return Err(SourceError::Disconnected);
        }
        let command_id = command.id;
        self.requests
            .send(WorkerRequest::Execute(command))
            .map_err(|_| SourceError::Disconnected)?;
        self.submitted_commands.insert(command_id);
        Ok(CommandReceipt {
            command: command_id,
            world_revision: self.simulation.world_revision,
            tick: self.simulation.tick(),
            snapshot_sequence: None,
            disposition: CommandDisposition::Submitted,
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
                let event = match source.execute(command) {
                    Ok(receipt) => WorkerEvent::CommandCompleted {
                        receipt,
                        state: SourceState::capture(&source),
                    },
                    Err(error) => WorkerEvent::CommandFailed {
                        command: command_id,
                        error,
                        state: SourceState::capture(&source),
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
