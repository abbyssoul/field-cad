//! Host-owned `wgpu` backend for the electrostatics plugin.
//!
//! The evaluator dispatches a whole probe, plane, or grid geometry at once and
//! returns ordinary CPU snapshot columns. The readback is synchronous today,
//! but only occurs when an analytic result is invalidated (world/subscription
//! edit), never once per rendered frame. A later compute service can own the
//! same kernel without changing visualization consumers.

use std::{sync::mpsc, time::Duration};

use bytemuck::{Pod, Zeroable};
use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    ChargeDistribution, Precision, SampleGeometry, SampleValidity, UndefinedReason,
};
use fieldcad_electromagnetic_sources::ChargeSource;
use fieldcad_electrostatics::{ElectrostaticBatchEvaluator, ElectrostaticSample};
use glam::DVec3;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("electrostatics.wgsl");
const WORKGROUP_SIZE: u32 = 64;

/// Agreement required between the f32 GPU backend and the f64 CPU oracle.
/// The absolute term protects expected zeroes; the relative term covers normal
/// f32 rounding and operation-order differences in superposed fields.
#[cfg(test)]
const GPU_RELATIVE_TOLERANCE: f64 = 5.0e-4;
#[cfg(test)]
const GPU_ABSOLUTE_TOLERANCE: f64 = 2.0e-3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuParams {
    counts: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuSource {
    position_charge: [f32; 4],
    distribution_radius: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuPosition {
    position: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuOutput {
    field_potential: [f32; 4],
    validity: [u32; 4],
}

pub(crate) struct GpuElectrostaticEvaluator {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
}

impl GpuElectrostaticEvaluator {
    pub(crate) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("electrostatics compute shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("electrostatics compute pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("evaluate"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            device,
            queue,
            pipeline,
        }
    }

    fn evaluate_inner(
        &self,
        sources: &[ChargeSource],
        _domain: &fieldcad_core::Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<ElectrostaticSample>, String> {
        if geometry.is_empty() {
            return Ok(Vec::new());
        }
        let sample_count = u32::try_from(geometry.len())
            .map_err(|_| "electrostatics sample batch exceeds u32 indexing".to_owned())?;
        let source_count = u32::try_from(sources.len())
            .map_err(|_| "electrostatics source batch exceeds u32 indexing".to_owned())?;

        let mut gpu_sources = sources
            .iter()
            .map(gpu_source)
            .collect::<Result<Vec<_>, _>>()?;
        // WebGPU disallows a zero-sized storage binding. The shader observes the
        // real count and never reads this placeholder.
        if gpu_sources.is_empty() {
            gpu_sources.push(GpuSource::zeroed());
        }
        let positions = geometry
            .positions()
            .map(|position| {
                Ok(GpuPosition {
                    position: vec4_f32(position, "sample position")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let params = GpuParams {
            counts: [source_count, sample_count, 0, 0],
        };

        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("electrostatics parameters"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let source_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("electrostatics sources"),
                contents: bytemuck::cast_slice(&gpu_sources),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let position_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("electrostatics sample positions"),
                contents: bytemuck::cast_slice(&positions),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let output_size = (geometry.len() * size_of::<GpuOutput>()) as wgpu::BufferAddress;
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("electrostatics sample outputs"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("electrostatics readback"),
            size: output_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("electrostatics compute bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: source_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: position_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: output_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("electrostatics compute encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("electrostatics batched evaluation"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(sample_count.div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging_buffer, 0, output_size);
        let submission = self.queue.submit([encoder.finish()]);

        let (sender, receiver) = mpsc::sync_channel(1);
        staging_buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(5)),
            })
            .map_err(|error| format!("electrostatics GPU wait failed: {error}"))?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| "electrostatics GPU readback callback did not arrive".to_owned())?
            .map_err(|error| format!("electrostatics GPU readback failed: {error}"))?;

        let mapped = staging_buffer.get_mapped_range(..);
        let raw: &[GpuOutput] = bytemuck::cast_slice(&mapped);
        let evaluated = raw
            .iter()
            .map(|sample| ElectrostaticSample {
                electric_field: DVec3::new(
                    f64::from(sample.field_potential[0]),
                    f64::from(sample.field_potential[1]),
                    f64::from(sample.field_potential[2]),
                ),
                potential: f64::from(sample.field_potential[3]),
                // The compute shader does not (yet) output a Jacobian —
                // consumers fall back to today's plain trilinear
                // reconstruction for batches this evaluator produces.
                gradient: None,
                validity: validity(sample.validity[0]),
            })
            .collect();
        drop(mapped);
        staging_buffer.unmap();
        Ok(evaluated)
    }
}

impl ElectrostaticBatchEvaluator for GpuElectrostaticEvaluator {
    fn precision(&self) -> Precision {
        Precision::F32
    }

    fn evaluate(
        &self,
        sources: &[ChargeSource],
        domain: &fieldcad_core::Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<ElectrostaticSample>, String> {
        self.evaluate_inner(sources, domain, geometry)
    }
}

fn gpu_source(source: &ChargeSource) -> Result<GpuSource, String> {
    let (kind, radius) = match source.distribution {
        ChargeDistribution::Point { exclusion_radius } => (0.0, exclusion_radius),
        ChargeDistribution::UniformSphere { radius } => (1.0, radius),
    };
    Ok(GpuSource {
        position_charge: [
            finite_f32(source.position.x, "source x")?,
            finite_f32(source.position.y, "source y")?,
            finite_f32(source.position.z, "source z")?,
            finite_f32(source.coupling_value.into_si(), "source charge")?,
        ],
        distribution_radius: [kind, finite_f32(radius, "source radius")?, 0.0, 0.0],
    })
}

fn vec4_f32(vector: DVec3, label: &str) -> Result<[f32; 4], String> {
    Ok([
        finite_f32(vector.x, label)?,
        finite_f32(vector.y, label)?,
        finite_f32(vector.z, label)?,
        0.0,
    ])
}

fn finite_f32(value: f64, label: &str) -> Result<f32, String> {
    let converted = value as f32;
    if converted.is_finite() {
        Ok(converted)
    } else {
        Err(format!("{label} cannot be represented as f32"))
    }
}

fn validity(code: u32) -> SampleValidity {
    match code {
        0 => SampleValidity::Exact,
        1 => SampleValidity::Undefined(UndefinedReason::InsideSourceRadius),
        2 => SampleValidity::Undefined(UndefinedReason::OutsideDomain),
        _ => SampleValidity::Undefined(UndefinedReason::NumericalOverflow),
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::quantities::ChargeCoulombs;
    use fieldcad_core::{
        BoundaryConditions, Domain, DomainBounds, GridLattice, PlaneLattice, Resolution,
        SampleGeometry,
    };
    use fieldcad_electrostatics::evaluate_sources;
    use glam::{UVec2, UVec3};

    use super::*;

    #[test]
    fn compute_shader_compiles_and_declares_its_entry_point() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("WGSL must parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("WGSL must validate");
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == "evaluate")
        );
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
}
