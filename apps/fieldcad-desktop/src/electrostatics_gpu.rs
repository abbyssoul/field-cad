//! Host-owned `wgpu` backend for the electrostatics plugin.
//!
//! A thin adapter over the shared [`crate::gpu_inverse_square`] core: maps
//! `ChargeSource` to and from the coupling-value-agnostic
//! `InverseSquareSource`/`GpuInverseSquareSample` shape that core operates
//! on, and supplies Coulomb's constant. The evaluator dispatches a whole
//! probe, plane, or grid geometry at once and returns ordinary CPU snapshot
//! columns. The readback is synchronous today, but only occurs when an
//! analytic result is invalidated (world/subscription edit), never once per
//! rendered frame. A later compute service can own the same kernel without
//! changing visualization consumers.

use fieldcad_core::{Domain, Precision, SampleGeometry};
use fieldcad_electromagnetic_sources::ChargeSource;
use fieldcad_electrostatics::{
    ElectrostaticBatchEvaluator, ElectrostaticSample, inverse_square_source,
};

use crate::gpu_inverse_square::GpuInverseSquareEvaluator;

/// Agreement required between the f32 GPU backend and the f64 CPU oracle.
/// The absolute term protects expected zeroes; the relative term covers normal
/// f32 rounding and operation-order differences in superposed fields.
#[cfg(test)]
const GPU_RELATIVE_TOLERANCE: f64 = 5.0e-4;
#[cfg(test)]
const GPU_ABSOLUTE_TOLERANCE: f64 = 2.0e-3;

pub(crate) struct GpuElectrostaticEvaluator {
    core: GpuInverseSquareEvaluator,
}

impl GpuElectrostaticEvaluator {
    pub(crate) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            core: GpuInverseSquareEvaluator::new(device, queue),
        }
    }
}

impl ElectrostaticBatchEvaluator for GpuElectrostaticEvaluator {
    fn precision(&self) -> Precision {
        Precision::F32
    }

    fn evaluate(
        &self,
        sources: &[ChargeSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<ElectrostaticSample>, String> {
        let sources: Vec<_> = sources.iter().map(inverse_square_source).collect();
        let samples = self.core.evaluate(
            fieldcad_electrostatics::COULOMB_CONSTANT,
            &sources,
            geometry,
        )?;
        Ok(samples
            .into_iter()
            .map(|sample| ElectrostaticSample {
                electric_field: sample.field,
                potential: sample.potential,
                // The compute shader does not (yet) output a Jacobian —
                // consumers fall back to today's plain trilinear
                // reconstruction for batches this evaluator produces.
                gradient: None,
                validity: sample.validity,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::quantities::{ChargeCoulombs, SiScalar};
    use fieldcad_core::{
        BoundaryConditions, ChargeDistribution, DomainBounds, GridLattice, PlaneLattice,
        Resolution, SampleGeometry,
    };
    use fieldcad_electrostatics::evaluate_sources;
    use glam::{DVec3, UVec2, UVec3};

    use super::*;

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
                    label: Some("electrostatics parity test device"),
                    ..Default::default()
                })
                .await
                .expect("adapter must provide a default device");
            let evaluator = GpuElectrostaticEvaluator::new(device, queue);
            let domain = Domain::new(
                DomainBounds::centred_cube(3.0).unwrap(),
                Resolution::uniform(8).unwrap(),
                BoundaryConditions::default(),
                Precision::F32,
            );
            let sources = [
                ChargeSource::new(
                    fieldcad_core::ObjectId::new(0),
                    DVec3::new(-0.3, 0.0, 0.0),
                    fieldcad_core::Velocity::default(),
                    ChargeCoulombs::from_si(1.2e-9),
                    ChargeDistribution::Point {
                        exclusion_radius: 0.08,
                    },
                ),
                ChargeSource::new(
                    fieldcad_core::ObjectId::new(1),
                    DVec3::new(0.5, -0.2, 0.3),
                    fieldcad_core::Velocity::default(),
                    ChargeCoulombs::from_si(-0.7e-9),
                    ChargeDistribution::Point {
                        exclusion_radius: 0.05,
                    },
                ),
                ChargeSource::new(
                    fieldcad_core::ObjectId::new(2),
                    DVec3::new(0.2, 0.7, -0.4),
                    fieldcad_core::Velocity::default(),
                    ChargeCoulombs::from_si(0.4e-9),
                    ChargeDistribution::UniformSphere { radius: 0.35 },
                ),
            ];
            let geometries = [
                SampleGeometry::Plane {
                    plane: fieldcad_core::PlaneId::new(0),
                    lattice: PlaneLattice::new(
                        // Deliberately crosses the ±3 m numerical grid domain:
                        // an analytic Coulomb field can evaluate the complete
                        // visualization plane rather than clipping its edges.
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
                            .electric_field
                            .to_array()
                            .into_iter()
                            .zip(cpu.electric_field.to_array())
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

    /// The scratch buffers this evaluator now reuses across calls never
    /// shrink — a later call with fewer samples than a previous, larger one
    /// reuses an over-sized staging buffer. This is the code path
    /// `GpuInverseSquareEvaluator::evaluate`'s buffer-reuse logic that no
    /// call was ever independent enough to exercise before: mapping the
    /// wrong byte range after a shrink would silently serve bytes a larger,
    /// earlier call wrote, rather than this call's own output.
    #[test]
    fn evaluate_reuses_buffers_across_growing_and_shrinking_calls() {
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
                eprintln!("skipping GPU buffer-reuse test: no headless adapter");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("electrostatics buffer-reuse test device"),
                    ..Default::default()
                })
                .await
                .expect("adapter must provide a default device");
            let evaluator = GpuElectrostaticEvaluator::new(device, queue);
            let domain = Domain::new(
                DomainBounds::centred_cube(3.0).unwrap(),
                Resolution::uniform(8).unwrap(),
                BoundaryConditions::default(),
                Precision::F32,
            );
            let sources = [ChargeSource::new(
                fieldcad_core::ObjectId::new(0),
                DVec3::new(-0.3, 0.1, 0.2),
                fieldcad_core::Velocity::default(),
                ChargeCoulombs::from_si(1.2e-9),
                ChargeDistribution::Point {
                    exclusion_radius: 0.08,
                },
            )];

            let probes_at = |positions: &[DVec3]| {
                SampleGeometry::probes(
                    (0..positions.len())
                        .map(|index| fieldcad_core::ProbeId::new(index as u64))
                        .collect(),
                    positions.to_vec(),
                )
                .unwrap()
            };

            // Small, then large (grows every buffer), then small again
            // (reuses the now-larger buffers) — the shrink step is what
            // would previously have read stale bytes from the large call.
            let small: Vec<DVec3> = vec![DVec3::new(0.4, -0.2, 0.1), DVec3::new(-0.6, 0.3, -0.1)];
            let large: Vec<DVec3> = (0..24)
                .map(|index| {
                    let t = index as f64 * 0.37;
                    DVec3::new(t.sin(), t.cos(), 0.1 * t)
                })
                .collect();

            for positions in [&small, &large, &small] {
                let geometry = probes_at(positions);
                let gpu = evaluator.evaluate(&sources, &domain, &geometry).unwrap();
                assert_eq!(gpu.len(), positions.len());
                for (index, (gpu, position)) in gpu.iter().zip(positions.iter()).enumerate() {
                    let cpu = evaluate_sources(&sources, *position);
                    assert_eq!(gpu.validity, cpu.validity, "validity at sample {index}");
                    if cpu.validity.is_usable() {
                        for (actual, expected) in gpu
                            .electric_field
                            .to_array()
                            .into_iter()
                            .zip(cpu.electric_field.to_array())
                        {
                            close(actual, expected, index);
                        }
                        close(gpu.potential, cpu.potential, index);
                    }
                }
            }
        });
    }
}
