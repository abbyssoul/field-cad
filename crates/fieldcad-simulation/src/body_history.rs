//! Bounded per-object kinematics history, recorded straight from the tick loop.
//!
//! This is deliberately not shaped like [`crate::history::ProbeHistory`],
//! which is assembled client-side from published snapshots because probe
//! readings are already published data. A body's force isn't: it never
//! crosses into `World`/`WorldSnapshot` (see `fieldcad_dynamics`'s module
//! docs on why), so a [`BodyHistory`] is [`SimulationRuntime`]-owned state,
//! populated directly inside `apply_tick`, exactly like `last_forces` — of
//! which this is the bounded, multi-sample extension.
//!
//! [`SimulationRuntime`]: crate::SimulationRuntime

use std::collections::{BTreeMap, VecDeque};

use fieldcad_core::{ObjectId, WorldRevision};
use glam::DVec3;

/// One recorded body sample, with everything needed to say when it was taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodySample {
    pub tick: u64,
    pub time_seconds: f64,
    pub world_revision: WorldRevision,
    pub position: DVec3,
    pub velocity: DVec3,
    /// The summed force this body carried away from the tick that produced
    /// this sample — the same value `SimulationRuntime::body_force` reports.
    /// Not acceleration: dividing by (γ·mass) to recover one is left to a
    /// consumer that already has the mass, rather than baking a derived,
    /// scheme-dependent quantity into the stored sample.
    pub force: DVec3,
}

/// Bounded history for every dynamic body the runtime has advanced.
#[derive(Clone, Debug)]
pub struct BodyHistory {
    capacity: usize,
    series: BTreeMap<ObjectId, VecDeque<BodySample>>,
}

impl BodyHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            series: BTreeMap::new(),
        }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record one sample for `object`, dropping the oldest sample first if the
    /// series is already at capacity.
    pub fn record(&mut self, object: ObjectId, sample: BodySample) {
        let series = self.series.entry(object).or_default();
        if series.len() == self.capacity {
            series.pop_front();
        }
        series.push_back(sample);
    }

    pub fn readings(&self, object: ObjectId) -> impl Iterator<Item = &BodySample> {
        self.series.get(&object).into_iter().flatten()
    }

    pub fn len(&self, object: ObjectId) -> usize {
        self.series.get(&object).map_or(0, VecDeque::len)
    }

    pub fn is_empty(&self) -> bool {
        self.series.values().all(VecDeque::is_empty)
    }

    pub fn clear(&mut self) {
        self.series.clear();
    }

    /// Drop the series of objects that no longer exist.
    ///
    /// Each series is bounded, but the set of series is not: object
    /// identifiers are minted monotonically and never reused, so a session
    /// that repeatedly creates and removes objects would otherwise retain
    /// every one of them.
    pub fn retain_objects(&mut self, live: impl Fn(ObjectId) -> bool) {
        self.series.retain(|object, _| live(*object));
    }

    /// Objects that have at least one recorded sample.
    pub fn tracked(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.series
            .iter()
            .filter(|(_, series)| !series.is_empty())
            .map(|(object, _)| *object)
    }
}

impl Default for BodyHistory {
    fn default() -> Self {
        Self::new(fieldcad_core::DEFAULT_BODY_HISTORY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(tick: u64) -> BodySample {
        BodySample {
            tick,
            time_seconds: tick as f64,
            world_revision: WorldRevision::INITIAL,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            force: DVec3::ZERO,
        }
    }

    #[test]
    fn recording_past_capacity_drops_the_oldest_sample() {
        let mut history = BodyHistory::new(2);
        let object = ObjectId::new(0);

        history.record(object, sample(0));
        history.record(object, sample(1));
        history.record(object, sample(2));

        let ticks: Vec<u64> = history.readings(object).map(|s| s.tick).collect();
        assert_eq!(ticks, vec![1, 2]);
    }

    #[test]
    fn deleted_objects_do_not_retain_their_history_forever() {
        let kept = ObjectId::new(0);
        let removed = ObjectId::new(1);
        let mut history = BodyHistory::new(8);
        history.record(kept, sample(0));
        history.record(removed, sample(0));
        assert_eq!(history.tracked().count(), 2);

        history.retain_objects(|object| object == kept);

        assert_eq!(history.len(kept), 1);
        assert_eq!(history.len(removed), 0);
        assert_eq!(history.tracked().count(), 1);
    }
}
