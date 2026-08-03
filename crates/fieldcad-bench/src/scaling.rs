//! Turning a sweep of scene sizes into a statement about complexity.
//!
//! A single timing says nothing useful about a solver: the cost of one Yee step
//! is meaningless without the lattice it stepped. What an optimizer needs is the
//! shape — whether doubling the cells doubles the cost or quadruples it — and
//! whether that shape is the one the implementation claims.
//!
//! Cost is modelled as `t = k · N^b`, so `ln t = ln k + b · ln N` and a least
//! squares fit over the sweep recovers `b`. The fit quality is reported
//! alongside it, because a clean exponent from a bad fit is a fiction: caches,
//! allocation, and a fixed overhead that dominates at small `N` all bend the
//! line, and the reader has to be able to see that.

use serde::{Deserialize, Serialize};

/// The complexity an implementation is expected to have, in the scaling
/// parameter the benchmark declares.
///
/// Writing this down is what makes a sweep a check rather than a table of
/// numbers: a measured exponent only means something against a claim.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Complexity {
    /// Independent of scene size. An edit that is correctly skipped, for example.
    Constant,
    Linear,
    /// `N log N`, reported as an approximate exponent over the swept range.
    Linearithmic,
    Quadratic,
    Cubic,
}

impl Complexity {
    /// The log-log slope this complexity predicts.
    ///
    /// `N log N` has no single slope; over the decade-ish range these sweeps
    /// cover it sits slightly above linear, and 1.1 is close enough to
    /// distinguish it from both linear and quadratic, which is all this is for.
    pub const fn expected_exponent(self) -> f64 {
        match self {
            Self::Constant => 0.0,
            Self::Linear => 1.0,
            Self::Linearithmic => 1.1,
            Self::Quadratic => 2.0,
            Self::Cubic => 3.0,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Constant => "O(1)",
            Self::Linear => "O(N)",
            Self::Linearithmic => "O(N log N)",
            Self::Quadratic => "O(N^2)",
            Self::Cubic => "O(N^3)",
        }
    }
}

/// One point in a sweep: a scene size and what it cost.
#[derive(Clone, Copy, Debug)]
pub struct ScalingPoint {
    /// The declared scaling parameter — cells, sources, or samples.
    pub n: f64,
    pub median_ns: f64,
}

/// What a sweep says about how a cost grows.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ScalingFit {
    /// Measured log-log slope.
    pub exponent: f64,
    /// Share of the sweep's variation the power law explains, from 0 to 1.
    ///
    /// Reported because it is the familiar number, but *not* what decides
    /// whether the exponent is trustworthy. A correctly flat cost has almost no
    /// variation to explain, so its R² collapses toward zero however clean the
    /// measurement was — judging a constant-time operation by R² would condemn
    /// exactly the result it should confirm.
    pub r_squared: f64,
    /// Root-mean-square residual in log space: the typical fractional distance
    /// between a measured point and the fitted power law.
    ///
    /// This is the honest fit criterion, because it is absolute rather than
    /// relative to the spread. A flat sweep and a steep sweep are both well
    /// described when their points sit close to the line.
    pub log_residual: f64,
    /// Cost per unit of the scaling parameter at the largest size measured.
    /// Usually more actionable than the exponent: it is what a budget is set in.
    pub per_unit_ns: f64,
    /// Points the fit was computed from.
    pub points: usize,
}

/// How far an observed exponent may sit from its declared complexity before the
/// harness calls it a mismatch.
///
/// Wide enough to absorb cache effects and fixed overhead across a sweep,
/// narrow enough that linear-versus-quadratic cannot hide inside it.
pub const EXPONENT_TOLERANCE: f64 = 0.35;

/// Largest log-space RMS residual an exponent may be quoted from.
///
/// Roughly a 20% typical deviation from the fitted line. Above that the sweep
/// is not describing a power law and no complexity claim should be made from it.
pub const MAX_LOG_RESIDUAL: f64 = 0.20;

impl ScalingFit {
    /// Whether the measurement supports the declared complexity.
    ///
    /// A poor fit is reported as `Unfit` rather than as agreement or
    /// disagreement: neither claim is supportable when the power law does not
    /// describe the data.
    pub fn verdict(&self, declared: Complexity) -> ScalingVerdict {
        if self.log_residual > MAX_LOG_RESIDUAL {
            return ScalingVerdict::Unfit;
        }
        let drift = self.exponent - declared.expected_exponent();
        if drift.abs() <= EXPONENT_TOLERANCE {
            ScalingVerdict::AsDeclared
        } else if drift > 0.0 {
            ScalingVerdict::WorseThanDeclared
        } else {
            ScalingVerdict::BetterThanDeclared
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalingVerdict {
    AsDeclared,
    /// Grows faster than claimed. This is the interesting one: it is where an
    /// accidental quadratic lives.
    WorseThanDeclared,
    /// Grows more slowly than claimed, so the declaration is pessimistic.
    BetterThanDeclared,
    /// No power law described the sweep well enough to judge.
    Unfit,
}

impl ScalingVerdict {
    pub const fn label(self) -> &'static str {
        match self {
            Self::AsDeclared => "as declared",
            Self::WorseThanDeclared => "WORSE than declared",
            Self::BetterThanDeclared => "better than declared",
            Self::Unfit => "no clean power law",
        }
    }

    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::WorseThanDeclared)
    }
}

/// Least-squares power-law fit over a sweep.
///
/// Returns `None` for fewer than two points, or when every point shares one
/// scene size — there is no slope to recover from a single size.
pub fn fit(points: &[ScalingPoint]) -> Option<ScalingFit> {
    let usable: Vec<_> = points
        .iter()
        .filter(|point| point.n > 0.0 && point.median_ns > 0.0)
        .collect();
    if usable.len() < 2 {
        return None;
    }

    let logs: Vec<(f64, f64)> = usable
        .iter()
        .map(|point| (point.n.ln(), point.median_ns.ln()))
        .collect();
    let count = logs.len() as f64;
    let mean_x = logs.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = logs.iter().map(|(_, y)| y).sum::<f64>() / count;

    let variance_x: f64 = logs.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    if variance_x <= f64::EPSILON {
        return None;
    }
    let covariance: f64 = logs.iter().map(|(x, y)| (x - mean_x) * (y - mean_y)).sum();
    let exponent = covariance / variance_x;
    let intercept = mean_y - exponent * mean_x;

    let total: f64 = logs.iter().map(|(_, y)| (y - mean_y).powi(2)).sum();
    let residual: f64 = logs
        .iter()
        .map(|(x, y)| (y - (intercept + exponent * x)).powi(2))
        .sum();
    // A sweep that is perfectly flat in log-space has no variance to explain.
    // The power law fits it exactly, so report that rather than dividing by zero.
    let r_squared = if total <= f64::EPSILON {
        1.0
    } else {
        (1.0 - residual / total).clamp(0.0, 1.0)
    };
    let log_residual = (residual / count).sqrt();

    let largest = usable
        .iter()
        .max_by(|left, right| left.n.total_cmp(&right.n))
        .expect("usable is non-empty");

    Some(ScalingFit {
        exponent,
        r_squared,
        log_residual,
        per_unit_ns: largest.median_ns / largest.n,
        points: usable.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep(exponent: f64) -> Vec<ScalingPoint> {
        [1.0e3, 2.0e3, 4.0e3, 8.0e3, 1.6e4]
            .into_iter()
            .map(|n| ScalingPoint {
                n,
                median_ns: 5.0 * n.powf(exponent),
            })
            .collect()
    }

    #[test]
    fn a_clean_power_law_recovers_its_exponent() {
        for expected in [0.0, 1.0, 2.0, 3.0] {
            let fit = fit(&sweep(expected)).expect("a five-point sweep must fit");
            assert!(
                (fit.exponent - expected).abs() < 1.0e-9,
                "expected {expected}, fitted {}",
                fit.exponent
            );
            assert!(fit.r_squared > 0.999);
        }
    }

    #[test]
    fn an_accidental_quadratic_is_reported_against_a_linear_claim() {
        let fit = fit(&sweep(2.0)).unwrap();

        assert_eq!(
            fit.verdict(Complexity::Linear),
            ScalingVerdict::WorseThanDeclared
        );
        assert!(fit.verdict(Complexity::Linear).needs_attention());
        assert_eq!(
            fit.verdict(Complexity::Quadratic),
            ScalingVerdict::AsDeclared
        );
    }

    #[test]
    fn a_skipped_edit_reads_as_constant() {
        let flat: Vec<_> = [1.0e3, 1.0e4, 1.0e5]
            .into_iter()
            .map(|n| ScalingPoint { n, median_ns: 42.0 })
            .collect();

        let fit = fit(&flat).unwrap();

        assert!(fit.exponent.abs() < 1.0e-9);
        assert_eq!(
            fit.verdict(Complexity::Constant),
            ScalingVerdict::AsDeclared
        );
    }

    /// A correctly constant-time operation measured with ordinary jitter.
    ///
    /// These are real numbers from `maxwell/edit-probe`, which skips the lattice
    /// rebuild when no charge changed. Judged by R² this sweep looks unfittable,
    /// because a flat line has almost no variance for a fit to explain — the
    /// criterion has to be absolute scatter instead, or the harness reports "no
    /// clean power law" precisely when an optimization is working.
    #[test]
    fn a_genuinely_flat_cost_is_confirmed_rather_than_dismissed() {
        let flat: Vec<_> = [(4.1e3, 110.643), (1.38e4, 110.270), (3.28e4, 112.594)]
            .into_iter()
            .map(|(n, median_ns)| ScalingPoint { n, median_ns })
            .collect();

        let fit = fit(&flat).unwrap();

        assert!(fit.r_squared < MIN_TRUSTWORTHY_R_SQUARED_FOR_REFERENCE);
        assert!(fit.log_residual < MAX_LOG_RESIDUAL);
        assert_eq!(
            fit.verdict(Complexity::Constant),
            ScalingVerdict::AsDeclared
        );
    }

    /// The old R²-based threshold, kept only so the test above can show that it
    /// would have rejected a good measurement.
    const MIN_TRUSTWORTHY_R_SQUARED_FOR_REFERENCE: f64 = 0.90;

    #[test]
    fn noise_without_a_trend_is_not_reported_as_an_exponent() {
        // Costs that jump around with no power-law relationship must not be
        // dressed up as a clean complexity claim.
        let noisy: Vec<_> = [
            (1.0e3, 900.0),
            (2.0e3, 100.0),
            (4.0e3, 5000.0),
            (8.0e3, 80.0),
        ]
        .into_iter()
        .map(|(n, median_ns)| ScalingPoint { n, median_ns })
        .collect();

        let fit = fit(&noisy).unwrap();

        assert!(fit.log_residual > MAX_LOG_RESIDUAL);
        assert_eq!(fit.verdict(Complexity::Linear), ScalingVerdict::Unfit);
    }

    #[test]
    fn a_single_scene_size_cannot_produce_a_slope() {
        let single = [ScalingPoint {
            n: 1.0e3,
            median_ns: 10.0,
        }];
        let repeated = [
            ScalingPoint {
                n: 1.0e3,
                median_ns: 10.0,
            },
            ScalingPoint {
                n: 1.0e3,
                median_ns: 20.0,
            },
        ];

        assert!(fit(&single).is_none());
        assert!(fit(&repeated).is_none());
    }
}
