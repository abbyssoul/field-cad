//! A bounded, multi-consumer broadcast of session state, replacing the
//! single destructively-drained `Vec<CommandEvent>` this crate used to hand
//! every transport. See `docs/tasks/session-events-and-queue-control.md`.
//!
//! Built on [`tokio::sync::broadcast`]: it is already bounded, multi-consumer
//! with an independent cursor per subscriber (no destructive drain), and its
//! own lag signal *is* the resync marker the task calls for — no custom ring
//! or synthetic event needs inventing.

use std::sync::Arc;

use fieldcad_core::{SnapshotIdentity, SolverDiagnostic};
use fieldcad_simulation::{
    AsyncLocalDataSource, CommandEvent, CommandId, DataSourceStatus, FieldDataSource, QueueSummary,
    SimulationStatus,
};
use tokio::sync::broadcast;

/// Retained events per subscriber before an idle watcher is told to resync
/// instead of catching up event-by-event. Matches the terminal-history cap
/// (`MAX_TERMINAL_HISTORY` in `fieldcad-simulation`): a watcher that lapses
/// for a full history's worth of activity has nothing left to gain from
/// replaying it one event at a time.
const EVENT_HUB_CAPACITY: usize = 256;

/// One invalidation/summary signal. A subscriber re-reads the authoritative
/// resource this names for the full payload — this is never the payload
/// itself.
///
/// `QueueUpdated` carries [`QueueSummary`] rather than the full
/// `QueueStatus`: change detection only ever compares four scalars, so
/// there is no reason to clone `pending`/`history` for it.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    SnapshotUpdated(SnapshotIdentity),
    DiagnosticsUpdated(SnapshotIdentity),
    StatusUpdated(SimulationStatus),
    SourceStatusUpdated(DataSourceStatus),
    QueueUpdated(QueueSummary),
    CommandTerminal(CommandId),
}

/// Owned by [`crate::HeadlessServer`]. Every publication flows through
/// [`EventHub::publish_state`]/[`EventHub::publish_command_event`], so
/// dedup state lives in exactly one place.
pub struct EventHub {
    sender: broadcast::Sender<SessionEvent>,
    last_snapshot: Option<u64>,
    last_diagnostics: Option<Arc<[SolverDiagnostic]>>,
    last_status: Option<SimulationStatus>,
    last_source_status: Option<DataSourceStatus>,
    last_queue: Option<QueueSummary>,
}

impl Default for EventHub {
    fn default() -> Self {
        let (sender, _receiver) = broadcast::channel(EVENT_HUB_CAPACITY);
        Self {
            sender,
            last_snapshot: None,
            last_diagnostics: None,
            last_status: None,
            last_source_status: None,
            last_queue: None,
        }
    }
}

impl EventHub {
    pub fn subscribe(&self) -> EventWatcher {
        EventWatcher(self.sender.subscribe())
    }

    /// Compares every state category against what was last published and
    /// sends (and remembers) only what changed. A snapshot update may be
    /// superseded under backpressure — that is normal, not a lost event —
    /// but no subscriber is ever told about the same state twice.
    pub fn publish_state(&mut self, source: &AsyncLocalDataSource) {
        if let Some(snapshot) = source.latest_snapshot() {
            let identity = snapshot.identity;
            if self.last_snapshot != Some(identity.sequence) {
                self.last_snapshot = Some(identity.sequence);
                self.send(SessionEvent::SnapshotUpdated(identity));
            }
            if self.last_diagnostics.as_ref() != Some(&snapshot.diagnostics) {
                self.last_diagnostics = Some(Arc::clone(&snapshot.diagnostics));
                self.send(SessionEvent::DiagnosticsUpdated(identity));
            }
        }

        let status = source.simulation_status();
        if self.last_status != Some(status) {
            self.last_status = Some(status);
            self.send(SessionEvent::StatusUpdated(status));
        }

        let source_status = source.status();
        if self.last_source_status.as_ref() != Some(&source_status) {
            self.last_source_status = Some(source_status.clone());
            self.send(SessionEvent::SourceStatusUpdated(source_status));
        }

        // `queue_summary`, not `get_queue`: this only ever compares four
        // scalars against what was last published, so there is no reason to
        // pay for cloning `pending`/`history` on every publish just to
        // discard them again immediately after.
        let queue = source.queue_summary();
        if self.last_queue != Some(queue) {
            self.last_queue = Some(queue);
            self.send(SessionEvent::QueueUpdated(queue));
        }
    }

    /// A terminal command transition is never "the same" twice, so — unlike
    /// the deduplicated state categories above — this is always sent.
    pub fn publish_command_event(&mut self, event: &CommandEvent) {
        self.send(SessionEvent::CommandTerminal(event.command_id()));
    }

    fn send(&self, event: SessionEvent) {
        // Zero receivers is the normal case for a session with no watchers
        // attached yet, not a failure.
        let _ = self.sender.send(event);
    }
}

/// What one [`EventWatcher::try_next`]/[`EventWatcher::recv`] call yielded.
#[derive(Clone, Debug)]
pub enum WatchEvent {
    Session(SessionEvent),
    /// This watcher fell behind the hub's bounded capacity and some events
    /// were dropped. Re-read every authoritative resource rather than trying
    /// to reconstruct what was missed.
    Lagged,
    /// The underlying broadcast sender was dropped and no more events will
    /// arrive. A polling consumer that receives this should stop; a
    /// previously-closed watcher continues to yield `Closed` on every call
    /// (the broadcast receiver's own behaviour).
    Closed,
}

/// One subscriber's independent cursor into the [`EventHub`]'s broadcast.
/// Never destructively drained by another subscriber — that is the property
/// this type exists to guarantee.
pub struct EventWatcher(broadcast::Receiver<SessionEvent>);

impl EventWatcher {
    /// For a synchronous caller.
    pub fn try_next(&mut self) -> Option<WatchEvent> {
        match self.0.try_recv() {
            Ok(event) => Some(WatchEvent::Session(event)),
            Err(broadcast::error::TryRecvError::Lagged(_)) => Some(WatchEvent::Lagged),
            Err(broadcast::error::TryRecvError::Closed) => Some(WatchEvent::Closed),
            Err(broadcast::error::TryRecvError::Empty) => None,
        }
    }

    /// Drain all available events up to and including the first `Closed`.
    /// Stops at `Closed` rather than looping forever (the broadcast receiver
    /// returns `Closed` on every subsequent call).
    pub fn drain(&mut self) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        loop {
            match self.try_next() {
                Some(WatchEvent::Closed) => {
                    events.push(WatchEvent::Closed);
                    return events;
                }
                Some(event) => events.push(event),
                None => return events,
            }
        }
    }

    /// For an async caller (an MCP `subscriptions/listen` loop).
    pub async fn recv(&mut self) -> Option<WatchEvent> {
        match self.0.recv().await {
            Ok(event) => Some(WatchEvent::Session(event)),
            Err(broadcast::error::RecvError::Lagged(_)) => Some(WatchEvent::Lagged),
            Err(broadcast::error::RecvError::Closed) => Some(WatchEvent::Closed),
        }
    }
}
