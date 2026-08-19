//! Shared `wgpu` adapter for a batched pairwise inverse-square-law
//! evaluator.
//!
//! Coulomb's law and Newton's law of gravitation are the same functional
//! form with a different coupling constant and, for gravity, an opposite
//! sign — see `fieldcad-superposition`'s module doc. [`GpuInverseSquareEvaluator`]
//! implements that crate's `InverseSquareBatchEvaluator` directly and is
//! injected into both `plugins/electrostatics` and `plugins/gravitostatics` with
//! their own coupling constant; there is no per-equation-system wrapper
//! left to keep in step, since source conversion and the coupling constant
//! are the plugins' own job now, not this adapter's.

use std::{
    sync::{Mutex, PoisonError, mpsc},
    time::Duration,
};

use bytemuck::{Pod, Zeroable};
use fieldcad_core::{
    ChargeDistribution, Domain, Precision, SampleGeometry, SampleValidity, UndefinedReason,
};
use fieldcad_superposition::{
    InverseSquareBatchEvaluator, InverseSquareSample, InverseSquareSource,
};
use glam::{DMat3, DVec3};

const SHADER: &str = include_str!("inverse_square.wgsl");
const WORKGROUP_SIZE: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuParams {
    counts: [u32; 4],
    coupling: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuSource {
    position_value: [f32; 4],
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
    // The field's own Jacobian, one column per lane — matches
    // `inverse_square.wgsl`'s `SampleOutput.gradient_col{0,1,2}` layout, each
    // padded to 16 bytes for `mat3x3<f32>`'s column alignment.
    gradient_col0: [f32; 4],
    gradient_col1: [f32; 4],
    gradient_col2: [f32; 4],
}

/// Buffers reused across `evaluate` calls instead of being created fresh
/// every dispatch. Each dynamic buffer tracks the element capacity it was
/// last created with; a call only recreates a buffer when its request
/// exceeds that capacity, and otherwise just uploads new contents with
/// `queue.write_buffer`. `params_buffer` never needs to grow (its size is
/// fixed by `GpuParams`), so it carries no capacity field.
struct GpuScratchBuffers {
    params_buffer: wgpu::Buffer,
    source_capacity: usize,
    source_buffer: wgpu::Buffer,
    position_capacity: usize,
    position_buffer: wgpu::Buffer,
    output_capacity: usize,
    output_buffer: wgpu::Buffer,
    staging_capacity: usize,
    staging_buffer: wgpu::Buffer,
    /// The bind group over `params_buffer`/`source_buffer`/`position_buffer`/
    /// `output_buffer`, cached across `evaluate` calls instead of being
    /// recreated every dispatch. `None` whenever one of those four buffers
    /// was just (re)created by `ensure_capacity` and needs a bind group
    /// pointing at the new one; `staging_buffer` is not bound to the shader,
    /// so resizing it alone does not invalidate this.
    bind_group: Option<wgpu::BindGroup>,
}

impl GpuScratchBuffers {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            params_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("inverse-square parameters"),
                size: size_of::<GpuParams>() as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            source_capacity: 0,
            source_buffer: placeholder_buffer(
                device,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            position_capacity: 0,
            position_buffer: placeholder_buffer(
                device,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            output_capacity: 0,
            output_buffer: placeholder_buffer(
                device,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            ),
            staging_capacity: 0,
            staging_buffer: placeholder_buffer(
                device,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            bind_group: None,
        }
    }
}

/// A zero-sized buffer, standing in until the first real `ensure_capacity`
/// call grows it — never bound as a GPU resource at this size.
fn placeholder_buffer(device: &wgpu::Device, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("inverse-square scratch placeholder"),
        size: 0,
        usage,
        mapped_at_creation: false,
    })
}

/// Grows `buffer` to hold at least `needed` elements, only when it doesn't
/// already. A no-op recreate-free path is the common case once buffers have
/// warmed up to the scene's usual source/sample counts. Returns whether it
/// actually recreated `buffer` — a caller whose buffer feeds a cached bind
/// group needs to know when that binding is now stale.
fn ensure_capacity(
    device: &wgpu::Device,
    buffer: &mut wgpu::Buffer,
    capacity: &mut usize,
    needed: usize,
    element_size: usize,
    usage: wgpu::BufferUsages,
    label: &'static str,
) -> bool {
    if needed <= *capacity {
        return false;
    }
    *buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (needed * element_size) as wgpu::BufferAddress,
        usage,
        mapped_at_creation: false,
    });
    *capacity = needed;
    true
}

/// Batched pairwise inverse-square-law evaluator, shared by electrostatics
/// and Newtonian gravity — see this module's doc comment.
pub(crate) struct GpuInverseSquareEvaluator {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    scratch: Mutex<GpuScratchBuffers>,
}

impl GpuInverseSquareEvaluator {
    pub(crate) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("inverse-square compute shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("inverse-square compute pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("evaluate"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let scratch = Mutex::new(GpuScratchBuffers::new(&device));
        Self {
            device,
            queue,
            pipeline,
            scratch,
        }
    }
}

impl InverseSquareBatchEvaluator for GpuInverseSquareEvaluator {
    fn precision(&self) -> Precision {
        Precision::F32
    }

    /// Evaluate one batch of sample positions against `sources`, superposed
    /// under `coupling_constant` (Coulomb's constant for electrostatics,
    /// `-G` for gravity), writing GPU readback fields directly into
    /// [`InverseSquareSample`] — no intermediate raw-sample `Vec` to map
    /// over afterward, since nothing else needs the GPU's own struct
    /// layout once the bytes are off the wire.
    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String> {
        if geometry.is_empty() {
            return Ok(Vec::new());
        }
        let sample_count = u32::try_from(geometry.len())
            .map_err(|_| "inverse-square sample batch exceeds u32 indexing".to_owned())?;
        let source_count = u32::try_from(sources.len())
            .map_err(|_| "inverse-square source batch exceeds u32 indexing".to_owned())?;

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
            coupling: [coupling_constant as f32, 0.0, 0.0, 0.0],
        };

        // Dereferenced once up front: taking `&mut guard.field_a` and
        // `&mut guard.field_b` separately doesn't borrow-check through a
        // `MutexGuard`'s `Deref` the way plain disjoint field borrows do, so
        // `scratch` here is a plain `&mut GpuScratchBuffers` instead.
        let mut guard = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        let scratch = &mut *guard;

        self.queue
            .write_buffer(&scratch.params_buffer, 0, bytemuck::bytes_of(&params));

        let source_resized = ensure_capacity(
            &self.device,
            &mut scratch.source_buffer,
            &mut scratch.source_capacity,
            gpu_sources.len(),
            size_of::<GpuSource>(),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            "inverse-square sources",
        );
        self.queue.write_buffer(
            &scratch.source_buffer,
            0,
            bytemuck::cast_slice(&gpu_sources),
        );

        let position_resized = ensure_capacity(
            &self.device,
            &mut scratch.position_buffer,
            &mut scratch.position_capacity,
            positions.len(),
            size_of::<GpuPosition>(),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            "inverse-square sample positions",
        );
        self.queue.write_buffer(
            &scratch.position_buffer,
            0,
            bytemuck::cast_slice(&positions),
        );

        let output_size = (geometry.len() * size_of::<GpuOutput>()) as wgpu::BufferAddress;
        let output_resized = ensure_capacity(
            &self.device,
            &mut scratch.output_buffer,
            &mut scratch.output_capacity,
            geometry.len(),
            size_of::<GpuOutput>(),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            "inverse-square sample outputs",
        );
        ensure_capacity(
            &self.device,
            &mut scratch.staging_buffer,
            &mut scratch.staging_capacity,
            geometry.len(),
            size_of::<GpuOutput>(),
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            "inverse-square readback",
        );

        if source_resized || position_resized || output_resized {
            scratch.bind_group = None;
        }
        if scratch.bind_group.is_none() {
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("inverse-square compute bindings"),
                layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: scratch.params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: scratch.source_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: scratch.position_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: scratch.output_buffer.as_entire_binding(),
                    },
                ],
            });
            scratch.bind_group = Some(bind_group);
        }
        let bind_group = scratch
            .bind_group
            .as_ref()
            .expect("just ensured Some above");

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("inverse-square compute encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("inverse-square batched evaluation"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(sample_count.div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &scratch.output_buffer,
            0,
            &scratch.staging_buffer,
            0,
            output_size,
        );
        let submission = self.queue.submit([encoder.finish()]);

        let (sender, receiver) = mpsc::sync_channel(1);
        // Explicit `0..output_size` rather than `..`: the staging buffer may
        // be larger than this call's output (it never shrinks once grown),
        // so mapping the whole buffer would read stale bytes past what this
        // dispatch actually wrote.
        scratch
            .staging_buffer
            .map_async(wgpu::MapMode::Read, 0..output_size, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(5)),
            })
            .map_err(|error| format!("inverse-square GPU wait failed: {error}"))?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| "inverse-square GPU readback callback did not arrive".to_owned())?
            .map_err(|error| format!("inverse-square GPU readback failed: {error}"))?;

        let mapped = scratch.staging_buffer.get_mapped_range(0..output_size);
        let raw: &[GpuOutput] = bytemuck::cast_slice(&mapped);
        let evaluated = raw
            .iter()
            .map(|sample| InverseSquareSample {
                field: DVec3::new(
                    f64::from(sample.field_potential[0]),
                    f64::from(sample.field_potential[1]),
                    f64::from(sample.field_potential[2]),
                ),
                potential: f64::from(sample.field_potential[3]),
                gradient: Some(DMat3::from_cols(
                    gradient_column(sample.gradient_col0),
                    gradient_column(sample.gradient_col1),
                    gradient_column(sample.gradient_col2),
                )),
                validity: validity(sample.validity[0]),
            })
            .collect();
        drop(mapped);
        scratch.staging_buffer.unmap();
        Ok(evaluated)
    }
}

fn gpu_source(source: &InverseSquareSource) -> Result<GpuSource, String> {
    let (kind, radius) = match source.distribution {
        ChargeDistribution::Point { exclusion_radius } => (0.0, exclusion_radius),
        ChargeDistribution::UniformSphere { radius } => (1.0, radius),
    };
    Ok(GpuSource {
        position_value: [
            finite_f32(source.position.x, "source x")?,
            finite_f32(source.position.y, "source y")?,
            finite_f32(source.position.z, "source z")?,
            finite_f32(source.strength, "source value")?,
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

/// One `mat3x3<f32>` column read back from the GPU (padded to 4 lanes on
/// the shader side) widened to `f64`, dropping the padding lane.
fn gradient_column(column: [f32; 4]) -> DVec3 {
    DVec3::new(
        f64::from(column[0]),
        f64::from(column[1]),
        f64::from(column[2]),
    )
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
    use fieldcad_core::quantities::{ChargeCoulombs, MassKg, SiScalar, kilogram};
    use fieldcad_core::{
        BoundaryConditions, ChargeDistribution, DomainBounds, GridLattice, ObjectId, PlaneLattice,
        ProbeId, Resolution, SampleGeometry, Velocity,
    };
    use glam::{DVec3, UVec2, UVec3};

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

    /// Agreement required between the f32 GPU backend and the f64 CPU
    /// oracle. The absolute term protects expected zeroes; the relative
    /// term covers normal f32 rounding and operation-order differences in
    /// superposed fields.
    const GPU_RELATIVE_TOLERANCE: f64 = 5.0e-4;
    const GPU_ABSOLUTE_TOLERANCE: f64 = 2.0e-3;

    fn close(actual: f64, expected: f64, index: usize) {
        let tolerance =
            GPU_ABSOLUTE_TOLERANCE + GPU_RELATIVE_TOLERANCE * actual.abs().max(expected.abs());
        assert!(
            (actual - expected).abs() <= tolerance,
            "GPU {actual:e} differs from CPU {expected:e} at sample {index}; tolerance {tolerance:e}"
        );
    }

    /// A headless GPU adapter, or `None` (with a message on stderr) when
    /// this environment has no usable GPU — every GPU test below skips
    /// gracefully rather than failing in that case.
    async fn headless_device(label: &'static str) -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = crate::gpu::GpuConfig::from_env().instance();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok()?;
        Some(
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some(label),
                    ..Default::default()
                })
                .await
                .expect("adapter must provide a default device"),
        )
    }

    fn test_geometries() -> [SampleGeometry; 2] {
        [
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
        ]
    }

    #[test]
    fn electrostatics_plane_and_grid_samples_match_the_f64_oracle() {
        pollster::block_on(async {
            let Some((device, queue)) = headless_device("electrostatics parity test device").await
            else {
                eprintln!("skipping GPU parity test: no headless adapter");
                return;
            };
            let evaluator = GpuInverseSquareEvaluator::new(device, queue);
            let domain = Domain::new(
                DomainBounds::centred_cube(3.0).unwrap(),
                Resolution::uniform(8).unwrap(),
                BoundaryConditions::default(),
                Precision::F32,
            );
            let sources = [
                fieldcad_electrostatics::ChargeSource::new(
                    ObjectId::new(0),
                    DVec3::new(-0.3, 0.0, 0.0),
                    Velocity::default(),
                    ChargeCoulombs::from_si(1.2e-9),
                    ChargeDistribution::Point {
                        exclusion_radius: 0.08,
                    },
                ),
                fieldcad_electrostatics::ChargeSource::new(
                    ObjectId::new(1),
                    DVec3::new(0.5, -0.2, 0.3),
                    Velocity::default(),
                    ChargeCoulombs::from_si(-0.7e-9),
                    ChargeDistribution::Point {
                        exclusion_radius: 0.05,
                    },
                ),
                fieldcad_electrostatics::ChargeSource::new(
                    ObjectId::new(2),
                    DVec3::new(0.2, 0.7, -0.4),
                    Velocity::default(),
                    ChargeCoulombs::from_si(0.4e-9),
                    ChargeDistribution::UniformSphere { radius: 0.35 },
                ),
            ];
            let inverse_square_sources: Vec<_> = sources
                .iter()
                .map(fieldcad_electrostatics::inverse_square_source)
                .collect();

            for geometry in test_geometries() {
                let gpu = InverseSquareBatchEvaluator::evaluate(
                    &evaluator,
                    fieldcad_electrostatics::COULOMB_CONSTANT,
                    &inverse_square_sources,
                    &domain,
                    &geometry,
                )
                .unwrap();
                assert_eq!(gpu.len(), geometry.len());
                for (index, (gpu, position)) in gpu.iter().zip(geometry.positions()).enumerate() {
                    let cpu = fieldcad_electrostatics::evaluate_sources(&sources, position);
                    assert_eq!(gpu.validity, cpu.validity, "validity at sample {index}");
                    if cpu.validity.is_usable() {
                        for (actual, expected) in
                            gpu.field.to_array().into_iter().zip(cpu.field.to_array())
                        {
                            close(actual, expected, index);
                        }
                        close(gpu.potential, cpu.potential, index);
                        let gpu_gradient =
                            gpu.gradient.expect("GPU evaluator now reports a gradient");
                        let cpu_gradient = cpu
                            .gradient
                            .expect("CPU evaluator always reports a gradient");
                        for (actual, expected) in gpu_gradient
                            .to_cols_array()
                            .into_iter()
                            .zip(cpu_gradient.to_cols_array())
                        {
                            close(actual, expected, index);
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn gravity_plane_and_grid_samples_match_the_f64_oracle() {
        pollster::block_on(async {
            let Some((device, queue)) = headless_device("gravity parity test device").await else {
                eprintln!("skipping GPU parity test: no headless adapter");
                return;
            };
            let evaluator = GpuInverseSquareEvaluator::new(device, queue);
            let domain = Domain::new(
                DomainBounds::centred_cube(3.0).unwrap(),
                Resolution::uniform(8).unwrap(),
                BoundaryConditions::default(),
                Precision::F32,
            );
            let source = |object: u64, position: DVec3, mass_kg: f64| {
                fieldcad_core::CoupledSource::new(
                    ObjectId::new(object),
                    position,
                    Velocity::default(),
                    MassKg::new::<kilogram>(mass_kg),
                    ChargeDistribution::Point {
                        exclusion_radius: 0.08,
                    },
                )
            };
            let sources = [
                source(0, DVec3::new(-0.3, 0.0, 0.0), 5.0e18),
                source(1, DVec3::new(0.5, -0.2, 0.3), 3.0e18),
                source(2, DVec3::new(0.2, 0.7, -0.4), 4.0e18),
            ];
            let inverse_square_sources: Vec<_> = sources
                .iter()
                .map(fieldcad_gravitostatics::inverse_square_source)
                .collect();

            for geometry in test_geometries() {
                let gpu = InverseSquareBatchEvaluator::evaluate(
                    &evaluator,
                    -fieldcad_gravitostatics::GRAVITATIONAL_CONSTANT,
                    &inverse_square_sources,
                    &domain,
                    &geometry,
                )
                .unwrap();
                assert_eq!(gpu.len(), geometry.len());
                for (index, (gpu, position)) in gpu.iter().zip(geometry.positions()).enumerate() {
                    let cpu = fieldcad_gravitostatics::evaluate_sources(&sources, position);
                    assert_eq!(gpu.validity, cpu.validity, "validity at sample {index}");
                    if cpu.validity.is_usable() {
                        for (actual, expected) in
                            gpu.field.to_array().into_iter().zip(cpu.field.to_array())
                        {
                            close(actual, expected, index);
                        }
                        close(gpu.potential, cpu.potential, index);
                        let gpu_gradient =
                            gpu.gradient.expect("GPU evaluator now reports a gradient");
                        let cpu_gradient = cpu
                            .gradient
                            .expect("CPU evaluator always reports a gradient");
                        for (actual, expected) in gpu_gradient
                            .to_cols_array()
                            .into_iter()
                            .zip(cpu_gradient.to_cols_array())
                        {
                            close(actual, expected, index);
                        }
                    }
                }
            }
        });
    }

    /// The scratch buffers this evaluator reuses across calls never shrink
    /// — a later call with fewer samples than a previous, larger one reuses
    /// an over-sized staging buffer. This is the code path
    /// `GpuInverseSquareEvaluator::evaluate`'s buffer-reuse logic that no
    /// call was ever independent enough to exercise before: mapping the
    /// wrong byte range after a shrink would silently serve bytes a larger,
    /// earlier call wrote, rather than this call's own output.
    #[test]
    fn evaluate_reuses_buffers_across_growing_and_shrinking_calls() {
        pollster::block_on(async {
            let Some((device, queue)) = headless_device("buffer-reuse test device").await else {
                eprintln!("skipping GPU buffer-reuse test: no headless adapter");
                return;
            };
            let evaluator = GpuInverseSquareEvaluator::new(device, queue);
            let domain = Domain::new(
                DomainBounds::centred_cube(3.0).unwrap(),
                Resolution::uniform(8).unwrap(),
                BoundaryConditions::default(),
                Precision::F32,
            );
            let sources = [fieldcad_electrostatics::ChargeSource::new(
                ObjectId::new(0),
                DVec3::new(-0.3, 0.1, 0.2),
                Velocity::default(),
                ChargeCoulombs::from_si(1.2e-9),
                ChargeDistribution::Point {
                    exclusion_radius: 0.08,
                },
            )];
            let inverse_square_sources: Vec<_> = sources
                .iter()
                .map(fieldcad_electrostatics::inverse_square_source)
                .collect();

            let probes_at = |positions: &[DVec3]| {
                SampleGeometry::probes(
                    (0..positions.len())
                        .map(|index| ProbeId::new(index as u64))
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
                let gpu = InverseSquareBatchEvaluator::evaluate(
                    &evaluator,
                    fieldcad_electrostatics::COULOMB_CONSTANT,
                    &inverse_square_sources,
                    &domain,
                    &geometry,
                )
                .unwrap();
                assert_eq!(gpu.len(), positions.len());
                for (index, (gpu, position)) in gpu.iter().zip(positions.iter()).enumerate() {
                    let cpu = fieldcad_electrostatics::evaluate_sources(&sources, *position);
                    assert_eq!(gpu.validity, cpu.validity, "validity at sample {index}");
                    if cpu.validity.is_usable() {
                        for (actual, expected) in
                            gpu.field.to_array().into_iter().zip(cpu.field.to_array())
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
