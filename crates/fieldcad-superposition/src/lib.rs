//! Generic point/sphere source superposition for an inverse-square coupling
//! law.
//!
//! Coulomb's law and Newton's law of gravitation are the same functional
//! form — a source's field falls off as the inverse square of distance
//! outside its own radius, and is a finite, different formula inside a
//! uniformly-distributed sphere — with a different coupling constant and
//! opposite sign. This crate owns that shared numerical core once; a
//! caller supplies only the constant (magnitude and sign) and its sources.
//! It has no notion of "charge" or "mass," no plugin, and no runtime
//! dependency, matching `fieldcad-newtonian-gravity`'s own existing split
//! between physics kernel and plugin glue.

use fieldcad_core::{PointOrSphere, SampleValidity, UndefinedReason};
use glam::DVec3;

/// One source of a superposed field: a position, a coupling strength
/// (charge or mass — sign convention is the caller's, via
/// `coupling_constant`), and the geometry it is spread over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InverseSquareSource {
    pub position: DVec3,
    pub strength: f64,
    pub distribution: PointOrSphere,
}

/// A superposed field vector and potential at one point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InverseSquareSample {
    pub field: DVec3,
    pub potential: f64,
    pub validity: SampleValidity,
}

impl InverseSquareSample {
    fn undefined(reason: UndefinedReason) -> Self {
        Self {
            field: DVec3::ZERO,
            potential: 0.0,
            validity: SampleValidity::Undefined(reason),
        }
    }
}

/// One source's field/potential contribution at `position`, or `None` if
/// `position` sits inside that source's own exclusion geometry — the
/// analytic exterior field is undefined there, not merely large.
fn contribution(
    coupling_constant: f64,
    source: InverseSquareSource,
    position: DVec3,
) -> Option<(DVec3, f64)> {
    let displacement = position - source.position;
    let distance_squared = displacement.length_squared();
    let distance = distance_squared.sqrt();
    match source.distribution {
        PointOrSphere::Point { exclusion_radius } => {
            if distance <= exclusion_radius {
                return None;
            }
            let inverse_distance = distance.recip();
            Some((
                coupling_constant * source.strength * displacement * inverse_distance.powi(3),
                coupling_constant * source.strength * inverse_distance,
            ))
        }
        PointOrSphere::UniformSphere { radius } if distance < radius => Some((
            coupling_constant * source.strength * displacement / radius.powi(3),
            coupling_constant * source.strength / (2.0 * radius)
                * (3.0 - distance_squared / radius.powi(2)),
        )),
        PointOrSphere::UniformSphere { .. } => {
            let inverse_distance = distance.recip();
            Some((
                coupling_constant * source.strength * displacement * inverse_distance.powi(3),
                coupling_constant * source.strength * inverse_distance,
            ))
        }
    }
}

/// Evaluate the superposed field and potential at `position`. A source
/// whose own exclusion geometry contains `position` makes the whole sample
/// undefined — the display grid needs one well-defined-or-not value, not a
/// partial sum (see [`field_excluding`] for the force-calculation case,
/// which needs the opposite).
pub fn evaluate_sources(
    coupling_constant: f64,
    sources: impl IntoIterator<Item = InverseSquareSource>,
    position: DVec3,
) -> InverseSquareSample {
    let mut field = DVec3::ZERO;
    let mut potential = 0.0;
    for source in sources {
        if source.strength == 0.0 {
            continue;
        }
        let Some((field_contribution, potential_contribution)) =
            contribution(coupling_constant, source, position)
        else {
            return InverseSquareSample::undefined(UndefinedReason::InsideSourceRadius);
        };
        field += field_contribution;
        potential += potential_contribution;
        if !field.is_finite() || !potential.is_finite() {
            return InverseSquareSample::undefined(UndefinedReason::NumericalOverflow);
        }
    }
    InverseSquareSample {
        field,
        potential,
        validity: SampleValidity::Exact,
    }
}

/// Field only, skipping each source whose own geometry contains `position`
/// rather than voiding the whole sample. A force calculation needs the
/// well-defined sources summed regardless of what one nearby, unrelated
/// source is doing — the opposite requirement from [`evaluate_sources`],
/// which the display grid needs a single well-defined-or-not sample from.
///
/// `sources` is expected to already exclude whichever source the query
/// point belongs to (a body does not feel its own field) — this function
/// has no notion of source identity to do that filtering itself.
///
/// `None` if the summed field overflowed to a non-finite value.
pub fn field_excluding(
    coupling_constant: f64,
    sources: impl IntoIterator<Item = InverseSquareSource>,
    position: DVec3,
) -> Option<DVec3> {
    let mut field = DVec3::ZERO;
    for source in sources {
        if source.strength == 0.0 {
            continue;
        }
        let Some((field_contribution, _)) = contribution(coupling_constant, source, position)
        else {
            continue;
        };
        field += field_contribution;
    }
    field.is_finite().then_some(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(strength: f64, position: DVec3) -> InverseSquareSource {
        InverseSquareSource {
            position,
            strength,
            distribution: PointOrSphere::Point {
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
            distribution: PointOrSphere::Point {
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
            distribution: PointOrSphere::UniformSphere { radius: 2.0 },
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
            distribution: PointOrSphere::Point {
                exclusion_radius: 2.0,
            },
        };
        // The origin is 1m inside `grazing`'s exclusion radius. A negative
        // coupling constant (attractive, like gravity) so the primary's own
        // contribution has an unambiguous sign to check.
        let field = field_excluding(-1.0, [primary, grazing], DVec3::ZERO).unwrap();
        assert!(field.x < 0.0, "the primary's pull must still come through");
    }

    #[test]
    fn field_excluding_uses_the_finite_interior_formula_for_a_sphere() {
        let source = InverseSquareSource {
            position: DVec3::ZERO,
            strength: 1.0,
            distribution: PointOrSphere::UniformSphere { radius: 2.0 },
        };
        let field = field_excluding(1.0, [source], DVec3::X).unwrap();
        // Interior of a uniform sphere: field grows linearly with distance
        // from centre, not the exterior's inverse-square falloff — and is
        // not simply excluded to zero.
        assert!(field.length() > 0.0);
    }
}
