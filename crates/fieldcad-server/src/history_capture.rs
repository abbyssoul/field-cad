//! Converts this crate's live, session-scoped observation histories
//! (`fieldcad_simulation::{ProbeHistory, DistanceHistory,
//! MassAggregateHistory}`) into `fieldcad_scene_document`'s plain,
//! serializable mirrors of them, for [`crate::HeadlessServer::save_run`].
//!
//! Mirrors `apps/fieldcad-desktop/src/probe_history_state.rs`'s capture
//! half; that module cannot be reused directly (an app crate is the wrong
//! dependency direction for a library crate), and this crate only ever
//! captures — it never needs to restore a live history from a document, so
//! there is no restore half here.

use fieldcad_scene_document::{
    DistanceHistoryState, DistanceReadingRecord, DistanceSeriesRecord, MassAggregateHistoryState,
    MassAggregateReadingRecord, MassAggregateSeriesRecord, ProbeHistoryState, ProbeReadingRecord,
    ProbeSeriesRecord,
};
use fieldcad_simulation::{DistanceHistory, MassAggregateHistory, ProbeHistory};

pub fn capture_probe_history(history: &ProbeHistory) -> ProbeHistoryState {
    ProbeHistoryState {
        series: history
            .entries()
            .map(|(probe, channel, readings)| ProbeSeriesRecord {
                probe,
                channel: channel.clone(),
                readings: readings
                    .iter()
                    .map(|reading| ProbeReadingRecord {
                        tick: reading.tick,
                        time_seconds: reading.time_seconds,
                        world_revision: reading.world_revision,
                        snapshot_sequence: reading.snapshot_sequence,
                        value: reading.value,
                        validity: reading.validity,
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn capture_distance_history(history: &DistanceHistory) -> DistanceHistoryState {
    DistanceHistoryState {
        series: history
            .entries()
            .map(|(probe, readings)| DistanceSeriesRecord {
                probe,
                readings: readings
                    .iter()
                    .map(|reading| DistanceReadingRecord {
                        tick: reading.tick,
                        time_seconds: reading.time_seconds,
                        world_revision: reading.world_revision,
                        snapshot_sequence: reading.snapshot_sequence,
                        distance: reading.distance,
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn capture_mass_aggregate_history(
    history: &MassAggregateHistory,
) -> MassAggregateHistoryState {
    MassAggregateHistoryState {
        series: history
            .entries()
            .map(|(probe, readings)| MassAggregateSeriesRecord {
                probe,
                readings: readings
                    .iter()
                    .map(|reading| MassAggregateReadingRecord {
                        tick: reading.tick,
                        time_seconds: reading.time_seconds,
                        world_revision: reading.world_revision,
                        snapshot_sequence: reading.snapshot_sequence,
                        center_of_mass: reading.center_of_mass,
                        velocity: reading.velocity,
                        total_momentum: reading.total_momentum,
                        angular_momentum: reading.angular_momentum,
                        total_kinetic_energy_j: reading.total_kinetic_energy_j,
                        total_mass_kg: reading.total_mass_kg,
                        member_count: reading.member_count,
                    })
                    .collect(),
            })
            .collect(),
    }
}
