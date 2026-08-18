//! Generic point/sphere source superposition for an inverse-square coupling law.
//!
//! Coulomb's law and Newton's law of gravitation are the same functional
//! form — a source's field falls off as the inverse square of distance
//! outside its own radius, and is a finite, different formula inside a
//! uniformly-distributed sphere — with a different coupling constant and
//! opposite sign. This crate owns that shared numerical core once; a
//! caller supplies only the constant (magnitude and sign) and its sources.
//! It has no notion of "charge" or "mass," no plugin, and no runtime
//! dependency — `plugins/electrostatics` and `plugins/gravity` are both
//! thin adapters over it, converting their own source/sample types to and
//! from the generic shapes here and supplying their own coupling constant.
//! [`InverseSquareBatchEvaluator`] is the shared evaluator seam both
//! plugins inject a CPU or GPU implementation through.

use fieldcad_core::{
    ChargeDistribution, Domain, Precision, SampleGeometry, SampleValidity, UndefinedReason,
};
use glam::{DMat3, DVec3};

/// One source of a superposed field: a position, a coupling strength
/// (charge or mass — sign convention is the caller's, via
/// `coupling_constant`), and the geometry it is spread over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InverseSquareSource {
    pub position: DVec3,
    pub strength: f64,
    pub distribution: ChargeDistribution,
}

/// A superposed field vector, potential, and Jacobian at one point.
///
/// `gradient` is the field's own spatial derivative (`∂field_i/∂x_j` in
/// column `j`), not the potential's — a caller that wants `∇φ` uses `-field`
/// directly, since `E = -∇φ` already.
///
/// `Some` for every sample from the CPU analytical solver
/// ([`evaluate_sources`] in this crate) and from the desktop's `wgpu`
/// compute-shader evaluator, which ports the same closed-form Jacobian —
/// including an undefined sample whose validity makes its zero placeholder
/// unusable — an undefined position must not withdraw the gradient
/// *capability* from every other position in the batch. `None` is for a
/// hypothetical evaluator without derivative support at all; nothing
/// shipped today produces that, but the type stays optional so one could
/// without breaking this contract. A caller uses that batch-wide capability
/// to decide whether to attach a gradient column to what it publishes; see
/// [`InverseSquareBatchEvaluator`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InverseSquareSample {
    pub field: DVec3,
    pub potential: f64,
    pub gradient: Option<DMat3>,
    pub validity: SampleValidity,
}

impl InverseSquareSample {
    fn undefined(reason: UndefinedReason) -> Self {
        Self {
            field: DVec3::ZERO,
            potential: 0.0,
            gradient: Some(DMat3::ZERO),
            validity: SampleValidity::Undefined(reason),
        }
    }
}

/// The exterior-field Jacobian shared by a point source and a sphere source
/// observed from outside its radius: `∇E = (k·strength / r³) · (I − 3 d̂⊗d̂)`.
/// Symmetric (`∂E_i/∂x_j = ∂E_j/∂x_i`), which is exactly the curl-free
/// property an electrostatic (or gravitational) field must have.
fn point_jacobian(coupling_strength: f64, displacement: DVec3, inverse_distance: f64) -> DMat3 {
    let d_hat = displacement * inverse_distance;
    let outer = DMat3::from_cols(d_hat.x * d_hat, d_hat.y * d_hat, d_hat.z * d_hat);
    (coupling_strength * inverse_distance.powi(3)) * (DMat3::IDENTITY - 3.0 * outer)
}

/// The exterior field shared by a point source and a sphere source observed
/// from outside its radius: `E = (k·strength / r³) · d`, taking the
/// pre-fused coupling strength and `1/r` so both call sites keep one
/// operation order (and therefore bit-identical results).
fn exterior_field(coupling_strength: f64, displacement: DVec3, inverse_distance: f64) -> DVec3 {
    coupling_strength * displacement * inverse_distance.powi(3)
}

/// The exterior potential shared by a point source and a sphere source
/// observed from outside its radius: `φ = k·strength / r`.
fn exterior_potential(coupling_strength: f64, inverse_distance: f64) -> f64 {
    coupling_strength * inverse_distance
}

/// One source's field contribution only, or `None` if `position` sits inside
/// that source's own exclusion geometry. The force path
/// ([`field_excluding_at`]) needs no potential and no Jacobian, so this
/// variant builds neither — half the per-(body × source) work of
/// [`contribution`].
fn field_contribution(
    coupling_constant: f64,
    source: InverseSquareSource,
    position: DVec3,
) -> Option<DVec3> {
    let displacement = position - source.position;
    let distance_squared = displacement.length_squared();
    let coupling_strength = coupling_constant * source.strength;
    match source.distribution {
        ChargeDistribution::Point { exclusion_radius } => {
            if distance_squared < exclusion_radius * exclusion_radius {
                return None;
            }
            let inverse_distance = distance_squared.sqrt().recip();
            Some(exterior_field(
                coupling_strength,
                displacement,
                inverse_distance,
            ))
        }
        ChargeDistribution::UniformSphere { radius } if distance_squared < radius * radius => {
            Some(coupling_strength * displacement / radius.powi(3))
        }
        ChargeDistribution::UniformSphere { .. } => {
            let inverse_distance = distance_squared.sqrt().recip();
            Some(exterior_field(
                coupling_strength,
                displacement,
                inverse_distance,
            ))
        }
    }
}

/// One source's field/potential/gradient contribution at `position`, or
/// `None` if `position` sits inside that source's own exclusion geometry —
/// the analytic exterior field is undefined there, not merely large.
fn contribution(
    coupling_constant: f64,
    source: InverseSquareSource,
    position: DVec3,
) -> Option<(DVec3, f64, DMat3)> {
    let displacement = position - source.position;
    let distance_squared = displacement.length_squared();
    let coupling_strength = coupling_constant * source.strength;
    match source.distribution {
        ChargeDistribution::Point { exclusion_radius } => {
            if distance_squared < exclusion_radius * exclusion_radius {
                return None;
            }
            let distance = distance_squared.sqrt();
            let inverse_distance = distance.recip();
            Some((
                exterior_field(coupling_strength, displacement, inverse_distance),
                exterior_potential(coupling_strength, inverse_distance),
                point_jacobian(coupling_strength, displacement, inverse_distance),
            ))
        }
        ChargeDistribution::UniformSphere { radius } if distance_squared < radius * radius => {
            let r2 = radius * radius;
            let r3 = r2 * radius;
            Some((
                coupling_strength * displacement / r3,
                coupling_strength / (2.0 * radius) * (3.0 - distance_squared / r2),
                // E_i = (k·strength/R³)·d_i is linear in `d`, so its Jacobian is
                // the constant isotropic (k·strength/R³)·I — no singularity,
                // unlike the exterior formula, which is exactly why the interior
                // case is handled separately here too.
                DMat3::from_diagonal(DVec3::splat(coupling_constant * source.strength / r3)),
            ))
        }
        ChargeDistribution::UniformSphere { .. } => {
            let distance = distance_squared.sqrt();
            let inverse_distance = distance.recip();
            Some((
                exterior_field(coupling_strength, displacement, inverse_distance),
                exterior_potential(coupling_strength, inverse_distance),
                point_jacobian(coupling_strength, displacement, inverse_distance),
            ))
        }
    }
}

/// Evaluate the superposed field and potential at `position`. A source
/// whose own exclusion geometry contains `position` makes the whole sample
/// undefined — the display grid needs one well-defined-or-not value, not a
/// partial sum (see [`field_excluding_at`] for the force-calculation case,
/// which needs the opposite).
pub fn evaluate_sources(
    coupling_constant: f64,
    sources: impl IntoIterator<Item = InverseSquareSource>,
    position: DVec3,
) -> InverseSquareSample {
    let mut field = DVec3::ZERO;
    let mut potential = 0.0;
    let mut gradient = DMat3::ZERO;
    for source in sources {
        if source.strength == 0.0 {
            continue;
        }
        let Some((field_contribution, potential_contribution, gradient_contribution)) =
            contribution(coupling_constant, source, position)
        else {
            return InverseSquareSample::undefined(UndefinedReason::InsideSourceRadius);
        };
        field += field_contribution;
        potential += potential_contribution;
        gradient += gradient_contribution;
    }
    // A non-finite contribution propagates through the accumulators, so one
    // end-of-sample check is equivalent to the per-source checks it replaces.
    // A containment hit found earlier in the loop still wins: it returns
    // before this line, keeping `InsideSourceRadius` the reported reason
    // even when an earlier source already overflowed.
    if !field.is_finite() || !potential.is_finite() || !matrix_is_finite(gradient) {
        return InverseSquareSample::undefined(UndefinedReason::NumericalOverflow);
    }
    InverseSquareSample {
        field,
        potential,
        gradient: Some(gradient),
        validity: SampleValidity::Exact,
    }
}

fn matrix_is_finite(matrix: DMat3) -> bool {
    matrix.x_axis.is_finite() && matrix.y_axis.is_finite() && matrix.z_axis.is_finite()
}

/// Field only, skipping each source whose own geometry contains `position`
/// rather than voiding the whole sample. A force calculation needs the
/// well-defined sources summed regardless of what one nearby, unrelated
/// source is doing — the opposite requirement from [`evaluate_sources`],
/// which the display grid needs a single well-defined-or-not sample from.
///
/// `sources[excluded]` is the query point's own source (a body does not
/// feel its own field); pass an out-of-range `excluded` (e.g. `sources.len()`
/// or `usize::MAX`) to sum every source with no exclusion. Zero-strength
/// sources are skipped for the same reason [`evaluate_sources`] skips them:
/// they contribute exactly zero, and skipping also keeps a coincident
/// zero-strength source (whose `d/r` direction would be `0/0`) from
/// poisoning the whole sum with NaN.
///
/// One plain loop over the slice, not an excluding iterator chain, is what
/// keeps this free of per-iteration iterator bookkeeping on the per-tick
/// force loop's hot path — the shape `fieldcad-superposition-solver`'s
/// `add_forces_excluding_into` wants: the slice is built once per world
/// change, and each body excludes itself by index.
///
/// `None` if the summed field overflowed to a non-finite value.
pub fn field_excluding_at(
    coupling_constant: f64,
    sources: &[InverseSquareSource],
    excluded: usize,
    position: DVec3,
) -> Option<DVec3> {
    let field_acc = sources
        .iter()
        .enumerate()
        .filter(|(index, source)| *index != excluded && source.strength != 0.0)
        .filter_map(|(_, source)| field_contribution(coupling_constant, *source, position))
        .fold(DVec3::ZERO, |acc, contribution| acc + contribution);
    field_acc.is_finite().then_some(field_acc)
}

/// Batch evaluator for an inverse-square coupling law.
///
/// A single evaluator can serve both electrostatic (Coulomb) and
/// gravitational equation systems — the caller supplies the coupling
/// constant (sign and magnitude) separately. A successful batch reports
/// gradient availability uniformly: every sample has `Some(gradient)`, or
/// every sample has `None` — see [`InverseSquareSample::gradient`].
pub trait InverseSquareBatchEvaluator: Send + Sync {
    /// The numerical precision this evaluator produces (e.g. `F64` for the
    /// CPU oracle, `F32` for a WGSL compute shader).
    fn precision(&self) -> Precision;

    /// Evaluate the field, potential, and (if available) gradient at every
    /// sample position described by `geometry`. On success, the returned
    /// vector has exactly `geometry.len()` entries; a backend that cannot
    /// meet that contract returns `Err`, never a partial result.
    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String>;

    /// [`Self::evaluate`], writing into a caller-owned buffer.
    ///
    /// `out.len()` must equal `geometry.len()`. The default forwards to
    /// [`Self::evaluate`] and copies the result — correct for any
    /// evaluator, but still allocating; an evaluator that can write its
    /// result directly should override this to skip that intermediate
    /// `Vec` on a cache's refill-in-place path.
    fn evaluate_into(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
        out: &mut [InverseSquareSample],
    ) -> Result<(), String> {
        if out.len() != geometry.len() {
            return Err(format!(
                "inverse-square output buffer has length {}, expected {}",
                out.len(),
                geometry.len()
            ));
        }
        let evaluated = self.evaluate(coupling_constant, sources, domain, geometry)?;
        if evaluated.len() != geometry.len() {
            return Err(format!(
                "inverse-square evaluator returned {} samples for a geometry of length {}",
                evaluated.len(),
                geometry.len()
            ));
        }
        out.copy_from_slice(&evaluated);
        Ok(())
    }
}

/// The reference CPU `f64` oracle, and the correctness baseline every
/// faster backend is checked against.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuInverseSquareEvaluator;

impl InverseSquareBatchEvaluator for CpuInverseSquareEvaluator {
    fn precision(&self) -> Precision {
        Precision::F64
    }

    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String> {
        Ok(geometry
            .positions()
            .map(|position| evaluate_sources(coupling_constant, sources.iter().copied(), position))
            .collect())
    }

    fn evaluate_into(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
        out: &mut [InverseSquareSample],
    ) -> Result<(), String> {
        if out.len() != geometry.len() {
            return Err(format!(
                "inverse-square output buffer has length {}, expected {}",
                out.len(),
                geometry.len()
            ));
        }
        for (position, out) in geometry.positions().zip(out) {
            *out = evaluate_sources(coupling_constant, sources.iter().copied(), position);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(strength: f64, position: DVec3) -> InverseSquareSource {
        InverseSquareSource {
            position,
            strength,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 0.0,
            },
        }
    }

    #[test]
    fn a_point_source_is_inverse_square_and_directional() {
        let near = evaluate_sources(2.0, [point(3.0, DVec3::ZERO)], DVec3::X);
        let far = evaluate_sources(2.0, [point(3.0, DVec3::ZERO)], DVec3::X * 2.0);
        assert_eq!(far.field.x / near.field.x, 0.25);
        assert!(near.potential > 0.0);
    }

    #[test]
    fn a_negative_coupling_constant_attracts() {
        let sample = evaluate_sources(-1.0, [point(1.0, DVec3::ZERO)], DVec3::X);
        assert!(sample.field.x < 0.0);
        assert!(sample.potential < 0.0);
    }

    #[test]
    fn inside_a_points_exclusion_radius_is_undefined() {
        let source = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 1.0,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 0.5,
            },
        };
        let sample = evaluate_sources(1.0, [source], DVec3::X * 0.4);
        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        );
    }

    #[test]
    fn a_uniform_sphere_is_finite_at_its_centre() {
        let source = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 3.0,
            distribution: ChargeDistribution::UniformSphere { radius: 2.0 },
        };
        let sample = evaluate_sources(1.0, [source], DVec3::ZERO);
        assert_eq!(sample.field, DVec3::ZERO);
        assert!(sample.potential.is_finite());
    }

    /// The bug this crate exists to make impossible to reintroduce in only
    /// one of two callers (PH-2/PH-3): a body grazing one source's own
    /// exclusion geometry must not lose the field from every *other*
    /// source too, and a uniformly-distributed sphere's interior must use
    /// its own finite formula rather than being treated as excluded.
    #[test]
    fn field_excluding_skips_only_the_offending_source() {
        let primary = point(1.0, DVec3::new(-10.0, 0.0, 0.0));
        let grazing = InverseSquareSource {
            position: DVec3::new(1.0, 0.0, 0.0),
            strength: 1.0,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 2.0,
            },
        };
        // The origin is 1m inside `grazing`'s exclusion radius. A negative
        // coupling constant (attractive, like gravity) so the primary's own
        // contribution has an unambiguous sign to check. Neither source is
        // excluded by index (`usize::MAX` never matches).
        let sources = [primary, grazing];
        let field = field_excluding_at(-1.0, &sources, usize::MAX, DVec3::ZERO).unwrap();
        assert!(field.x < 0.0, "the primary's pull must still come through");
    }

    #[test]
    fn field_excluding_uses_the_finite_interior_formula_for_a_sphere() {
        let sources = [InverseSquareSource {
            position: DVec3::ZERO,
            strength: 1.0,
            distribution: ChargeDistribution::UniformSphere { radius: 2.0 },
        }];
        let field = field_excluding_at(1.0, &sources, usize::MAX, DVec3::X).unwrap();
        // Interior of a uniform sphere: field grows linearly with distance
        // from centre, not the exterior's inverse-square falloff — and is
        // not simply excluded to zero.
        assert!(field.length() > 0.0);
    }

    /// The force path's field-only variant must produce exactly the field
    /// the sampling path computes — same formulas, same summation order,
    /// bit-for-bit — over exterior points, sphere interiors, and
    /// multi-source superpositions alike.
    #[test]
    fn field_excluding_matches_the_sampling_paths_field() {
        let sources = [
            point(1.5, DVec3::new(-1.0, 0.2, 0.0)),
            point(-0.8, DVec3::new(2.0, -0.5, 1.0)),
            InverseSquareSource {
                position: DVec3::new(0.3, 0.3, 0.3),
                strength: 2.0,
                distribution: ChargeDistribution::UniformSphere { radius: 1.5 },
            },
        ];
        for position in [
            DVec3::ZERO,
            DVec3::X,
            DVec3::new(0.5, -0.5, 1.5),
            DVec3::new(-2.0, 1.0, 0.3),
        ] {
            let sampled = evaluate_sources(1.0, sources, position);
            assert_eq!(sampled.validity, SampleValidity::Exact);
            assert_eq!(
                field_excluding_at(1.0, &sources, usize::MAX, position),
                Some(sampled.field),
                "force and sampling paths diverged at {position:?}"
            );
        }
    }

    /// Pins the reason ordering the hoisted finiteness check introduced:
    /// containment returns early from the loop, so a sample where an
    /// earlier source overflows *and* a later source contains the position
    /// now reports `InsideSourceRadius` where the per-source check used to
    /// report `NumericalOverflow` first. Both remain `Undefined`; only the
    /// reason differs.
    #[test]
    fn a_later_containment_hit_outranks_an_earlier_overflow() {
        let overflowing = InverseSquareSource {
            position: DVec3::X,
            strength: f64::MAX,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 0.0,
            },
        };
        let containing = InverseSquareSource {
            position: DVec3::new(-2.0, 0.0, 0.0),
            strength: 1.0,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 4.0,
            },
        };
        let sample = evaluate_sources(1.0, [overflowing, containing], DVec3::new(0.5, 0.0, 0.0));
        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        );
    }

    #[test]
    fn an_overflow_alone_still_reports_numerical_overflow() {
        let overflowing = InverseSquareSource {
            position: DVec3::X,
            strength: f64::MAX,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 0.0,
            },
        };
        let sample = evaluate_sources(1.0, [overflowing], DVec3::new(0.5, 0.0, 0.0));
        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::NumericalOverflow)
        );
    }

    /// A zero-strength source contributes nothing anywhere — not even its
    /// exclusion geometry may undefine a sample. The skip precedes the
    /// containment check in both paths; this pins that ordering.
    #[test]
    fn a_zero_strength_sources_exclusion_radius_does_not_undefine_a_sample() {
        let zero = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 0.0,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 2.0,
            },
        };
        let real = point(1.0, DVec3::new(5.0, 0.0, 0.0));
        let sample = evaluate_sources(1.0, [zero, real], DVec3::X);
        assert_eq!(sample.validity, SampleValidity::Exact);
        // Positive source at +5, sample at +1: the field points back
        // toward the source, i.e. −x.
        assert!(sample.field.x < 0.0);
    }

    /// Before the zero-strength skip, a zero-strength point source sitting
    /// exactly on the query point produced a `0 · inf = NaN` direction and
    /// voided the *entire* summed field to `None` — one coincident
    /// mass-less object killed all gravity on that body. The skip must
    /// hold: the source is ignored, and the real source's field comes
    /// through bit-for-bit as the sampling path computes it.
    #[test]
    fn field_excluding_skips_a_coincident_zero_strength_source() {
        let coincident = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 0.0,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 0.0,
            },
        };
        let real = point(1.0, DVec3::new(-3.0, 0.0, 0.0));
        let expected = evaluate_sources(1.0, [real], DVec3::ZERO).field;
        let sources = [coincident, real];
        assert_eq!(
            field_excluding_at(1.0, &sources, usize::MAX, DVec3::ZERO),
            Some(expected)
        );
    }

    fn matrix_close(a: DMat3, b: DMat3, tolerance: f64) -> bool {
        (a.x_axis - b.x_axis).abs().max_element() < tolerance
            && (a.y_axis - b.y_axis).abs().max_element() < tolerance
            && (a.z_axis - b.z_axis).abs().max_element() < tolerance
    }

    /// A strong, direct self-consistency oracle: the closed-form Jacobian
    /// must agree with a central difference of the very field it was
    /// derived from.
    #[test]
    fn a_point_sources_jacobian_matches_a_central_difference_of_its_own_field() {
        let source = point(2.0, DVec3::ZERO);
        let coupling_constant = 3.0;
        let position = DVec3::new(1.0, 0.5, -0.3);
        let sample = evaluate_sources(coupling_constant, [source], position);

        let field_at = |p: DVec3| evaluate_sources(coupling_constant, [source], p).field;
        let h = 1.0e-5;
        let numerical = DMat3::from_cols(
            (field_at(position + DVec3::X * h) - field_at(position - DVec3::X * h)) / (2.0 * h),
            (field_at(position + DVec3::Y * h) - field_at(position - DVec3::Y * h)) / (2.0 * h),
            (field_at(position + DVec3::Z * h) - field_at(position - DVec3::Z * h)) / (2.0 * h),
        );

        let gradient = sample
            .gradient
            .expect("CPU evaluator always reports a gradient");
        assert!(
            matrix_close(gradient, numerical, 1.0e-4),
            "closed-form {gradient:?} vs numerical {numerical:?}"
        );
    }

    #[test]
    fn jacobians_superpose_linearly_across_sources() {
        let a = point(1.0, DVec3::new(-2.0, 0.0, 0.0));
        let b = point(-1.5, DVec3::new(3.0, 1.0, 0.0));
        let position = DVec3::new(0.5, 0.2, 0.1);

        let combined = evaluate_sources(1.0, [a, b], position).gradient.unwrap();
        let separate = evaluate_sources(1.0, [a], position).gradient.unwrap()
            + evaluate_sources(1.0, [b], position).gradient.unwrap();

        assert!(matrix_close(combined, separate, 1.0e-9));
    }

    #[test]
    fn a_uniform_spheres_interior_jacobian_is_isotropic() {
        let source = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 3.0,
            distribution: ChargeDistribution::UniformSphere { radius: 2.0 },
        };
        let sample = evaluate_sources(1.0, [source], DVec3::new(0.3, -0.1, 0.5));

        let expected = DMat3::from_diagonal(DVec3::splat(3.0 / 2.0_f64.powi(3)));
        assert!(matrix_close(sample.gradient.unwrap(), expected, 1.0e-9));
    }

    /// Regression: an undefined sample must still carry a gradient
    /// capability, or a batch with one undefined position (validity
    /// already marks it unusable) would silently withdraw the gradient
    /// column from every *other* position in the same batch too.
    #[test]
    fn an_undefined_sample_still_reports_a_placeholder_gradient() {
        let source = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 1.0,
            distribution: ChargeDistribution::Point {
                exclusion_radius: 0.5,
            },
        };
        let sample = evaluate_sources(1.0, [source], DVec3::X * 0.4);
        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        );
        assert_eq!(sample.gradient, Some(DMat3::ZERO));
    }
}
