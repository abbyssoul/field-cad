//! Headless compute performance harness for Field CAD.
//!
//! The project's objective is a high-performance environment, which makes
//! "is this fast enough" a question the repository has to be able to answer
//! mechanically rather than by opinion. This crate exists so that an agent or a
//! developer can ask it: run a named scene, get a cost, and get a statement
//! about how that cost grows with scene size.
//!
//! Three commitments shape it:
//!
//! - **Computation is the target.** Solvers and the runtime are measured;
//!   scene extraction and rendering are not. Visualization performs acceptably
//!   today, and a badly-built renderer benchmark would be worse than none.
//! - **A number without its scene is not a result.** Every measurement carries
//!   the domain, source count, and sample count that produced it.
//! - **Complexity is declared, then checked.** Each benchmark states the
//!   complexity it believes it has; the harness fits the measured sweep and
//!   reports agreement, divergence, or an unusable fit.
//!
//! It does not optimize anything, and it deliberately does not encode
//! performance budgets. Budgets belong to the Milestone 5 review gate, which
//! sets them from measurements on named hardware.

pub mod measure;
pub mod report;
pub mod scaling;
pub mod scene;
pub mod workload;

use measure::MeasureConfig;
use report::{BenchmarkReport, PointReport, Report, SCHEMA_VERSION, SCOPE};
use scaling::{ScalingPoint, fit};
use workload::{Benchmark, benchmarks};

/// How a run is configured.
pub struct RunConfig {
    /// Only run benchmarks whose ID contains this substring.
    pub filter: Option<String>,
    /// Fewer samples and smaller sweeps, for iterating rather than recording.
    pub quick: bool,
}

impl RunConfig {
    pub fn profile(&self) -> String {
        let mode = if self.quick { "quick" } else { "full" };
        match &self.filter {
            Some(filter) => format!("{mode}, filtered by '{filter}'"),
            None => mode.to_owned(),
        }
    }
}

/// The benchmarks a config selects, without running them.
pub fn selected(config: &RunConfig) -> Vec<Benchmark> {
    benchmarks(config.quick)
        .into_iter()
        .filter(|bench| {
            config
                .filter
                .as_ref()
                .is_none_or(|filter| bench.id.contains(filter.as_str()))
        })
        .collect()
}

/// Run the selected benchmarks, reporting progress through `observer`.
///
/// Progress is surfaced rather than printed here because a full sweep takes
/// minutes and silence is indistinguishable from a hang.
pub fn run(config: &RunConfig, mut observer: impl FnMut(&str, &str)) -> Report {
    let measure_config = if config.quick {
        MeasureConfig::quick()
    } else {
        MeasureConfig::default()
    };

    let mut reports = Vec::new();
    for bench in selected(config) {
        let mut points = Vec::new();
        for scene in &bench.scenes {
            observer(bench.id, &scene.name);
            let timing = (bench.runner)(scene, &measure_config);
            let n = bench.parameter.value(scene);
            points.push(PointReport {
                scene: scene.name.clone(),
                scene_summary: scene.summary.clone(),
                scene_size: scene.size_label(),
                cells: scene.cells(),
                charges: scene.charges,
                samples_per_channel: scene.samples_per_channel(),
                n,
                timing,
                ns_per_unit: if n > 0.0 { timing.median_ns / n } else { 0.0 },
            });
        }

        let scaling = fit(&points
            .iter()
            .map(|point| ScalingPoint {
                n: point.n,
                median_ns: point.timing.median_ns,
            })
            .collect::<Vec<_>>());
        reports.push(BenchmarkReport {
            id: bench.id.to_owned(),
            group: bench.group.to_owned(),
            what: bench.what.to_owned(),
            why: bench.why.to_owned(),
            parameter: bench.parameter.label().to_owned(),
            declared_complexity: bench.declared,
            declared_label: bench.declared.label().to_owned(),
            verdict: scaling.map(|scaling| scaling.verdict(bench.declared)),
            scaling,
            points,
        });
    }

    Report {
        schema_version: SCHEMA_VERSION,
        scope: SCOPE.to_owned(),
        profile: config.profile(),
        benchmarks: reports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_selects_by_benchmark_id() {
        let all = selected(&RunConfig {
            filter: None,
            quick: true,
        });
        let maxwell = selected(&RunConfig {
            filter: Some("maxwell/".to_owned()),
            quick: true,
        });

        assert!(maxwell.len() < all.len());
        assert!(maxwell.iter().all(|bench| bench.group == "maxwell"));
        assert!(!maxwell.is_empty());
    }

    #[test]
    fn quick_mode_sweeps_fewer_sizes_than_a_full_run() {
        let quick = selected(&RunConfig {
            filter: Some("maxwell/step".to_owned()),
            quick: true,
        });
        let full = selected(&RunConfig {
            filter: Some("maxwell/step".to_owned()),
            quick: false,
        });

        assert!(quick[0].scenes.len() < full[0].scenes.len());
    }
}
