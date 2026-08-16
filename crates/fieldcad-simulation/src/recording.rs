//! Deterministic command and pacing fixtures.
//!
//! Recordings contain semantic commands and elapsed wall-clock intervals, not
//! rendered frames. Replaying the same recording against a freshly constructed
//! source therefore exercises fixed-tick determinism without depending on GUI
//! cadence or on whether compute is local or remote.

use std::time::Duration;

use fieldcad_core::{SnapshotIdentity, WorldSnapshot};
use serde::{Deserialize, Serialize};

use crate::{
    CommandPayload, CommandReceipt, CommandSequencer, FieldDataSource, FieldSystemStatus,
    PlaybackSpeed, PollOutcome, SimulationStatus, SourceError,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RecordedEvent {
    Command(CommandPayload),
    Poll(Duration),
}

/// `Serialize`/`Deserialize` so a recording can be written to and read back
/// from a file — see `fieldcad_server::HeadlessServer::{start_recording,
/// stop_recording, replay_recording}` and
/// `docs/tasks/session-recording-and-replay.md`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionRecording {
    events: Vec<RecordedEvent>,
}

impl SessionRecording {
    pub const fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn record_command(&mut self, payload: CommandPayload) {
        self.events.push(RecordedEvent::Command(payload));
    }

    pub fn record_poll(&mut self, elapsed: Duration) {
        self.events.push(RecordedEvent::Poll(elapsed));
    }

    pub fn with_command(mut self, payload: CommandPayload) -> Self {
        self.record_command(payload);
        self
    }

    pub fn with_poll(mut self, elapsed: Duration) -> Self {
        self.record_poll(elapsed);
        self
    }

    pub fn events(&self) -> &[RecordedEvent] {
        &self.events
    }

    pub fn replay(
        &self,
        source: &mut dyn FieldDataSource,
    ) -> Result<Vec<ReplayObservation>, SourceError> {
        let mut sequencer = CommandSequencer::default();
        let mut observations = Vec::with_capacity(self.events.len());

        for (event_index, event) in self.events.iter().enumerate() {
            let (receipt, poll) = match event {
                RecordedEvent::Command(payload) => {
                    let receipt = source.execute(sequencer.issue(payload.clone()))?;
                    (Some(receipt), None)
                }
                RecordedEvent::Poll(elapsed) => (None, Some(source.poll(*elapsed)?)),
            };
            observations.push(ReplayObservation::capture(
                event_index,
                source,
                receipt,
                poll,
            ));
        }

        Ok(observations)
    }
}

/// Observable core/session state after one recorded event.
///
/// This intentionally excludes presentation and wall-clock timestamps. Floating
/// simulation values are retained exactly so equality catches any drift.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayObservation {
    pub event_index: usize,
    pub receipt: Option<CommandReceipt>,
    pub poll: Option<PollOutcome>,
    pub simulation: SimulationStatus,
    pub playback_speed: PlaybackSpeed,
    pub pending_commands: usize,
    pub field_systems: Vec<FieldSystemStatus>,
    pub world: WorldSnapshot,
    pub snapshot: Option<SnapshotIdentity>,
}

impl ReplayObservation {
    fn capture(
        event_index: usize,
        source: &dyn FieldDataSource,
        receipt: Option<CommandReceipt>,
        poll: Option<PollOutcome>,
    ) -> Self {
        Self {
            event_index,
            receipt,
            poll,
            simulation: source.simulation_status(),
            playback_speed: source.playback_speed(),
            pending_commands: source.pending_command_count(),
            field_systems: source.field_systems(),
            world: source.world(),
            snapshot: source.latest_snapshot().map(|snapshot| snapshot.identity),
        }
    }
}
