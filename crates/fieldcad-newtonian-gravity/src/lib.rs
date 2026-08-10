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

use fieldcad_core::quantities::{MassKg, SiScalar};
use fieldcad_core::{CoupledSource, SampleGeometry, SampleValidity};
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

/// `None` for a source with zero coupling value.
fn inverse_square_source(source: &CoupledSource<MassKg>) -> Option<InverseSquareSource> {
    let mass = source.coupling_value.into_si();
    (mass != 0.0).then_some(InverseSquareSource {
        position: source.position,
        strength: mass,
        distribution: source.distribution,
    })
}

/// Evaluate the superposed Newtonian field and potential in SI units.
pub fn evaluate_sources(sources: &[CoupledSource<MassKg>], position: DVec3) -> NewtonianSample {
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
/// own exclusion geometry contains it rather than voiding the whole sample.
pub fn evaluate_acceleration_excluding<'a>(
    sources: impl IntoIterator<Item = &'a CoupledSource<MassKg>>,
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
    sources: &[CoupledSource<MassKg>],
    geometry: &SampleGeometry,
) -> Vec<NewtonianSample> {
    geometry
        .positions()
        .map(|position| evaluate_sources(sources, position))
        .collect()
}

/// [`evaluate_geometry`], writing into a caller-owned buffer instead of
/// allocating a fresh `Vec` — for a cache that already holds a
/// correctly-sized buffer from a previous evaluation of this geometry and
/// only needs its values refreshed.
pub fn evaluate_geometry_into(
    sources: &[CoupledSource<MassKg>],
    geometry: &SampleGeometry,
    out: &mut [NewtonianSample],
) {
    for (position, out) in geometry.positions().zip(out) {
        *out = evaluate_sources(sources, position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::quantities::kilogram;
    use fieldcad_core::{ChargeDistribution, ObjectId, Velocity};

    fn point(mass: f64) -> CoupledSource<MassKg> {
        CoupledSource::new(
            ObjectId::new(0),
            DVec3::ZERO,
            Velocity::default(),
            MassKg::new::<kilogram>(mass),
            ChargeDistribution::Point {
                exclusion_radius: 0.0,
            },
        )
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
        let source = CoupledSource {
            distribution: ChargeDistribution::UniformSphere { radius: 2.0 },
            ..point(3.0)
        };
        let sample = evaluate_sources(&[source], DVec3::ZERO);
        assert_eq!(sample.acceleration, DVec3::ZERO);
        assert!(sample.potential.is_finite());
    }
}
