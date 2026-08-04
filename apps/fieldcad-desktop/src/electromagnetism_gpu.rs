//! Host-owned `wgpu` backend for the Maxwell equation-system plugin.
//!
//! Yee field state remains in storage buffers between ticks. A complete step is
//! submitted as magnetic half-step, electric full-step, magnetic half-step.
//! Readback occurs only when the runtime publishes an immutable snapshot and is
//! cached across all channels and sample geometries in that publication.

use std::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use bytemuck::{Pod, Zeroable};
use fieldcad_core::{
    DiagnosticSeverity, Domain, Precision, SampleGeometry, SolverDiagnostic, StepContext, TimeStep,
    WorldSnapshot,
};
use fieldcad_electromagnetism::{
    MaxwellCore, MaxwellSolverBackend, MaxwellSolverSetup, YeeFieldState, plugin_id,
    sample_yee_fields, yee_conservation,
};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemSolver, PluginError, SampledColumn, SolverCancellation,
    SolverKind, SolverStepOutcome,
};
use glam::DVec3;
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("electromagnetism.wgsl");
const WORKGROUP_SIZE: u32 = 64;
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuParams {
    counts: [u32; 4],
    spacing_dt: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuField {
    value: [f32; 4],
}

pub(crate) struct GpuMaxwellBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl GpuMaxwellBackend {
    pub(crate) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { device, queue }
    }
}

impl MaxwellSolverBackend for GpuMaxwellBackend {
    fn precision(&self) -> Precision {
        Precision::F32
    }

    fn create_solver(
        &self,
        setup: MaxwellSolverSetup,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        GpuMaxwellSolver::new(self.device.clone(), self.queue.clone(), setup)
            .map(|solver| Box::new(solver) as Box<dyn EquationSystemSolver>)
            .map_err(PluginError::Solver)
    }
}

struct StepBindings {
    magnetic_first: wgpu::BindGroup,
    electric: wgpu::BindGroup,
    magnetic_second: wgpu::BindGroup,
}

struct GpuMaxwellSolver {
    device: wgpu::Device,
    queue: wgpu::Queue,
    core: MaxwellCore,
    cell_count: u32,
    grid_bytes: wgpu::BufferAddress,
    electric: [wgpu::Buffer; 2],
    magnetic: [wgpu::Buffer; 2],
    current_density: wgpu::Buffer,
    current_electric: usize,
    half_step_params: wgpu::Buffer,
    full_step_params: wgpu::Buffer,
    magnetic_pipeline: wgpu::ComputePipeline,
    electric_pipeline: wgpu::ComputePipeline,
    bindings: [StepBindings; 2],
    staging: wgpu::Buffer,
    cached_state: Mutex<Option<Arc<YeeFieldState>>>,
    cancellation: SolverCancellation,
}

impl GpuMaxwellSolver {
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        setup: MaxwellSolverSetup,
    ) -> Result<Self, String> {
        let initial = &setup.initial_state;
        let electric_initial = gpu_fields(&initial.electric, "initial electric field")?;
        let magnetic_initial = gpu_fields(&initial.magnetic, "initial magnetic field")?;
        let zeros = vec![GpuField::zeroed(); electric_initial.len()];
        let cell_count = u32::try_from(electric_initial.len())
            .map_err(|_| "Maxwell grid exceeds u32 indexing".to_owned())?;
        let grid_bytes = (electric_initial.len() * size_of::<GpuField>()) as wgpu::BufferAddress;
        if grid_bytes == 0 {
            return Err("Maxwell grid cannot be empty".to_owned());
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Maxwell Yee compute shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let magnetic_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Maxwell magnetic update pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("advance_magnetic"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let electric_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Maxwell electric update pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("advance_electric"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let electric = [
            field_buffer(&device, "Maxwell electric A", &electric_initial),
            field_buffer(&device, "Maxwell electric B", &zeros),
        ];
        let magnetic = [
            field_buffer(&device, "Maxwell magnetic state", &magnetic_initial),
            field_buffer(&device, "Maxwell magnetic half-step", &zeros),
        ];
        let current_density = field_buffer(&device, "Maxwell current density", &zeros);
        let zero_params = gpu_params(setup.domain, 0.0)?;
        let half_step_params =
            uniform_buffer(&device, "Maxwell half-step parameters", &zero_params);
        let full_step_params =
            uniform_buffer(&device, "Maxwell full-step parameters", &zero_params);

        let magnetic_layout = magnetic_pipeline.get_bind_group_layout(0);
        let electric_layout = electric_pipeline.get_bind_group_layout(0);
        let bindings = [
            StepBindings {
                magnetic_first: field_bindings(
                    &device,
                    "Maxwell B half, E A",
                    &magnetic_layout,
                    &half_step_params,
                    [&electric[0], &magnetic[0], &magnetic[1]],
                    None,
                ),
                electric: field_bindings(
                    &device,
                    "Maxwell E full, A to B",
                    &electric_layout,
                    &full_step_params,
                    [&electric[0], &magnetic[1], &electric[1]],
                    Some(&current_density),
                ),
                magnetic_second: field_bindings(
                    &device,
                    "Maxwell B half, E B",
                    &magnetic_layout,
                    &half_step_params,
                    [&electric[1], &magnetic[1], &magnetic[0]],
                    None,
                ),
            },
            StepBindings {
                magnetic_first: field_bindings(
                    &device,
                    "Maxwell B half, E B",
                    &magnetic_layout,
                    &half_step_params,
                    [&electric[1], &magnetic[0], &magnetic[1]],
                    None,
                ),
                electric: field_bindings(
                    &device,
                    "Maxwell E full, B to A",
                    &electric_layout,
                    &full_step_params,
                    [&electric[1], &magnetic[1], &electric[0]],
                    Some(&current_density),
                ),
                magnetic_second: field_bindings(
                    &device,
                    "Maxwell B half, E A",
                    &magnetic_layout,
                    &half_step_params,
                    [&electric[0], &magnetic[1], &magnetic[0]],
                    None,
                ),
            },
        ];
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Maxwell field readback"),
            size: grid_bytes * 2,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            device,
            queue,
            core: MaxwellCore::new(&setup, "GPU"),
            cell_count,
            grid_bytes,
            electric,
            magnetic,
            current_density,
            current_electric: 0,
            half_step_params,
            full_step_params,
            magnetic_pipeline,
            electric_pipeline,
            bindings,
            staging,
            cached_state: Mutex::new(None),
            cancellation: setup.cancellation,
        })
    }

    fn field_state(&self) -> Result<Arc<YeeFieldState>, PluginError> {
        let mut cache = self
            .cached_state
            .lock()
            .map_err(|_| PluginError::Solver("Maxwell readback cache was poisoned".to_owned()))?;
        if let Some(state) = cache.as_ref() {
            return Ok(Arc::clone(state));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Maxwell readback encoder"),
            });
        encoder.copy_buffer_to_buffer(
            &self.electric[self.current_electric],
            0,
            &self.staging,
            0,
            self.grid_bytes,
        );
        encoder.copy_buffer_to_buffer(
            &self.magnetic[0],
            0,
            &self.staging,
            self.grid_bytes,
            self.grid_bytes,
        );
        self.queue.submit([encoder.finish()]);

        let (sender, receiver) = mpsc::sync_channel(1);
        self.staging
            .map_async(wgpu::MapMode::Read, .., move |result| {
                let _ = sender.send(result);
            });
        let deadline = Instant::now() + GPU_WAIT_TIMEOUT;
        let completion = loop {
            if self.cancellation.is_cancelled() {
                break Err(PluginError::Solver(
                    "Maxwell GPU readback was cancelled".to_owned(),
                ));
            }
            if let Err(error) = self.device.poll(wgpu::PollType::Poll) {
                break Err(PluginError::Solver(format!(
                    "Maxwell GPU poll failed: {error}"
                )));
            }
            match receiver.recv_timeout(Duration::from_millis(1)) {
                Ok(result) => {
                    break result.map_err(|error| {
                        PluginError::Solver(format!("Maxwell GPU readback failed: {error}"))
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(PluginError::Solver(
                        "Maxwell GPU readback callback disconnected".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    break Err(PluginError::Solver(
                        "Maxwell GPU readback timed out".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        if let Err(error) = completion {
            // Also cancels a callback that did not complete before the timeout,
            // leaving the persistent staging buffer reusable after recovery.
            self.staging.unmap();
            return Err(error);
        }

        let mapped = self.staging.get_mapped_range(..);
        let raw: &[GpuField] = bytemuck::cast_slice(&mapped);
        let count = self.cell_count as usize;
        let decoded = (|| {
            if raw.len() != count * 2 {
                return Err(PluginError::Solver(format!(
                    "Maxwell readback returned {} vectors, expected {}",
                    raw.len(),
                    count * 2
                )));
            }
            Ok(YeeFieldState {
                electric: decode_fields(&raw[..count])?,
                magnetic: decode_fields(&raw[count..])?,
            })
        })();
        drop(mapped);
        self.staging.unmap();

        let state = Arc::new(decoded?);
        *cache = Some(Arc::clone(&state));
        Ok(state)
    }

    fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        bind_group: &wgpu::BindGroup,
        label: &'static str,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.dispatch_workgroups(self.cell_count.div_ceil(WORKGROUP_SIZE), 1, 1);
    }
}

impl EquationSystemSolver for GpuMaxwellSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::TimeStepped
    }

    fn validate_time_step(&self, time_step: TimeStep) -> Result<(), PluginError> {
        self.core.validate_time_step(time_step)
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.core.validate_world(world)
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        let Some(state) = self.core.constrained_state_for(world)? else {
            return Ok(());
        };
        let electric =
            gpu_fields(&state.electric, "static electric field").map_err(PluginError::Solver)?;
        let magnetic =
            gpu_fields(&state.magnetic, "static magnetic field").map_err(PluginError::Solver)?;
        self.queue.write_buffer(
            &self.electric[self.current_electric],
            0,
            bytemuck::cast_slice(&electric),
        );
        self.queue
            .write_buffer(&self.magnetic[0], 0, bytemuck::cast_slice(&magnetic));
        let zero_current = vec![GpuField::zeroed(); self.cell_count as usize];
        self.queue.write_buffer(
            &self.current_density,
            0,
            bytemuck::cast_slice(&zero_current),
        );
        *self
            .cached_state
            .get_mut()
            .map_err(|_| PluginError::Solver("Maxwell readback cache was poisoned".to_owned()))? =
            None;
        Ok(())
    }

    fn kinematic_objects(&self) -> &[fieldcad_core::ObjectId] {
        self.core.kinematic_objects()
    }

    fn step(&mut self, context: StepContext) -> Result<SolverStepOutcome, PluginError> {
        let particle_fields = if self.core.has_particle_coupling() {
            Some(self.field_state()?)
        } else {
            None
        };
        self.core.accept_tick(context)?;
        let coupled = match particle_fields {
            Some(fields) => self
                .core
                .advance_particles(&fields, context.time_step.seconds())?,
            None => None,
        };
        if let Some(advance) = &coupled {
            let current = gpu_fields(&advance.current_density, "Maxwell current density")
                .map_err(PluginError::Solver)?;
            self.queue
                .write_buffer(&self.current_density, 0, bytemuck::cast_slice(&current));
        }

        let domain = self.core.domain();
        let half =
            gpu_params(domain, context.time_step.seconds() * 0.5).map_err(PluginError::Solver)?;
        let full = gpu_params(domain, context.time_step.seconds()).map_err(PluginError::Solver)?;
        self.queue
            .write_buffer(&self.half_step_params, 0, bytemuck::bytes_of(&half));
        self.queue
            .write_buffer(&self.full_step_params, 0, bytemuck::bytes_of(&full));

        let bindings = &self.bindings[self.current_electric];
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Maxwell Yee step encoder"),
            });
        self.dispatch(
            &mut encoder,
            &self.magnetic_pipeline,
            &bindings.magnetic_first,
            "Maxwell magnetic first half-step",
        );
        self.dispatch(
            &mut encoder,
            &self.electric_pipeline,
            &bindings.electric,
            "Maxwell electric full-step",
        );
        self.dispatch(
            &mut encoder,
            &self.magnetic_pipeline,
            &bindings.magnetic_second,
            "Maxwell magnetic second half-step",
        );
        self.queue.submit([encoder.finish()]);

        self.current_electric ^= 1;
        *self
            .cached_state
            .get_mut()
            .map_err(|_| PluginError::Solver("Maxwell readback cache was poisoned".to_owned()))? =
            None;
        Ok(coupled.map_or_else(SolverStepOutcome::default, |advance| advance.outcome))
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let state = self.field_state()?;
        sample_yee_fields(
            self.core.domain(),
            &state.electric,
            &state.magnetic,
            self.core.periodicity(),
            channel,
            geometry,
        )
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        let conservation = match self.field_state().and_then(|state| {
            yee_conservation(
                self.core.domain(),
                &state.electric,
                &state.magnetic,
                self.core.periodicity(),
            )
        }) {
            Ok(conservation) => conservation,
            Err(error) => {
                return vec![SolverDiagnostic {
                    plugin: plugin_id(),
                    severity: DiagnosticSeverity::Error,
                    code: "maxwell-gpu-readback".to_owned(),
                    message: error.to_string(),
                }];
            }
        };
        let mut diagnostics = self.core.diagnostics(conservation);
        if self.core.has_particle_coupling() {
            diagnostics.push(SolverDiagnostic {
                plugin: plugin_id(),
                severity: DiagnosticSeverity::Info,
                code: "maxwell-gpu-reference-particle-coupling".to_owned(),
                message: "GPU Yee update with CPU f64 reference particle interpolation/deposition; one full E/B readback per coupled tick"
                    .to_owned(),
            });
        }
        diagnostics
    }
}

fn field_buffer(device: &wgpu::Device, label: &'static str, fields: &[GpuField]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(fields),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
    })
}

fn uniform_buffer(device: &wgpu::Device, label: &'static str, params: &GpuParams) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::bytes_of(params),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

fn field_bindings(
    device: &wgpu::Device,
    label: &'static str,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    fields: [&wgpu::Buffer; 3],
    current_density: Option<&wgpu::Buffer>,
) -> wgpu::BindGroup {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: params.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: fields[0].as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: fields[1].as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 3,
            resource: fields[2].as_entire_binding(),
        },
    ];
    if let Some(current_density) = current_density {
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: current_density.as_entire_binding(),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &entries,
    })
}

fn gpu_params(domain: Domain, seconds: f64) -> Result<GpuParams, String> {
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    Ok(GpuParams {
        counts: [counts.x, counts.y, counts.z, 0],
        spacing_dt: [
            finite_f32(spacing.x, "Maxwell dx")?,
            finite_f32(spacing.y, "Maxwell dy")?,
            finite_f32(spacing.z, "Maxwell dz")?,
            finite_f32(seconds, "Maxwell dt")?,
        ],
    })
}

fn gpu_fields(fields: &[DVec3], label: &str) -> Result<Vec<GpuField>, String> {
    fields
        .iter()
        .map(|field| {
            Ok(GpuField {
                value: [
                    finite_f32(field.x, label)?,
                    finite_f32(field.y, label)?,
                    finite_f32(field.z, label)?,
                    0.0,
                ],
            })
        })
        .collect()
}

fn decode_fields(fields: &[GpuField]) -> Result<Vec<DVec3>, PluginError> {
    fields
        .iter()
        .map(|field| {
            let value = DVec3::new(
                f64::from(field.value[0]),
                f64::from(field.value[1]),
                f64::from(field.value[2]),
            );
            value.is_finite().then_some(value).ok_or_else(|| {
                PluginError::Solver("Maxwell GPU produced a non-finite field value".to_owned())
            })
        })
        .collect()
}

fn finite_f32(value: f64, label: &str) -> Result<f32, String> {
    let converted = value as f32;
    if converted.is_finite() {
        Ok(converted)
    } else {
        Err(format!("{label} cannot be represented as f32"))
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, DomainBounds, FieldColumn, GridLattice, ObjectShape,
        ObjectSpec, Precision, ProbeId, Resolution, SampleGeometry, SimulationClock, Transform,
        World, WorldCommand,
    };
    use fieldcad_electromagnetic_sources::{
        charge_component_id, charge_component_schema, charge_properties,
    };
    use fieldcad_electromagnetism::{
        ELECTRIC_DIVERGENCE_HANDLE, ELECTRIC_FIELD_HANDLE, ENERGY_DENSITY_HANDLE,
        ElectromagnetismPlugin, MAGNETIC_DIVERGENCE_HANDLE, MAGNETIC_FIELD_HANDLE, courant_limit,
        prescribed_plane_wave_configuration,
    };
    use fieldcad_mass_sources::mass_component_schemas;
    use fieldcad_particles::{ParticleTemplate, particle_component_schema, template_particle_spec};
    use fieldcad_plugin_api::{EquationSystemPlugin, SolverContext};
    use glam::UVec3;

    use super::*;

    #[test]
    fn compute_shader_compiles_and_declares_both_update_phases() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("WGSL must parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("WGSL must validate");
        for expected in ["advance_magnetic", "advance_electric"] {
            assert!(
                module
                    .entry_points
                    .iter()
                    .any(|entry| entry.name == expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn gpu_steps_match_the_f64_reference_on_a_small_grid() {
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
                eprintln!("skipping Maxwell GPU parity test: no headless adapter");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("Maxwell parity test device"),
                    ..Default::default()
                })
                .await
                .expect("adapter must provide a default device");

            let bounds = DomainBounds::new(DVec3::ZERO, DVec3::ONE).unwrap();
            let resolution = Resolution::new(16, 4, 4).unwrap();
            let boundaries = BoundaryConditions::uniform(BoundaryCondition::Periodic);
            let cpu_domain = Domain::new(bounds, resolution, boundaries, Precision::F64);
            let gpu_domain = Domain::new(bounds, resolution, boundaries, Precision::F32);
            let time_step = TimeStep::from_seconds(courant_limit(&cpu_domain) * 0.65).unwrap();
            let clock = SimulationClock::new(time_step);
            let world = World::new().snapshot();
            let configuration = prescribed_plane_wave_configuration(1.0, 1).unwrap();
            let cpu_plugin = ElectromagnetismPlugin::new();
            let mut cpu = cpu_plugin
                .create_solver(SolverContext {
                    configuration: &configuration,
                    domain: &cpu_domain,
                    world: &world,
                    initial_step: clock.snapshot().step,
                    cancellation: SolverCancellation::default(),
                })
                .unwrap();
            let gpu_plugin = ElectromagnetismPlugin::with_backend(Arc::new(
                GpuMaxwellBackend::new(device, queue),
            ));
            let mut gpu = gpu_plugin
                .create_solver(SolverContext {
                    configuration: &configuration,
                    domain: &gpu_domain,
                    world: &world,
                    initial_step: clock.snapshot().step,
                    cancellation: SolverCancellation::default(),
                })
                .unwrap();
            for tick in 1..=4 {
                let context = StepContext {
                    tick,
                    time_seconds: f64::from(tick as u32) * time_step.seconds(),
                    time_step,
                };
                cpu.step(context).unwrap();
                gpu.step(context).unwrap();
            }

            let geometry = SampleGeometry::Grid(GridLattice::new(
                DVec3::splat(0.0625),
                DVec3::new(0.125, 0.25, 0.25),
                UVec3::new(8, 4, 4),
            ));
            for handle in [
                ELECTRIC_FIELD_HANDLE,
                MAGNETIC_FIELD_HANDLE,
                ENERGY_DENSITY_HANDLE,
                ELECTRIC_DIVERGENCE_HANDLE,
                MAGNETIC_DIVERGENCE_HANDLE,
            ] {
                let expected = cpu.sample(handle, &geometry).unwrap();
                let actual = gpu.sample(handle, &geometry).unwrap();
                assert_eq!(actual.validity, expected.validity);
                compare_columns(handle, &actual.values, &expected.values);
            }

            // The desktop's default is not the validation wave: it is a
            // constrained field sourced by the authored stationary charge.
            // Exercise that path on the actual GPU backend as well as the CPU
            // oracle so a source-blind default cannot return unnoticed.
            let bounds = DomainBounds::centred_cube(5.0).unwrap();
            let resolution = Resolution::uniform(32).unwrap();
            let cpu_domain = Domain::new(bounds, resolution, boundaries, Precision::F64);
            let gpu_domain = Domain::new(bounds, resolution, boundaries, Precision::F32);
            let time_step = TimeStep::from_seconds(courant_limit(&cpu_domain) * 0.8).unwrap();
            let clock = SimulationClock::new(time_step);
            let mut charged_world = World::new();
            charged_world
                .commit([
                    WorldCommand::RegisterComponentSchema(charge_component_schema()),
                    WorldCommand::CreateObject(
                        ObjectSpec::new("static charge")
                            .with_transform(Transform::at(DVec3::ZERO).unwrap())
                            .with_shape(ObjectShape::point(0.15).unwrap())
                            .with_component(
                                charge_component_id(),
                                charge_properties(1.0e-9).unwrap(),
                            ),
                    ),
                ])
                .unwrap();
            let charged_world = charged_world.snapshot();
            let configuration = cpu_plugin.default_configuration();
            let mut cpu = cpu_plugin
                .create_solver(SolverContext {
                    configuration: &configuration,
                    domain: &cpu_domain,
                    world: &charged_world,
                    initial_step: clock.snapshot().step,
                    cancellation: SolverCancellation::default(),
                })
                .unwrap();
            let mut gpu = gpu_plugin
                .create_solver(SolverContext {
                    configuration: &configuration,
                    domain: &gpu_domain,
                    world: &charged_world,
                    initial_step: clock.snapshot().step,
                    cancellation: SolverCancellation::default(),
                })
                .unwrap();
            // SimulationRuntime invokes this after solver construction and on
            // every authored source edit; it must be able to replace resident
            // GPU state without recreating the backend.
            gpu.on_world_changed(&charged_world).unwrap();
            for tick in 1..=8 {
                let context = StepContext {
                    tick,
                    time_seconds: tick as f64 * time_step.seconds(),
                    time_step,
                };
                cpu.step(context).unwrap();
                gpu.step(context).unwrap();
            }
            let geometry = SampleGeometry::probes(
                vec![ProbeId::new(0), ProbeId::new(1)],
                vec![DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, 1.5, 0.0)],
            )
            .unwrap();
            for handle in [ELECTRIC_FIELD_HANDLE, MAGNETIC_FIELD_HANDLE] {
                let expected = cpu.sample(handle, &geometry).unwrap();
                let actual = gpu.sample(handle, &geometry).unwrap();
                compare_columns(handle, &actual.values, &expected.values);
            }
        });
    }

    #[test]
    fn gpu_current_update_matches_the_cpu_particle_coupling_reference() {
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
                eprintln!("skipping Maxwell particle GPU parity test: no headless adapter");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("Maxwell particle parity test device"),
                    ..Default::default()
                })
                .await
                .expect("adapter must provide a default device");

            let bounds = DomainBounds::centred_cube(1.0).unwrap();
            let resolution = Resolution::uniform(8).unwrap();
            let boundaries = BoundaryConditions::uniform(BoundaryCondition::Periodic);
            let cpu_domain = Domain::new(bounds, resolution, boundaries, Precision::F64);
            let gpu_domain = Domain::new(bounds, resolution, boundaries, Precision::F32);
            let time_step = TimeStep::from_seconds(courant_limit(&cpu_domain) * 0.5).unwrap();
            let clock = SimulationClock::new(time_step);
            let mut world = World::new();
            world
                .commit(
                    [charge_component_schema(), particle_component_schema()]
                        .into_iter()
                        .chain(mass_component_schemas())
                        .map(WorldCommand::RegisterComponentSchema)
                        .chain([WorldCommand::CreateObject(
                            template_particle_spec(
                                ParticleTemplate::Electron,
                                true,
                                DVec3::new(-0.2, 0.0, 0.0),
                                DVec3::X * 1.0e8,
                                0.01,
                            )
                            .unwrap(),
                        )]),
                )
                .unwrap();
            let world = world.snapshot();
            let configuration = ElectromagnetismPlugin::new().default_configuration();
            let cpu_plugin = ElectromagnetismPlugin::new();
            let mut cpu = cpu_plugin
                .create_solver(SolverContext {
                    configuration: &configuration,
                    domain: &cpu_domain,
                    world: &world,
                    initial_step: clock.snapshot().step,
                    cancellation: SolverCancellation::default(),
                })
                .unwrap();
            let gpu_plugin = ElectromagnetismPlugin::with_backend(Arc::new(
                GpuMaxwellBackend::new(device, queue),
            ));
            let mut gpu = gpu_plugin
                .create_solver(SolverContext {
                    configuration: &configuration,
                    domain: &gpu_domain,
                    world: &world,
                    initial_step: clock.snapshot().step,
                    cancellation: SolverCancellation::default(),
                })
                .unwrap();

            for tick in 1..=3 {
                let context = StepContext {
                    tick,
                    time_seconds: tick as f64 * time_step.seconds(),
                    time_step,
                };
                let expected_motion = cpu.step(context).unwrap();
                let actual_motion = gpu.step(context).unwrap();
                assert_eq!(actual_motion, expected_motion);
            }

            let geometry = SampleGeometry::probes(
                vec![ProbeId::new(0), ProbeId::new(1), ProbeId::new(2)],
                vec![
                    DVec3::new(-0.4, 0.1, 0.0),
                    DVec3::new(0.0, 0.25, 0.0),
                    DVec3::new(0.35, -0.2, 0.1),
                ],
            )
            .unwrap();
            for handle in [ELECTRIC_FIELD_HANDLE, MAGNETIC_FIELD_HANDLE] {
                let expected = cpu.sample(handle, &geometry).unwrap();
                let actual = gpu.sample(handle, &geometry).unwrap();
                assert_eq!(actual.validity, expected.validity);
                compare_columns(handle, &actual.values, &expected.values);
            }
        });
    }

    fn compare_columns(handle: ChannelHandle, actual: &FieldColumn, expected: &FieldColumn) {
        match (actual, expected) {
            (FieldColumn::Vector(actual), FieldColumn::Vector(expected)) => {
                for (actual, expected) in actual.iter().zip(expected.iter()) {
                    let scale = if handle == MAGNETIC_FIELD_HANDLE {
                        2.0e-11
                    } else {
                        3.0e-3
                    };
                    assert!(
                        (*actual - *expected).length() <= scale,
                        "GPU {actual:?} differs from CPU {expected:?}"
                    );
                }
            }
            (FieldColumn::Scalar(actual), FieldColumn::Scalar(expected)) => {
                let absolute = if handle == ENERGY_DENSITY_HANDLE {
                    1.0e-18
                } else if handle == MAGNETIC_DIVERGENCE_HANDLE {
                    2.0e-10
                } else {
                    2.0e-2
                };
                for (actual, expected) in actual.iter().zip(expected.iter()) {
                    assert!(
                        (actual - expected).abs() <= absolute,
                        "GPU {actual:e} differs from CPU {expected:e}"
                    );
                }
            }
            _ => panic!("CPU and GPU channel shapes differ"),
        }
    }
}
