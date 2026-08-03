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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Timing {
    /// The headline figure. Median rather than mean: one descheduled rep should
    /// not move the reported cost.
    pub median_ns: f64,
    /// The least-contended observation. Closest to the cost with no interference.
    pub min_ns: f64,
    /// Tail cost. A median far below this means the measurement was noisy.
    pub p95_ns: f64,
    /// Iterations batched into each timed region.
    pub iterations: usize,
    /// Timed regions kept.
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

    samples.sort_by(f64::total_cmp);
    Timing {
        median_ns: percentile(&samples, 0.5),
        min_ns: samples.first().copied().unwrap_or(0.0),
        p95_ns: percentile(&samples, 0.95),
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
