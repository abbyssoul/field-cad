//! The boundary the visualizer talks to, in place of a solver.
//!
//! Both implementations here — an in-process runtime and a loopback stand-in for
//! a remote service — offer the *same* guarantees, not merely the same method
//! names. In particular both publish through a [`SnapshotMailbox`], so the rules
//! about completeness, session identity, and supersession are enforced on one
//! code path rather than only on the path that happens to be remote.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use fieldcad_core::{
    FieldSnapshot, SimulationMode, TimeStep, WorldCommand, WorldRevision, WorldSnapshot,
};

use crate::runtime::{RuntimeError, SimulationRuntime, SimulationStatus, TickPacer};

/// Client-issued identity for one command, echoed in its acknowledgement.
///
/// Play, pause, and step are commands with correlated acknowledgements; they are
/// never inferred from the timing of incoming frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(u64);

impl CommandId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Mints monotonic command identities on the client side.
#[derive(Clone, Debug, Default)]
pub struct CommandSequencer {
    next: u64,
}

impl CommandSequencer {
    pub fn issue(&mut self, payload: CommandPayload) -> Command {
        let id = CommandId::new(self.next);
        self.next += 1;
        Command { id, payload }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandPayload {
    Play,
    Pause,
    Step,
    SetTimeStep(TimeStep),
    SetPlaybackSpeed(PlaybackSpeed),
    CommitWorld(Vec<WorldCommand>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub payload: CommandPayload,
}

/// Wall-clock playback rate, kept separate from the numerical time step.
///
/// A multiplier of `2.0` asks the source to schedule twice as many unchanged
/// fixed-size ticks per wall-clock second. It never stretches `dt`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct PlaybackSpeed(f64);

impl PlaybackSpeed {
    pub fn from_multiplier(multiplier: f64) -> Result<Self, PlaybackSpeedError> {
        if !multiplier.is_finite() || multiplier <= 0.0 {
            return Err(PlaybackSpeedError { multiplier });
        }
        Ok(Self(multiplier))
    }

    pub const fn multiplier(self) -> f64 {
        self.0
    }
}

impl Default for PlaybackSpeed {
    fn default() -> Self {
        Self(1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("playback speed must be finite and greater than zero, received {multiplier}")]
pub struct PlaybackSpeedError {
    pub multiplier: f64,
}

/// Whether an acknowledgement describes an already-applied command or an edit
/// accepted for the next fixed-tick boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandDisposition {
    Applied,
    Queued,
}

/// The authoritative side's answer to one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandReceipt {
    /// Which command this acknowledges.
    pub command: CommandId,
    /// The authoritative world revision when the acknowledgement was issued.
    /// This remains unchanged for a queued edit until a tick boundary applies
    /// that edit.
    pub world_revision: WorldRevision,
    pub tick: u64,
    /// The sequence of the snapshot that reflects this command, if one was
    /// produced.
    pub snapshot_sequence: u64,
    pub disposition: CommandDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataSourceStatus {
    Connecting,
    Ready,
    /// The last complete snapshot may still be shown, marked stale.
    Disconnected,
    Failed(String),
}

impl DataSourceStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Connecting => "Connecting".to_owned(),
            Self::Ready => "Ready".to_owned(),
            Self::Disconnected => "Disconnected".to_owned(),
            Self::Failed(message) => format!("Failed: {message}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollOutcome {
    /// A newer complete snapshot became visible.
    pub snapshot_updated: bool,
    pub ticks_advanced: u32,
    /// Wall-clock time demanded more ticks than the budget allowed. `dt` was not
    /// changed to compensate.
    pub fell_behind: bool,
    /// Running world edits applied immediately before this poll's first tick.
    pub commands_applied: u32,
}

/// Transport-neutral contract consumed by the desktop visualizer.
pub trait FieldDataSource: Send {
    fn description(&self) -> &str;
    fn status(&self) -> DataSourceStatus;
    fn simulation_status(&self) -> SimulationStatus;
    fn playback_speed(&self) -> PlaybackSpeed;
    fn pending_command_count(&self) -> usize;

    /// The world the client currently believes in.
    ///
    /// For a local source this is the authoritative world. For a remote one it
    /// is a replica, updated when the service acknowledges an edit — the desktop
    /// draws what it has been told, not what it hopes it submitted.
    fn world(&self) -> WorldSnapshot;

    /// Accept a transport command. World edits issued while running may return
    /// a queued receipt and become authoritative at the next fixed-tick
    /// boundary; consumers can inspect `pending_command_count` meanwhile.
    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError>;
    /// Take in wall-clock time and pick up whatever the source has produced.
    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError>;
    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>>;
}

/// The client-side presentation buffer.
///
/// Holds the newest *complete* snapshot of the current session. An older
/// snapshot arriving late is normal under backpressure and is simply not
/// adopted; an incomplete or foreign one is a protocol violation and is
/// reported.
#[derive(Clone, Debug, Default)]
pub struct SnapshotMailbox {
    latest: Option<Arc<FieldSnapshot>>,
}

impl SnapshotMailbox {
    /// Returns whether this snapshot became the visible state.
    pub fn offer(&mut self, snapshot: Arc<FieldSnapshot>) -> Result<bool, SnapshotRejection> {
        if !snapshot.is_complete() {
            return Err(SnapshotRejection::Incomplete);
        }
        if let Some(current) = &self.latest {
            if snapshot.identity.session != current.identity.session {
                return Err(SnapshotRejection::UnexpectedSession);
            }
            if snapshot.identity.sequence <= current.identity.sequence {
                // Superseded, not corrupt: drop it and keep what we have.
                return Ok(false);
            }
        }
        self.latest = Some(snapshot);
        Ok(true)
    }

    pub fn latest(&self) -> Option<Arc<FieldSnapshot>> {
        self.latest.as_ref().map(Arc::clone)
    }

    pub fn sequence(&self) -> Option<u64> {
        self.latest
            .as_ref()
            .map(|snapshot| snapshot.identity.sequence)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotRejection {
    #[error("an incomplete snapshot cannot replace the last complete visual state")]
    Incomplete,
    #[error("snapshot belongs to a different simulation session")]
    UnexpectedSession,
}

/// Errors a visualizer can receive from any data source.
///
/// Deliberately carries no in-process solver types: a remote source could never
/// produce them, and a contract that names them is not transport-neutral. Solver
/// failures cross as a stable code plus a message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("snapshot rejected: {0}")]
    Rejected(#[from] SnapshotRejection),
    #[error("this data source does not accept commands")]
    Unsupported,
    #[error("the data source is not connected")]
    Disconnected,
    #[error("solver reported '{code}': {message}")]
    Solver { code: String, message: String },
}

impl From<RuntimeError> for SourceError {
    fn from(error: RuntimeError) -> Self {
        Self::Solver {
            code: error.code().to_owned(),
            message: error.to_string(),
        }
    }
}

/// Wraps an in-process runtime.
pub struct LocalDataSource {
    runtime: SimulationRuntime,
    mailbox: SnapshotMailbox,
    pacer: TickPacer,
    playback_speed: PlaybackSpeed,
    pending_world_edits: VecDeque<Vec<WorldCommand>>,
}

impl LocalDataSource {
    pub fn new(runtime: SimulationRuntime) -> Self {
        let mut source = Self {
            runtime,
            mailbox: SnapshotMailbox::default(),
            pacer: TickPacer::default(),
            playback_speed: PlaybackSpeed::default(),
            pending_world_edits: VecDeque::new(),
        };
        // Publishing through the mailbox even in local mode is what makes the
        // two sources equivalent for consumers.
        let _ = source.publish();
        source
    }

    pub const fn runtime(&self) -> &SimulationRuntime {
        &self.runtime
    }

    pub const fn runtime_mut(&mut self) -> &mut SimulationRuntime {
        &mut self.runtime
    }

    fn publish(&mut self) -> Result<bool, SourceError> {
        Ok(self.mailbox.offer(self.runtime.latest_snapshot())?)
    }

    fn receipt(&self, command: CommandId, disposition: CommandDisposition) -> CommandReceipt {
        let status = self.runtime.status();
        CommandReceipt {
            command,
            world_revision: status.world_revision,
            tick: status.tick(),
            snapshot_sequence: self.runtime.latest_snapshot().identity.sequence,
            disposition,
        }
    }

    fn apply_pending_world_edits(&mut self) -> Result<u32, SourceError> {
        let mut applied = 0;
        while let Some(commands) = self.pending_world_edits.pop_front() {
            self.runtime.commit_world_commands(commands)?;
            applied += 1;
        }
        Ok(applied)
    }
}

impl FieldDataSource for LocalDataSource {
    fn description(&self) -> &str {
        "Local equation runtime"
    }

    fn status(&self) -> DataSourceStatus {
        DataSourceStatus::Ready
    }

    fn simulation_status(&self) -> SimulationStatus {
        self.runtime.status()
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.pending_world_edits.len()
    }

    fn world(&self) -> WorldSnapshot {
        self.runtime.world_snapshot()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        let mut disposition = CommandDisposition::Applied;
        match command.payload {
            CommandPayload::Play => {
                self.runtime.play();
                self.pacer.reset();
            }
            CommandPayload::Pause => {
                self.apply_pending_world_edits()?;
                self.runtime.pause();
            }
            CommandPayload::Step => {
                self.apply_pending_world_edits()?;
                self.runtime.step_once()?;
            }
            CommandPayload::SetTimeStep(step) => {
                self.runtime.set_time_step(step);
                self.pacer.reset();
            }
            CommandPayload::SetPlaybackSpeed(speed) => {
                self.playback_speed = speed;
                self.pacer.reset();
            }
            CommandPayload::CommitWorld(commands) => {
                if self.runtime.status().mode() == SimulationMode::Running {
                    self.pending_world_edits.push_back(commands);
                    disposition = CommandDisposition::Queued;
                } else {
                    self.runtime.commit_world_commands(commands)?;
                }
            }
        }
        self.publish()?;
        Ok(self.receipt(command.id, disposition))
    }

    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        let status = self.runtime.status();
        let elapsed = scale_elapsed(elapsed, self.playback_speed);
        let demand = self.pacer.ticks_due(elapsed, status.time_step());

        let commands_applied = if demand.ticks > 0 && status.mode() == SimulationMode::Running {
            self.apply_pending_world_edits()?
        } else {
            0
        };

        let mut advanced = 0;
        for _ in 0..demand.ticks {
            if !self.runtime.advance_running()? {
                break;
            }
            advanced += 1;
        }

        Ok(PollOutcome {
            snapshot_updated: self.publish()?,
            ticks_advanced: advanced,
            fell_behind: demand.fell_behind && advanced > 0,
            commands_applied,
        })
    }

    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.mailbox.latest()
    }
}

/// A remote service standing in for a network transport.
///
/// The runtime here represents the authoritative side, which in deployment lives
/// in another process. Commands are acknowledged immediately by that side, but
/// snapshots travel over `link` and only become visible on a later `poll` — so a
/// consumer that assumes an acknowledgement means the pixels changed fails here,
/// which is the point.
pub struct LoopbackDataSource {
    server: SimulationRuntime,
    link: VecDeque<Arc<FieldSnapshot>>,
    mailbox: SnapshotMailbox,
    /// What the client currently believes, updated only by acknowledgements and
    /// received snapshots.
    believed: SimulationStatus,
    /// The client's replica of the authoritative world.
    believed_world: WorldSnapshot,
    connected: bool,
    pacer: TickPacer,
    playback_speed: PlaybackSpeed,
    pending_world_edits: VecDeque<Vec<WorldCommand>>,
}

impl LoopbackDataSource {
    pub fn new(server: SimulationRuntime) -> Self {
        let believed = server.status();
        let believed_world = server.world_snapshot();
        let mut source = Self {
            server,
            link: VecDeque::new(),
            mailbox: SnapshotMailbox::default(),
            believed,
            believed_world,
            connected: true,
            pacer: TickPacer::default(),
            playback_speed: PlaybackSpeed::default(),
            pending_world_edits: VecDeque::new(),
        };
        source.transmit();
        source
    }

    /// Move whatever the server has produced onto the wire.
    fn transmit(&mut self) {
        let snapshot = self.server.latest_snapshot();
        let already_queued = self
            .link
            .back()
            .is_some_and(|queued| queued.identity.same_result_as(snapshot.identity));
        if !already_queued {
            self.link.push_back(snapshot);
        }
    }

    /// Simulate losing the connection. The last complete snapshot is retained.
    pub fn disconnect(&mut self) {
        self.connected = false;
        self.link.clear();
    }

    pub fn reconnect(&mut self) {
        self.connected = true;
        self.pacer.reset();
        // Reconciliation: the server's authoritative state is re-sent before any
        // new data is labelled current.
        self.believed = self.server.status();
        self.believed_world = self.server.world_snapshot();
        self.transmit();
    }

    /// How many snapshots are in flight but not yet presented.
    pub fn queued_snapshots(&self) -> usize {
        self.link.len()
    }

    fn apply_pending_world_edits(&mut self) -> Result<u32, SourceError> {
        let mut applied = 0;
        while let Some(commands) = self.pending_world_edits.pop_front() {
            self.server.commit_world_commands(commands)?;
            applied += 1;
        }
        if applied > 0 {
            self.believed = self.server.status();
            self.believed_world = self.server.world_snapshot();
        }
        Ok(applied)
    }
}

impl FieldDataSource for LoopbackDataSource {
    fn description(&self) -> &str {
        "Loopback compute session"
    }

    fn status(&self) -> DataSourceStatus {
        if self.connected {
            DataSourceStatus::Ready
        } else {
            DataSourceStatus::Disconnected
        }
    }

    fn simulation_status(&self) -> SimulationStatus {
        self.believed
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.pending_world_edits.len()
    }

    fn world(&self) -> WorldSnapshot {
        self.believed_world.clone()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        if !self.connected {
            return Err(SourceError::Disconnected);
        }

        let mut disposition = CommandDisposition::Applied;
        match command.payload {
            CommandPayload::Play => {
                self.server.play();
                self.pacer.reset();
            }
            CommandPayload::Pause => {
                self.apply_pending_world_edits()?;
                self.server.pause();
            }
            CommandPayload::Step => {
                self.apply_pending_world_edits()?;
                self.server.step_once()?;
            }
            CommandPayload::SetTimeStep(step) => {
                self.server.set_time_step(step);
                self.pacer.reset();
            }
            CommandPayload::SetPlaybackSpeed(speed) => {
                self.playback_speed = speed;
                self.pacer.reset();
            }
            CommandPayload::CommitWorld(commands) => {
                if self.server.status().mode() == SimulationMode::Running {
                    self.pending_world_edits.push_back(commands);
                    disposition = CommandDisposition::Queued;
                } else {
                    self.server.commit_world_commands(commands)?;
                }
            }
        }

        if disposition == CommandDisposition::Queued {
            let status = self.server.status();
            return Ok(CommandReceipt {
                command: command.id,
                world_revision: status.world_revision,
                tick: status.tick(),
                snapshot_sequence: self.server.latest_snapshot().identity.sequence,
                disposition,
            });
        }

        self.transmit();

        // The acknowledgement carries the authoritative state; the client adopts
        // it rather than assuming its own edit took effect.
        let status = self.server.status();
        self.believed = status;
        self.believed_world = self.server.world_snapshot();
        Ok(CommandReceipt {
            command: command.id,
            world_revision: status.world_revision,
            tick: status.tick(),
            snapshot_sequence: self.server.latest_snapshot().identity.sequence,
            disposition,
        })
    }

    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        if !self.connected {
            return Ok(PollOutcome::default());
        }

        let status = self.server.status();
        let elapsed = scale_elapsed(elapsed, self.playback_speed);
        let demand = self.pacer.ticks_due(elapsed, status.time_step());
        let commands_applied = if demand.ticks > 0 && status.mode() == SimulationMode::Running {
            self.apply_pending_world_edits()?
        } else {
            0
        };
        let mut advanced = 0;
        for _ in 0..demand.ticks {
            if !self.server.advance_running()? {
                break;
            }
            advanced += 1;
        }
        if advanced > 0 || commands_applied > 0 {
            self.transmit();
        }

        let mut updated = false;
        if let Some(snapshot) = self.link.pop_front() {
            updated = self.mailbox.offer(Arc::clone(&snapshot))?;
            if updated {
                self.believed.clock.step.tick = snapshot.identity.tick;
                self.believed.clock.step.time_seconds = snapshot.identity.time_seconds;
                self.believed.world_revision = snapshot.identity.world_revision;
            }
        }

        Ok(PollOutcome {
            snapshot_updated: updated,
            ticks_advanced: advanced,
            fell_behind: demand.fell_behind && advanced > 0,
            commands_applied,
        })
    }

    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.mailbox.latest()
    }
}

fn scale_elapsed(elapsed: Duration, speed: PlaybackSpeed) -> Duration {
    Duration::try_from_secs_f64(elapsed.as_secs_f64() * speed.multiplier()).unwrap_or(Duration::MAX)
}
