//! Backend-neutral Newtonian gravity evaluation.
//!
//! This crate owns no plugin, renderer, runtime, or transport. Local Field CAD,
//! a future accelerator, and an Orishu workload adapter can therefore use the
//! same source law and singularity semantics.
//!
//! The actual point/sphere superposition kernel lives in
//! `fieldcad-superposition`, shared with `plugins/electrostatics` — Newton's
//! law of gravitation and Coulomb's law are the same functional form with a
//! different coupling constant and an opposite sign. This crate is the thin,
//! gravity-specific adapter over it: `MassSource` in, `NewtonianSample` out.

use fieldcad_core::{SampleGeometry, SampleValidity};
use fieldcad_mass_sources::MassSource;
use fieldcad_superposition::InverseSquareSource;
use glam::DVec3;

/// Newton's gravitational constant in m³·kg⁻¹·s⁻² (CODATA 2018).
pub const GRAVITATIONAL_CONSTANT: f64 = 6.674_30e-11;

/// Gravity acceleration and gravitational potential at one position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NewtonianSample {
    pub acceleration: DVec3,
    pub potential: f64,
    pub validity: SampleValidity,
}

/// `None` for a body with inertia but no gravitational mass (the
/// gravitational equivalent of an uncharged body) or with zero
/// gravitational mass — neither sources a field.
fn inverse_square_source(source: &MassSource) -> Option<InverseSquareSource> {
    let mass = source.gravitational_mass_kg?;
    (mass != 0.0).then_some(InverseSquareSource {
        position: source.position,
        strength: mass,
        distribution: source.distribution.into(),
    })
}

/// Evaluate the superposed Newtonian field and potential in SI units.
pub fn evaluate_sources(sources: &[MassSource], position: DVec3) -> NewtonianSample {
    let sample = fieldcad_superposition::evaluate_sources(
        -GRAVITATIONAL_CONSTANT,
        sources.iter().filter_map(inverse_square_source),
        position,
    );
    NewtonianSample {
        acceleration: sample.field,
        potential: sample.potential,
        validity: sample.validity,
    }
}

/// Superposed acceleration at `position`, skipping only whichever source's
/// own exclusion geometry contains it rather than voiding the whole sample —
/// the analytic point field is undefined near that source specifically, not
/// near an unrelated, perfectly well-defined one. Unlike [`evaluate_sources`],
/// which the display grid needs a single well-defined-or-not sample from, a
/// force calculation needs the well-defined sources summed regardless of
/// what a nearby, unrelated one is doing. `None` if the summed acceleration
/// overflowed to a non-finite value.
pub fn evaluate_acceleration_excluding<'a>(
    sources: impl IntoIterator<Item = &'a MassSource>,
    position: DVec3,
) -> Option<DVec3> {
    fieldcad_superposition::field_excluding(
        -GRAVITATIONAL_CONSTANT,
        sources.into_iter().filter_map(inverse_square_source),
        position,
    )
}

/// Evaluate one complete geometry through the canonical source law.
pub fn evaluate_geometry(
    sources: &[MassSource],
    geometry: &SampleGeometry,
) -> Vec<NewtonianSample> {
    geometry
        .positions()
        .map(|position| evaluate_sources(sources, position))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{ObjectId, Velocity};
    use fieldcad_mass_sources::MassDistribution;

    fn point(mass: f64) -> MassSource {
        MassSource {
            object: ObjectId::new(0),
            position: DVec3::ZERO,
            velocity: Velocity::default(),
            inertial_mass_kg: mass,
            gravitational_mass_kg: Some(mass),
            pinned: true,
            distribution: MassDistribution::Point {
                exclusion_radius: 0.0,
            },
        }
    }

    #[test]
    fn a_point_mass_is_attractive_and_inverse_square() {
        let near = evaluate_sources(&[point(2.0)], DVec3::X);
        let far = evaluate_sources(&[point(2.0)], DVec3::X * 2.0);
        assert!(near.acceleration.x < 0.0);
        assert_eq!(far.acceleration.x / near.acceleration.x, 0.25);
        assert!(near.potential < 0.0);
    }

    #[test]
    fn a_uniform_sphere_is_finite_at_its_centre() {
        let source = MassSource {
            distribution: MassDistribution::UniformSphere { radius: 2.0 },
            ..point(3.0)
        };
        let sample = evaluate_sources(&[source], DVec3::ZERO);
        assert_eq!(sample.acceleration, DVec3::ZERO);
        assert!(sample.potential.is_finite());
    }
}
