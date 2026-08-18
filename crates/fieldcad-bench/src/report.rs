//! Results, in a form a person can read and a form a tool can diff.
//!
//! The JSON shape is the harness's contract with whatever drives it. Keys are
//! stable; adding a field is fine, renaming one is not.

use serde::{Deserialize, Serialize};

use crate::{
    measure::Timing,
    scaling::{Complexity, ScalingFit, ScalingVerdict},
};

/// One scene size within one benchmark.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PointReport {
    pub scene: String,
    /// What the scene is, so a number is never orphaned from its physics.
    pub scene_summary: String,
    pub scene_size: String,
    pub cells: u64,
    pub charges: usize,
    pub samples_per_channel: u64,
    /// Value of the swept parameter at this point.
    pub n: f64,
    pub timing: Timing,
    /// Cost divided by the swept parameter, the figure a budget is set in.
    pub ns_per_unit: f64,
}

/// One benchmark: its sweep, and what that sweep says about complexity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub id: String,
    pub group: String,
    pub what: String,
    pub why: String,
    /// The parameter the exponent is expressed in.
    pub parameter: String,
    pub declared_complexity: Complexity,
    pub declared_label: String,
    pub points: Vec<PointReport>,
    /// Absent when the sweep could not support a power-law fit.
    pub scaling: Option<ScalingFit>,
    pub verdict: Option<ScalingVerdict>,
}

impl BenchmarkReport {
    /// Whether this benchmark grew faster than it claims to.
    pub fn needs_attention(&self) -> bool {
        self.verdict.is_some_and(ScalingVerdict::needs_attention)
    }
}

/// Everything one invocation produced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    /// Bumped when the JSON shape changes incompatibly.
    pub schema_version: u32,
    /// What was measured and what was deliberately not.
    pub scope: String,
    pub profile: String,
    pub benchmarks: Vec<BenchmarkReport>,
}

pub const SCHEMA_VERSION: u32 = 1;

pub const SCOPE: &str = "Headless CPU compute: equation-system solvers and the simulation runtime. \
Visualization and GPU backends are not measured; see the crate README.";

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report is serializable")
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text)
            .map_err(|error| format!("baseline is not a valid report: {error}"))
    }

    pub fn benchmark(&self, id: &str) -> Option<&BenchmarkReport> {
        self.benchmarks.iter().find(|bench| bench.id == id)
    }

    /// Benchmarks whose measured growth outpaced their declared complexity.
    pub fn attention(&self) -> Vec<&BenchmarkReport> {
        self.benchmarks
            .iter()
            .filter(|bench| bench.needs_attention())
            .collect()
    }
}

/// Regression threshold for a baseline comparison.
///
/// Wall-clock benchmarking on a developer machine drifts by several percent
/// between runs. A gate below that is noise; this is set where a real change
/// should be visible above it.
pub const REGRESSION_THRESHOLD: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Faster,
    Slower,
    Unchanged,
    /// Present now, absent from the baseline.
    New,
}

impl Change {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Faster => "faster",
            Self::Slower => "SLOWER",
            Self::Unchanged => "~",
            Self::New => "new",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Comparison {
    pub id: String,
    pub scene: String,
    pub baseline_ns: f64,
    pub current_ns: f64,
    /// Positive means the current run is slower.
    pub delta: f64,
    pub change: Change,
}

/// Compare a run against a saved baseline, matching on benchmark ID and scene.
pub fn compare(baseline: &Report, current: &Report) -> Vec<Comparison> {
    let mut comparisons = Vec::new();
    for bench in &current.benchmarks {
        let previous = baseline.benchmark(&bench.id);
        for point in &bench.points {
            let baseline_point = previous.and_then(|previous| {
                previous
                    .points
                    .iter()
                    .find(|candidate| candidate.scene == point.scene)
            });
            let (baseline_ns, delta, change) = match baseline_point {
                Some(baseline_point) if baseline_point.timing.median_ns > 0.0 => {
                    let baseline_ns = baseline_point.timing.median_ns;
                    let delta = (point.timing.median_ns - baseline_ns) / baseline_ns;
                    let change = if delta > REGRESSION_THRESHOLD {
                        Change::Slower
                    } else if delta < -REGRESSION_THRESHOLD {
                        Change::Faster
                    } else {
                        Change::Unchanged
                    };
                    (baseline_ns, delta, change)
                }
                _ => (0.0, 0.0, Change::New),
            };
            comparisons.push(Comparison {
                id: bench.id.clone(),
                scene: point.scene.clone(),
                baseline_ns,
                current_ns: point.timing.median_ns,
                delta,
                change,
            });
        }
    }
    comparisons
}

/// Human-readable duration at a sensible SI scale.
pub fn format_ns(nanoseconds: f64) -> String {
    let (scale, suffix) = if nanoseconds >= 1.0e9 {
        (1.0e9, "s")
    } else if nanoseconds >= 1.0e6 {
        (1.0e6, "ms")
    } else if nanoseconds >= 1.0e3 {
        (1.0e3, "µs")
    } else {
        (1.0, "ns")
    };
    format!("{:.3} {suffix}", nanoseconds / scale)
}

pub fn format_count(value: f64) -> String {
    if value >= 1.0e6 {
        format!("{:.2}M", value / 1.0e6)
    } else if value >= 1.0e3 {
        format!("{:.1}k", value / 1.0e3)
    } else {
        format!("{value:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(scene: &str, median_ns: f64) -> PointReport {
        PointReport {
            scene: scene.to_owned(),
            scene_summary: "test".to_owned(),
            scene_size: "test".to_owned(),
            cells: 1,
            charges: 1,
            samples_per_channel: 1,
            n: 1.0,
            timing: Timing {
                median_ns,
                min_ns: median_ns,
                mean_ns: median_ns,
                p95_ns: median_ns,
                max_ns: median_ns,
                iterations: 1,
                reps: 1,
            },
            ns_per_unit: median_ns,
        }
    }

    fn report(median_ns: f64) -> Report {
        Report {
            schema_version: SCHEMA_VERSION,
            scope: SCOPE.to_owned(),
            profile: "test".to_owned(),
            benchmarks: vec![BenchmarkReport {
                id: "maxwell/step".to_owned(),
                group: "maxwell".to_owned(),
                what: "step".to_owned(),
                why: "hot".to_owned(),
                parameter: "cells".to_owned(),
                declared_complexity: Complexity::Linear,
                declared_label: "O(N)".to_owned(),
                points: vec![point("a", median_ns)],
                scaling: None,
                verdict: None,
            }],
        }
    }

    #[test]
    fn a_report_survives_a_json_round_trip() {
        let original = report(1234.5);

        let restored = Report::from_json(&original.to_json()).unwrap();

        assert_eq!(restored.schema_version, SCHEMA_VERSION);
        assert_eq!(restored.benchmarks[0].id, "maxwell/step");
        assert_eq!(restored.benchmarks[0].points[0].timing.median_ns, 1234.5);
    }

    #[test]
    fn baseline_comparison_flags_only_moves_beyond_the_noise_threshold() {
        let baseline = report(1000.0);

        let within = compare(&baseline, &report(1050.0));
        let slower = compare(&baseline, &report(1500.0));
        let faster = compare(&baseline, &report(500.0));

        assert_eq!(within[0].change, Change::Unchanged);
        assert_eq!(slower[0].change, Change::Slower);
        assert!((slower[0].delta - 0.5).abs() < 1.0e-9);
        assert_eq!(faster[0].change, Change::Faster);
    }

    #[test]
    fn a_benchmark_missing_from_the_baseline_is_reported_as_new() {
        let mut baseline = report(1000.0);
        baseline.benchmarks[0].id = "maxwell/other".to_owned();

        let comparisons = compare(&baseline, &report(1000.0));

        assert_eq!(comparisons[0].change, Change::New);
    }

    #[test]
    fn durations_are_formatted_at_a_readable_scale() {
        assert_eq!(format_ns(250.0), "250.000 ns");
        assert_eq!(format_ns(2_500.0), "2.500 µs");
        assert_eq!(format_ns(2_500_000.0), "2.500 ms");
        assert_eq!(format_ns(2.5e9), "2.500 s");
    }
}
