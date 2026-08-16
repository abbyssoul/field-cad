//! Transport-neutral probe/distance/mass-aggregate observation recording.
//!
//! Extracted from what used to be `fieldcad_server::HeadlessServer`'s own
//! `record_observations` — moved down to this crate so it works identically
//! for every [`crate::FieldDataSource`] implementor that owns one, not just
//! sessions wrapped in `HeadlessServer`. [`crate::AsyncLocalDataSource`] owns
//! one, updated at its own snapshot-adoption point (`adopt`) — see
//! `docs/tasks/authoritative-observation-history.md` for why this crate
//! (rather than `fieldcad-server`) is the right home: `ProbeHistory` and
//! siblings were always designed to be assembled from nothing but a
//! published snapshot and a `WorldSnapshot`, which is exactly what any
//! `FieldDataSource` implementor already has on hand.
//!
//! `LocalDataSource`/`LoopbackDataSource` do not currently own one of these:
//! no production code constructs either directly (every real path wraps one
//! in `AsyncLocalDataSource`, which does own a recorder), so adding it there
//! is left until a concrete need appears — the snapshot-adoption points for
//! both are already known (`LocalDataSource::publish`,
//! `LoopbackDataSource::poll`'s link-dequeue block, both in `source.rs`),
//! and `observe`/`restore` below are reusable as-is.

use std::collections::VecDeque;

use fieldcad_core::{
    ChannelId, DistanceProbeId, FieldSnapshot, MassAggregateProbeId, ProbeId, WorldRevision,
    WorldSnapshot,
};

use crate::history::{
    DistanceHistory, DistanceReading, MassAggregateHistory, MassAggregateReading, ProbeHistory,
    ProbeReading,
};

/// Owns bounded, authoritative probe/distance/mass-aggregate observation
/// history for one session, fed by every complete published snapshot.
#[derive(Clone, Debug, Default)]
pub struct ObservationRecorder {
    probe_history: ProbeHistory,
    distance_history: DistanceHistory,
    mass_aggregate_history: MassAggregateHistory,
    /// The `run_generation` these histories were last recorded against —
    /// see [`Self::observe`]. A mismatch against the session's current
    /// `run_generation` means a domain/field-system reconfiguration
    /// happened since the last observation, and every reading held so far
    /// belongs to a numerical run that no longer exists.
    observed_run_generation: u64,
    /// The world revision `probe_history`'s per-probe capacity overrides
    /// were last synced against. `None` means never synced (forces the
    /// first `observe`/`restore` to always sync).
    probes_synced_at: Option<WorldRevision>,
}

impl ObservationRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn probe_history(&self) -> &ProbeHistory {
        &self.probe_history
    }

    pub fn distance_history(&self) -> &DistanceHistory {
        &self.distance_history
    }

    pub fn mass_aggregate_history(&self) -> &MassAggregateHistory {
        &self.mass_aggregate_history
    }

    /// Fold one complete published snapshot into these histories, and drop
    /// the series of any probe deleted since the last observation.
    ///
    /// A domain/field-system reconfiguration bumps `run_generation` and
    /// restarts the numerical run from `t = 0` — every reading recorded so
    /// far then belongs to a run that no longer exists, so a jump in
    /// `run_generation` since the last observation discards everything
    /// held before recording this snapshot; otherwise a run reset
    /// mid-session would silently mix two runs' readings into one series.
    pub fn observe(
        &mut self,
        snapshot: &FieldSnapshot,
        world: &WorldSnapshot,
        run_generation: u64,
    ) {
        if run_generation != self.observed_run_generation {
            self.probe_history.clear();
            self.distance_history.clear();
            self.mass_aggregate_history.clear();
            self.observed_run_generation = run_generation;
        }
        self.sync_capacities(world);
        self.probe_history.record(snapshot);
        self.distance_history.record(snapshot);
        self.mass_aggregate_history.record(snapshot);
        self.retain_live(world);
    }

    /// Rebuild these histories from a scene document's saved observation
    /// history — see `fieldcad_server::HeadlessServer::restore_observation_history`.
    /// Takes raw per-series data (the same shape [`ProbeHistory::entries`]
    /// yields), not a pre-built `ProbeHistory`, so capacities can be synced
    /// from `world` *before* each series is inserted: `world` is the
    /// already-loaded document's world (probes and their declared
    /// `history_capacity` exist by the time this runs), and inserting into
    /// a `ProbeHistory` still on the flat default capacity would truncate a
    /// probe's large declared series before its override is even known
    /// (see `ProbeHistory::insert_series`) — building the caller's own
    /// `ProbeHistory` first and handing it over here would already be too
    /// late.
    pub fn restore(
        &mut self,
        world: &WorldSnapshot,
        probe_series: Vec<(ProbeId, ChannelId, VecDeque<ProbeReading>)>,
        distance_series: Vec<(DistanceProbeId, VecDeque<DistanceReading>)>,
        mass_aggregate_series: Vec<(MassAggregateProbeId, VecDeque<MassAggregateReading>)>,
    ) {
        self.probe_history = ProbeHistory::default();
        self.distance_history = DistanceHistory::default();
        self.mass_aggregate_history = MassAggregateHistory::default();
        self.probes_synced_at = None;
        self.sync_capacities(world);
        for (probe, channel, readings) in probe_series {
            self.probe_history.insert_series(probe, channel, readings);
        }
        for (probe, readings) in distance_series {
            self.distance_history.insert_series(probe, readings);
        }
        for (probe, readings) in mass_aggregate_series {
            self.mass_aggregate_history.insert_series(probe, readings);
        }
        self.retain_live(world);
    }

    /// A probe's `history_capacity` is fixed at creation (no command
    /// changes it afterward — see `fieldcad_core::ProbeSpec`), so this only
    /// needs to run when the set of probes could have changed, not on
    /// every observation.
    fn sync_capacities(&mut self, world: &WorldSnapshot) {
        if self.probes_synced_at == Some(world.revision()) {
            return;
        }
        for probe in world.probes().values() {
            for channel in &probe.channels {
                self.probe_history
                    .set_capacity(probe.id, channel, probe.history_capacity);
            }
        }
        self.probes_synced_at = Some(world.revision());
    }

    fn retain_live(&mut self, world: &WorldSnapshot) {
        self.probe_history
            .retain_probes(|probe| world.probe(probe).is_some());
        self.distance_history
            .retain_probes(|probe| world.distance_probe(probe).is_some());
        self.mass_aggregate_history
            .retain_probes(|probe| world.mass_aggregate_probe(probe).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{
        ChannelId, ChannelSchema, ChannelSnapshot, Dimension, Domain, FieldBatch, FieldColumn,
        FieldValueKind, PluginId, ProbeId, ProbeSpec, SampleGeometry, SampleValidity, SessionId,
        SnapshotCompleteness, SnapshotIdentity, World, WorldCommand,
    };
    use glam::DVec3;
    use std::{collections::BTreeMap, sync::Arc};

    fn channel_id() -> ChannelId {
        ChannelId::new(PluginId::new("test").unwrap(), "scalar").unwrap()
    }

    fn world_with_one_probe(history_capacity: usize) -> (WorldSnapshot, ProbeId) {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(
                ProbeSpec::at("probe", DVec3::X, vec![channel_id()])
                    .with_history_capacity(history_capacity),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let probe = *snapshot.probes().keys().next().unwrap();
        (snapshot, probe)
    }

    fn snapshot_with(probe: ProbeId, sequence: u64, run_generation: u64) -> FieldSnapshot {
        let schema = Arc::new(ChannelSchema {
            id: channel_id(),
            display_name: "Scalar".to_owned(),
            value_kind: FieldValueKind::Scalar(Dimension::LENGTH),
        });
        let geometry = SampleGeometry::probes(vec![probe], vec![DVec3::X]).unwrap();
        let batch = FieldBatch::new(
            geometry,
            FieldColumn::scalars(vec![1.0]),
            vec![SampleValidity::Exact],
        )
        .unwrap();
        FieldSnapshot {
            identity: SnapshotIdentity {
                session: SessionId::from_u128(1),
                sequence,
                run_generation,
                world_revision: fieldcad_core::WorldRevision::INITIAL,
                tick: sequence,
                time_seconds: sequence as f64,
            },
            completeness: SnapshotCompleteness::Complete,
            domain: Domain::centred_cube(1.0, 2).unwrap(),
            plugins: Arc::from([]),
            channels: BTreeMap::from([(
                channel_id(),
                ChannelSnapshot {
                    schema,
                    provider: PluginId::new("test").unwrap(),
                    batches: Arc::from([batch]),
                },
            )]),
            diagnostics: Arc::from([]),
            distances: Arc::from([]),
            mass_aggregates: Arc::from([]),
        }
    }

    #[test]
    fn observe_records_a_reading_and_honors_the_probes_declared_capacity() {
        let (world, probe) = world_with_one_probe(2);
        let mut recorder = ObservationRecorder::new();

        for sequence in 0..5 {
            recorder.observe(&snapshot_with(probe, sequence, 0), &world, 0);
        }

        assert_eq!(
            recorder.probe_history().len(probe, &channel_id()),
            2,
            "the probe's own declared history_capacity=2 must bound its series"
        );
    }

    #[test]
    fn observe_clears_every_history_when_run_generation_changes() {
        let (world, probe) = world_with_one_probe(8);
        let mut recorder = ObservationRecorder::new();
        recorder.observe(&snapshot_with(probe, 0, 0), &world, 0);
        assert_eq!(recorder.probe_history().len(probe, &channel_id()), 1);

        // A fresh run republishes at tick/sequence 0 too — the reset must
        // still discard the prior run's reading before recording this one.
        recorder.observe(&snapshot_with(probe, 0, 1), &world, 1);

        assert_eq!(
            recorder.probe_history().len(probe, &channel_id()),
            1,
            "the prior run's reading must not survive a run-generation reset"
        );
    }

    #[test]
    fn observe_prunes_deleted_probes() {
        let (mut world_state, probe) = {
            let mut world = World::new();
            world
                .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                    "probe",
                    DVec3::X,
                    vec![channel_id()],
                ))])
                .unwrap();
            let snapshot = world.snapshot();
            let probe = *snapshot.probes().keys().next().unwrap();
            (world, probe)
        };
        let mut recorder = ObservationRecorder::new();
        recorder.observe(&snapshot_with(probe, 0, 0), &world_state.snapshot(), 0);
        assert_eq!(recorder.probe_history().len(probe, &channel_id()), 1);

        world_state
            .commit([WorldCommand::RemoveProbe(probe)])
            .unwrap();
        recorder.observe(&snapshot_with(probe, 1, 0), &world_state.snapshot(), 0);

        assert_eq!(recorder.probe_history().len(probe, &channel_id()), 0);
    }

    #[test]
    fn restore_syncs_capacity_before_inserting_so_a_large_series_is_not_truncated() {
        let (world, probe) = world_with_one_probe(10);
        let mut readings = VecDeque::new();
        for sequence in 0..10 {
            readings.push_back(ProbeReading {
                tick: sequence,
                time_seconds: sequence as f64,
                world_revision: fieldcad_core::WorldRevision::INITIAL,
                snapshot_sequence: sequence,
                value: fieldcad_core::FieldValue::Scalar(
                    fieldcad_core::Quantity::new(1.0, Dimension::LENGTH).unwrap(),
                ),
                validity: SampleValidity::Exact,
            });
        }

        let mut recorder = ObservationRecorder::new();
        recorder.restore(
            &world,
            vec![(probe, channel_id(), readings)],
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            recorder.probe_history().len(probe, &channel_id()),
            10,
            "the probe's declared capacity (10), synced before insertion, must be honored"
        );
    }
}
