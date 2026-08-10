//! Bounded probe time series assembled from published snapshots.
//!
//! This reads only what a snapshot carries, so it works identically behind a
//! local runtime and a remote session. It deliberately does not consult the
//! world: a remote client does not own one.

use std::collections::{BTreeMap, VecDeque};

use fieldcad_core::{
    ChannelId, DistanceProbeId, FieldSnapshot, FieldValue, ProbeId, SampleValidity, WorldRevision,
};

/// One recorded probe sample, with everything needed to say where it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeReading {
    pub tick: u64,
    pub time_seconds: f64,
    pub world_revision: WorldRevision,
    pub snapshot_sequence: u64,
    pub value: FieldValue,
    pub validity: SampleValidity,
}

/// Bounded history for every probe/channel pair seen so far.
#[derive(Clone, Debug)]
pub struct ProbeHistory {
    capacity: usize,
    series: BTreeMap<(ProbeId, ChannelId), VecDeque<ProbeReading>>,
}

impl ProbeHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            series: BTreeMap::new(),
        }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record every probe sample in a snapshot.
    ///
    /// Recording the same snapshot twice is a no-op, so a caller that polls more
    /// often than the source produces data does not duplicate samples.
    pub fn record(&mut self, snapshot: &FieldSnapshot) -> usize {
        let mut recorded = 0;
        for (channel_id, channel) in &snapshot.channels {
            let dimension = channel.schema.dimension();
            for batch in channel.batches.iter() {
                let fieldcad_core::SampleGeometry::Probes { ids, .. } = batch.geometry() else {
                    continue;
                };
                for (index, probe) in ids.iter().enumerate() {
                    let Some(sample) = batch.sample(index, dimension) else {
                        continue;
                    };
                    let key = (*probe, channel_id.clone());
                    let series = self.series.entry(key).or_default();
                    if series
                        .back()
                        .is_some_and(|last| last.snapshot_sequence >= snapshot.identity.sequence)
                    {
                        continue;
                    }
                    if series.len() == self.capacity {
                        series.pop_front();
                    }
                    series.push_back(ProbeReading {
                        tick: snapshot.identity.tick,
                        time_seconds: snapshot.identity.time_seconds,
                        world_revision: snapshot.identity.world_revision,
                        snapshot_sequence: snapshot.identity.sequence,
                        value: sample.value,
                        validity: sample.validity,
                    });
                    recorded += 1;
                }
            }
        }
        recorded
    }

    pub fn readings(
        &self,
        probe: ProbeId,
        channel: &ChannelId,
    ) -> impl Iterator<Item = &ProbeReading> {
        self.series
            .get(&(probe, channel.clone()))
            .into_iter()
            .flatten()
    }

    pub fn len(&self, probe: ProbeId, channel: &ChannelId) -> usize {
        self.series
            .get(&(probe, channel.clone()))
            .map_or(0, VecDeque::len)
    }

    pub fn is_empty(&self) -> bool {
        self.series.values().all(VecDeque::is_empty)
    }

    pub fn clear(&mut self) {
        self.series.clear();
    }

    /// Drop the series of probes that no longer exist.
    ///
    /// Each series is bounded, but the set of series is not: probe identifiers
    /// are minted monotonically and never reused, so a session that repeatedly
    /// adds and removes probes would otherwise retain every one of them.
    pub fn retain_probes(&mut self, live: impl Fn(ProbeId) -> bool) {
        self.series.retain(|(probe, _), _| live(*probe));
    }

    /// Probe/channel pairs that have at least one reading.
    pub fn tracked(&self) -> impl Iterator<Item = (ProbeId, &ChannelId)> {
        self.series
            .iter()
            .filter(|(_, series)| !series.is_empty())
            .map(|((probe, channel), _)| (*probe, channel))
    }

    /// Every non-empty series, for a caller that persists the whole history
    /// (a scene save) rather than reading one probe/channel at a time.
    pub fn entries(&self) -> impl Iterator<Item = (ProbeId, &ChannelId, &VecDeque<ProbeReading>)> {
        self.series
            .iter()
            .filter(|(_, series)| !series.is_empty())
            .map(|((probe, channel), series)| (*probe, channel, series))
    }

    /// Replace a whole series directly, dropping the oldest readings past
    /// `capacity` — the counterpart to [`Self::entries`] for restoring a
    /// history saved with a possibly different capacity than this one's.
    pub fn insert_series(
        &mut self,
        probe: ProbeId,
        channel: ChannelId,
        mut readings: VecDeque<ProbeReading>,
    ) {
        while readings.len() > self.capacity {
            readings.pop_front();
        }
        self.series.insert((probe, channel), readings);
    }
}

impl Default for ProbeHistory {
    fn default() -> Self {
        Self::new(fieldcad_core::DEFAULT_PROBE_HISTORY)
    }
}

/// One recorded distance-probe reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistanceReading {
    pub tick: u64,
    pub time_seconds: f64,
    pub world_revision: WorldRevision,
    pub snapshot_sequence: u64,
    pub distance: f64,
}

/// Bounded history for every distance probe seen so far.
///
/// A distance has no [`ChannelId`] — it isn't a field sample — so this
/// mirrors [`ProbeHistory`]'s shape but keys directly on [`DistanceProbeId`]
/// and reads `snapshot.distances` instead of `snapshot.channels`.
#[derive(Clone, Debug)]
pub struct DistanceHistory {
    capacity: usize,
    series: BTreeMap<DistanceProbeId, VecDeque<DistanceReading>>,
}

impl DistanceHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            series: BTreeMap::new(),
        }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record every distance reading in a snapshot.
    ///
    /// Recording the same snapshot twice is a no-op, so a caller that polls more
    /// often than the source produces data does not duplicate samples.
    pub fn record(&mut self, snapshot: &FieldSnapshot) -> usize {
        let mut recorded = 0;
        for &(probe, distance) in snapshot.distances.iter() {
            let series = self.series.entry(probe).or_default();
            if series
                .back()
                .is_some_and(|last| last.snapshot_sequence >= snapshot.identity.sequence)
            {
                continue;
            }
            if series.len() == self.capacity {
                series.pop_front();
            }
            series.push_back(DistanceReading {
                tick: snapshot.identity.tick,
                time_seconds: snapshot.identity.time_seconds,
                world_revision: snapshot.identity.world_revision,
                snapshot_sequence: snapshot.identity.sequence,
                distance,
            });
            recorded += 1;
        }
        recorded
    }

    pub fn readings(&self, probe: DistanceProbeId) -> impl Iterator<Item = &DistanceReading> {
        self.series.get(&probe).into_iter().flatten()
    }

    pub fn len(&self, probe: DistanceProbeId) -> usize {
        self.series.get(&probe).map_or(0, VecDeque::len)
    }

    pub fn is_empty(&self) -> bool {
        self.series.values().all(VecDeque::is_empty)
    }

    pub fn clear(&mut self) {
        self.series.clear();
    }

    /// Drop the series of probes that no longer exist — see
    /// [`ProbeHistory::retain_probes`] for why the set of series is
    /// otherwise unbounded.
    pub fn retain_probes(&mut self, live: impl Fn(DistanceProbeId) -> bool) {
        self.series.retain(|probe, _| live(*probe));
    }

    /// Probes that have at least one reading.
    pub fn tracked(&self) -> impl Iterator<Item = DistanceProbeId> {
        self.series
            .iter()
            .filter(|(_, series)| !series.is_empty())
            .map(|(probe, _)| *probe)
    }

    /// Every non-empty series, for a caller that persists the whole history
    /// (a scene save) rather than reading one probe at a time.
    pub fn entries(&self) -> impl Iterator<Item = (DistanceProbeId, &VecDeque<DistanceReading>)> {
        self.series
            .iter()
            .filter(|(_, series)| !series.is_empty())
            .map(|(probe, series)| (*probe, series))
    }

    /// Replace a whole series directly, dropping the oldest readings past
    /// `capacity` — the counterpart to [`Self::entries`] for restoring a
    /// history saved with a possibly different capacity than this one's.
    pub fn insert_series(
        &mut self,
        probe: DistanceProbeId,
        mut readings: VecDeque<DistanceReading>,
    ) {
        while readings.len() > self.capacity {
            readings.pop_front();
        }
        self.series.insert(probe, readings);
    }
}

impl Default for DistanceHistory {
    fn default() -> Self {
        Self::new(fieldcad_core::DEFAULT_PROBE_HISTORY)
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        ChannelSchema, ChannelSnapshot, Dimension, Domain, FieldBatch, FieldColumn, FieldValueKind,
        PluginId, SampleGeometry, SampleValidity, SessionId, SnapshotCompleteness,
        SnapshotIdentity,
    };
    use glam::DVec3;
    use std::{collections::BTreeMap, sync::Arc};

    use super::*;

    fn channel_id() -> ChannelId {
        ChannelId::new(PluginId::new("test").unwrap(), "scalar").unwrap()
    }

    fn snapshot_with(probes: &[ProbeId]) -> FieldSnapshot {
        let schema = Arc::new(ChannelSchema {
            id: channel_id(),
            display_name: "Scalar".to_owned(),
            value_kind: FieldValueKind::Scalar(Dimension::LENGTH),
        });
        let geometry =
            SampleGeometry::probes(probes.to_vec(), vec![DVec3::X; probes.len()]).unwrap();
        let batch = FieldBatch::new(
            geometry,
            FieldColumn::scalars(vec![1.0; probes.len()]),
            vec![SampleValidity::Exact; probes.len()],
        )
        .unwrap();
        FieldSnapshot {
            identity: SnapshotIdentity {
                session: SessionId::from_u128(1),
                sequence: 0,
                run_generation: 0,
                world_revision: WorldRevision::INITIAL,
                tick: 0,
                time_seconds: 0.0,
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
            universe: None,
        }
    }

    #[test]
    fn deleted_probes_do_not_retain_their_history_forever() {
        let kept = ProbeId::new(0);
        let removed = ProbeId::new(1);
        let mut history = ProbeHistory::new(8);
        history.record(&snapshot_with(&[kept, removed]));
        assert_eq!(history.tracked().count(), 2);

        history.retain_probes(|probe| probe == kept);

        assert_eq!(history.len(kept, &channel_id()), 1);
        assert_eq!(history.len(removed, &channel_id()), 0);
        assert_eq!(history.tracked().count(), 1);
    }

    fn snapshot_with_distances(
        sequence: u64,
        readings: &[(DistanceProbeId, f64)],
    ) -> FieldSnapshot {
        let mut snapshot = snapshot_with(&[]);
        snapshot.identity.sequence = sequence;
        snapshot.distances = Arc::from(readings.to_vec().into_boxed_slice());
        snapshot
    }

    #[test]
    fn distance_history_records_every_probe_in_a_snapshot() {
        let a = DistanceProbeId::new(0);
        let b = DistanceProbeId::new(1);
        let mut history = DistanceHistory::new(8);
        history.record(&snapshot_with_distances(0, &[(a, 1.5), (b, 2.5)]));

        assert_eq!(history.tracked().count(), 2);
        assert_eq!(
            history.readings(a).next().map(|reading| reading.distance),
            Some(1.5)
        );
        assert_eq!(
            history.readings(b).next().map(|reading| reading.distance),
            Some(2.5)
        );
    }

    #[test]
    fn distance_history_does_not_duplicate_the_same_snapshot() {
        let probe = DistanceProbeId::new(0);
        let mut history = DistanceHistory::new(8);
        let snapshot = snapshot_with_distances(0, &[(probe, 1.0)]);
        history.record(&snapshot);
        history.record(&snapshot);

        assert_eq!(history.len(probe), 1);
    }

    #[test]
    fn distance_history_is_bounded_by_capacity() {
        let probe = DistanceProbeId::new(0);
        let mut history = DistanceHistory::new(2);
        for sequence in 0..5 {
            history.record(&snapshot_with_distances(
                sequence,
                &[(probe, sequence as f64)],
            ));
        }

        assert_eq!(history.len(probe), 2);
        let last: Vec<f64> = history
            .readings(probe)
            .map(|reading| reading.distance)
            .collect();
        assert_eq!(last, vec![3.0, 4.0]);
    }

    #[test]
    fn deleted_distance_probes_do_not_retain_their_history_forever() {
        let kept = DistanceProbeId::new(0);
        let removed = DistanceProbeId::new(1);
        let mut history = DistanceHistory::new(8);
        history.record(&snapshot_with_distances(0, &[(kept, 1.0), (removed, 2.0)]));
        assert_eq!(history.tracked().count(), 2);

        history.retain_probes(|probe| probe == kept);

        assert_eq!(history.len(kept), 1);
        assert_eq!(history.len(removed), 0);
        assert_eq!(history.tracked().count(), 1);
    }
}
