use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use fieldcad_core::{
    BoundaryConditions, Domain, DomainBounds, ObjectShape, ObjectSpec, Precision, ProbeSpec,
    Resolution, SessionId, SlicePlaneSpec, TimeStep, Transform, WorldCommand, WorldSnapshot,
};
use fieldcad_electrostatics::{
    ElectrostaticBatchEvaluator, ElectrostaticsPlugin, charge_component_id, charge_properties,
    electric_field_channel_id, electric_potential_channel_id,
};
use fieldcad_simulation::{
    CommandSequencer, FieldDataSource, LocalDataSource, ProbeHistory, RuntimeConfig,
    SimulationRuntime, Subscription,
};
use glam::{DVec2, DVec3, UVec2, Vec2};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

use crate::{
    camera::{AxisView, OrbitCamera, Viewport},
    electrostatics_gpu::GpuElectrostaticEvaluator,
    renderer::{GuiPaint, RenderStatus, SceneFrame, ViewportRenderer},
    scene::{self, TransformHandle},
    ui::{self, CameraAction, ComputeView, UiModel, ViewportGesture},
};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("desktop event loop failed: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    #[error("application initialization failed: {0}")]
    Initialization(String),
}

/// Redraw cadence while the simulation is advancing. Short enough to look
/// continuous, long enough that the event loop actually sleeps between frames
/// rather than spinning.
const RUNNING_FRAME_INTERVAL: Duration = Duration::from_millis(4);
/// How long to wait before trying again when the window cannot present.
const OCCLUDED_RETRY_INTERVAL: Duration = Duration::from_millis(200);
/// Upper bound on how long the loop will sleep, so a paused, idle app still
/// notices external state changes within a reasonable time.
const MAX_IDLE_INTERVAL: Duration = Duration::from_secs(1);

pub fn run() -> Result<(), RunError> {
    run_for(None)
}

/// Run the application, optionally quitting by itself after `lifetime`.
///
/// A self-imposed deadline makes an interactive test safe to attempt on a
/// machine where a windowed run has previously wedged the compositor: the
/// process leaves on its own rather than needing to be killed from elsewhere.
pub fn run_for(lifetime: Option<Duration>) -> Result<(), RunError> {
    let event_loop = EventLoop::new()?;
    // Deliberately not `Poll`. Requesting a redraw unconditionally on every
    // iteration keeps a Wayland compositor permanently busy with this client
    // and never lets the loop idle; redraws are demand-driven instead, from
    // egui's requested repaint time and whether the simulation is advancing.
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut application = DesktopApplication {
        deadline: lifetime.map(|lifetime| Instant::now() + lifetime),
        ..DesktopApplication::default()
    };
    event_loop.run_app(&mut application)?;

    match application.initialization_error {
        Some(error) => Err(RunError::Initialization(error)),
        None => Ok(()),
    }
}

#[derive(Default)]
struct DesktopApplication {
    window_state: Option<WindowState>,
    initialization_error: Option<String>,
    /// When set, the application exits cleanly at this instant.
    deadline: Option<Instant>,
}

impl ApplicationHandler for DesktopApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window_state.is_some() {
            return;
        }

        match WindowState::new(event_loop) {
            Ok(window_state) => self.window_state = Some(window_state),
            Err(error) => {
                tracing::error!(%error, "application initialization failed");
                self.initialization_error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.window_state = None;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window_state) = self.window_state.as_mut() else {
            return;
        };
        if window_state.window.id() != window_id {
            return;
        }

        if let Err(error) = window_state.handle_window_event(event_loop, event) {
            tracing::error!(%error, "unrecoverable window error");
            self.initialization_error = Some(error);
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some(deadline) = self.deadline
            && now >= deadline
        {
            tracing::info!("self-imposed lifetime reached; exiting");
            event_loop.exit();
            return;
        }

        let Some(window_state) = &self.window_state else {
            return;
        };

        // Ask for a frame only when one is actually due. Everything else sleeps.
        if now >= window_state.next_redraw {
            window_state.window.request_redraw();
        }

        // Wake for whichever comes first: the next frame or the exit deadline.
        let wake = match self.deadline {
            Some(deadline) => window_state.next_redraw.min(deadline),
            None => window_state.next_redraw,
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Tear the graphics stack down here, while the event loop is still
        // alive, rather than during process teardown.
        self.window_state = None;
        tracing::info!("shut down cleanly");
    }
}

/// Field order is drop order. `egui_state` and `renderer` both reference the
/// window, so they are declared before it; `ViewportRenderer` additionally
/// drains the GPU queue on drop. Dropping the window first tears out the native
/// surface from under objects that still hold it.
struct WindowState {
    egui_state: egui_winit::State,
    renderer: ViewportRenderer,
    window: Arc<Window>,
    egui_context: egui::Context,
    camera: OrbitCamera,
    ui_model: UiModel,
    viewport: Viewport,
    data_source: Box<dyn FieldDataSource>,
    /// Mirrors the source's world so panels and picking read one consistent
    /// revision for the whole frame.
    world: WorldSnapshot,
    probe_history: ProbeHistory,
    commands: CommandSequencer,
    active_transform: Option<ActiveTransformDrag>,
    frame_stats: FrameStats,
    /// When the next frame is due. Drives the event loop's control flow.
    next_redraw: Instant,
    /// Set from `WindowEvent::Occluded`; suppresses rendering entirely.
    occluded: bool,
}

impl WindowState {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self, String> {
        let attributes = WindowAttributes::default()
            .with_title("Field CAD")
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(LogicalSize::new(720.0, 480.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| error.to_string())?,
        );
        let gpu_config = crate::gpu::GpuConfig::from_env();
        tracing::info!(
            backends = ?gpu_config.backends,
            present_mode = ?gpu_config.present_mode,
            force_fallback = gpu_config.force_fallback_adapter,
            "requesting graphics stack"
        );
        let renderer = pollster::block_on(ViewportRenderer::new(window.clone(), gpu_config))
            .map_err(|error| error.to_string())?;

        let egui_context = egui::Context::default();
        egui_context.set_visuals(egui::Visuals::dark());
        let egui_state = egui_winit::State::new(
            egui_context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        let (compute_device, compute_queue) = renderer.compute_handles();
        let evaluator: Arc<dyn ElectrostaticBatchEvaluator> = Arc::new(
            GpuElectrostaticEvaluator::new(compute_device, compute_queue),
        );
        let data_source = create_local_data_source(evaluator)?;
        let world = data_source.world();

        Ok(Self {
            egui_state,
            renderer,
            window,
            egui_context,
            camera: OrbitCamera::default(),
            ui_model: UiModel::new(),
            viewport: Viewport::default(),
            data_source: Box::new(data_source),
            world,
            probe_history: ProbeHistory::default(),
            commands: CommandSequencer::default(),
            active_transform: None,
            frame_stats: FrameStats::default(),
            next_redraw: Instant::now(),
            occluded: false,
        })
    }

    fn handle_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) -> Result<(), String> {
        let response = self.egui_state.on_window_event(&self.window, &event);
        if response.repaint {
            self.schedule_redraw(Duration::ZERO);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.renderer.resize(size);
                self.schedule_redraw(Duration::ZERO);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.renderer.resize(self.window.inner_size());
                self.schedule_redraw(Duration::ZERO);
            }
            WindowEvent::Occluded(occluded) => {
                // A covered or minimized window cannot present. Rendering anyway
                // means blocking on a surface that will not release a frame.
                self.occluded = occluded;
                tracing::debug!(occluded, "window occlusion changed");
                self.schedule_redraw(if occluded {
                    OCCLUDED_RETRY_INTERVAL
                } else {
                    Duration::ZERO
                });
            }
            WindowEvent::KeyboardInput { event, .. }
                if !response.consumed && event.state == ElementState::Pressed =>
            {
                self.handle_key(event.physical_key);
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop)?,
            _ => {}
        }

        Ok(())
    }

    /// Note that a frame is due after `delay`, never moving the deadline later.
    fn schedule_redraw(&mut self, delay: Duration) {
        let deadline = Instant::now() + delay.min(MAX_IDLE_INTERVAL);
        self.next_redraw = self.next_redraw.min(deadline);
    }

    /// Replace the deadline outright, including moving it later.
    fn set_next_redraw(&mut self, delay: Duration) {
        self.next_redraw = Instant::now() + delay.min(MAX_IDLE_INTERVAL);
    }

    fn handle_key(&mut self, key: PhysicalKey) {
        match key {
            PhysicalKey::Code(KeyCode::Escape) => {
                self.ui_model.set_scene_selection(None);
                self.active_transform = None;
            }
            PhysicalKey::Code(KeyCode::KeyF) => self.focus_selection(),
            PhysicalKey::Code(KeyCode::Digit1) => self.camera.set_axis_view(AxisView::PositiveX),
            PhysicalKey::Code(KeyCode::Digit3) => self.camera.set_axis_view(AxisView::PositiveY),
            PhysicalKey::Code(KeyCode::Digit7) => self.camera.set_axis_view(AxisView::PositiveZ),
            _ => return,
        }
        self.schedule_redraw(Duration::ZERO);
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let frame_started = Instant::now();
        let elapsed = self.frame_stats.begin_frame();

        // Advance the source by real elapsed time, not by one tick per frame.
        // The numerical `dt` is the source's business; a slow frame must not
        // change it.
        self.data_source
            .poll(elapsed)
            .map_err(|error| format!("simulation update failed: {error}"))?;
        self.refresh_world();

        if self.occluded {
            // Nothing can be presented, so do no GPU work at all and check back
            // periodically. Simulation time still advances above.
            self.set_next_redraw(OCCLUDED_RETRY_INTERVAL);
            return Ok(());
        }

        let compute = ComputeView::build(self.data_source.as_ref(), &self.world);
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let mut ui_frame = ui::UiFrameOutput::default();
        let adapter_name = self.renderer.adapter_name().to_owned();
        let frame_time_ms = self.frame_stats.smoothed_frame_ms;
        let world = self.world.clone();

        let full_output = self.egui_context.run_ui(raw_input, |root_ui| {
            ui_frame = ui::show(
                root_ui,
                &mut self.ui_model,
                ui::FrameContext {
                    compute: &compute,
                    world: &world,
                    probe_history: &self.probe_history,
                    adapter_name: &adapter_name,
                    frame_time_ms,
                    active_translation: self.active_transform.map(|drag| drag.constraint.label()),
                },
            );
        });

        let pixels_per_point = full_output.pixels_per_point;
        if ui_frame.viewport.is_positive() {
            self.viewport = Viewport::from_logical(
                Vec2::new(ui_frame.viewport.min.x, ui_frame.viewport.min.y),
                Vec2::new(ui_frame.viewport.width(), ui_frame.viewport.height()),
                pixels_per_point,
                self.renderer.surface_size(),
            );
        }

        self.apply_camera_action(ui_frame.camera_action);
        self.apply_viewport_gesture(ui_frame.viewport_gesture, pixels_per_point)?;

        if let Some(payload) = ui_frame.command {
            let command = self.commands.issue(payload);
            self.data_source
                .execute(command)
                .map_err(|error| format!("simulation command failed: {error}"))?;
            self.refresh_world();
        }

        // Decide when the next frame is due before consuming `full_output`.
        // egui reports how long it is content to wait; a running simulation
        // overrides that, because it must keep advancing whether or not the UI
        // has anything new to say.
        let ui_repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(MAX_IDLE_INTERVAL, |viewport| viewport.repaint_delay);
        let next_frame_delay = if compute.mode == fieldcad_core::SimulationMode::Running {
            RUNNING_FRAME_INTERVAL.min(ui_repaint_delay)
        } else {
            ui_repaint_delay
        };
        self.set_next_redraw(next_frame_delay);

        let egui::FullOutput {
            platform_output,
            textures_delta,
            shapes,
            ..
        } = full_output;
        self.egui_state.handle_platform_output_with_event_loop(
            &self.window,
            event_loop,
            platform_output,
        );
        let primitives = self.egui_context.tessellate(shapes, pixels_per_point);
        let instances = scene::instances(&self.world, self.ui_model.selection);
        let mut field = self.data_source.latest_snapshot().map_or_else(
            scene::FieldGeometry::default,
            |snapshot| {
                scene::field_geometry(
                    &snapshot,
                    self.ui_model.field_layers,
                    &self.ui_model.plane_layers,
                )
            },
        );
        scene::append_authoring_geometry(&mut field, &self.world, self.ui_model.scene_selection());
        scene::append_translation_gizmo(
            &mut field,
            &self.world,
            self.ui_model.selection,
            self.active_transform
                .and_then(|drag| drag.constraint.handle()),
        );

        let status = self.renderer.render(
            SceneFrame {
                camera: &self.camera,
                viewport: self.viewport,
                grid_visible: self.ui_model.grid_visible,
                axes_visible: self.ui_model.axes_visible,
                instances: &instances,
                field: &field,
            },
            GuiPaint {
                primitives: &primitives,
                textures_delta: &textures_delta,
                pixels_per_point,
            },
        );

        match status {
            RenderStatus::SurfaceLost => {
                tracing::warn!("presentation surface was lost; recreating it");
                self.renderer
                    .recreate_surface(self.window.clone())
                    .map_err(|error| error.to_string())?;
            }
            RenderStatus::Occluded => {
                // The surface reported it cannot present even though winit has
                // not told us so. Back off rather than retry at frame rate.
                self.occluded = true;
                self.set_next_redraw(OCCLUDED_RETRY_INTERVAL);
            }
            RenderStatus::Presented | RenderStatus::Skipped => {}
        }

        self.frame_stats.finish_frame(frame_started.elapsed());

        Ok(())
    }

    /// Pick up the current world and record any new probe samples.
    fn refresh_world(&mut self) {
        if let Some(snapshot) = self.data_source.latest_snapshot() {
            self.probe_history.record(&snapshot);
        }
        self.world = self.data_source.world();

        // A selection that no longer resolves must not linger in the inspector.
        if self
            .ui_model
            .selection
            .is_some_and(|id| self.world.object(id).is_none())
        {
            self.ui_model.selection = None;
        }
        if self
            .ui_model
            .plane_selection
            .is_some_and(|id| !self.world.planes().contains_key(&id))
        {
            self.ui_model.plane_selection = None;
        }
        if self
            .ui_model
            .probe_selection
            .is_some_and(|id| self.world.probe(id).is_none())
        {
            self.ui_model.probe_selection = None;
        }
        self.ui_model
            .plane_layers
            .retain(|id, _| self.world.planes().contains_key(id));
        if self.active_transform.is_some_and(|drag| {
            self.world
                .object(drag.object)
                .is_none_or(|object| !object.visible)
        }) {
            self.active_transform = None;
        }
    }

    fn apply_camera_action(&mut self, action: Option<CameraAction>) {
        match action {
            Some(CameraAction::Axis(view)) => self.camera.set_axis_view(view),
            Some(CameraAction::FocusSelection) => self.focus_selection(),
            None => {}
        }
    }

    fn apply_viewport_gesture(
        &mut self,
        gesture: ViewportGesture,
        pixels_per_point: f32,
    ) -> Result<(), String> {
        let drag_delta = Vec2::new(gesture.drag_delta.x, gesture.drag_delta.y);
        if gesture.middle_dragged {
            if gesture.shift {
                self.camera.pan(drag_delta, self.viewport.height as f32);
            } else {
                self.camera.orbit(drag_delta);
            }
        }
        if gesture.scroll_delta != 0.0 {
            self.camera.dolly(gesture.scroll_delta);
        }
        let pointer = gesture.pointer_position.map(|pointer| {
            Viewport::pointer_to_physical(Vec2::new(pointer.x, pointer.y), pixels_per_point)
        });
        let pointer_delta = drag_delta * pixels_per_point.max(0.01);
        let was_active = self.active_transform.is_some();

        if gesture.primary_pressed
            && let (Some(object_id), Some(pointer)) = (self.ui_model.selection, pointer)
            && let Some(object) = self.world.object(object_id)
        {
            let constraint =
                scene::pick_transform_handle(&self.camera, self.viewport, pointer, object)
                    .map(TranslationConstraint::Handle)
                    .or_else(|| {
                        (scene::pick_object(&self.world, &self.camera, self.viewport, pointer)
                            == Some(object_id))
                        .then_some(TranslationConstraint::ViewPlane)
                    });
            if let Some(constraint) = constraint {
                self.active_transform = Some(ActiveTransformDrag {
                    object: object_id,
                    constraint,
                    translation: object.transform.translation,
                });
            }
        }

        if gesture.primary_dragged
            && let (Some(active), Some(pointer)) = (self.active_transform, pointer)
            && let Some(object) = self.world.object(active.object)
            && let Some(translation) = match active.constraint {
                TranslationConstraint::Handle(handle) => scene::constrained_translation(
                    handle,
                    &self.camera,
                    self.viewport,
                    pointer,
                    pointer_delta,
                    active.translation.as_vec3(),
                    object,
                ),
                TranslationConstraint::ViewPlane => scene::view_plane_translation(
                    &self.camera,
                    self.viewport,
                    pointer,
                    pointer_delta,
                    active.translation.as_vec3(),
                ),
            }
            && translation.length_squared() > 0.0
        {
            let transform = Transform::new(
                active.translation + translation.as_dvec3(),
                object.transform.rotation,
            )
            .map_err(|error| error.to_string())?;
            if let Some(active) = self.active_transform.as_mut() {
                active.translation = transform.translation;
            }
            let command = self
                .commands
                .issue(fieldcad_simulation::CommandPayload::CommitWorld(vec![
                    WorldCommand::SetTransform {
                        object: active.object,
                        transform,
                    },
                ]));
            self.data_source
                .execute(command)
                .map_err(|error| format!("object move failed: {error}"))?;
            self.refresh_world();
        }

        let drag_consumed = was_active || self.active_transform.is_some();
        if gesture.primary_clicked
            && !drag_consumed
            && let Some(pointer) = pointer
        {
            self.ui_model.set_scene_selection(scene::pick_scene(
                &self.world,
                &self.camera,
                self.viewport,
                pointer,
            ));
        }
        if gesture.primary_released {
            self.active_transform = None;
        }
        Ok(())
    }

    fn focus_selection(&mut self) {
        let Some(id) = self.ui_model.selection else {
            return;
        };
        let Some(object) = self.world.object(id) else {
            return;
        };
        let (centre, radius) = object.bounding_sphere();
        self.camera.focus(centre.as_vec3(), radius as f32);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActiveTransformDrag {
    object: fieldcad_core::ObjectId,
    constraint: TranslationConstraint,
    /// Latest absolute translation submitted during this drag. The
    /// authoritative world intentionally remains unchanged while Running edits
    /// wait for a tick boundary, so accumulating against the replica would lose
    /// pointer deltas between ticks.
    translation: DVec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranslationConstraint {
    Handle(TransformHandle),
    ViewPlane,
}

impl TranslationConstraint {
    const fn handle(self) -> Option<TransformHandle> {
        match self {
            Self::Handle(handle) => Some(handle),
            Self::ViewPlane => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Handle(handle) => match handle {
                TransformHandle::AxisX => "Constrained move · X axis",
                TransformHandle::AxisY => "Constrained move · Y axis",
                TransformHandle::AxisZ => "Constrained move · Z axis",
                TransformHandle::PlaneXY => "Constrained move · XY plane",
                TransformHandle::PlaneYZ => "Constrained move · YZ plane",
                TransformHandle::PlaneZX => "Constrained move · ZX plane",
            },
            Self::ViewPlane => "Free move · camera plane",
        }
    }
}

fn create_local_data_source(
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
) -> Result<LocalDataSource, String> {
    let domain = Domain::new(
        DomainBounds::centred_cube(5.0).map_err(|error| error.to_string())?,
        Resolution::uniform(32).map_err(|error| error.to_string())?,
        BoundaryConditions::default(),
        Precision::F32,
    );
    let mut runtime = SimulationRuntime::new(
        RuntimeConfig::new(
            domain,
            TimeStep::from_seconds(1.0 / 60.0).map_err(|error| error.to_string())?,
            SessionId::from_u128(1),
        )
        .with_subscription(
            Subscription::PROBES_ONLY
                .with_planes(UVec2::splat(33))
                .with_domain_stride(8),
        )
        .with_plugin(Box::new(ElectrostaticsPlugin::with_evaluator(evaluator))),
    )
    .map_err(|error| error.to_string())?;
    runtime
        .commit_world_commands(vec![
            WorldCommand::CreateObject(
                ObjectSpec::new("Positive point charge")
                    .with_transform(
                        Transform::at(DVec3::new(0.0, 0.0, 0.6)).map_err(|e| e.to_string())?,
                    )
                    .with_shape(ObjectShape::point(0.15).map_err(|error| error.to_string())?)
                    .with_component(
                        charge_component_id(),
                        charge_properties(1.0e-9).map_err(|error| error.to_string())?,
                    ),
            ),
            WorldCommand::CreateProbe(ProbeSpec::at(
                "Field probe",
                DVec3::new(1.0, 0.0, 0.6),
                vec![electric_field_channel_id(), electric_potential_channel_id()],
            )),
            WorldCommand::CreatePlane(
                SlicePlaneSpec::new("XY electric field", DVec3::ZERO, DVec3::Z)
                    .and_then(|plane| plane.with_half_extent(DVec2::splat(4.0)))
                    .map_err(|error| error.to_string())?,
            ),
        ])
        .map_err(|error| error.to_string())?;

    Ok(LocalDataSource::new(runtime))
}

struct FrameStats {
    previous_frame: Instant,
    smoothed_frame_ms: f32,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            previous_frame: Instant::now(),
            smoothed_frame_ms: 0.0,
        }
    }
}

impl FrameStats {
    /// Returns wall-clock time since the previous redraw for simulation pacing.
    /// Idle time is deliberately not reported as render work.
    fn begin_frame(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = now - self.previous_frame;
        self.previous_frame = now;
        elapsed
    }

    fn finish_frame(&mut self, duration: Duration) {
        let elapsed_ms = duration.as_secs_f32() * 1_000.0;
        self.smoothed_frame_ms = if self.smoothed_frame_ms == 0.0 {
            elapsed_ms
        } else {
            self.smoothed_frame_ms * 0.9 + elapsed_ms * 0.1
        };
    }
}

#[cfg(test)]
mod tests {
    use super::FrameStats;
    use std::time::Duration;

    #[test]
    fn frame_diagnostics_measure_work_not_time_spent_idle() {
        let mut stats = FrameStats::default();

        stats.finish_frame(Duration::from_millis(10));
        stats.finish_frame(Duration::from_millis(2));

        assert!((stats.smoothed_frame_ms - 9.2).abs() < 1.0e-6);
    }
}
