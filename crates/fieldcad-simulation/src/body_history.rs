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
///
/// Bounded per-object, not just overall: most bodies in a scene never have a
/// trajectory display asking for their history at all, so they stay at
/// `default_capacity` — cheap. A body a trajectory display *is* watching can
/// have its own capacity raised (see [`Self::set_capacity`]) to cover
/// however long a trail was actually asked for, without paying that same
/// memory cost on every other body in the scene.
#[derive(Clone, Debug)]
pub struct BodyHistory {
    default_capacity: usize,
    /// Explicit per-object overrides. Absent means `default_capacity`.
    capacities: BTreeMap<ObjectId, usize>,
    series: BTreeMap<ObjectId, VecDeque<BodySample>>,
}

impl BodyHistory {
    pub fn new(default_capacity: usize) -> Self {
        Self {
            default_capacity: default_capacity.max(1),
            capacities: BTreeMap::new(),
            series: BTreeMap::new(),
        }
    }

    pub const fn default_capacity(&self) -> usize {
        self.default_capacity
    }

    /// The capacity `object` currently records against — its own override if
    /// one was ever set via [`Self::set_capacity`], otherwise
    /// [`Self::default_capacity`].
    pub fn capacity(&self, object: ObjectId) -> usize {
        self.capacities
            .get(&object)
            .copied()
            .unwrap_or(self.default_capacity)
    }

    /// Override how many samples `object` keeps, independent of every other
    /// body's. Takes effect immediately: samples already past the new
    /// capacity are dropped right away rather than left to age out on the
    /// next `record`, so lowering a capacity frees the memory promptly.
    pub fn set_capacity(&mut self, object: ObjectId, capacity: usize) {
        let capacity = capacity.max(1);
        self.capacities.insert(object, capacity);
        if let Some(series) = self.series.get_mut(&object) {
            while series.len() > capacity {
                series.pop_front();
            }
        }
    }

    /// Record one sample for `object`, dropping the oldest sample first if the
    /// series is already at its (possibly overridden) capacity.
    pub fn record(&mut self, object: ObjectId, sample: BodySample) {
        let capacity = self.capacity(object);
        let series = self.series.entry(object).or_default();
        while series.len() >= capacity {
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

    /// Drop the series (and any capacity override) of objects that no longer
    /// exist.
    ///
    /// Each series is bounded, but the set of series is not: object
    /// identifiers are minted monotonically and never reused, so a session
    /// that repeatedly creates and removes objects would otherwise retain
    /// every one of them.
    pub fn retain_objects(&mut self, live: impl Fn(ObjectId) -> bool) {
        self.series.retain(|object, _| live(*object));
        self.capacities.retain(|object, _| live(*object));
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

    /// A raised per-object capacity lets that one body keep deeper history —
    /// e.g. to cover a trajectory display's requested trail length — without
    /// changing what any other body records against.
    #[test]
    fn a_raised_capacity_applies_to_only_the_one_object_it_was_set_for() {
        let watched = ObjectId::new(0);
        let ordinary = ObjectId::new(1);
        let mut history = BodyHistory::new(2);
        history.set_capacity(watched, 5);

        for tick in 0..5 {
            history.record(watched, sample(tick));
            history.record(ordinary, sample(tick));
        }

        assert_eq!(
            history.len(watched),
            5,
            "the raised object keeps every sample"
        );
        assert_eq!(
            history.len(ordinary),
            2,
            "an object with no override still uses the default capacity"
        );
    }

    /// Lowering a capacity below what's already recorded frees the excess
    /// immediately, rather than waiting for enough future `record` calls to
    /// age it out — the memory a raised-then-lowered capacity used should be
    /// given back promptly, not linger.
    #[test]
    fn lowering_a_capacity_trims_existing_samples_immediately() {
        let object = ObjectId::new(0);
        let mut history = BodyHistory::new(10);
        for tick in 0..10 {
            history.record(object, sample(tick));
        }
        assert_eq!(history.len(object), 10);

        history.set_capacity(object, 3);

        let ticks: Vec<u64> = history.readings(object).map(|s| s.tick).collect();
        assert_eq!(
            ticks,
            vec![7, 8, 9],
            "trimming down keeps the newest samples, oldest first"
        );
    }

    /// `retain_objects` must forget a deleted object's capacity override too
    /// — otherwise a reused-looking (but never actually reused, since
    /// identifiers are monotonic) map entry would just accumulate forever
    /// alongside the series it used to size.
    #[test]
    fn deleted_objects_do_not_retain_their_capacity_override_forever() {
        let removed = ObjectId::new(0);
        let mut history = BodyHistory::new(2);
        history.set_capacity(removed, 100);
        history.record(removed, sample(0));

        history.retain_objects(|_| false);

        // Re-recording after the override was dropped must fall back to the
        // default capacity, not silently keep honouring the old override.
        for tick in 0..5 {
            history.record(removed, sample(tick));
        }
        assert_eq!(history.len(removed), 2);
    }
}
