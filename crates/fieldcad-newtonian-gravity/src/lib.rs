//! Backend-neutral Newtonian gravity evaluation.
//!
//! This crate owns no plugin, renderer, runtime, or transport. Local Field CAD,
//! a future accelerator, and an Orishu workload adapter can therefore use the
//! same source law and singularity semantics.

use fieldcad_core::{SampleGeometry, SampleValidity, UndefinedReason};
use fieldcad_mass_sources::{MassDistribution, MassSource};
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

impl NewtonianSample {
    fn undefined(reason: UndefinedReason) -> Self {
        Self {
            acceleration: DVec3::ZERO,
            potential: 0.0,
            validity: SampleValidity::Undefined(reason),
        }
    }
}

/// Evaluate the superposed Newtonian field and potential in SI units.
pub fn evaluate_sources(sources: &[MassSource], position: DVec3) -> NewtonianSample {
    let mut acceleration = DVec3::ZERO;
    let mut potential = 0.0;
    for source in sources {
        let Some(mass) = source.gravitational_mass_kg else {
            continue;
        };
        if mass == 0.0 {
            continue;
        }
        let displacement = position - source.position;
        let distance_squared = displacement.length_squared();
        let distance = distance_squared.sqrt();
        let (field, phi) = match source.distribution {
            MassDistribution::Point { exclusion_radius } => {
                if distance <= exclusion_radius {
                    return NewtonianSample::undefined(UndefinedReason::InsideSourceRadius);
                }
                let inverse = distance.recip();
                (
                    -GRAVITATIONAL_CONSTANT * mass * displacement * inverse.powi(3),
                    -GRAVITATIONAL_CONSTANT * mass * inverse,
                )
            }
            MassDistribution::UniformSphere { radius } if distance < radius => (
                -GRAVITATIONAL_CONSTANT * mass * displacement / radius.powi(3),
                -GRAVITATIONAL_CONSTANT * mass / (2.0 * radius)
                    * (3.0 - distance_squared / radius.powi(2)),
            ),
            MassDistribution::UniformSphere { .. } => {
                let inverse = distance.recip();
                (
                    -GRAVITATIONAL_CONSTANT * mass * displacement * inverse.powi(3),
                    -GRAVITATIONAL_CONSTANT * mass * inverse,
                )
            }
        };
        acceleration += field;
        potential += phi;
        if !acceleration.is_finite() || !potential.is_finite() {
            return NewtonianSample::undefined(UndefinedReason::NumericalOverflow);
        }
    }
    NewtonianSample {
        acceleration,
        potential,
        validity: SampleValidity::Exact,
    }
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
