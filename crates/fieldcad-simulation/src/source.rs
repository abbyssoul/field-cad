//! The boundary the visualizer talks to, in place of a solver.
//!
//! Both implementations here — an in-process runtime and a loopback stand-in for
//! a remote service — offer the *same* guarantees, not merely the same method
//! names. In particular both publish through a [`SnapshotMailbox`], so the rules
//! about completeness, session identity, and supersession are enforced on one
//! code path rather than only on the path that happens to be remote.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use fieldcad_core::{
    ChannelId, CommitReport, Domain, FieldSnapshot, ObjectId, PluginId, SceneScale, SimulationMode,
    TimeStep, WorldCommand, WorldRevision, WorldSnapshot,
};
use fieldcad_plugin_api::FieldBrushStroke;
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::async_source::CommandEvent;
use crate::runtime::{
    EditHistoryStatus, FieldSystemStatus, RuntimeError, SimulationRuntime, SimulationStatus,
    Subscription, TickPacer,
};

/// Retained terminal command records per session, per
/// `docs/tasks/session-events-and-queue-control.md`.
const MAX_TERMINAL_HISTORY: usize = 256;

/// Client-issued identity for one command, echoed in its acknowledgement.
///
/// Play, pause, and step are commands with correlated acknowledgements; they are
/// never inferred from the timing of incoming frames.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CommandPayload {
    Play,
    Pause,
    Step,
    SetTimeStep(TimeStep),
    /// Replace the numerical lattice and restart from the initial boundary.
    /// When submitted during a run, adoption is queued at the next tick boundary.
    ReconfigureDomain(Domain),
    SetPlaybackSpeed(PlaybackSpeed),
    /// Change what the source samples when it publishes.
    ///
    /// Purely a visualization concern: it changes how densely a result is
    /// observed, never the result itself, so it does not advance the world
    /// revision and is never queued behind a tick boundary. It is a command
    /// rather than a local setting because a remote session must renew its
    /// subscriptions after a reconnect.
    SetSubscription(Subscription),
    /// How many metres one render/camera unit represents, for the desktop
    /// viewport's camera range and gizmo/proxy sizing.
    ///
    /// Purely a presentation concern, exactly like `SetSubscription`: it
    /// changes nothing a solver reads or any stored `Transform`, so it does
    /// not advance the world revision. It is a command rather than a local
    /// UI setting so a remote MCP client can discover and drive the working
    /// scale the same way the desktop app does.
    SetSceneScale(SceneScale),
    /// Activate or deactivate one equation system in this scene. Its declared
    /// object-component schemas remain registered either way.
    SetFieldSystemEnabled {
        plugin: PluginId,
        enabled: bool,
    },
    /// Choose whether one equation system follows every intermediate value of an
    /// interactive edit, or waits for the edit to be committed.
    ///
    /// Purely a cost/latency choice: the same committed world produces the same
    /// result either way. It is a command rather than a local setting because
    /// the deferral has to happen where the solving does.
    SetFieldSystemRealtime {
        plugin: PluginId,
        realtime: bool,
    },
    /// Choose which equation system computes one field, or none.
    ///
    /// A scene has one electric field however many models of it are composed
    /// in, so this is one command rather than a deactivation followed by an
    /// activation: the state in between, where nothing computes the field, is
    /// not one a user asked for.
    SetFieldModel {
        channel: ChannelId,
        provider: Option<PluginId>,
    },
    /// Open or close an interactive edit — a scene edit that spans frames, such
    /// as a viewport drag or an inspector control being held.
    ///
    /// Never queued: it carries no world change of its own, and queueing it
    /// behind a tick boundary would open the gesture after the edits it is
    /// supposed to bracket.
    SetInteractiveEdit(bool),
    /// Add a localized value to a mutable numerical vector field.
    ApplyFieldBrushStroke(FieldBrushStroke),
    CommitWorld(Vec<WorldCommand>),
    /// Step the scene back to how it stood before the most recent authored edit,
    /// or forward again.
    ///
    /// Authoritative, like any other world change: the history belongs with the
    /// world it describes, because only that side can say what the scene was
    /// and validate that it may be restored. A client that kept its own stack
    /// would be guessing.
    Undo,
    Redo,
    /// Hold queued scene/domain mutations at their tick boundary until
    /// resumed. Simulation ticks continue; new eligible mutations are still
    /// accepted and appended. Never itself queued, and idempotent.
    PauseQueue,
    /// Resume a paused queue: held mutations apply at the next eligible tick
    /// boundary, in submission order. Idempotent.
    ResumeQueue,
    /// Cancel one command still waiting for a tick boundary. Only a command
    /// that has not yet applied is cancellable — there is no per-command
    /// cancellation of in-flight solver work here; `SolverCancellation`
    /// remains session-level.
    CancelQueuedCommand(CommandId),
}

impl CommandPayload {
    /// The stable, payload-free label carried by this command's
    /// [`CommandRecord`] while it is queued or after it goes terminal.
    pub fn kind(&self) -> CommandKind {
        match self {
            Self::Play => CommandKind::Play,
            Self::Pause => CommandKind::Pause,
            Self::Step => CommandKind::Step,
            Self::SetTimeStep(_) => CommandKind::SetTimeStep,
            Self::ReconfigureDomain(_) => CommandKind::ReconfigureDomain,
            Self::SetPlaybackSpeed(_) => CommandKind::SetPlaybackSpeed,
            Self::SetSubscription(_) => CommandKind::SetSubscription,
            Self::SetSceneScale(_) => CommandKind::SetSceneScale,
            Self::SetFieldSystemEnabled { .. } => CommandKind::SetFieldSystemEnabled,
            Self::SetFieldSystemRealtime { .. } => CommandKind::SetFieldSystemRealtime,
            Self::SetFieldModel { .. } => CommandKind::SetFieldModel,
            Self::SetInteractiveEdit(_) => CommandKind::SetInteractiveEdit,
            Self::ApplyFieldBrushStroke(_) => CommandKind::ApplyFieldBrushStroke,
            Self::CommitWorld(_) => CommandKind::CommitWorld,
            Self::Undo => CommandKind::Undo,
            Self::Redo => CommandKind::Redo,
            Self::PauseQueue => CommandKind::PauseQueue,
            Self::ResumeQueue => CommandKind::ResumeQueue,
            Self::CancelQueuedCommand(_) => CommandKind::CancelQueuedCommand,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub id: CommandId,
    pub payload: CommandPayload,
}

/// The payload-free shape of one [`CommandPayload`] variant, retained in a
/// [`CommandRecord`] after the payload itself is discarded (queued only, or
/// never serialized at all).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Play,
    Pause,
    Step,
    SetTimeStep,
    ReconfigureDomain,
    SetPlaybackSpeed,
    SetSubscription,
    SetSceneScale,
    SetFieldSystemEnabled,
    SetFieldSystemRealtime,
    SetFieldModel,
    SetInteractiveEdit,
    ApplyFieldBrushStroke,
    CommitWorld,
    Undo,
    Redo,
    PauseQueue,
    ResumeQueue,
    CancelQueuedCommand,
}

impl CommandKind {
    /// A short, human-facing label — for a queue inspector, not the wire
    /// format (which uses the `snake_case` `Serialize` form above).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::Step => "Step",
            Self::SetTimeStep => "Set time step",
            Self::ReconfigureDomain => "Reconfigure domain",
            Self::SetPlaybackSpeed => "Set playback speed",
            Self::SetSubscription => "Set subscription",
            Self::SetSceneScale => "Set scene scale",
            Self::SetFieldSystemEnabled => "Set field system enabled",
            Self::SetFieldSystemRealtime => "Set field system realtime",
            Self::SetFieldModel => "Set field model",
            Self::SetInteractiveEdit => "Set interactive edit",
            Self::ApplyFieldBrushStroke => "Field brush stroke",
            Self::CommitWorld => "Commit world",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::PauseQueue => "Pause queue",
            Self::ResumeQueue => "Resume queue",
            Self::CancelQueuedCommand => "Cancel queued command",
        }
    }
}

/// Where one command currently stands in the mutation queue's lifecycle.
///
/// A `Submitted` record is never produced by [`SessionCore`] itself — it
/// exists only for [`crate::async_source::AsyncLocalDataSource`] to
/// synthesize a display entry for a command already sent to its worker
/// thread but not yet executed there (see
/// [`CommandRecord::submitted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandLifecycle {
    Submitted,
    Queued,
    Applied,
    Rejected,
    Cancelled,
}

/// The mutation a queued [`CommandRecord`] will apply at the next eligible
/// tick boundary. Cleared the instant a record goes terminal, and never
/// serialized — an MCP client never needs to see or reconstruct it, and a
/// `CommitWorld` payload can be arbitrarily large.
#[derive(Clone, Debug, PartialEq)]
enum PendingPayload {
    World(Vec<WorldCommand>),
    Domain(Domain),
}

/// One command's identity, order, and lifecycle — replaces a payload-only
/// pending mutation so identity survives worker submission and tick-boundary
/// application, and so a terminal command remains inspectable afterward.
///
/// Derives `Serialize` only: like every other outward-only wire type in this
/// crate, nothing ever reconstructs one from JSON.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommandRecord {
    pub command: CommandId,
    pub kind: CommandKind,
    /// Submission order. Assigned by `SessionCore`'s own monotonic counter or
    /// by `AsyncLocalDataSource`'s submission counter for a `Submitted` record
    /// synthesized before it reaches the queue — a wall-clock timestamp isn't
    /// needed for "submission order" and this keeps the type deterministic and
    /// dependency-free.
    pub sequence: u64,
    pub state: CommandLifecycle,
    /// Set once `state` is `Applied`.
    pub receipt: Option<CommandReceipt>,
    /// Set once `state` is `Rejected`.
    pub error: Option<String>,
    #[serde(skip)]
    payload: Option<PendingPayload>,
}

impl CommandRecord {
    fn queued(
        command: CommandId,
        kind: CommandKind,
        sequence: u64,
        payload: PendingPayload,
    ) -> Self {
        Self {
            command,
            kind,
            sequence,
            state: CommandLifecycle::Queued,
            receipt: None,
            error: None,
            payload: Some(payload),
        }
    }

    /// A display-only record for a command already sent to a worker thread
    /// but not yet executed there. See [`CommandLifecycle::Submitted`].
    pub fn submitted(command: CommandId, kind: CommandKind, sequence: u64) -> Self {
        Self {
            command,
            kind,
            sequence,
            state: CommandLifecycle::Submitted,
            receipt: None,
            error: None,
            payload: None,
        }
    }
}

/// Authoritative queue state: whether it is paused, the ordered commands
/// still waiting for a tick boundary, and recent terminal history (capped at
/// [`MAX_TERMINAL_HISTORY`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QueueStatus {
    pub paused: bool,
    /// Oldest first: index `0` applies next when eligible.
    pub pending: Vec<CommandRecord>,
    /// Oldest first.
    pub history: Vec<CommandRecord>,
}

/// The shape of a [`QueueStatus`] without its contents: whether it is
/// paused, how many pending/history records it holds, and the newest
/// history entry's id. A change-detecting caller (a publish/broadcast loop
/// that only needs to notice "the queue moved," not read what moved) can
/// build one of these from `pending.len()`/`history.len()`/`history.last()`
/// — none of which need `pending` or `history` themselves cloned, unlike
/// building a whole [`QueueStatus`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueSummary {
    pub paused: bool,
    pub pending_len: usize,
    pub history_len: usize,
    pub newest_history: Option<CommandId>,
}

/// Wall-clock playback rate, kept separate from the numerical time step.
///
/// A multiplier of `2.0` asks the source to schedule twice as many unchanged
/// fixed-size ticks per wall-clock second. It never stretches `dt`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandDisposition {
    Applied,
    Queued,
    /// Accepted by a non-blocking client and awaiting authoritative completion.
    Submitted,
}

/// The authoritative side's answer to one command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Entities this command created, if it was a `CommitWorld` transaction
    /// that has actually applied.
    ///
    /// Empty (never `None`) for every other command, and for a `CommitWorld`
    /// that returned [`CommandDisposition::Queued`] or
    /// [`CommandDisposition::Submitted`] — the creations it will eventually
    /// produce are not yet knowable, since the transaction has not applied.
    /// A caller that needs the IDs from a queued transaction must re-read the
    /// world after it applies rather than wait on this field.
    pub created: CommitReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    fn domain(&self) -> Domain;
    fn playback_speed(&self) -> PlaybackSpeed;
    fn pending_command_count(&self) -> usize;
    /// Authoritative queue state: paused flag, ordered pending commands, and
    /// recent terminal history. See `docs/tasks/session-events-and-queue-control.md`.
    fn get_queue(&self) -> QueueStatus;
    /// The queue's shape without its contents — for a caller (a change
    /// detector, a per-frame view builder deciding whether to keep last
    /// frame's `get_queue()` result) that only needs to know whether the
    /// queue moved, not read what moved. The default falls back to
    /// `get_queue()` itself, so it is always correct; override it wherever a
    /// cheaper path exists — nothing overriding this should ever need to
    /// clone `pending`/`history` to answer it.
    fn queue_summary(&self) -> QueueSummary {
        let queue = self.get_queue();
        QueueSummary {
            paused: queue.paused,
            pending_len: queue.pending.len(),
            history_len: queue.history.len(),
            newest_history: queue.history.last().map(|record| record.command),
        }
    }
    /// What the source is currently asked to publish. Acknowledged, not hoped
    /// for: it reflects the last accepted [`CommandPayload::SetSubscription`].
    fn subscription(&self) -> Subscription;
    /// How many metres one render/camera unit represents. Acknowledged, not
    /// hoped for: it reflects the last accepted
    /// [`CommandPayload::SetSceneScale`]. Defaults to
    /// [`SceneScale::metre`], matching the desktop app's original behaviour.
    fn scene_scale(&self) -> SceneScale;
    /// Equation systems composed into the scene, including inactive systems
    /// that consequently have no channels in the latest snapshot.
    fn field_systems(&self) -> Vec<FieldSystemStatus>;
    /// What undo and redo currently offer, for a control that presents them.
    fn edit_history(&self) -> EditHistoryStatus;

    /// The world the client currently believes in.
    ///
    /// For a local source this is the authoritative world. For a remote one it
    /// is a replica, updated when the service acknowledges an edit — the desktop
    /// draws what it has been told, not what it hopes it submitted.
    fn world(&self) -> WorldSnapshot;

    /// The dynamics system's summed force on every body it advanced, as of the
    /// most recent tick — for an inspector's read-only display. A body with no
    /// entry covers every reason there is nothing to show for it: no mass,
    /// pinned, kinematically owned by a solver's own pusher, or no tick yet.
    /// Empty for a source that does not locally hold this at all (a remote
    /// session that has not chosen to transmit it). Presentation only, like
    /// the rest of this trait: nothing reads this to decide a physical result.
    fn body_forces(&self) -> BTreeMap<ObjectId, DVec3> {
        BTreeMap::new()
    }

    /// Wall-clock milliseconds the most recent simulation tick took to
    /// compute — for an inspector telling a user whether their machine can
    /// keep up with the configured dt. Zero for a source that does not
    /// locally hold this (no tick yet, or a remote session that has not
    /// chosen to transmit it), same convention as `body_forces`.
    fn step_compute_ms(&self) -> f32 {
        0.0
    }

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
    #[error(
        "the mutation queue is paused; '{command}' cannot proceed until the queue resumes or its blocking work is cancelled"
    )]
    QueuePaused { command: &'static str },
    #[error(
        "no queued command with id {0:?} to cancel (already applied, rejected, cancelled, or unknown)"
    )]
    CommandNotQueued(CommandId),
    #[error("command {0:?} is in flight on the compute worker and cannot be cancelled")]
    CommandInFlight(CommandId),
    #[error(
        "'{command}' cannot proceed: a queued mutation was rejected while flushing the pending queue"
    )]
    FlushRejected { command: &'static str },
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
    pending_mutations: VecDeque<CommandRecord>,
    /// Capped at [`MAX_TERMINAL_HISTORY`], oldest evicted first.
    terminal_history: VecDeque<CommandRecord>,
    /// Holds queued scene/domain mutations at their tick boundary. Distinct
    /// from `SimulationMode`: simulation ticks continue while this is set.
    queue_paused: bool,
    next_sequence: u64,
    /// Terminal [`CommandEvent`]s produced by something other than the
    /// current call's own direct return (a flushed mutation, a
    /// cancellation), accumulated across calls until [`Self::take_emitted`]
    /// is called — the one buffer both `LocalDataSource` and
    /// `LoopbackDataSource` used to keep a second, separately-drained copy
    /// of, forwarding through their own `command_events` field and
    /// `drain_command_events` on every `execute`/`poll`. Living here once
    /// removes that duplication: a wrapper's own `drain_command_events` is
    /// just `self.core.take_emitted()`.
    emitted: Vec<CommandEvent>,
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
            pending_mutations: VecDeque::new(),
            terminal_history: VecDeque::new(),
            queue_paused: false,
            next_sequence: 0,
            emitted: Vec::new(),
        }
    }

    fn status(&self) -> SimulationStatus {
        self.runtime.status()
    }

    fn latest_snapshot(&self) -> Arc<FieldSnapshot> {
        self.runtime.latest_snapshot()
    }

    fn pending_count(&self) -> usize {
        self.pending_mutations.len()
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    fn queue_status(&self) -> QueueStatus {
        QueueStatus {
            paused: self.queue_paused,
            pending: self.pending_mutations.iter().cloned().collect(),
            history: self.terminal_history.iter().cloned().collect(),
        }
    }

    fn queue_summary(&self) -> QueueSummary {
        QueueSummary {
            paused: self.queue_paused,
            pending_len: self.pending_mutations.len(),
            history_len: self.terminal_history.len(),
            newest_history: self.terminal_history.back().map(|record| record.command),
        }
    }

    /// Drains every terminal event accumulated since the last drain — a
    /// flushed mutation, a cancellation, or an ordinary `execute`/`advance`
    /// completion. The wrapping source's own `drain_command_events` is
    /// exactly this call.
    fn take_emitted(&mut self) -> Vec<CommandEvent> {
        std::mem::take(&mut self.emitted)
    }

    /// Clears the record's payload (a terminal record needs no payload to
    /// replay, only its outcome) and appends it to the capped history ring.
    fn record_terminal(&mut self, mut record: CommandRecord) {
        record.payload = None;
        self.terminal_history.push_back(record);
        while self.terminal_history.len() > MAX_TERMINAL_HISTORY {
            self.terminal_history.pop_front();
        }
    }

    /// Apply one command to the authoritative side and report how it landed.
    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        let id = command.id;
        let kind = command.payload.kind();
        let mut created = None;
        match command.payload {
            CommandPayload::Play => {
                self.runtime.play();
                self.pacer.reset();
            }
            CommandPayload::Pause => {
                self.reject_if_queue_paused(id, kind, "pause")?;
                self.flush_and_check("pause")?;
                self.runtime.pause();
            }
            CommandPayload::Step => {
                self.reject_if_queue_paused(id, kind, "step")?;
                self.flush_and_check("step")?;
                self.runtime.step_once()?;
            }
            CommandPayload::SetTimeStep(step) => {
                self.runtime.set_time_step(step)?;
                self.pacer.reset();
            }
            CommandPayload::ReconfigureDomain(domain) => {
                if self.should_queue_mutation() {
                    let record = CommandRecord::queued(
                        id,
                        kind,
                        self.next_sequence(),
                        PendingPayload::Domain(domain),
                    );
                    self.pending_mutations.push_back(record);
                    return Ok(self.receipt(
                        id,
                        CommandDisposition::Queued,
                        CommitReport::empty(self.runtime.status().world_revision),
                    ));
                }
                self.runtime.reconfigure_domain(domain)?;
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
            CommandPayload::SetSceneScale(scale) => {
                // Never queued, like `SetSubscription`: it is a presentation
                // setting, not a computed value, so there is no tick boundary
                // for it to be atomic with.
                self.runtime.set_scene_scale(scale);
            }
            CommandPayload::SetFieldSystemEnabled { plugin, enabled } => {
                self.runtime.set_field_system_enabled(&plugin, enabled)?;
            }
            CommandPayload::SetFieldSystemRealtime { plugin, realtime } => {
                self.runtime.set_field_system_realtime(&plugin, realtime)?;
            }
            CommandPayload::SetFieldModel { channel, provider } => {
                self.runtime.set_field_model(&channel, provider.as_ref())?;
            }
            CommandPayload::SetInteractiveEdit(editing) => {
                self.runtime.set_interactive_edit(editing)?;
            }
            CommandPayload::ApplyFieldBrushStroke(stroke) => {
                self.runtime.apply_field_brush_stroke(stroke)?;
            }
            CommandPayload::CommitWorld(commands) => {
                if self.should_queue_mutation() {
                    let record = CommandRecord::queued(
                        id,
                        kind,
                        self.next_sequence(),
                        PendingPayload::World(commands),
                    );
                    self.pending_mutations.push_back(record);
                    return Ok(self.receipt(
                        id,
                        CommandDisposition::Queued,
                        CommitReport::empty(self.runtime.status().world_revision),
                    ));
                }
                created = Some(self.runtime.commit_world_commands(commands)?);
            }
            CommandPayload::Undo => {
                // Never queued. An edit waiting for a tick boundary is an edit
                // the history has not recorded yet, so undoing past it would
                // step over an edit that is still on its way in.
                self.reject_if_queue_paused(id, kind, "undo")?;
                self.flush_and_check("undo")?;
                self.runtime.undo()?;
            }
            CommandPayload::Redo => {
                self.reject_if_queue_paused(id, kind, "redo")?;
                self.flush_and_check("redo")?;
                self.runtime.redo()?;
            }
            CommandPayload::PauseQueue => {
                self.queue_paused = true;
            }
            CommandPayload::ResumeQueue => {
                self.queue_paused = false;
                // `Running` leaves this to the next tick boundary (ADR 0011):
                // `advance` already flushes as soon as it sees the queue is
                // no longer paused, and doing it here too would apply a
                // mutation off the tick it is supposed to be atomic with.
                // `Paused` has no boundary to wait for — a mutation held here
                // only because the queue was paused (not because the sim
                // was) is due immediately once that reason is gone, same as
                // any other edit submitted while paused.
                if self.runtime.status().mode() != SimulationMode::Running {
                    self.flush_pending_mutations();
                }
            }
            CommandPayload::CancelQueuedCommand(target) => {
                let Some(index) = self
                    .pending_mutations
                    .iter()
                    .position(|record| record.command == target)
                else {
                    return Err(SourceError::CommandNotQueued(target));
                };
                let mut record = self
                    .pending_mutations
                    .remove(index)
                    .expect("index was just found in this deque");
                record.state = CommandLifecycle::Cancelled;
                self.record_terminal(record);
                self.emitted.push(CommandEvent::Cancelled(target));
            }
        }
        let created =
            created.unwrap_or_else(|| CommitReport::empty(self.runtime.status().world_revision));
        Ok(self.receipt(id, CommandDisposition::Applied, created))
    }

    /// Whether a scene/domain mutation must wait rather than land immediately:
    /// either there is a tick boundary it needs to be atomic with
    /// (`Running`, per ADR 0011), or the mutation queue is explicitly paused
    /// and holding *everything* is exactly what was asked for, boundary or
    /// not. Without the second half, a mutation submitted while genuinely
    /// paused would always apply on the spot — true per ADR 0011 on its own,
    /// but it defeats "paused" as a promise the queue makes: a user who
    /// paused it to hold a slow-solving edit for cancellation, or simply to
    /// keep the queue's contents predictable, would see it apply anyway the
    /// moment the drag that produced it released.
    fn should_queue_mutation(&self) -> bool {
        self.runtime.status().mode() == SimulationMode::Running || self.queue_paused
    }

    /// `Pause`/`Step`/`Undo`/`Redo` must not silently flush a paused queue.
    /// Fires only when there is something a flush would actually hold back —
    /// pausing an idle queue must not block these commands.
    ///
    /// Records the rejection in terminal history and emits a `CommandEvent::Failed`
    /// before returning the error, matching the contract that every terminal
    /// command state is recoverable through queue history (BE-7).
    fn reject_if_queue_paused(
        &mut self,
        id: CommandId,
        kind: CommandKind,
        command_label: &'static str,
    ) -> Result<(), SourceError> {
        if self.queue_paused && !self.pending_mutations.is_empty() {
            let error = SourceError::QueuePaused {
                command: command_label,
            };
            let record = CommandRecord {
                command: id,
                kind,
                sequence: self.next_sequence(),
                state: CommandLifecycle::Rejected,
                receipt: None,
                error: Some(error.to_string()),
                payload: None,
            };
            self.record_terminal(record);
            self.emitted.push(CommandEvent::Failed {
                command: id,
                error: error.clone(),
            });
            return Err(error);
        }
        Ok(())
    }

    /// Flushes the pending queue and aborts `command` if the flush rejected
    /// a mutation. `Pause`/`Step`/`Undo`/`Redo` all move the runtime past a
    /// tick boundary, so they must not proceed on top of a boundary whose
    /// preceding queued edit never actually landed — the mode is left
    /// unchanged so the rejected mutation's still-queued successors get
    /// another flush attempt on the next `advance` rather than being
    /// stranded by a state change to non-`Running` (see `advance`).
    fn flush_and_check(&mut self, command: &'static str) -> Result<(), SourceError> {
        let summary = self.flush_pending_mutations();
        if summary.rejected {
            return Err(SourceError::FlushRejected { command });
        }
        Ok(())
    }

    /// Take in wall-clock time and advance whole fixed ticks, applying queued
    /// edits immediately before the first of them, unless the queue is
    /// paused — a paused queue holds its mutations across ticks.
    fn advance(&mut self, elapsed: Duration) -> Result<TickProgress, SourceError> {
        let status = self.runtime.status();
        let elapsed = scale_elapsed(elapsed, self.playback_speed);
        let demand = self.pacer.ticks_due(elapsed, status.time_step());

        let summary =
            if demand.ticks > 0 && status.mode() == SimulationMode::Running && !self.queue_paused {
                self.flush_pending_mutations()
            } else {
                FlushSummary::default()
            };
        let status_after_flush = self.runtime.status();

        // A rejected flush stops this cycle's ticks rather than advancing
        // past a boundary whose mutation just failed. The pacer has already
        // been paid for the demand, so the vetoed ticks' budget is handed
        // back: the blockage re-surfaces as ordinary demand on the next
        // `advance` (reported as `fell_behind` there if it has outgrown the
        // per-poll budget) instead of silently discarding simulation time.
        // `status.time_step()` is stable across the flush — the only queued
        // mutation that resets the pacer, `ReconfigureDomain`, does not
        // change `dt`.
        let mut ticks_advanced = 0;
        if summary.rejected {
            self.pacer.return_ticks(demand.ticks, status.time_step());
        } else {
            for _ in 0..demand.ticks {
                if !self.runtime.advance_running()? {
                    break;
                }
                ticks_advanced += 1;
            }
        }

        Ok(TickProgress {
            ticks_advanced,
            commands_applied: summary.applied,
            fell_behind: demand.fell_behind && ticks_advanced > 0,
            status_after_flush,
        })
    }

    fn receipt(
        &self,
        command: CommandId,
        disposition: CommandDisposition,
        created: CommitReport,
    ) -> CommandReceipt {
        let status = self.status();
        CommandReceipt {
            command,
            world_revision: status.world_revision,
            tick: status.tick(),
            snapshot_sequence: Some(self.latest_snapshot().identity.sequence),
            disposition,
            created,
        }
    }

    /// Applies every still-queued mutation in submission order. Infallible:
    /// a rejected mutation becomes its own terminal `Rejected` record
    /// (observable through `get_queue()`'s history) rather than failing the
    /// whole tick boundary or command that triggered the flush.
    fn flush_pending_mutations(&mut self) -> FlushSummary {
        let mut summary = FlushSummary::default();
        while let Some(mut record) = self.pending_mutations.pop_front() {
            let command_id = record.command;
            let payload = record
                .payload
                .take()
                .expect("a queued record always carries its payload");
            let result: Result<CommitReport, RuntimeError> = match payload {
                PendingPayload::World(commands) => self.runtime.commit_world_commands(commands),
                PendingPayload::Domain(domain) => {
                    let outcome = self.runtime.reconfigure_domain(domain);
                    if outcome.is_ok() {
                        self.pacer.reset();
                    }
                    outcome.map(|()| CommitReport::empty(self.runtime.status().world_revision))
                }
            };
            match result {
                Ok(created) => {
                    let receipt = self.receipt(command_id, CommandDisposition::Applied, created);
                    record.state = CommandLifecycle::Applied;
                    record.receipt = Some(receipt.clone());
                    self.record_terminal(record);
                    self.emitted.push(CommandEvent::Completed(receipt));
                    summary.applied += 1;
                }
                Err(error) => {
                    let error: SourceError = error.into();
                    record.state = CommandLifecycle::Rejected;
                    record.error = Some(error.to_string());
                    self.record_terminal(record);
                    self.emitted.push(CommandEvent::Failed {
                        command: command_id,
                        error,
                    });
                    summary.rejected = true;
                    break;
                }
            }
        }
        summary
    }
}

/// What one call to [`SessionCore::flush_pending_mutations`] did.
#[derive(Default)]
struct FlushSummary {
    applied: u32,
    rejected: bool,
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

    pub fn cancellation(&self) -> fieldcad_plugin_api::SolverCancellation {
        self.core.runtime.cancellation()
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

    fn domain(&self) -> Domain {
        *self.core.runtime.domain()
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.core.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.core.pending_count()
    }

    fn get_queue(&self) -> QueueStatus {
        self.core.queue_status()
    }

    fn queue_summary(&self) -> QueueSummary {
        self.core.queue_summary()
    }

    fn subscription(&self) -> Subscription {
        self.core.runtime.subscription()
    }

    fn scene_scale(&self) -> SceneScale {
        self.core.runtime.scene_scale()
    }

    fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.core.runtime.field_systems()
    }

    fn edit_history(&self) -> EditHistoryStatus {
        self.core.runtime.edit_history()
    }

    fn world(&self) -> WorldSnapshot {
        self.core.runtime.world_snapshot()
    }

    fn body_forces(&self) -> BTreeMap<ObjectId, DVec3> {
        self.core.runtime.body_forces()
    }

    fn step_compute_ms(&self) -> f32 {
        self.core.runtime.last_tick_compute_ms()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        // `core.emitted` accumulates regardless of the outcome below, so a
        // side-effect flush's events are never lost even when the command
        // that triggered them is itself rejected — nothing here needs to
        // drain it before propagating `?`, unlike when each wrapper kept a
        // second, separately-drained copy of this buffer.
        let receipt = self.core.execute(command)?;
        self.publish()?;
        Ok(receipt)
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

    fn drain_command_events(&mut self) -> Vec<CommandEvent> {
        self.core.take_emitted()
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
    /// The numerical domain acknowledged by the server. Kept alongside the
    /// world because domain edits, like scene edits, may be queued remotely.
    believed_domain: Domain,
    believed_field_systems: Vec<FieldSystemStatus>,
    believed_edit_history: EditHistoryStatus,
    connected: bool,
}

impl LoopbackDataSource {
    pub fn new(server: SimulationRuntime) -> Self {
        let believed = server.status();
        let believed_world = server.world_snapshot();
        let believed_domain = *server.domain();
        let believed_field_systems = server.field_systems();
        let believed_edit_history = server.edit_history();
        let mut source = Self {
            core: SessionCore::new(server),
            link: VecDeque::new(),
            mailbox: SnapshotMailbox::default(),
            believed,
            believed_world,
            believed_domain,
            believed_field_systems,
            believed_edit_history,
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
        self.believed_domain = *self.core.runtime.domain();
        self.believed_field_systems = self.core.runtime.field_systems();
        self.believed_edit_history = self.core.runtime.edit_history();
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

    fn domain(&self) -> Domain {
        self.believed_domain
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.core.playback_speed
    }

    fn pending_command_count(&self) -> usize {
        self.core.pending_count()
    }

    fn get_queue(&self) -> QueueStatus {
        // Unfiltered by `believed_*`: the queue itself, unlike a snapshot, is
        // already known synchronously in this stand-in, matching the
        // existing precedent set by `pending_command_count` above.
        self.core.queue_status()
    }

    fn queue_summary(&self) -> QueueSummary {
        self.core.queue_summary()
    }

    fn subscription(&self) -> Subscription {
        self.core.runtime.subscription()
    }

    fn scene_scale(&self) -> SceneScale {
        self.core.runtime.scene_scale()
    }

    fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.believed_field_systems.clone()
    }

    fn edit_history(&self) -> EditHistoryStatus {
        self.believed_edit_history.clone()
    }

    fn world(&self) -> WorldSnapshot {
        self.believed_world.clone()
    }

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        if !self.connected {
            return Err(SourceError::Disconnected);
        }

        // `core.emitted` accumulates regardless of the outcome below — see
        // `LocalDataSource::execute`.
        let receipt = self.core.execute(command)?;
        // A queued edit has changed nothing the server can publish or the client
        // can believe yet, so nothing goes on the wire until its boundary.
        if receipt.disposition == CommandDisposition::Applied {
            self.transmit();
            self.adopt(self.core.status());
        }
        Ok(receipt)
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

    fn drain_command_events(&mut self) -> Vec<CommandEvent> {
        self.core.take_emitted()
    }
}

fn scale_elapsed(elapsed: Duration, speed: PlaybackSpeed) -> Duration {
    Duration::try_from_secs_f64(elapsed.as_secs_f64() * speed.multiplier()).unwrap_or(Duration::MAX)
}
