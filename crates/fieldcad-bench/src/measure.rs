//! Wall-clock measurement of one compute operation.
//!
//! This is deliberately small. The harness exists to find hot paths and to
//! state how a cost scales with scene size, not to certify nanosecond-level
//! differences: a claim that fine needs a quiet machine and a statistical model
//! this file does not pretend to have.
//!
//! Two habits keep the numbers honest anyway. Setup is excluded from the timed
//! region, so building a 128³ lattice is never counted as the cost of stepping
//! it. And a cheap operation is repeated inside one timed region until that
//! region is long enough to dwarf clock resolution, because timing a 200 ns call
//! against a ~20 ns clock otherwise measures the clock.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

/// How much work to do before believing a number.
#[derive(Clone, Copy, Debug)]
pub struct MeasureConfig {
    /// Timed regions to keep.
    pub reps: usize,
    /// Timed regions to discard first, so caches and branch predictors are warm.
    pub warmup_reps: usize,
    /// Grow the iteration count until one timed region lasts at least this long.
    pub min_rep_time: Duration,
    /// Never batch more than this many iterations into one region, so a slow
    /// operation still produces several independent samples.
    pub max_iterations: usize,
}

impl Default for MeasureConfig {
    fn default() -> Self {
        Self {
            reps: 12,
            warmup_reps: 3,
            min_rep_time: Duration::from_millis(20),
            max_iterations: 1 << 20,
        }
    }
}

impl MeasureConfig {
    /// Fewer, shorter samples. For iterating on a change, not for recording a
    /// baseline.
    pub fn quick() -> Self {
        Self {
            reps: 4,
            warmup_reps: 1,
            min_rep_time: Duration::from_millis(5),
            ..Self::default()
        }
    }
}

/// The timing of one operation at one scene size.
///
/// Carries the full min/mean/median/max distribution, not just a headline
/// figure, so a conclusion ("this got faster") can be checked against the
/// spread rather than trusted on one noisy run.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Timing {
    /// The headline figure used for regression comparisons and scaling fits.
    /// Median rather than mean: one descheduled rep should not move it.
    pub median_ns: f64,
    /// The least-contended observation. Closest to the cost with no interference.
    pub min_ns: f64,
    /// Arithmetic mean of the kept reps. Unlike the median, this is pulled by
    /// outliers — comparing it against `min_ns`/`max_ns` is what shows
    /// whether a run was noisy, rather than asserting it as a single ratio.
    pub mean_ns: f64,
    /// Tail cost. A median far below this means the measurement was noisy.
    pub p95_ns: f64,
    /// The most-contended observation kept.
    pub max_ns: f64,
    /// Iterations batched into each timed region.
    pub iterations: usize,
    /// Timed regions kept (excludes `warmup_reps`).
    pub reps: usize,
}

impl Timing {
    /// Spread of the kept samples, as a fraction of the median. Large values
    /// mean the machine was busy and the number should not be trusted for a
    /// small regression claim.
    pub fn noise(&self) -> f64 {
        if self.median_ns <= 0.0 {
            return 0.0;
        }
        (self.p95_ns - self.min_ns) / self.median_ns
    }

    /// Total operations actually timed: every kept rep's batch, summed. The
    /// number a noisy-run-independent conclusion should be weighed against —
    /// a "flat" result over 12 iterations means less than the same result
    /// over 12 million.
    pub fn total_iterations(&self) -> u64 {
        self.iterations as u64 * self.reps as u64
    }
}

/// Time `run`, rebuilding state with `setup` before each timed region.
///
/// `setup` is what makes a mutating operation measurable: a Yee `step` must
/// start from a fresh solver at a known tick, and rebuilding that solver is not
/// part of the cost being reported.
pub fn measure<S, R>(
    config: &MeasureConfig,
    mut setup: impl FnMut() -> S,
    mut run: impl FnMut(&mut S, u64) -> R,
) -> Timing {
    let iterations = calibrate(config, &mut setup, &mut run);

    let mut samples = Vec::with_capacity(config.reps);
    for rep in 0..(config.warmup_reps + config.reps) {
        let mut state = setup();
        let start = Instant::now();
        for iteration in 0..iterations {
            black_box(run(&mut state, iteration as u64));
        }
        let elapsed = start.elapsed();
        // Keep `state` alive across the timed region so a solver's storage
        // cannot be dropped, and therefore freed, while the clock is running.
        drop(black_box(state));
        if rep >= config.warmup_reps {
            samples.push(elapsed.as_secs_f64() * 1.0e9 / iterations as f64);
        }
    }

    summarize(samples, iterations)
}

/// Turn a set of nanosecond-per-iteration samples into the same
/// min/mean/median/max distribution [`measure`] reports.
///
/// Public so a hand-rolled timing loop that cannot fit `measure`'s
/// setup/run shape — `profile_scene`'s per-tick durations under a
/// profiler, where rebuilding the runtime between reps would defeat the
/// point — can still report on the same footing as every benchmark in
/// this crate, instead of inventing its own single-number average.
/// `iterations` is the batch size each sample already represents (1 for a
/// loop that timed one operation at a time, as `profile_scene` does).
pub fn summarize(mut samples: Vec<f64>, iterations: usize) -> Timing {
    samples.sort_by(f64::total_cmp);
    let mean_ns = if samples.is_empty() {
        0.0
    } else {
        samples.iter().sum::<f64>() / samples.len() as f64
    };
    Timing {
        median_ns: percentile(&samples, 0.5),
        min_ns: samples.first().copied().unwrap_or(0.0),
        mean_ns,
        p95_ns: percentile(&samples, 0.95),
        max_ns: samples.last().copied().unwrap_or(0.0),
        iterations,
        reps: samples.len(),
    }
}

/// How many iterations make one timed region long enough to be worth reading.
fn calibrate<S, R>(
    config: &MeasureConfig,
    setup: &mut impl FnMut() -> S,
    run: &mut impl FnMut(&mut S, u64) -> R,
) -> usize {
    let mut iterations = 1;
    loop {
        let mut state = setup();
        let start = Instant::now();
        for iteration in 0..iterations {
            black_box(run(&mut state, iteration as u64));
        }
        let elapsed = start.elapsed();
        drop(black_box(state));

        if elapsed >= config.min_rep_time || iterations >= config.max_iterations {
            return iterations;
        }
        // Estimate the count that reaches the target, but never trust one
        // sample enough to jump more than 100x at a time.
        let growth = if elapsed.is_zero() {
            100.0
        } else {
            (config.min_rep_time.as_secs_f64() / elapsed.as_secs_f64()).clamp(2.0, 100.0)
        };
        iterations = ((iterations as f64 * growth).ceil() as usize)
            .max(iterations + 1)
            .min(config.max_iterations);
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = quantile * (sorted.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = rank.ceil() as usize;
    if low == high {
        sorted[low]
    } else {
        let weight = rank - low as f64;
        sorted[low] * (1.0 - weight) + sorted[high] * weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_interpolate_between_neighbouring_samples() {
        let sorted = [10.0, 20.0, 30.0, 40.0];

        assert_eq!(percentile(&sorted, 0.0), 10.0);
        assert_eq!(percentile(&sorted, 1.0), 40.0);
        assert_eq!(percentile(&sorted, 0.5), 25.0);
    }

    #[test]
    fn a_cheap_operation_is_batched_until_the_clock_can_resolve_it() {
        let config = MeasureConfig::quick();
        let timing = measure(
            &config,
            || 0u64,
            |state, _| {
                *state = state.wrapping_add(1);
                *state
            },
        );

        // The point of calibration: a single wrapping add must not be timed one
        // call at a time against a coarse clock.
        assert!(timing.iterations > 1, "cheap work was not batched");
        assert!(timing.median_ns > 0.0);
        assert_eq!(timing.reps, config.reps);
    }

    /// The distribution reported must actually bound itself: `min <= mean <=
    /// max` and `min <= median <= max`, and the reported iteration total must
    /// be the batch size actually run, not just the configured rep count —
    /// exactly what a noisy-run-independent conclusion needs to check.
    #[test]
    fn the_reported_distribution_is_internally_consistent() {
        let config = MeasureConfig::quick();
        let timing = measure(&config, || (), |(), _| black_box(1u64 + 1));

        assert!(timing.min_ns <= timing.mean_ns);
        assert!(timing.mean_ns <= timing.max_ns);
        assert!(timing.min_ns <= timing.median_ns);
        assert!(timing.median_ns <= timing.max_ns);
        assert_eq!(
            timing.total_iterations(),
            timing.iterations as u64 * config.reps as u64
        );
    }

    #[test]
    fn setup_runs_outside_the_timed_region() {
        let config = MeasureConfig {
            reps: 2,
            warmup_reps: 0,
            min_rep_time: Duration::from_millis(1),
            max_iterations: 16,
        };

        // Setup sleeps far longer than the measured work. If it were counted,
        // the reported per-iteration cost would be milliseconds.
        let timing = measure(
            &config,
            || {
                std::thread::sleep(Duration::from_millis(5));
                1u64
            },
            |state, _| *state + 1,
        );

        assert!(
            timing.median_ns < 1.0e6,
            "setup leaked into the timed region: {} ns",
            timing.median_ns
        );
    }
}
