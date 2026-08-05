use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, Domain, DomainBounds, FieldBoxSpec, FieldSphereSpec,
    ObjectShape, ObjectSpec, Precision, ProbePosition, ProbeSpec, Resolution, SessionId,
    SimulationMode, SlicePlaneSpec, TimeStep, Transform, WorldCommand, WorldSnapshot,
};
use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
use fieldcad_electromagnetism::{
    ElectromagnetismPlugin, MaxwellSolverBackend, courant_limit,
    electric_divergence_channel_id as maxwell_electric_divergence_channel_id,
    energy_density_channel_id as maxwell_energy_density_channel_id,
    magnetic_divergence_channel_id as maxwell_magnetic_divergence_channel_id,
    magnetic_field_channel_id as maxwell_magnetic_field_channel_id,
};
use fieldcad_electrostatics::{
    ElectrostaticBatchEvaluator, ElectrostaticsPlugin, electric_field_channel_id,
    electric_potential_channel_id,
};
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{
    AsyncLocalDataSource, CommandEvent, CommandPayload, FieldDataSource, LocalDataSource,
    PluginRegistration, ProbeHistory, RuntimeConfig, SimulationRuntime, Subscription,
};
use glam::{DQuat, DVec2, DVec3, UVec2, UVec3, Vec2};
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
    electromagnetism_gpu::GpuMaxwellBackend,
    electrostatics_gpu::GpuElectrostaticEvaluator,
    mcp::{self, McpAction, McpSession},
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

/// Lock the shared model. A free function, not a method on `WindowState`: a
/// `&self` method returning a guard borrowed from it would tie the guard's
/// lifetime to all of `self`, making every other field of `WindowState`
/// un-borrowable for as long as the guard is held — the opposite of what a
/// per-field lock is for.
fn lock_model(data_source: &Mutex<HeadlessServer>) -> MutexGuard<'_, HeadlessServer> {
    data_source.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// Shared, not owned outright: an embedded MCP server (see `crate::mcp`)
    /// locks the same model from its own thread, so an agent drives the
    /// exact session this window is drawing rather than a separate one.
    /// `std::sync::Mutex`, not `tokio::sync::Mutex` — this thread never
    /// awaits it, and it's what lets `crate::mcp`'s tokio thread and this
    /// synchronous frame loop share one lock without either needing the
    /// other's runtime.
    data_source: Arc<Mutex<HeadlessServer>>,
    /// Mirrors the source's world so panels and picking read one consistent
    /// revision for the whole frame.
    world: WorldSnapshot,
    probe_history: ProbeHistory,
    /// The numerical run generation whose recorder data is currently held.
    /// A domain reset creates a fresh t=0 run and must not join its samples to
    /// the previous lattice's history.
    run_generation: u64,
    active_transform: Option<ActiveTransformDrag>,
    /// Set from the last UI frame: an inspector control is being held.
    inspector_editing: bool,
    /// The interactive edit currently in progress, if any.
    edit_gesture: Option<EditGesture>,
    frame_stats: FrameStats,
    /// When the next frame is due. Drives the event loop's control flow.
    next_redraw: Instant,
    /// Set from `WindowEvent::Occluded`; suppresses rendering entirely.
    occluded: bool,
    /// Whether an embedded MCP server is running against `data_source`, and
    /// its token, if so. Disabled until the user opts in from the MCP panel.
    mcp: McpSession,
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
            GpuElectrostaticEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let maxwell: Arc<dyn MaxwellSolverBackend> =
            Arc::new(GpuMaxwellBackend::new(compute_device, compute_queue));
        let data_source = create_local_data_source(evaluator, maxwell)?;
        let world = data_source.world();
        let run_generation = data_source.simulation_status().run_generation;
        // Which layer opens visible is `UiModel`'s own rule — it reveals the
        // first field a session sees — so the shell does not also decide it here
        // and leave two places that can disagree.
        let ui_model = UiModel::new();

        Ok(Self {
            egui_state,
            renderer,
            window,
            egui_context,
            camera: OrbitCamera::default(),
            ui_model,
            viewport: Viewport::default(),
            data_source: Arc::new(Mutex::new(HeadlessServer::new(data_source))),
            world,
            probe_history: ProbeHistory::default(),
            run_generation,
            active_transform: None,
            inspector_editing: false,
            edit_gesture: None,
            frame_stats: FrameStats::default(),
            next_redraw: Instant::now(),
            occluded: false,
            mcp: McpSession::default(),
        })
    }

    /// Lock the shared model, for a call site that doesn't also need to
    /// mutate another field of `self` while holding the guard (this being a
    /// `&self` method ties the guard's lifetime to all of `self`, not just
    /// `data_source` — a site that needs both, like `refresh_world`, calls
    /// [`lock_model`] directly on the field instead).
    /// `unwrap_or_else(PoisonError::into_inner)` rather than a bare
    /// `.unwrap()`: `std::sync::Mutex` (unlike `tokio::sync::Mutex`)
    /// poisons on a panic-while-held, and a panic reachable from an MCP tool
    /// call on another thread must not crash this window's next frame on
    /// its own next lock attempt.
    fn model(&self) -> MutexGuard<'_, HeadlessServer> {
        lock_model(&self.data_source)
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

        // If an embedded MCP server died after a successful bind (a panic in
        // a tool handler, the listener erroring out), stop claiming it's
        // still `Running` with a token nothing answers to.
        if let McpSession::Running(running) = &self.mcp
            && let Some(error) = mcp::check_alive(running)
        {
            self.mcp = McpSession::Failed(error);
        }

        // Advance the source by real elapsed time, not by one tick per frame.
        // The numerical `dt` is the source's business; a slow frame must not
        // change it. One lock hold: this thread is the only place that
        // drains completion events for its own commands, but an MCP tool
        // call sharing this model registers its own waiter and is notified
        // by whichever side calls `drain_command_events` next — see
        // `HeadlessServer::drain_events`.
        {
            let mut model = lock_model(&self.data_source);
            model
                .poll(elapsed)
                .map_err(|error| format!("simulation update failed: {error}"))?;
            for event in model.drain_command_events() {
                match event {
                    CommandEvent::Completed(receipt) => {
                        self.ui_model.command_error = None;
                        tracing::debug!(
                            command = receipt.command.get(),
                            disposition = ?receipt.disposition,
                            "compute command completed"
                        );
                    }
                    CommandEvent::Failed { command, error } => {
                        self.ui_model.command_error = Some(error.to_string());
                        tracing::warn!(command = command.get(), %error, "compute command rejected");
                    }
                }
            }
        }
        self.refresh_world();

        if self.occluded {
            // Nothing can be presented, so do no GPU work at all and check back
            // periodically. Simulation time still advances above.
            self.set_next_redraw(OCCLUDED_RETRY_INTERVAL);
            return Ok(());
        }

        let compute = ComputeView::build(&*self.model(), &self.world);
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let pixels_per_point_before_frame = self.egui_context.pixels_per_point().max(0.01);
        let transform_preview = self.active_transform.map(ActiveTransformDrag::preview);
        let plane_normal_label = self
            .ui_model
            .scene_selection()
            .and_then(|selection| {
                scene::plane_normal_label_position(
                    &self.world,
                    &self.camera,
                    self.viewport,
                    selection,
                    transform_preview,
                )
            })
            .map(|position| {
                egui::pos2(
                    position.x / pixels_per_point_before_frame,
                    position.y / pixels_per_point_before_frame,
                )
            });
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
                    plane_normal_label,
                    plane_normal_active: self
                        .active_transform
                        .is_some_and(|drag| drag.constraint == ManipulationConstraint::PlaneNormal),
                    paused_for_edit: self.edit_gesture.is_some_and(|gesture| gesture.resume),
                    edit_in_progress: self.edit_gesture.is_some(),
                    projection: self.camera.projection(),
                    mcp: &self.mcp,
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
        if let Some(action) = ui_frame.mcp_action {
            self.apply_mcp_action(action);
        }
        // Before the frame's own commands are dispatched: a held inspector
        // control submits an edit every frame, and the pause has to precede the
        // first of them rather than arrive a frame late.
        self.inspector_editing = ui_frame.scene_edit_in_progress;
        self.synchronize_edit_gesture(compute.mode)?;
        self.apply_viewport_gesture(ui_frame.viewport_gesture, pixels_per_point, compute.mode)?;

        // In submission order, which is also the order queued edits are applied
        // in at a tick boundary (ADR 0011). Goes through the shared model's
        // own `submit`, minting from its one `CommandSequencer` — the same
        // one MCP tool calls use — rather than a private per-window
        // sequencer, so two transports sharing this model can never mint the
        // same `CommandId`.
        if !ui_frame.commands.is_empty() {
            for payload in std::mem::take(&mut ui_frame.commands) {
                self.model()
                    .submit(payload)
                    .map_err(|error| format!("simulation command failed: {error}"))?;
            }
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
        let next_frame_delay = if compute.mode == fieldcad_core::SimulationMode::Running
            || self.model().pending_command_count() > 0
        {
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
        let show = self.scene_visibility();
        let instances = scene::instances(&self.world, self.ui_model.selection, show);
        // Every visible layer names a channel declared by the snapshot. Several
        // channels (for example Maxwell E and B) can be drawn independently.
        let mut field = scene::FieldGeometry::default();
        if let Some(snapshot) = self.model().latest_snapshot() {
            for (channel, layer) in &self.ui_model.field_layers {
                if !layer.visible || !compute.vector_channels.contains(channel) {
                    continue;
                }
                let layer_geometry = scene::field_geometry(
                    &snapshot,
                    channel,
                    layer.whole_domain,
                    &layer.planes,
                    &layer.boxes,
                    &layer.spheres,
                );
                field
                    .surface_triangles
                    .extend(layer_geometry.surface_triangles);
                field.vector_lines.extend(layer_geometry.vector_lines);
            }
        }
        scene::append_authoring_geometry(
            &mut field,
            &self.world,
            self.ui_model.scene_selection(),
            show,
        );
        // The gizmo is drawn only for a selection that is actually on screen.
        // Handles floating over a hidden object would be draggable targets for
        // something the user cannot see.
        scene::append_transform_gizmo(
            &mut field,
            &self.world,
            &self.camera,
            self.viewport,
            self.visible_selection(),
            self.active_transform
                .and_then(|drag| drag.constraint.handle()),
            transform_preview,
        );

        let status = self.renderer.render(
            SceneFrame {
                camera: &self.camera,
                viewport: self.viewport,
                grid_visible: self.ui_model.view.grid,
                axes_visible: self.ui_model.view.axes,
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
        let model = lock_model(&self.data_source);
        let generation = model.simulation_status().run_generation;
        if generation != self.run_generation {
            self.probe_history = ProbeHistory::new(self.probe_history.capacity());
            self.run_generation = generation;
        }
        if let Some(snapshot) = model.latest_snapshot() {
            self.probe_history.record(&snapshot);
        }
        self.world = model.world();
        drop(model);

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
            .box_selection
            .is_some_and(|id| !self.world.boxes().contains_key(&id))
        {
            self.ui_model.box_selection = None;
        }
        if self
            .ui_model
            .sphere_selection
            .is_some_and(|id| !self.world.spheres().contains_key(&id))
        {
            self.ui_model.sphere_selection = None;
        }
        if self
            .ui_model
            .probe_selection
            .is_some_and(|id| self.world.probe(id).is_none())
        {
            self.ui_model.probe_selection = None;
        }
        for layer in self.ui_model.field_layers.values_mut() {
            layer
                .planes
                .retain(|id, _| self.world.planes().contains_key(id));
            layer
                .boxes
                .retain(|id, _| self.world.boxes().contains_key(id));
            layer
                .spheres
                .retain(|id, _| self.world.spheres().contains_key(id));
        }
        // Each series is bounded, but the set of them is not: probe IDs are
        // never reused, so deleted probes would accumulate for the session.
        self.probe_history
            .retain_probes(|probe| self.world.probe(probe).is_some());
        if self
            .active_transform
            .is_some_and(|drag| scene::selection_origin(&self.world, drag.target).is_none())
        {
            self.active_transform = None;
        }
    }

    fn apply_camera_action(&mut self, action: Option<CameraAction>) {
        match action {
            Some(CameraAction::Axis(view)) => self.camera.set_axis_view(view),
            Some(CameraAction::FocusSelection) => self.focus_selection(),
            Some(CameraAction::Reset) => self.camera.reset(),
            Some(CameraAction::SetProjection(projection)) => {
                self.camera.set_projection(projection);
            }
            None => {}
        }
    }

    fn apply_viewport_gesture(
        &mut self,
        gesture: ViewportGesture,
        pixels_per_point: f32,
        mode: SimulationMode,
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

        // `visible_selection` rather than `scene_selection`: a hidden entity
        // has no handles on screen, so it must not start a drag either.
        if gesture.primary_pressed
            && let (Some(selection), Some(pointer)) = (self.visible_selection(), pointer)
            && let Some(origin) = scene::selection_origin(&self.world, selection)
        {
            let picked_handle = scene::pick_transform_handle(
                &self.world,
                selection,
                &self.camera,
                self.viewport,
                pointer,
            );
            let constraint = match picked_handle {
                Some(TransformHandle::PlaneNormal) => Some(ManipulationConstraint::PlaneNormal),
                Some(
                    handle @ (TransformHandle::RotateX
                    | TransformHandle::RotateY
                    | TransformHandle::RotateZ
                    | TransformHandle::RotateView
                    | TransformHandle::RotateFree),
                ) => Some(ManipulationConstraint::Rotate(handle)),
                Some(handle) => Some(ManipulationConstraint::Handle(handle)),
                None if scene::pick_scene(
                    &self.world,
                    self.scene_visibility(),
                    &self.camera,
                    self.viewport,
                    pointer,
                ) == Some(selection) =>
                {
                    Some(ManipulationConstraint::ViewPlane)
                }
                None => None,
            };
            if let Some(constraint) = constraint {
                let plane_frame = match selection {
                    scene::SceneSelection::Plane(id) => {
                        self.world.planes().get(&id).map(|plane| PlaneFrame {
                            normal: plane.normal,
                            u_axis: plane.u_axis,
                        })
                    }
                    _ => None,
                };
                let box_frame = match selection {
                    scene::SceneSelection::Box(id) => {
                        self.world.boxes().get(&id).map(|field_box| BoxFrame {
                            rotation: field_box.rotation,
                        })
                    }
                    _ => None,
                };
                self.active_transform = Some(ActiveTransformDrag {
                    target: selection,
                    constraint,
                    origin: origin.as_dvec3(),
                    plane_frame,
                    box_frame,
                });
                // Immediately, not at the end of the frame: the same frame that
                // starts the drag can already submit its first move.
                self.synchronize_edit_gesture(mode)?;
            }
        }

        if gesture.primary_dragged
            && let (Some(active), Some(pointer)) = (self.active_transform, pointer)
        {
            match active.constraint {
                ManipulationConstraint::PlaneNormal => {
                    self.drag_plane_normal(active, pointer)?;
                }
                ManipulationConstraint::Rotate(handle) => {
                    self.drag_box_rotation(active, handle, pointer, pointer_delta)?;
                }
                ManipulationConstraint::Handle(handle) => {
                    let length = scene::selection_gizmo_length(
                        &self.world,
                        &self.camera,
                        self.viewport,
                        active.target,
                    )
                    .ok_or_else(|| "selected entity no longer has a transform gizmo".to_owned())?;
                    if let Some(translation) = scene::constrained_translation(
                        handle,
                        &self.camera,
                        self.viewport,
                        pointer,
                        pointer_delta,
                        active.origin.as_vec3(),
                        length,
                    ) && translation.length_squared() > 0.0
                    {
                        self.translate_selection(active, translation.as_dvec3())?;
                    }
                }
                ManipulationConstraint::ViewPlane => {
                    if let Some(translation) = scene::view_plane_translation(
                        &self.camera,
                        self.viewport,
                        pointer,
                        pointer_delta,
                        active.origin.as_vec3(),
                    ) && translation.length_squared() > 0.0
                    {
                        self.translate_selection(active, translation.as_dvec3())?;
                    }
                }
            }
        }

        let drag_consumed = was_active || self.active_transform.is_some();
        if gesture.primary_clicked
            && !drag_consumed
            && let Some(pointer) = pointer
        {
            self.ui_model.set_scene_selection(scene::pick_scene(
                &self.world,
                self.scene_visibility(),
                &self.camera,
                self.viewport,
                pointer,
            ));
        }
        if gesture.primary_released {
            self.active_transform = None;
        }
        self.synchronize_edit_gesture(mode)
    }

    /// What the 3D view is currently drawing, as the scene module sees it.
    ///
    /// One accessor, so drawing, gizmos, and hit-testing cannot be given
    /// different answers.
    fn scene_visibility(&self) -> scene::SceneVisibility {
        scene::SceneVisibility {
            objects: self.ui_model.view.objects,
            probes: self.ui_model.view.probes,
            planes: self.ui_model.view.planes,
            boxes: self.ui_model.view.boxes,
            spheres: self.ui_model.view.spheres,
        }
    }

    /// The current selection, but only while it is on screen.
    ///
    /// Selection itself survives a class being hidden — the scene list and the
    /// inspector still show it — but nothing that requires seeing the thing is
    /// offered for it.
    fn visible_selection(&self) -> Option<scene::SceneSelection> {
        self.ui_model
            .scene_selection()
            .filter(|selection| self.scene_visibility().shows(*selection))
    }

    fn translate_selection(
        &mut self,
        active: ActiveTransformDrag,
        translation: DVec3,
    ) -> Result<(), String> {
        let next_origin = active.origin + translation;
        let world_command = match active.target {
            scene::SceneSelection::Object(object_id) => {
                let object = self
                    .world
                    .object(object_id)
                    .ok_or_else(|| format!("object {object_id} no longer exists"))?;
                WorldCommand::SetTransform {
                    object: object_id,
                    transform: Transform::new(next_origin, object.transform.rotation)
                        .map_err(|error| error.to_string())?,
                }
            }
            scene::SceneSelection::Plane(plane_id) => {
                let plane = self
                    .world
                    .planes()
                    .get(&plane_id)
                    .ok_or_else(|| format!("plane {plane_id} no longer exists"))?;
                WorldCommand::SetPlane {
                    plane: plane_id,
                    spec: SlicePlaneSpec::from_plane(plane)
                        .with_origin(next_origin)
                        .map_err(|error| error.to_string())?,
                }
            }
            scene::SceneSelection::Probe(probe_id) => {
                let probe = self
                    .world
                    .probe(probe_id)
                    .ok_or_else(|| format!("probe {probe_id} no longer exists"))?;
                let position = match probe.position {
                    ProbePosition::World(_) => ProbePosition::World(next_origin),
                    ProbePosition::Attached { object, .. } => {
                        let parent = self
                            .world
                            .object(object)
                            .ok_or_else(|| format!("attached object {object} no longer exists"))?;
                        ProbePosition::Attached {
                            object,
                            offset: parent.transform.rotation.inverse()
                                * (next_origin - parent.transform.translation),
                        }
                    }
                };
                WorldCommand::SetProbePosition {
                    probe: probe_id,
                    position,
                }
            }
            scene::SceneSelection::Box(region_id) => {
                let field_box = self
                    .world
                    .boxes()
                    .get(&region_id)
                    .ok_or_else(|| format!("field box {region_id} no longer exists"))?;
                WorldCommand::SetBox {
                    region: region_id,
                    spec: FieldBoxSpec::from_box(field_box)
                        .with_origin(next_origin)
                        .map_err(|error| error.to_string())?,
                }
            }
            scene::SceneSelection::Sphere(sphere_id) => {
                let sphere = self
                    .world
                    .spheres()
                    .get(&sphere_id)
                    .ok_or_else(|| format!("field sphere {sphere_id} no longer exists"))?;
                WorldCommand::SetSphere {
                    sphere: sphere_id,
                    spec: FieldSphereSpec::from_sphere(sphere)
                        .with_origin(next_origin)
                        .map_err(|error| error.to_string())?,
                }
            }
        };
        if let Some(current) = self.active_transform.as_mut() {
            current.origin = next_origin;
        }
        self.submit_world_manipulation(world_command, "scene move")
    }

    fn drag_plane_normal(
        &mut self,
        active: ActiveTransformDrag,
        pointer: Vec2,
    ) -> Result<(), String> {
        let scene::SceneSelection::Plane(plane_id) = active.target else {
            return Ok(());
        };
        let Some(frame) = active.plane_frame else {
            return Ok(());
        };
        let plane = self
            .world
            .planes()
            .get(&plane_id)
            .ok_or_else(|| format!("plane {plane_id} no longer exists"))?;
        let (_, tip) = scene::plane_normal_tip(
            &self.world,
            &self.camera,
            self.viewport,
            active.target,
            Some(active.preview()),
        )
        .ok_or_else(|| "selected plane no longer has a normal handle".to_owned())?;
        let radius = tip.distance(active.origin.as_vec3());
        let Some(normal) = scene::dragged_plane_normal(
            &self.camera,
            self.viewport,
            pointer,
            active.origin.as_vec3(),
            radius,
            frame.normal.as_vec3(),
        ) else {
            return Ok(());
        };
        let normal = normal.as_dvec3().normalize();
        if normal.dot(frame.normal) > 1.0 - 1.0e-12 {
            return Ok(());
        }
        let rotation = DQuat::from_rotation_arc(frame.normal, normal);
        let u_axis = (rotation * frame.u_axis).normalize();
        let spec = SlicePlaneSpec::new(&plane.name, active.origin, normal)
            .and_then(|spec| spec.with_u_axis(u_axis))
            .and_then(|spec| spec.with_half_extent(plane.half_extent))
            .map(|spec| spec.with_visibility(plane.visible))
            .map_err(|error| error.to_string())?;
        if let Some(current) = self.active_transform.as_mut() {
            current.plane_frame = Some(PlaneFrame { normal, u_axis });
        }
        self.submit_world_manipulation(
            WorldCommand::SetPlane {
                plane: plane_id,
                spec,
            },
            "plane rotation",
        )
    }

    fn drag_box_rotation(
        &mut self,
        active: ActiveTransformDrag,
        handle: TransformHandle,
        pointer: Vec2,
        pointer_delta: Vec2,
    ) -> Result<(), String> {
        let scene::SceneSelection::Box(region_id) = active.target else {
            return Ok(());
        };
        let Some(frame) = active.box_frame else {
            return Ok(());
        };
        let field_box = self
            .world
            .boxes()
            .get(&region_id)
            .ok_or_else(|| format!("field box {region_id} no longer exists"))?;
        let current_rotation = scene::quat_from_dquat(frame.rotation);
        let origin = active.origin.as_vec3();
        let rotated = match handle {
            TransformHandle::RotateX | TransformHandle::RotateY | TransformHandle::RotateZ => {
                let Some(local_axis) = handle.rotation_axis() else {
                    return Ok(());
                };
                scene::dragged_box_rotation(
                    &self.camera,
                    self.viewport,
                    pointer,
                    pointer_delta,
                    origin,
                    local_axis,
                    current_rotation,
                )
            }
            TransformHandle::RotateView => scene::dragged_view_rotation(
                &self.camera,
                self.viewport,
                pointer,
                pointer_delta,
                origin,
                current_rotation,
            ),
            TransformHandle::RotateFree => {
                let Some(radius) = scene::rotation_gizmo_radius(
                    &self.world,
                    &self.camera,
                    self.viewport,
                    active.target,
                ) else {
                    return Ok(());
                };
                scene::dragged_trackball_rotation(
                    &self.camera,
                    self.viewport,
                    pointer,
                    pointer_delta,
                    origin,
                    radius,
                    current_rotation,
                )
            }
            _ => return Ok(()),
        };
        let Some(rotation) = rotated else {
            return Ok(());
        };
        let rotation = DQuat::from_xyzw(
            rotation.x as f64,
            rotation.y as f64,
            rotation.z as f64,
            rotation.w as f64,
        )
        .normalize();
        let spec = FieldBoxSpec::from_box(field_box)
            .with_rotation(rotation)
            .map_err(|error| error.to_string())?;
        if let Some(current) = self.active_transform.as_mut() {
            current.box_frame = Some(BoxFrame { rotation });
        }
        self.submit_world_manipulation(
            WorldCommand::SetBox {
                region: region_id,
                spec,
            },
            "box rotation",
        )
    }

    fn submit_world_manipulation(
        &mut self,
        world_command: WorldCommand,
        operation: &str,
    ) -> Result<(), String> {
        self.submit(
            fieldcad_simulation::CommandPayload::CommitWorld(vec![world_command]),
            operation,
        )?;
        self.refresh_world();
        Ok(())
    }

    fn submit(
        &mut self,
        payload: fieldcad_simulation::CommandPayload,
        operation: &str,
    ) -> Result<(), String> {
        self.model()
            .submit(payload)
            .map_err(|error| format!("{operation} failed: {error}"))?;
        Ok(())
    }

    /// Enable or disable the embedded MCP server against this window's
    /// shared model. `Enable` blocks briefly (see `crate::mcp::enable`) —
    /// acceptable for a rare, explicit button click.
    fn apply_mcp_action(&mut self, action: McpAction) {
        match action {
            McpAction::Enable => {
                self.mcp = match mcp::enable(self.data_source.clone()) {
                    Ok(running) => McpSession::Running(running),
                    Err(error) => McpSession::Failed(error),
                };
            }
            McpAction::Disable => {
                if let McpSession::Running(running) =
                    std::mem::replace(&mut self.mcp, McpSession::Disabled)
                {
                    mcp::disable(running);
                }
            }
        }
    }

    /// Whether anything is currently editing the scene.
    ///
    /// One accessor over both input paths, so a drag in the viewport and a value
    /// held in the inspector cannot be treated as different kinds of edit.
    const fn scene_is_being_edited(&self) -> bool {
        self.active_transform.is_some() || self.inspector_editing
    }

    /// Open or close the interactive edit to match what the user is doing.
    ///
    /// Idempotent, and called at every point in a frame where either input path
    /// can change: the gesture's boundaries have to be exact, because the pause
    /// must land before the first edit it brackets and the resume after the
    /// last.
    fn synchronize_edit_gesture(&mut self, mode: SimulationMode) -> Result<(), String> {
        let (next, commands) =
            EditGesture::transition(self.edit_gesture, self.scene_is_being_edited(), mode);
        self.edit_gesture = next;
        for payload in commands {
            self.submit(payload, "scene edit")?;
        }
        Ok(())
    }

    fn focus_selection(&mut self) {
        let Some(selection) = self.ui_model.scene_selection() else {
            return;
        };
        match selection {
            scene::SceneSelection::Object(id) => {
                let Some(object) = self.world.object(id) else {
                    return;
                };
                let (centre, radius) = object.bounding_sphere();
                self.camera.focus(centre.as_vec3(), radius as f32);
            }
            scene::SceneSelection::Plane(id) => {
                let Some(plane) = self.world.planes().get(&id) else {
                    return;
                };
                self.camera
                    .focus(plane.origin.as_vec3(), plane.half_extent.length() as f32);
            }
            scene::SceneSelection::Probe(id) => {
                let Some(probe) = self.world.probe(id) else {
                    return;
                };
                if let Ok(position) = self.world.resolve_probe_position(probe) {
                    self.camera.focus(position.as_vec3(), 0.2);
                }
            }
            scene::SceneSelection::Box(id) => {
                let Some(field_box) = self.world.boxes().get(&id) else {
                    return;
                };
                self.camera
                    .focus(field_box.origin.as_vec3(), field_box.half_extent.length() as f32);
            }
            scene::SceneSelection::Sphere(id) => {
                let Some(sphere) = self.world.spheres().get(&id) else {
                    return;
                };
                self.camera
                    .focus(sphere.origin.as_vec3(), sphere.radius as f32);
            }
        };
    }
}

/// A scene edit that spans frames: a viewport drag, or an inspector control
/// held down or being typed into.
///
/// Dragging a body from one place to another is authoring, not motion: it
/// teleports the object, and the intermediate poses are not states the equations
/// ever produced. Letting a simulation advance through that would interleave
/// solver ticks with values nothing computed, and the result would be neither
/// the trajectory the solver was following nor the experiment the user is
/// building. So the gesture suspends the run and hands it back when it commits.
///
/// The gesture is the desktop's, not the runtime's — it is made of pointer
/// events — but its effect is authoritative and reaches the source as ordinary
/// correlated commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditGesture {
    /// The simulation was advancing when this gesture began, and so is resumed
    /// when it commits. A run the user had already paused stays paused.
    resume: bool,
}

impl EditGesture {
    /// What opening or closing a gesture asks of the source, and the state that
    /// leaves behind.
    ///
    /// Pure, because the ordering here is the whole contract and it is not worth
    /// standing up a window to check: the pause precedes the edits it brackets,
    /// the commit precedes the resume, and a run the user had already paused is
    /// given back exactly as it was found.
    fn transition(
        current: Option<Self>,
        editing: bool,
        mode: SimulationMode,
    ) -> (Option<Self>, Vec<CommandPayload>) {
        match (editing, current) {
            (true, None) => {
                let resume = mode == SimulationMode::Running;
                let mut commands = Vec::new();
                if resume {
                    commands.push(CommandPayload::Pause);
                }
                commands.push(CommandPayload::SetInteractiveEdit(true));
                (Some(Self { resume }), commands)
            }
            (false, Some(gesture)) => {
                let mut commands = vec![CommandPayload::SetInteractiveEdit(false)];
                if gesture.resume {
                    commands.push(CommandPayload::Play);
                }
                (None, commands)
            }
            (true, Some(gesture)) => (Some(gesture), Vec::new()),
            (false, None) => (None, Vec::new()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActiveTransformDrag {
    target: scene::SceneSelection,
    constraint: ManipulationConstraint,
    /// Latest absolute origin submitted during this drag. The
    /// authoritative world intentionally remains unchanged while Running edits
    /// wait for a tick boundary, so accumulating against the replica would lose
    /// pointer deltas between ticks.
    origin: DVec3,
    plane_frame: Option<PlaneFrame>,
    box_frame: Option<BoxFrame>,
}

impl ActiveTransformDrag {
    fn preview(self) -> scene::TransformPreview {
        scene::TransformPreview {
            origin: self.origin.as_vec3(),
            plane_normal: self.plane_frame.map(|frame| frame.normal.as_vec3()),
            rotation: self
                .box_frame
                .map(|frame| scene::quat_from_dquat(frame.rotation)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManipulationConstraint {
    Handle(TransformHandle),
    ViewPlane,
    PlaneNormal,
    /// Rotates a selected field box about one of its own rings; never
    /// translates it. Kept apart from `Handle` because `constrained_translation`
    /// has nothing to do with a rotation handle.
    Rotate(TransformHandle),
}

impl ManipulationConstraint {
    const fn handle(self) -> Option<TransformHandle> {
        match self {
            Self::Handle(handle) | Self::Rotate(handle) => Some(handle),
            Self::ViewPlane => None,
            Self::PlaneNormal => Some(TransformHandle::PlaneNormal),
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
                TransformHandle::PlaneNormal => "Rotate plane · normal N",
                TransformHandle::RotateX
                | TransformHandle::RotateY
                | TransformHandle::RotateZ
                | TransformHandle::RotateView
                | TransformHandle::RotateFree => "Rotate box",
            },
            Self::ViewPlane => "Free move · camera plane",
            Self::PlaneNormal => "Rotate plane · normal N",
            Self::Rotate(TransformHandle::RotateX) => "Rotate box · local X",
            Self::Rotate(TransformHandle::RotateY) => "Rotate box · local Y",
            Self::Rotate(TransformHandle::RotateZ) => "Rotate box · local Z",
            Self::Rotate(TransformHandle::RotateView) => "Rotate box · view axis",
            Self::Rotate(TransformHandle::RotateFree) => "Rotate box · free",
            Self::Rotate(_) => "Rotate box",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PlaneFrame {
    normal: DVec3,
    u_axis: DVec3,
}

/// A field box's orientation at the last drag step, updated after each
/// rotation-ring move the same way [`PlaneFrame`] tracks a plane's normal.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BoxFrame {
    rotation: DQuat,
}

fn create_local_data_source(
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
    maxwell: Arc<dyn MaxwellSolverBackend>,
) -> Result<AsyncLocalDataSource, String> {
    let domain = Domain::new(
        DomainBounds::centred_cube(5.0).map_err(|error| error.to_string())?,
        Resolution::uniform(32).map_err(|error| error.to_string())?,
        BoundaryConditions::uniform(BoundaryCondition::Periodic),
        Precision::F32,
    );
    let time_step =
        TimeStep::from_seconds(courant_limit(&domain) * 0.8).map_err(|error| error.to_string())?;
    let mut runtime = SimulationRuntime::new(
        RuntimeConfig::new(domain, time_step, SessionId::from_u128(1))
            .with_subscription(
                Subscription::PROBES_ONLY
                    .with_planes(UVec2::splat(33))
                    .with_domain_stride(8)
                    .with_boxes(UVec3::splat(9))
                    .with_spheres(9),
            )
            // Two models of one electric field. Both are composed into the
            // scene so either can compute it, and the analytic one is active
            // because the default scene is a single stationary charge — a case
            // it answers exactly and immediately. Choosing Maxwell instead is
            // one control in the inspector's Fields section, and brings the
            // magnetic field with it.
            .with_plugin(Box::new(ElectrostaticsPlugin::with_evaluator(evaluator)))
            .with_plugin_registration(
                PluginRegistration::with_default_configuration(Box::new(
                    ElectromagnetismPlugin::with_backend(maxwell),
                ))
                .with_enabled(false),
            ),
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
                // One entry for the electric field, whichever model computes
                // it. The rest are Maxwell's own method diagnostics, recorded
                // when that model is the active one.
                vec![
                    electric_field_channel_id(),
                    electric_potential_channel_id(),
                    maxwell_magnetic_field_channel_id(),
                    maxwell_energy_density_channel_id(),
                    maxwell_electric_divergence_channel_id(),
                    maxwell_magnetic_divergence_channel_id(),
                ],
            )),
            WorldCommand::CreatePlane(
                SlicePlaneSpec::new("XY field plane", DVec3::ZERO, DVec3::Z)
                    .and_then(|plane| plane.with_half_extent(DVec2::splat(4.0)))
                    .map_err(|error| error.to_string())?,
            ),
        ])
        .map_err(|error| error.to_string())?;
    // The default scene is where this session starts, not the user's first
    // edit. Without this the opening undo would empty the workspace.
    runtime.clear_edit_history();

    Ok(AsyncLocalDataSource::new(LocalDataSource::new(runtime)))
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
    use super::{EditGesture, FrameStats};
    use fieldcad_core::SimulationMode;
    use fieldcad_simulation::CommandPayload;
    use std::time::Duration;

    /// The order is the point: the pause has to arrive before the first edit of
    /// the gesture, and the resume after the commit that brings deferred systems
    /// current — otherwise the resumed run starts from a field nothing
    /// recomputed.
    #[test]
    fn a_running_simulation_is_suspended_for_an_edit_and_resumed_when_it_commits() {
        let (opened, commands) = EditGesture::transition(None, true, SimulationMode::Running);

        assert_eq!(
            commands,
            vec![
                CommandPayload::Pause,
                CommandPayload::SetInteractiveEdit(true)
            ]
        );

        // Nothing further while the gesture is held, however many frames it
        // spans.
        let (held, commands) = EditGesture::transition(opened, true, SimulationMode::Paused);
        assert_eq!(held, opened);
        assert!(commands.is_empty());

        let (closed, commands) = EditGesture::transition(held, false, SimulationMode::Paused);
        assert_eq!(closed, None);
        assert_eq!(
            commands,
            vec![
                CommandPayload::SetInteractiveEdit(false),
                CommandPayload::Play
            ]
        );
    }

    /// The gesture hands the transport back as it found it. Resuming a run the
    /// user had deliberately paused would make dragging a body start the
    /// simulation.
    #[test]
    fn editing_an_already_paused_simulation_leaves_it_paused() {
        let (opened, commands) = EditGesture::transition(None, true, SimulationMode::Paused);

        assert_eq!(commands, vec![CommandPayload::SetInteractiveEdit(true)]);

        let (closed, commands) = EditGesture::transition(opened, false, SimulationMode::Paused);

        assert_eq!(closed, None);
        assert_eq!(commands, vec![CommandPayload::SetInteractiveEdit(false)]);
    }

    #[test]
    fn no_edit_in_progress_asks_nothing_of_the_source() {
        let (gesture, commands) = EditGesture::transition(None, false, SimulationMode::Running);

        assert_eq!(gesture, None);
        assert!(commands.is_empty());
    }

    #[test]
    fn frame_diagnostics_measure_work_not_time_spent_idle() {
        let mut stats = FrameStats::default();

        stats.finish_frame(Duration::from_millis(10));
        stats.finish_frame(Duration::from_millis(2));

        assert!((stats.smoothed_frame_ms - 9.2).abs() < 1.0e-6);
    }
}
