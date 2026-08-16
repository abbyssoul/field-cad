//! Converts this crate's live, session-scoped observation histories
//! (`fieldcad_simulation::{ProbeHistory, DistanceHistory,
//! MassAggregateHistory}`) into `fieldcad_scene_document`'s plain,
//! serializable mirrors of them, for [`crate::HeadlessServer::save_run`] and
//! [`crate::HeadlessServer::export_observations`].
//!
//! Mirrors `apps/fieldcad-desktop/src/probe_history_state.rs`'s capture
//! half; that module cannot be reused directly (an app crate is the wrong
//! dependency direction for a library crate), and this crate only ever
//! captures — it never needs to restore a live history from a document, so
//! there is no restore half here.

use fieldcad_core::{ChannelId, DistanceProbeId, MassAggregateProbeId, ProbeId};
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
                readings: readings.iter().map(probe_reading_record).collect(),
            })
            .collect(),
    }
}

/// One probe/channel series — see [`crate::HeadlessServer::export_observations`]:
/// a caller wanting only a specific probe/channel, not the whole retained
/// history. `None` when that probe/channel has no recorded readings, so a
/// caller building a scoped export skips it rather than emitting an empty
/// series — same "every *non-empty* series" discipline
/// [`ProbeHistory::entries`] itself already applies.
pub fn capture_probe_series(
    history: &ProbeHistory,
    probe: ProbeId,
    channel: &ChannelId,
) -> Option<ProbeSeriesRecord> {
    let readings: Vec<_> = history
        .readings(probe, channel)
        .map(probe_reading_record)
        .collect();
    (!readings.is_empty()).then(|| ProbeSeriesRecord {
        probe,
        channel: channel.clone(),
        readings,
    })
}

fn probe_reading_record(reading: &fieldcad_simulation::ProbeReading) -> ProbeReadingRecord {
    ProbeReadingRecord {
        tick: reading.tick,
        time_seconds: reading.time_seconds,
        world_revision: reading.world_revision,
        snapshot_sequence: reading.snapshot_sequence,
        value: reading.value,
        validity: reading.validity,
    }
}

pub fn capture_distance_history(history: &DistanceHistory) -> DistanceHistoryState {
    DistanceHistoryState {
        series: history
            .entries()
            .map(|(probe, readings)| DistanceSeriesRecord {
                probe,
                readings: readings.iter().map(distance_reading_record).collect(),
            })
            .collect(),
    }
}

/// See [`capture_probe_series`].
pub fn capture_distance_series(
    history: &DistanceHistory,
    probe: DistanceProbeId,
) -> Option<DistanceSeriesRecord> {
    let readings: Vec<_> = history
        .readings(probe)
        .map(distance_reading_record)
        .collect();
    (!readings.is_empty()).then_some(DistanceSeriesRecord { probe, readings })
}

fn distance_reading_record(
    reading: &fieldcad_simulation::DistanceReading,
) -> DistanceReadingRecord {
    DistanceReadingRecord {
        tick: reading.tick,
        time_seconds: reading.time_seconds,
        world_revision: reading.world_revision,
        snapshot_sequence: reading.snapshot_sequence,
        distance: reading.distance,
    }
}

pub fn capture_mass_aggregate_history(history: &MassAggregateHistory) -> MassAggregateHistoryState {
    MassAggregateHistoryState {
        series: history
            .entries()
            .map(|(probe, readings)| MassAggregateSeriesRecord {
                probe,
                readings: readings.iter().map(mass_aggregate_reading_record).collect(),
            })
            .collect(),
    }
}

/// See [`capture_probe_series`].
pub fn capture_mass_aggregate_series(
    history: &MassAggregateHistory,
    probe: MassAggregateProbeId,
) -> Option<MassAggregateSeriesRecord> {
    let readings: Vec<_> = history
        .readings(probe)
        .map(mass_aggregate_reading_record)
        .collect();
    (!readings.is_empty()).then_some(MassAggregateSeriesRecord { probe, readings })
}

fn mass_aggregate_reading_record(
    reading: &fieldcad_simulation::MassAggregateReading,
) -> MassAggregateReadingRecord {
    MassAggregateReadingRecord {
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
    }
}

// --- Restore direction: `*ReadingRecord` (saved, from a scene document) ->
// `fieldcad_simulation::*Reading` (live). Counterpart to the capture
// functions above, for `HeadlessServer::restore_observation_history` — see
// that method for why only the per-reading conversion lives here rather
// than a whole rebuilt `ProbeHistory`/etc: capacity-aware series insertion
// happens in `ObservationRecorder::restore`, which needs raw readings, not
// an already-built (and therefore potentially already-truncated) history.

pub(crate) fn probe_reading_from_record(
    record: ProbeReadingRecord,
) -> fieldcad_simulation::ProbeReading {
    fieldcad_simulation::ProbeReading {
        tick: record.tick,
        time_seconds: record.time_seconds,
        world_revision: record.world_revision,
        snapshot_sequence: record.snapshot_sequence,
        value: record.value,
        validity: record.validity,
    }
}

pub(crate) fn distance_reading_from_record(
    record: DistanceReadingRecord,
) -> fieldcad_simulation::DistanceReading {
    fieldcad_simulation::DistanceReading {
        tick: record.tick,
        time_seconds: record.time_seconds,
        world_revision: record.world_revision,
        snapshot_sequence: record.snapshot_sequence,
        distance: record.distance,
    }
}

pub(crate) fn mass_aggregate_reading_from_record(
    record: MassAggregateReadingRecord,
) -> fieldcad_simulation::MassAggregateReading {
    fieldcad_simulation::MassAggregateReading {
        tick: record.tick,
        time_seconds: record.time_seconds,
        world_revision: record.world_revision,
        snapshot_sequence: record.snapshot_sequence,
        center_of_mass: record.center_of_mass,
        velocity: record.velocity,
        total_momentum: record.total_momentum,
        angular_momentum: record.angular_momentum,
        total_kinetic_energy_j: record.total_kinetic_energy_j,
        total_mass_kg: record.total_mass_kg,
        member_count: record.member_count,
    }
}
