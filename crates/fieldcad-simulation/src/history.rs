//! Bounded probe time series assembled from published snapshots.
//!
//! This reads only what a snapshot carries, so it works identically behind a
//! local runtime and a remote session. It deliberately does not consult the
//! world: a remote client does not own one.

use std::collections::{BTreeMap, VecDeque};

use fieldcad_core::{ChannelId, FieldSnapshot, FieldValue, ProbeId, SampleValidity, WorldRevision};

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

    /// Probe/channel pairs that have at least one reading.
    pub fn tracked(&self) -> impl Iterator<Item = (ProbeId, &ChannelId)> {
        self.series
            .iter()
            .filter(|(_, series)| !series.is_empty())
            .map(|((probe, channel), _)| (*probe, channel))
    }
}

impl Default for ProbeHistory {
    fn default() -> Self {
        Self::new(fieldcad_core::DEFAULT_PROBE_HISTORY)
    }
}
