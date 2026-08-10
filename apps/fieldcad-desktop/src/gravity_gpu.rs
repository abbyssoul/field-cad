//! Host-owned `wgpu` backend for the Newtonian gravity plugin.
//!
//! A thin adapter over the shared [`crate::gpu_inverse_square`] core — see
//! `electrostatics_gpu.rs`, which this mirrors exactly: maps
//! `CoupledSource<MassKg>` to and from the coupling-value-agnostic
//! `InverseSquareSource`/`GpuInverseSquareSample` shape that core operates
//! on, and supplies Newton's gravitational constant with the sign flipped
//! for attraction.

use fieldcad_core::quantities::{MassKg, SiScalar};
use fieldcad_core::{CoupledSource, Domain, Precision, SampleGeometry};
use fieldcad_gravity::GravityBatchEvaluator;
use fieldcad_newtonian_gravity::{GRAVITATIONAL_CONSTANT, NewtonianSample};
use fieldcad_superposition::InverseSquareSource;

use crate::gpu_inverse_square::GpuInverseSquareEvaluator;

/// Agreement required between the f32 GPU backend and the f64 CPU oracle —
/// see `electrostatics_gpu.rs`'s identical constants for the reasoning.
#[cfg(test)]
const GPU_RELATIVE_TOLERANCE: f64 = 5.0e-4;
#[cfg(test)]
const GPU_ABSOLUTE_TOLERANCE: f64 = 2.0e-3;

pub(crate) struct GpuNewtonianGravityEvaluator {
    core: GpuInverseSquareEvaluator,
}

impl GpuNewtonianGravityEvaluator {
    pub(crate) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            core: GpuInverseSquareEvaluator::new(device, queue),
        }
    }
}

impl GravityBatchEvaluator for GpuNewtonianGravityEvaluator {
    fn precision(&self) -> Precision {
        Precision::F32
    }

    fn evaluate(
        &self,
        sources: &[CoupledSource<MassKg>],
        _domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<NewtonianSample>, String> {
        let sources: Vec<_> = sources.iter().map(inverse_square_source).collect();
        let samples = self
            .core
            .evaluate(-GRAVITATIONAL_CONSTANT, &sources, geometry)?;
        Ok(samples
            .into_iter()
            .map(|sample| NewtonianSample {
                acceleration: sample.field,
                potential: sample.potential,
                validity: sample.validity,
            })
            .collect())
    }
}

/// `CoupledSource<MassKg>` → the shared, coupling-value-agnostic source
/// shape. Keeps zero-mass sources in the list (strength `0.0`) rather than
/// filtering them out before dispatch — matches the compute shader, which
/// already skips a zero-value source per-invocation, and keeps GPU buffer
/// sizing independent of runtime mass values.
fn inverse_square_source(source: &CoupledSource<MassKg>) -> InverseSquareSource {
    InverseSquareSource {
        position: source.position,
        strength: source.coupling_value.into_si(),
        distribution: source.distribution,
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::quantities::kilogram;
    use fieldcad_core::{
        BoundaryConditions, ChargeDistribution, DomainBounds, GridLattice, PlaneLattice,
        Resolution, SampleGeometry, Velocity,
    };
    use fieldcad_newtonian_gravity::evaluate_sources;
    use glam::{DVec3, UVec2, UVec3};

    use super::*;

    fn source(object: u64, position: DVec3, mass_kg: f64) -> CoupledSource<MassKg> {
        CoupledSource::new(
            fieldcad_core::ObjectId::new(object),
            position,
            Velocity::default(),
            MassKg::new::<kilogram>(mass_kg),
            ChargeDistribution::Point {
                exclusion_radius: 0.08,
            },
        )
    }

    #[test]
    fn plane_and_grid_samples_match_the_f64_oracle() {
        pollster::block_on(async {
            let instance = crate::gpu::GpuConfig::from_env().instance();
            let Ok(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                })
                .await
            else {
                eprintln!("skipping GPU parity test: no headless adapter");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("gravity parity test device"),
                    ..Default::default()
                })
                .await
                .expect("adapter must provide a default device");
            let evaluator = GpuNewtonianGravityEvaluator::new(device, queue);
            let domain = Domain::new(
                DomainBounds::centred_cube(3.0).unwrap(),
                Resolution::uniform(8).unwrap(),
                BoundaryConditions::default(),
                Precision::F32,
            );
            let sources = [
                source(0, DVec3::new(-0.3, 0.0, 0.0), 5.0e18),
                source(1, DVec3::new(0.5, -0.2, 0.3), 3.0e18),
                source(2, DVec3::new(0.2, 0.7, -0.4), 4.0e18),
            ];
            let geometries = [
                SampleGeometry::Plane {
                    plane: fieldcad_core::PlaneId::new(0),
                    lattice: PlaneLattice::new(
                        DVec3::new(-3.5, -3.5, 0.0),
                        DVec3::new(1.75, 0.0, 0.0),
                        DVec3::new(0.0, 1.75, 0.0),
                        UVec2::new(5, 4),
                    ),
                },
                SampleGeometry::Grid(GridLattice::new(
                    DVec3::new(-1.1, -0.9, -0.7),
                    DVec3::new(0.55, 0.45, 0.4),
                    UVec3::new(4, 3, 3),
                )),
            ];

            for geometry in geometries {
                let gpu = evaluator.evaluate(&sources, &domain, &geometry).unwrap();
                assert_eq!(gpu.len(), geometry.len());
                for (index, (gpu, position)) in gpu.iter().zip(geometry.positions()).enumerate() {
                    let cpu = evaluate_sources(&sources, position);
                    assert_eq!(gpu.validity, cpu.validity, "validity at sample {index}");
                    if cpu.validity.is_usable() {
                        for (actual, expected) in gpu
                            .acceleration
                            .to_array()
                            .into_iter()
                            .zip(cpu.acceleration.to_array())
                        {
                            close(actual, expected, index);
                        }
                        close(gpu.potential, cpu.potential, index);
                    }
                }
            }
        });
    }

    fn close(actual: f64, expected: f64, index: usize) {
        let tolerance =
            GPU_ABSOLUTE_TOLERANCE + GPU_RELATIVE_TOLERANCE * actual.abs().max(expected.abs());
        assert!(
            (actual - expected).abs() <= tolerance,
            "GPU {actual:e} differs from CPU {expected:e} at sample {index}; tolerance {tolerance:e}"
        );
    }
}
