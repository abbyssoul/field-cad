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

use crate::runtime::{RuntimeError, SimulationRuntime, SimulationStatus, Subscription, TickPacer};

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
    /// Change what the source samples when it publishes.
    ///
    /// Purely a visualization concern: it changes how densely a result is
    /// observed, never the result itself, so it does not advance the world
    /// revision and is never queued behind a tick boundary. It is a command
    /// rather than a local setting because a remote session must renew its
    /// subscriptions after a reconnect.
    SetSubscription(Subscription),
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
    /// Accepted by a non-blocking client and awaiting authoritative completion.
    Submitted,
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
    pub snapshot_sequence: Option<u64>,
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
    /// What the source is currently asked to publish. Acknowledged, not hoped
    /// for: it reflects the last accepted [`CommandPayload::SetSubscription`].
    fn subscription(&self) -> Subscription;

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

    /// Completion/rejection events produced after a non-blocking submission.
    /// Synchronous and remote-loopback sources return their receipt directly and
    /// therefore have no deferred events to drain.
    fn drain_command_events(&mut self) -> Vec<crate::CommandEvent> {
        Vec::new()
    }
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

/// The authoritative side of a session: a runtime, its wall-clock pacing, and
/// the queue of edits waiting for a fixed-tick boundary.
///
/// Both data sources own one of these. The rules in ADR 0011 — what may be
/// applied immediately, what is queued, when a queue is flushed, and how
/// wall-clock time becomes whole fixed ticks — are therefore implemented once.
/// What genuinely differs between a local runtime and a compute service is
/// *delivery*, and that is all the two source types below still contain.
struct SessionCore {
    runtime: SimulationRuntime,
    pacer: TickPacer,
    playback_speed: PlaybackSpeed,
    pending_world_edits: VecDeque<Vec<WorldCommand>>,
}

/// What one wall-clock advance actually did.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TickProgress {
    ticks_advanced: u32,
    commands_applied: u32,
    fell_behind: bool,
    /// Authoritative state as it stood after queued edits were flushed but
    /// before any tick was taken.
    ///
    /// A remote client adopts *this*, not the state after the ticks: an edit was
    /// acknowledged at the boundary, but the ticks that followed are only known
    /// to the client once their snapshots arrive over the link.
    status_after_flush: SimulationStatus,
}

impl SessionCore {
    fn new(runtime: SimulationRuntime) -> Self {
        Self {
            runtime,
            pacer: TickPacer::default(),
            playback_speed: PlaybackSpeed::default(),
            pending_world_edits: VecDeque::new(),
        }
    }

    fn status(&self) -> SimulationStatus {
        self.runtime.status()
    }

    fn latest_snapshot(&self) -> Arc<FieldSnapshot> {
        self.runtime.latest_snapshot()
    }

    fn pending_count(&self) -> usize {
        self.pending_world_edits.len()
    }

    /// Apply one command to the authoritative side and report how it landed.
    fn execute(&mut self, payload: CommandPayload) -> Result<CommandDisposition, SourceError> {
        match payload {
            CommandPayload::Play => {
                self.runtime.play();
                self.pacer.reset();
            }
            CommandPayload::Pause => {
                self.flush_pending_world_edits()?;
                self.runtime.pause();
            }
            CommandPayload::Step => {
                self.flush_pending_world_edits()?;
                self.runtime.step_once()?;
            }
            CommandPayload::SetTimeStep(step) => {
                self.runtime.set_time_step(step)?;
                self.pacer.reset();
            }
            CommandPayload::SetPlaybackSpeed(speed) => {
                self.playback_speed = speed;
                self.pacer.reset();
            }
            CommandPayload::SetSubscription(subscription) => {
                // Never queued: it cannot change a computed value, so there is
                // no boundary for it to be atomic with.
                self.runtime.set_subscription(subscription)?;
            }
            CommandPayload::CommitWorld(commands) => {
                if self.runtime.status().mode() == SimulationMode::Running {
                    self.pending_world_edits.push_back(commands);
                    return Ok(CommandDisposition::Queued);
                }
                self.runtime.commit_world_commands(commands)?;
            }
        }
        Ok(CommandDisposition::Applied)
    }

    /// Take in wall-clock time and advance whole fixed ticks, applying queued
    /// edits immediately before the first of them.
    fn advance(&mut self, elapsed: Duration) -> Result<TickProgress, SourceError> {
        let status = self.runtime.status();
        let elapsed = scale_elapsed(elapsed, self.playback_speed);
        let demand = self.pacer.ticks_due(elapsed, status.time_step());

        let commands_applied = if demand.ticks > 0 && status.mode() == SimulationMode::Running {
            self.flush_pending_world_edits()?
        } else {
            0
        };
        let status_after_flush = self.runtime.status();

        let mut ticks_advanced = 0;
        for _ in 0..demand.ticks {
            if !self.runtime.advance_running()? {
                break;
            }
            ticks_advanced += 1;
        }

        Ok(TickProgress {
            ticks_advanced,
            commands_applied,
            fell_behind: demand.fell_behind && ticks_advanced > 0,
            status_after_flush,
        })
    }

    fn receipt(&self, command: CommandId, disposition: CommandDisposition) -> CommandReceipt {
        let status = self.status();
        CommandReceipt {
            command,
            world_revision: status.world_revision,
            tick: status.tick(),
            snapshot_sequence: Some(self.latest_snapshot().identity.sequence),
            disposition,
        }
    }

    fn flush_pending_world_edits(&mut self) -> Result<u32, SourceError> {
        let mut applied = 0;
        while let Some(commands) = self.pending_world_edits.pop_front() {
            self.runtime.commit_world_commands(commands)?;
            applied += 1;
        }
        Ok(applied)
    }
}

/// Wraps an in-process runtime.
pub struct LocalDataSource {
    core: SessionCore,
    mailbox: SnapshotMailbox,
}

impl LocalDataSource {
    pub fn new(runtime: SimulationRuntime) -> Self {
        let mut source = Self {
            core: SessionCore::new(runtime),
            mailbox: SnapshotMailbox::default(),
        };
        // Publishing through the mailbox even in local mode is what makes the
        // two sources equivalent for consumers.
        let _ = source.publish();
        source
    }

    pub const fn runtime(&self) -> &SimulationRuntime {
        &self.core.runtime
    }

    pub const fn runtime_mut(&mut self) -> &mut SimulationRuntime {
        &mut self.core.runtime
    }

    fn publish(&mut self) -> Result<bool, SourceError> {
        Ok(self.mailbox.offer(self.core.latest_snapshot())?)
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
        self.core.status()
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.core.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.core.pending_count()
    }

    fn subscription(&self) -> Subscription {
        self.core.runtime.subscription()
    }

    fn world(&self) -> WorldSnapshot {
        self.core.runtime.world_snapshot()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        let disposition = self.core.execute(command.payload)?;
        self.publish()?;
        Ok(self.core.receipt(command.id, disposition))
    }

    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        let progress = self.core.advance(elapsed)?;
        Ok(PollOutcome {
            snapshot_updated: self.publish()?,
            ticks_advanced: progress.ticks_advanced,
            fell_behind: progress.fell_behind,
            commands_applied: progress.commands_applied,
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
    core: SessionCore,
    link: VecDeque<Arc<FieldSnapshot>>,
    mailbox: SnapshotMailbox,
    /// What the client currently believes, updated only by acknowledgements and
    /// received snapshots.
    believed: SimulationStatus,
    /// The client's replica of the authoritative world.
    believed_world: WorldSnapshot,
    connected: bool,
}

impl LoopbackDataSource {
    pub fn new(server: SimulationRuntime) -> Self {
        let believed = server.status();
        let believed_world = server.world_snapshot();
        let mut source = Self {
            core: SessionCore::new(server),
            link: VecDeque::new(),
            mailbox: SnapshotMailbox::default(),
            believed,
            believed_world,
            connected: true,
        };
        source.transmit();
        source
    }

    /// Move whatever the server has produced onto the wire.
    fn transmit(&mut self) {
        let snapshot = self.core.latest_snapshot();
        let already_queued = self
            .link
            .back()
            .is_some_and(|queued| queued.identity.same_result_as(snapshot.identity));
        if !already_queued {
            self.link.push_back(snapshot);
        }
    }

    /// Adopt authoritative state the server has acknowledged, rather than
    /// assuming the client's own edit took effect.
    fn adopt(&mut self, status: SimulationStatus) {
        self.believed = status;
        self.believed_world = self.core.runtime.world_snapshot();
    }

    /// Simulate losing the connection. The last complete snapshot is retained.
    pub fn disconnect(&mut self) {
        self.connected = false;
        self.link.clear();
    }

    pub fn reconnect(&mut self) {
        self.connected = true;
        self.core.pacer.reset();
        // Reconciliation: the server's authoritative state is re-sent before any
        // new data is labelled current.
        self.adopt(self.core.status());
        self.transmit();
    }

    /// How many snapshots are in flight but not yet presented.
    pub fn queued_snapshots(&self) -> usize {
        self.link.len()
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
        self.core.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.core.pending_count()
    }

    fn subscription(&self) -> Subscription {
        self.core.runtime.subscription()
    }

    fn world(&self) -> WorldSnapshot {
        self.believed_world.clone()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        if !self.connected {
            return Err(SourceError::Disconnected);
        }

        let disposition = self.core.execute(command.payload)?;
        // A queued edit has changed nothing the server can publish or the client
        // can believe yet, so nothing goes on the wire until its boundary.
        if disposition == CommandDisposition::Applied {
            self.transmit();
            self.adopt(self.core.status());
        }
        Ok(self.core.receipt(command.id, disposition))
    }

    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        if !self.connected {
            return Ok(PollOutcome::default());
        }

        let progress = self.core.advance(elapsed)?;
        if progress.commands_applied > 0 {
            self.adopt(progress.status_after_flush);
        }
        if progress.ticks_advanced > 0 || progress.commands_applied > 0 {
            self.transmit();
        }

        let mut snapshot_updated = false;
        if let Some(snapshot) = self.link.pop_front() {
            snapshot_updated = self.mailbox.offer(Arc::clone(&snapshot))?;
            if snapshot_updated {
                self.believed.clock.step.tick = snapshot.identity.tick;
                self.believed.clock.step.time_seconds = snapshot.identity.time_seconds;
                self.believed.world_revision = snapshot.identity.world_revision;
            }
        }

        Ok(PollOutcome {
            snapshot_updated,
            ticks_advanced: progress.ticks_advanced,
            fell_behind: progress.fell_behind,
            commands_applied: progress.commands_applied,
        })
    }

    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.mailbox.latest()
    }
}

fn scale_elapsed(elapsed: Duration, speed: PlaybackSpeed) -> Duration {
    Duration::try_from_secs_f64(elapsed.as_secs_f64() * speed.multiplier()).unwrap_or(Duration::MAX)
}
