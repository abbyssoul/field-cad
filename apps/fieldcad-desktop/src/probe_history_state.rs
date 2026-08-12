//! Bridges this app's live [`ProbeHistory`]/[`DistanceHistory`] ring buffers
//! and `fieldcad_scene_document`'s plain, serializable mirrors of them — see
//! `scene_view_state.rs` for the same pattern applied to camera/view state.
//! `capture_*` for [`crate::app::WindowState::save_scene`], `restore_*` for
//! startup and `replace_session`.

use std::collections::VecDeque;

use fieldcad_scene_document::{
    DistanceHistoryState, DistanceReadingRecord, DistanceSeriesRecord, MassAggregateHistoryState,
    MassAggregateReadingRecord, MassAggregateSeriesRecord, ProbeHistoryState, ProbeReadingRecord,
    ProbeSeriesRecord,
};
use fieldcad_simulation::{
    DistanceHistory, DistanceReading, MassAggregateHistory, MassAggregateReading, ProbeHistory,
    ProbeReading,
};

pub fn capture_probe_history(history: &ProbeHistory) -> ProbeHistoryState {
    ProbeHistoryState {
        series: history
            .entries()
            .map(|(probe, channel, readings)| ProbeSeriesRecord {
                probe,
                channel: channel.clone(),
                readings: readings
                    .iter()
                    .copied()
                    .map(capture_probe_reading)
                    .collect(),
            })
            .collect(),
    }
}

fn capture_probe_reading(reading: ProbeReading) -> ProbeReadingRecord {
    ProbeReadingRecord {
        tick: reading.tick,
        time_seconds: reading.time_seconds,
        world_revision: reading.world_revision,
        snapshot_sequence: reading.snapshot_sequence,
        value: reading.value,
        validity: reading.validity,
    }
}

/// Rebuild a [`ProbeHistory`] from a saved state, bounded to `capacity` —
/// the session's own default, not whatever capacity the saving session
/// happened to use.
pub fn restore_probe_history(state: ProbeHistoryState, capacity: usize) -> ProbeHistory {
    let mut history = ProbeHistory::new(capacity);
    for series in state.series {
        let readings: VecDeque<ProbeReading> = series
            .readings
            .into_iter()
            .map(restore_probe_reading)
            .collect();
        history.insert_series(series.probe, series.channel, readings);
    }
    history
}

fn restore_probe_reading(record: ProbeReadingRecord) -> ProbeReading {
    ProbeReading {
        tick: record.tick,
        time_seconds: record.time_seconds,
        world_revision: record.world_revision,
        snapshot_sequence: record.snapshot_sequence,
        value: record.value,
        validity: record.validity,
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
                    .copied()
                    .map(capture_distance_reading)
                    .collect(),
            })
            .collect(),
    }
}

fn capture_distance_reading(reading: DistanceReading) -> DistanceReadingRecord {
    DistanceReadingRecord {
        tick: reading.tick,
        time_seconds: reading.time_seconds,
        world_revision: reading.world_revision,
        snapshot_sequence: reading.snapshot_sequence,
        distance: reading.distance,
    }
}

pub fn restore_distance_history(state: DistanceHistoryState, capacity: usize) -> DistanceHistory {
    let mut history = DistanceHistory::new(capacity);
    for series in state.series {
        let readings: VecDeque<DistanceReading> = series
            .readings
            .into_iter()
            .map(restore_distance_reading)
            .collect();
        history.insert_series(series.probe, readings);
    }
    history
}

fn restore_distance_reading(record: DistanceReadingRecord) -> DistanceReading {
    DistanceReading {
        tick: record.tick,
        time_seconds: record.time_seconds,
        world_revision: record.world_revision,
        snapshot_sequence: record.snapshot_sequence,
        distance: record.distance,
    }
}

pub fn capture_mass_aggregate_history(history: &MassAggregateHistory) -> MassAggregateHistoryState {
    MassAggregateHistoryState {
        series: history
            .entries()
            .map(|(probe, readings)| MassAggregateSeriesRecord {
                probe,
                readings: readings
                    .iter()
                    .copied()
                    .map(capture_mass_aggregate_reading)
                    .collect(),
            })
            .collect(),
    }
}

fn capture_mass_aggregate_reading(reading: MassAggregateReading) -> MassAggregateReadingRecord {
    MassAggregateReadingRecord {
        tick: reading.tick,
        time_seconds: reading.time_seconds,
        world_revision: reading.world_revision,
        snapshot_sequence: reading.snapshot_sequence,
        center_of_mass: reading.center_of_mass,
        velocity: reading.velocity,
        total_momentum: reading.total_momentum,
        total_kinetic_energy_j: reading.total_kinetic_energy_j,
        total_mass_kg: reading.total_mass_kg,
        member_count: reading.member_count,
    }
}

pub fn restore_mass_aggregate_history(
    state: MassAggregateHistoryState,
    capacity: usize,
) -> MassAggregateHistory {
    let mut history = MassAggregateHistory::new(capacity);
    for series in state.series {
        let readings: VecDeque<MassAggregateReading> = series
            .readings
            .into_iter()
            .map(restore_mass_aggregate_reading)
            .collect();
        history.insert_series(series.probe, readings);
    }
    history
}

fn restore_mass_aggregate_reading(record: MassAggregateReadingRecord) -> MassAggregateReading {
    MassAggregateReading {
        tick: record.tick,
        time_seconds: record.time_seconds,
        world_revision: record.world_revision,
        snapshot_sequence: record.snapshot_sequence,
        center_of_mass: record.center_of_mass,
        velocity: record.velocity,
        total_momentum: record.total_momentum,
        total_kinetic_energy_j: record.total_kinetic_energy_j,
        total_mass_kg: record.total_mass_kg,
        member_count: record.member_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{ChannelId, Dimension, DistanceProbeId, PluginId, ProbeId, Quantity};
    use fieldcad_core::{FieldValue, SampleValidity, WorldRevision};

    fn channel_id() -> ChannelId {
        ChannelId::new(PluginId::new("test").unwrap(), "scalar").unwrap()
    }

    #[test]
    fn probe_history_round_trips_through_capture_and_restore() {
        let mut history = ProbeHistory::new(8);
        history.insert_series(
            ProbeId::new(0),
            channel_id(),
            VecDeque::from([ProbeReading {
                tick: 1,
                time_seconds: 0.5,
                world_revision: WorldRevision::INITIAL,
                snapshot_sequence: 1,
                value: FieldValue::Scalar(Quantity::new(3.0, Dimension::MASS).unwrap()),
                validity: SampleValidity::Exact,
            }]),
        );

        let restored = restore_probe_history(capture_probe_history(&history), 8);

        assert_eq!(restored.readings(ProbeId::new(0), &channel_id()).count(), 1);
        assert_eq!(
            restored
                .readings(ProbeId::new(0), &channel_id())
                .next()
                .unwrap()
                .value,
            FieldValue::Scalar(Quantity::new(3.0, Dimension::MASS).unwrap())
        );
    }

    #[test]
    fn distance_history_round_trips_through_capture_and_restore() {
        let mut history = DistanceHistory::new(8);
        history.insert_series(
            DistanceProbeId::new(0),
            VecDeque::from([DistanceReading {
                tick: 1,
                time_seconds: 0.5,
                world_revision: WorldRevision::INITIAL,
                snapshot_sequence: 1,
                distance: 42.0,
            }]),
        );

        let restored = restore_distance_history(capture_distance_history(&history), 8);

        let readings: Vec<_> = restored.readings(DistanceProbeId::new(0)).collect();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].distance, 42.0);
    }

    #[test]
    fn mass_aggregate_history_round_trips_through_capture_and_restore() {
        use fieldcad_core::MassAggregateProbeId;
        use glam::DVec3;

        let mut history = MassAggregateHistory::new(8);
        history.insert_series(
            MassAggregateProbeId::new(0),
            VecDeque::from([MassAggregateReading {
                tick: 1,
                time_seconds: 0.5,
                world_revision: WorldRevision::INITIAL,
                snapshot_sequence: 1,
                center_of_mass: DVec3::new(1.0, 2.0, 3.0),
                velocity: DVec3::new(0.1, 0.0, 0.0),
                total_momentum: DVec3::new(4.0, 0.0, 0.0),
                total_kinetic_energy_j: 5.0,
                total_mass_kg: 6.0,
                member_count: 2,
            }]),
        );

        let restored = restore_mass_aggregate_history(capture_mass_aggregate_history(&history), 8);

        let readings: Vec<_> = restored.readings(MassAggregateProbeId::new(0)).collect();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].center_of_mass, DVec3::new(1.0, 2.0, 3.0));
        assert_eq!(readings[0].member_count, 2);
    }
}
