use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    time::{Duration, Instant},
};

use fieldcad_core::quantities::{ChargeCoulombs, LengthMetres, SiScalar};
use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, ChannelId, Domain, DomainBounds, FieldBoxSpec,
    FieldSphereSpec, ObjectShape, ObjectSpec, Precision, ProbePosition, ProbeSpec, Resolution,
    SessionId, SimulationMode, SlicePlaneSpec, TimeStep, Transform, WorldCommand, WorldRevision,
    WorldSnapshot,
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
    ElectrostaticsPlugin, electric_field_channel_id, electric_potential_channel_id,
};
use fieldcad_gravity::NewtonianGravityPlugin;
use fieldcad_plugin_api::{FieldBrushFalloff, FieldBrushStroke};
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{
    AsyncLocalDataSource, CommandEvent, CommandId, CommandPayload, DistanceHistory,
    FieldDataSource, LocalDataSource, MassAggregateHistory, PluginRegistration, ProbeHistory,
    RuntimeConfig, SimulationRuntime, Subscription,
};
use fieldcad_superposition::InverseSquareBatchEvaluator;
use glam::{DQuat, DVec2, DVec3, UVec2, UVec3, Vec2, Vec4};
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
    gpu_inverse_square::GpuInverseSquareEvaluator,
    mcp::{self, McpAction, McpSession},
    probe_history_state,
    profile::UserProfile,
    renderer::{GuiPaint, RenderStatus, SceneFrame, ViewportRenderer},
    scene::{self, TransformHandle},
    scene_view_state,
    ui::{self, AppAction, CameraAction, ComputeView, UiModel, ViewportGesture, ViewportTool},
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
    run_for(LaunchOptions::default())
}

/// What a caller (currently only `main.rs`'s CLI parsing) can ask of a run
/// before the window exists.
#[derive(Default)]
pub struct LaunchOptions {
    /// Quit by itself after this long, instead of running until the window
    /// closes.
    pub lifetime: Option<Duration>,
    /// Start with the embedded MCP server already listening here, its bearer
    /// token printed to the startup log instead of waiting for a user to
    /// open the MCP panel — for an agent that launches this process itself
    /// and needs the token before it can connect.
    pub mcp: Option<SocketAddr>,
    /// Load this `fieldcad.scene/v1` document at startup instead of the
    /// built-in demo scene — for `field-cad path/to/scene.fcscene` from a
    /// terminal. A path that fails to load is a startup error, not a silent
    /// fall-back to the demo scene: a user naming a specific file expects
    /// that file, not a surprise substitute.
    pub open_path: Option<PathBuf>,
}

/// Run the application per `options`.
///
/// A self-imposed deadline (`options.lifetime`) makes an interactive test
/// safe to attempt on a machine where a windowed run has previously wedged
/// the compositor: the process leaves on its own rather than needing to be
/// killed from elsewhere.
pub fn run_for(options: LaunchOptions) -> Result<(), RunError> {
    let event_loop = EventLoop::new()?;
    // Deliberately not `Poll`. Requesting a redraw unconditionally on every
    // iteration keeps a Wayland compositor permanently busy with this client
    // and never lets the loop idle; redraws are demand-driven instead, from
    // egui's requested repaint time and whether the simulation is advancing.
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut application = DesktopApplication {
        deadline: options.lifetime.map(|lifetime| Instant::now() + lifetime),
        mcp_autostart: options.mcp,
        open_path: options.open_path,
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
    /// Consumed by the first `resumed()` — `WindowState::new` starts the MCP
    /// server itself, once, rather than this being re-applied on every
    /// suspend/resume cycle a window can go through.
    mcp_autostart: Option<SocketAddr>,
    /// Consumed by the first `resumed()`, same reasoning as `mcp_autostart`.
    open_path: Option<PathBuf>,
}

impl ApplicationHandler for DesktopApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window_state.is_some() {
            return;
        }

        match WindowState::new(event_loop, self.mcp_autostart.take(), self.open_path.take()) {
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

/// The window title for `known_path` — just the file name, not the full
/// path (the title bar is for "which document", not a path browser).
fn window_title(known_path: Option<&Path>) -> String {
    match known_path.and_then(|path| path.file_name()) {
        Some(name) => format!("{} — Field CAD", name.to_string_lossy()),
        None => "Field CAD".to_owned(),
    }
}

/// One region's memoized contribution to the channel-layer geometry, plus
/// the inputs it was built from — see [`compute_field_layer_geometry`].
struct RegionGeometryCache {
    inputs: RegionGeometryInputs,
    geometry: Arc<scene::FieldGeometry>,
}

/// Everything one region's rendered geometry depends on — verified against
/// every `show.*`/`world.*().get(id).visible`/`layers.*.get(id)` read in
/// [`scene::region_geometry`]'s match arms, so this covers exactly what the
/// output can differ on, nothing more.
///
/// `batch` carries both this region's current sample positions (so a moved
/// plane's own batch content differs from last frame's) and the field
/// values sampled there (so a value-only change — a moved charge, a solver
/// tick — is caught too) — and it is *this*, not the snapshot's
/// `(session, sequence)` identity, that actually determines the output: a
/// sibling region's drag bumps the sequence without touching this region's
/// batch at all, and must not force a rebuild here. `world_visible` is kept
/// separate because a `Set*Visible` world edit can land with no new
/// snapshot published, so it needs to invalidate on its own.
#[derive(Clone, PartialEq)]
struct RegionGeometryInputs {
    batch: fieldcad_core::FieldBatch,
    settings: RegionSettings,
    world_visible: bool,
    show: scene::SceneVisibility,
    scene_scale: fieldcad_core::SceneScale,
}

/// The one region-type-specific settings value relevant to a given
/// [`scene::RegionId`], resolved the same way [`scene::region_geometry`]'s
/// match arms resolve it internally — kept as an enum rather than the whole
/// [`scene::RegionLayers`] bundle so that one plane's settings change does
/// not appear to invalidate every other region's cache entry too.
#[derive(Clone, Copy, PartialEq)]
enum RegionSettings {
    Plane(scene::PlaneLayerSettings),
    Box(scene::BoxLayerSettings),
    Sphere(scene::SphereLayerSettings),
    Grid(scene::FieldLayerSettings),
}

fn resolve_region_settings(
    region: scene::RegionId,
    layers: scene::RegionLayers<'_>,
    whole_domain: scene::FieldLayerSettings,
) -> RegionSettings {
    match region {
        scene::RegionId::Plane(id) => {
            RegionSettings::Plane(layers.planes.get(&id).copied().unwrap_or_default())
        }
        scene::RegionId::Box(id) => {
            RegionSettings::Box(layers.boxes.get(&id).copied().unwrap_or_default())
        }
        scene::RegionId::Sphere(id) => {
            RegionSettings::Sphere(layers.spheres.get(&id).copied().unwrap_or_default())
        }
        scene::RegionId::Grid => RegionSettings::Grid(whole_domain),
    }
}

/// The believed world's own visibility for a region — `true` unconditionally
/// for [`scene::RegionId::Grid`], which names no world entity to hide (see
/// [`scene::region_geometry`]'s `Grid` arm, which has no `world.*()` check).
fn region_world_visible(region: scene::RegionId, world: &WorldSnapshot) -> bool {
    match region {
        scene::RegionId::Plane(id) => world.planes().get(&id).is_some_and(|plane| plane.visible),
        scene::RegionId::Box(id) => world.boxes().get(&id).is_some_and(|region| region.visible),
        scene::RegionId::Sphere(id) => world
            .spheres()
            .get(&id)
            .is_some_and(|region| region.visible),
        scene::RegionId::Grid => true,
    }
}

/// A vector layer's triangles and arrows are pure functions of each visible
/// region's own published batch, its own layer settings, and a few global
/// draw-mode flags — see [`scene::region_geometry`]. None of that changes
/// between two frames of a paused, static scene, yet the interpolation this
/// drives (trilinear per glyph, both surface and vector passes, and RK4
/// flow-line tracing) is the most expensive thing this module does per
/// frame. Cached per `(channel, region)` rather than once for the whole
/// scene: a slice-plane drag commits — and republishes a snapshot for —
/// every pointer-move frame, and a whole-scene cache keyed on that
/// snapshot's `(session, sequence)` would treat every visible region as
/// stale on every one of those frames, retracing flow lines scene-wide for
/// a single moving plane.
///
/// `cache` is taken by value (moved out of `WindowState` via `mem::take`)
/// rather than borrowed, so a hit can move its entry into the returned map
/// at no cost; any entry not revisited this frame — a deleted region, a
/// hidden channel — is simply left behind and dropped, which is all the
/// pruning a stale entry needs.
///
/// Free rather than a `WindowState` method so the caching decision is
/// testable without a window or a GPU device.
/// `previous_geometry` is last frame's merged result, reused verbatim (a
/// refcount bump, no rebuild) when every region below is a cache hit and no
/// region present last frame has disappeared — see the `unchanged` check
/// below. Without this, every frame rebuilds and copies the full merged
/// buffer regardless of whether any region actually changed, which is cheap
/// for a static scene's occasional redraw but not for the continuous
/// redraws animated flow lines force (`WindowState::has_visible_animated_flow_lines`)
/// — those exist purely to scroll a shader uniform, not to change any
/// region's geometry, so paying a fresh multi-megabyte allocate-and-copy on
/// every one of them was pure waste.
#[allow(clippy::too_many_arguments)]
fn compute_field_layer_geometry(
    mut cache: BTreeMap<(ChannelId, scene::RegionId), RegionGeometryCache>,
    previous_geometry: Option<Arc<scene::FieldGeometry>>,
    field_snapshot: Option<&fieldcad_core::FieldSnapshot>,
    world: &WorldSnapshot,
    field_layers: &BTreeMap<ChannelId, ui::ChannelLayerSettings>,
    show: scene::SceneVisibility,
    vector_channels: &[ChannelId],
    scene_scale: fieldcad_core::SceneScale,
) -> (
    Arc<scene::FieldGeometry>,
    BTreeMap<(ChannelId, scene::RegionId), RegionGeometryCache>,
) {
    let mut new_cache = BTreeMap::new();
    // Each visible region's geometry, in the same order the merge below
    // reads them in — kept separate from `new_cache` (a `BTreeMap`, whose
    // key order need not match publish order) so a full rebuild still
    // merges in exactly the order it always has.
    let mut ordered = Vec::new();
    let mut any_rebuilt = false;
    if let Some(field_snapshot) = field_snapshot {
        for (channel_id, layer) in field_layers {
            if !layer.visible || !vector_channels.contains(channel_id) {
                continue;
            }
            let Some(channel_snapshot) = field_snapshot.channel(channel_id) else {
                continue;
            };
            let layers = scene::RegionLayers {
                planes: &layer.planes,
                boxes: &layer.boxes,
                spheres: &layer.spheres,
            };
            for batch in channel_snapshot.batches.iter() {
                let Some(region) = scene::RegionId::of(batch.geometry()) else {
                    continue;
                };
                let settings = resolve_region_settings(region, layers, layer.whole_domain);
                let world_visible = region_world_visible(region, world);
                let key = (channel_id.clone(), region);
                let entry = match cache.remove(&key) {
                    Some(entry)
                        if entry.inputs.batch == *batch
                            && entry.inputs.settings == settings
                            && entry.inputs.world_visible == world_visible
                            && entry.inputs.show == show
                            && entry.inputs.scene_scale == scene_scale =>
                    {
                        entry
                    }
                    _ => {
                        any_rebuilt = true;
                        RegionGeometryCache {
                            inputs: RegionGeometryInputs {
                                batch: batch.clone(),
                                settings,
                                world_visible,
                                show,
                                scene_scale,
                            },
                            geometry: Arc::new(scene::region_geometry(
                                batch,
                                layer.whole_domain,
                                layers,
                                show,
                                world,
                                scene_scale,
                            )),
                        }
                    }
                };
                ordered.push(Arc::clone(&entry.geometry));
                new_cache.insert(key, entry);
            }
        }
    }
    // Every region visited above was a cache hit, and nothing left in
    // `cache` (every entry present last frame that wasn't reused above —
    // a region that disappeared or went invisible) — so the merged result
    // cannot differ from last frame's.
    let unchanged = !any_rebuilt && cache.is_empty();
    if unchanged && let Some(previous) = previous_geometry {
        return (previous, new_cache);
    }
    let mut geometry = scene::FieldGeometry::default();
    for entry_geometry in &ordered {
        geometry
            .surface_triangles
            .extend(entry_geometry.surface_triangles.iter().copied());
        geometry
            .vector_lines
            .extend(entry_geometry.vector_lines.iter().copied());
        geometry
            .flow_ribbons
            .extend(entry_geometry.flow_ribbons.iter().copied());
    }
    (Arc::new(geometry), new_cache)
}

/// Field order is drop order. `egui_state` and `renderer` both reference the
/// window, so they are declared before it; `ViewportRenderer` additionally
/// drains the GPU queue on drop. Dropping the window first tears out the native
/// surface from under objects that still hold it.
struct WindowState {
    egui_state: egui_winit::State,
    renderer: ViewportRenderer,
    adapter_name: String,
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
    /// Last frame's [`ComputeView`], reused where its snapshot- and
    /// queue-derived fields still match the source — see
    /// [`ComputeView::build`].
    compute: Option<ComputeView>,
    /// Each visible region's last-built geometry, plus the inputs it was
    /// built from — see [`WindowState::field_layer_geometry`].
    region_geometry_cache: BTreeMap<(ChannelId, scene::RegionId), RegionGeometryCache>,
    /// Last frame's merged result from [`WindowState::field_layer_geometry`]
    /// — reused verbatim when every region in `region_geometry_cache` is
    /// still a hit, so a redraw that changes nothing about the scene (an
    /// animated flow line's shader-only scroll, most commonly) costs a
    /// refcount bump instead of a fresh multi-region copy.
    cached_field_layer_geometry: Option<Arc<scene::FieldGeometry>>,
    probe_history: ProbeHistory,
    distance_history: DistanceHistory,
    mass_aggregate_history: MassAggregateHistory,
    /// The numerical run generation whose recorder data is currently held.
    /// A domain reset creates a fresh t=0 run and must not join its samples to
    /// the previous lattice's history.
    run_generation: u64,
    active_transform: Option<ActiveTransformDrag>,
    active_field_brush: Option<ActiveFieldBrushDrag>,
    /// Set from the last UI frame: an inspector control is being held.
    inspector_editing: bool,
    /// The interactive edit currently in progress, if any.
    edit_gesture: Option<EditGesture>,
    /// The latest not-yet-submitted transaction of a deferred gesture — see
    /// [`EditGesture::deferred`]. Replaced every frame the gesture continues
    /// (a viewport drag's pose, or a held/typed inspector control's value),
    /// taken and submitted as the gesture's single `CommitWorld` when it
    /// closes.
    pending_deferred_edit: Option<Vec<WorldCommand>>,
    /// Object moves submitted while the mutation queue was paused, not yet
    /// known to have resolved. Rendered as ghost previews until their
    /// `CommandEvent` (`Completed`, `Failed`, or `Cancelled`) arrives in
    /// [`Self::redraw`]'s event-draining loop — the same signal that already
    /// distinguishes those three outcomes for every other command, so this
    /// needs no separate "did it apply" tracking of its own.
    deferred_edits: Vec<DeferredEdit>,
    frame_stats: FrameStats,
    step_compute_stats: StepComputeStats,
    /// When the next frame is due. Drives the event loop's control flow.
    next_redraw: Instant,
    /// Set once at window creation. Drives animated flow lines, independent
    /// of the simulation clock — the flowing look should run whether or not
    /// the simulation itself is paused, since it is a display effect on a
    /// possibly-static field.
    animation_clock: Instant,
    /// Set from `WindowEvent::Occluded`; suppresses rendering entirely.
    occluded: bool,
    /// Whether an embedded MCP server is running against `data_source`, and
    /// its token, if so. Disabled until the user opts in from the MCP panel.
    mcp: McpSession,
    /// The file the current session was last saved to or loaded from —
    /// `None` for an unsaved new scene. `Save` writes here directly; `Save
    /// As`/`Open` update it on success.
    known_path: Option<PathBuf>,
    /// The world revision as of the last successful save/load — compared
    /// against the live revision to warn before New/Open would discard
    /// unsaved work. Not a complete "modified" tracker (domain/plugin-config
    /// changes aren't compared), but catches the common case cheaply.
    last_saved_revision: Option<WorldRevision>,
    /// The current document's `created_at`, carried forward so a re-save
    /// (not Save As) doesn't reset it to "now".
    last_created_at: Option<String>,
    /// Recent files, default dialog directory, and startup-window
    /// preferences — local to this machine, never part of a saved scene.
    profile: UserProfile,
    /// A background scene-save writing to disk, if one is in flight. The
    /// document is captured synchronously (cheap — see
    /// `WorldState`'s `Arc<BTreeMap>` structural sharing) but the actual
    /// `fieldcad_scene_document::save_to_path` — JSON encode, fsync, `.bak`
    /// copy, atomic rename — runs on its own `std::thread` (this app has no
    /// ambient tokio runtime; see `crate::mcp`'s doc comment) so a slow disk
    /// never freezes the render loop. `redraw` polls it non-blockingly once
    /// per frame rather than joining it.
    saving_scene: Option<SavingScene>,
}

/// One background scene-save's completion channel, plus the bookkeeping
/// `WindowState::poll_save_task` needs to fold a successful save back into
/// `WindowState` once it lands — captured at the moment the document was
/// captured, not when the save finishes, so a further edit made while the
/// save is still writing doesn't get misattributed to it.
struct SavingScene {
    path: PathBuf,
    created_at: String,
    revision: WorldRevision,
    outcome: mpsc::Receiver<Result<(), String>>,
}

impl WindowState {
    fn new(
        event_loop: &ActiveEventLoop,
        mcp_autostart: Option<SocketAddr>,
        open_path: Option<PathBuf>,
    ) -> Result<Self, String> {
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
        let adapter_name = renderer.adapter_name().to_owned();

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
        let evaluator: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
            GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let gravity: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
            GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let maxwell: Arc<dyn MaxwellSolverBackend> =
            Arc::new(GpuMaxwellBackend::new(compute_device, compute_queue));
        let mut profile = UserProfile::load();
        // `open_path` (from `field-cad path/to/scene.fcscene` on the command
        // line) takes over startup entirely; otherwise startup keeps showing
        // the built-in demo scene by default — no change to first-run
        // behavior now that File > New offers an explicit empty alternative.
        // A path that fails to load is a startup error (propagated via `?`),
        // never a silent fall-back to the demo scene.
        let (
            data_source,
            warnings,
            queue,
            known_path,
            created_at,
            view,
            playback_speed,
            probe_history,
            distance_history,
            mass_aggregate_history,
        ) = match open_path {
            None => {
                let (source, warnings) = build_session(
                    desktop_plugin_catalog(evaluator, gravity, maxwell),
                    None,
                    true,
                )?;
                (
                    source, warnings, None, None, None, None, None, None, None, None,
                )
            }
            Some(path) => {
                let outcome = fieldcad_scene_document::load_newest_valid(&path)
                    .map_err(|error| format!("opening {}: {error}", path.display()))?;
                let queue = outcome.document.queue.clone();
                let created_at = outcome.document.metadata.created_at.clone();
                let view = outcome.document.view.clone();
                let playback_speed = outcome.document.playback_speed;
                let probe_history = outcome.document.probe_history.clone();
                let distance_history = outcome.document.distance_history.clone();
                let mass_aggregate_history = outcome.document.mass_aggregate_history.clone();
                let (source, warnings) = build_session(
                    desktop_plugin_catalog(evaluator, gravity, maxwell),
                    Some(outcome.document),
                    false,
                )?;
                (
                    source,
                    warnings,
                    Some(queue),
                    Some(path),
                    Some(created_at),
                    Some(view),
                    Some(playback_speed),
                    Some(probe_history),
                    Some(distance_history),
                    Some(mass_aggregate_history),
                )
            }
        };
        let world = data_source.world();
        let world_revision = world.revision();
        let run_generation = data_source.simulation_status().run_generation;
        if let Some(path) = &known_path {
            profile.push_recent_file(path.clone());
        }
        window.set_title(&window_title(known_path.as_deref()));
        // Which layer opens visible is `UiModel`'s own rule — it reveals the
        // first field a session sees — so the shell does not also decide it here
        // and leave two places that can disagree.
        let mut ui_model = UiModel::new();
        ui_model.diagnostics_visible = profile.show_diagnostics_on_startup;
        ui_model.help_visible = profile.show_help_on_startup;
        if !warnings.is_empty() {
            ui_model.command_error = Some(format_resolve_warnings(&warnings));
        }
        // Restore the saved camera/follow/view-toggle/per-channel display
        // state for a scene opened straight from the command line — same
        // restore `replace_session` applies for File > Open on a running
        // window, needed again here since startup builds `WindowState` from
        // scratch rather than going through that method.
        let mut camera = OrbitCamera::default();
        if let Some(view) = view {
            if let Some(camera_state) = &view.camera {
                scene_view_state::restore_camera(&mut camera, camera_state);
            }
            ui_model.following = view.following;
            ui_model.view = view
                .view_options
                .map(scene_view_state::restore_view_options)
                .unwrap_or_default();
            ui_model.field_layers = scene_view_state::restore_field_layers(view.channels);
            ui_model.object_trajectories =
                scene_view_state::restore_object_trajectories(view.objects);
        }
        let data_source = Arc::new(Mutex::new(HeadlessServer::new(data_source)));
        // Replay a loaded document's paused-queue write-ahead log through the
        // ordinary command path — see `replace_session` for the same
        // sequence used after New/Open at runtime.
        if let Some(queue) = queue {
            let mut model = lock_model(&data_source);
            if queue.paused {
                model
                    .submit(CommandPayload::PauseQueue)
                    .map_err(|error| error.to_string())?;
            }
            for payload in queue.pending {
                model.submit(payload).map_err(|error| error.to_string())?;
            }
        }
        // Restore a loaded document's wall-clock playback rate the same way:
        // `PlaybackSpeed` has no constructor path through `RuntimeConfig`, so
        // it's applied as an ordinary live command rather than threaded
        // through `build_session`.
        if let Some(speed) = playback_speed {
            let mut model = lock_model(&data_source);
            model
                .submit(CommandPayload::SetPlaybackSpeed(speed))
                .map_err(|error| error.to_string())?;
        }

        // Started here rather than left to the MCP panel's own "Enable"
        // button: an agent driving `--mcp <address>` needs the token before
        // the window has even finished coming up, and the startup log is
        // the one place guaranteed to reach it before any UI does.
        let mcp = match mcp_autostart {
            Some(addr) => {
                match mcp::enable_at(data_source.clone(), addr, mcp_plugin_catalog_for(&renderer)) {
                    Ok(running) => {
                        tracing::info!(
                            addr = %running.addr,
                            token = %running.token,
                            "MCP server listening — pass this token and address to your agent's MCP client config"
                        );
                        McpSession::Running(running)
                    }
                    Err(error) => {
                        tracing::error!(%error, "MCP server failed to start at launch");
                        McpSession::Failed(error)
                    }
                }
            }
            None => McpSession::default(),
        };

        Ok(Self {
            egui_state,
            renderer,
            adapter_name,
            window,
            egui_context,
            camera,
            ui_model,
            viewport: Viewport::default(),
            data_source,
            world,
            compute: None,
            region_geometry_cache: BTreeMap::new(),
            cached_field_layer_geometry: None,
            probe_history: probe_history.map_or_else(ProbeHistory::default, |state| {
                probe_history_state::restore_probe_history(
                    state,
                    fieldcad_core::DEFAULT_PROBE_HISTORY,
                )
            }),
            distance_history: distance_history.map_or_else(DistanceHistory::default, |state| {
                probe_history_state::restore_distance_history(
                    state,
                    fieldcad_core::DEFAULT_PROBE_HISTORY,
                )
            }),
            mass_aggregate_history: mass_aggregate_history.map_or_else(
                MassAggregateHistory::default,
                |state| {
                    probe_history_state::restore_mass_aggregate_history(
                        state,
                        fieldcad_core::DEFAULT_PROBE_HISTORY,
                    )
                },
            ),
            run_generation,
            active_transform: None,
            active_field_brush: None,
            inspector_editing: false,
            edit_gesture: None,
            pending_deferred_edit: None,
            deferred_edits: Vec::new(),
            frame_stats: FrameStats::default(),
            step_compute_stats: StepComputeStats::default(),
            next_redraw: Instant::now(),
            animation_clock: Instant::now(),
            occluded: false,
            mcp,
            known_path,
            last_saved_revision: Some(world_revision),
            last_created_at: created_at,
            profile,
            saving_scene: None,
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

    /// The channel-layer loop's contribution to the scene, alone (authoring
    /// proxies, compute bounds, and the gizmo are appended separately, since
    /// they depend on live drag/selection state that changes far more often
    /// than a snapshot). Thin wrapper around [`compute_field_layer_geometry`]
    /// that owns the cache across frames — split out so the caching decision
    /// itself is testable without a window or a GPU device.
    /// The scale to render at, for a caller that runs outside the per-frame
    /// closure and so has no local `ComputeView` in scope. Falls back to the
    /// default (metre) scale before the first frame has ever built one.
    fn scene_scale(&self) -> fieldcad_core::SceneScale {
        self.compute
            .as_ref()
            .map_or_else(fieldcad_core::SceneScale::default, |compute| {
                compute.scene_scale
            })
    }

    fn field_layer_geometry(
        &mut self,
        field_snapshot: Option<&fieldcad_core::FieldSnapshot>,
        show: scene::SceneVisibility,
        vector_channels: &[ChannelId],
        scene_scale: fieldcad_core::SceneScale,
    ) -> Arc<scene::FieldGeometry> {
        let (geometry, new_cache) = compute_field_layer_geometry(
            std::mem::take(&mut self.region_geometry_cache),
            self.cached_field_layer_geometry.clone(),
            field_snapshot,
            &self.world,
            &self.ui_model.field_layers,
            show,
            vector_channels,
            scene_scale,
        );
        self.region_geometry_cache = new_cache;
        self.cached_field_layer_geometry = Some(Arc::clone(&geometry));
        geometry
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
        let metrics_interval =
            Duration::from_millis(self.ui_model.diagnostics_config.update_interval_ms as u64);
        let elapsed = self.frame_stats.begin_frame(metrics_interval);

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
                        self.deferred_edits
                            .retain(|edit| edit.command != receipt.command);
                        // Auto-select whatever this commit created, whatever
                        // kind it is — see `UiModel::select_created` and
                        // `CommitReport::first_created` for where a new
                        // entity type gets registered instead of here.
                        if let Some(created) = receipt.created.first_created() {
                            self.ui_model.select_created(created);
                        }
                    }
                    CommandEvent::Failed { command, error } => {
                        self.ui_model.command_error = Some(error.to_string());
                        tracing::warn!(command = command.get(), %error, "compute command rejected");
                        if let Some(gesture) = &mut self.edit_gesture {
                            gesture.pause_rejected(command);
                        }
                        self.deferred_edits.retain(|edit| edit.command != command);
                    }
                    CommandEvent::Cancelled(command) => {
                        tracing::debug!(command = command.get(), "queued command cancelled");
                        self.deferred_edits.retain(|edit| edit.command != command);
                    }
                }
            }
        }
        self.refresh_world();
        self.poll_save_task();
        self.ui_model.save_in_progress = self.saving_scene.is_some();

        if self.occluded {
            // Nothing can be presented, so do no GPU work at all and check back
            // periodically. Simulation time still advances above.
            self.set_next_redraw(OCCLUDED_RETRY_INTERVAL);
            return Ok(());
        }

        let compute = ComputeView::build(&*self.model(), &self.world, self.compute.as_ref());
        self.step_compute_stats
            .observe(compute.tick, compute.step_compute_ms);
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let pixels_per_point_before_frame = self.egui_context.pixels_per_point().max(0.01);
        let transform_preview = self
            .active_transform
            .map(|drag| drag.preview(compute.scene_scale));
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
                    self.ui_model.view.gizmo_display,
                    pixels_per_point_before_frame,
                    compute.scene_scale,
                )
            })
            .map(|position| {
                egui::pos2(
                    position.x / pixels_per_point_before_frame,
                    position.y / pixels_per_point_before_frame,
                )
            });
        let mut ui_frame = ui::UiFrameOutput::default();
        let frame_time_ms = self.frame_stats.smoothed_frame_ms;
        let frame_history = self.frame_stats.frame_history.ordered();
        let frame_min_ms = self.frame_stats.min_ms;
        let frame_max_ms = self.frame_stats.max_ms;
        let process_rss_kb = self.frame_stats.metrics.rss_kb;
        let process_cpu_ms = self.frame_stats.metrics.cpu_time_ms;
        let mem_history = self.frame_stats.mem_history_mib.ordered();
        let cpu_history = self.frame_stats.cpu_history_seconds.ordered();
        let step_compute_history = self.step_compute_stats.history.ordered();
        let world = self.world.clone();

        let full_output = self.egui_context.run_ui(raw_input, |root_ui| {
            ui_frame = ui::show(
                root_ui,
                &mut self.ui_model,
                ui::FrameContext {
                    compute: &compute,
                    world: &world,
                    probe_history: &self.probe_history,
                    distance_history: &self.distance_history,
                    mass_aggregate_history: &self.mass_aggregate_history,
                    adapter_name: &self.adapter_name,
                    frame_time_ms,
                    frame_history,
                    frame_min_ms,
                    frame_max_ms,
                    process_rss_kb,
                    process_cpu_ms,
                    mem_history,
                    cpu_history,
                    step_compute_history,
                    active_translation: self.active_transform.map(|drag| drag.constraint.label()),
                    plane_normal_label,
                    plane_normal_active: self
                        .active_transform
                        .is_some_and(|drag| drag.constraint == ManipulationConstraint::PlaneNormal),
                    paused_for_edit: self.edit_gesture.is_some_and(|gesture| gesture.resume),
                    edit_in_progress: self.edit_gesture.is_some(),
                    projection: self.camera.projection(),
                    camera_distance: self.camera.distance(),
                    camera_yaw: self.camera.yaw(),
                    camera_pitch: self.camera.pitch(),
                    mcp: &self.mcp,
                },
                &mut self.profile,
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
        self.apply_camera_follow(compute.scene_scale);
        if let Some(action) = ui_frame.mcp_action {
            self.apply_mcp_action(action);
        }
        if let Some(action) = ui_frame.app_action {
            self.apply_app_action(action);
        }
        // Before the frame's own commands are dispatched: a held inspector
        // control submits an edit every frame, and the pause has to precede the
        // first of them rather than arrive a frame late.
        self.inspector_editing = ui_frame.scene_edit_in_progress;
        self.synchronize_edit_gesture(compute.mode, compute.queue.paused)?;
        self.apply_viewport_gesture(
            ui_frame.viewport_gesture,
            pixels_per_point,
            compute.mode,
            compute.queue.paused,
            compute.scene_scale,
        )?;

        // In submission order, which is also the order queued edits are applied
        // in at a tick boundary (ADR 0011). Goes through the shared model's
        // own `submit`, minting from its one `CommandSequencer` — the same
        // one MCP tool calls use — rather than a private per-window
        // sequencer, so two transports sharing this model can never mint the
        // same `CommandId`.
        //
        // A `CommitWorld` from a *deferred* gesture (see `EditGesture::deferred`)
        // is stashed instead of submitted here, same as a deferred viewport
        // drag's own `submit_world_manipulation` — a held/typed inspector
        // control resubmits every frame exactly like a drag resubmits every
        // pointer-moved frame, and would flood a paused queue the same way
        // if it went straight through.
        if !ui_frame.commands.is_empty() {
            for payload in std::mem::take(&mut ui_frame.commands) {
                if self.edit_gesture.is_some_and(|gesture| gesture.deferred)
                    && let CommandPayload::CommitWorld(commands) = payload
                {
                    self.pending_deferred_edit = Some(commands);
                    continue;
                }
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
            || self.ui_model.has_visible_animated_flow_lines()
            || self.ui_model.has_visible_animated_trajectories()
            || self.saving_scene.is_some()
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
        let scene_scale = compute.scene_scale;
        let instances = scene::instances(&self.world, self.ui_model.selection, show, scene_scale);
        // Every visible layer names a channel declared by the snapshot. Several
        // channels (for example Maxwell E and B) can be drawn independently.
        let field_snapshot = self.model().latest_snapshot();
        // The cached channel-layer geometry (`base`) is never mutated in
        // place — everything below that depends on live drag/selection
        // state, not on the snapshot, goes into a separate `overlay` built
        // fresh every frame, so a cache hit above stays a refcount bump
        // instead of a clone at the first `append_*` call.
        let base = self.field_layer_geometry(
            field_snapshot.as_deref(),
            show,
            &compute.vector_channels,
            scene_scale,
        );
        let mut overlay = scene::FieldGeometry::default();
        scene::append_authoring_geometry(
            &mut overlay,
            &self.world,
            self.ui_model.scene_selection(),
            show,
            scene_scale,
            &compute.mass_aggregates,
        );
        if self.ui_model.view.compute_bounds {
            scene::append_compute_bounds(&mut overlay, compute.domain.bounds(), scene_scale);
        }
        if !self.deferred_edits.is_empty() || self.pending_deferred_edit.is_some() {
            let edits: Vec<&WorldCommand> = self
                .deferred_edits
                .iter()
                .flat_map(|edit| edit.world_commands.iter())
                .chain(self.pending_deferred_edit.iter().flatten())
                .collect();
            scene::append_pending_edit_ghosts(&mut overlay, &self.world, &edits, scene_scale);
        }
        // Trajectory trails: selection/toggle-driven like the rest of
        // `overlay`, and cheap to skip entirely when nothing has one turned
        // on (the common case). `set_body_history_capacity` keeps the
        // runtime's retention for this object sized to what `trail_seconds`
        // actually needs at the session's current `dt` (deduped against the
        // last value sent, so a steady state costs nothing); this is what
        // lets a long trail on a coarse-`dt` scene span more than the
        // runtime's flat default depth without raising it for every other
        // body too. `request_body_history` fires (and dedupes) an async
        // fetch for next frame; `body_history` reads back whatever the most
        // recently completed fetch found, same one-round-trip staleness
        // tolerance the field snapshot itself already has.
        if show.objects {
            for (&object_id, &display) in &self.ui_model.object_trajectories {
                if !display.visible {
                    continue;
                }
                let Some(object) = self.world.object(object_id) else {
                    continue;
                };
                if !object.visible {
                    continue;
                }
                let mut model = self.model();
                model.set_body_history_capacity(
                    object_id,
                    display.required_body_history_capacity(compute.time_step_seconds),
                );
                model.request_body_history(object_id);
                let history = model.body_history(object_id);
                drop(model);
                scene::append_trajectory_geometry(
                    &mut overlay,
                    &history,
                    display,
                    Vec4::ONE,
                    scene_scale,
                );
            }
        }
        // Kept for next frame's `ComputeView::build` to reuse whatever is
        // still current — every other use of `compute` above is a borrow, so
        // nothing here is left dangling.
        self.compute = Some(compute);
        // The gizmo is drawn only for a selection that is actually on screen.
        // Handles floating over a hidden object would be draggable targets for
        // something the user cannot see.
        if self.ui_model.viewport_tool == ViewportTool::Transform {
            scene::append_transform_gizmo_with_display(
                &mut overlay,
                &self.world,
                &self.camera,
                self.viewport,
                self.visible_selection(),
                self.active_transform
                    .and_then(|drag| drag.constraint.handle()),
                transform_preview,
                self.ui_model.view.gizmo_display,
                pixels_per_point,
                scene_scale,
            );
        }

        let status = self.renderer.render(
            SceneFrame {
                camera: &self.camera,
                viewport: self.viewport,
                grid_visible: self.ui_model.view.grid,
                axes_visible: self.ui_model.view.axes,
                instances: &instances,
                base: &base,
                overlay: &overlay,
                time_seconds: self.animation_clock.elapsed().as_secs_f32(),
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
            self.distance_history = DistanceHistory::new(self.distance_history.capacity());
            self.mass_aggregate_history =
                MassAggregateHistory::new(self.mass_aggregate_history.capacity());
            self.run_generation = generation;
        }
        if let Some(snapshot) = model.latest_snapshot() {
            self.probe_history.record(&snapshot);
            self.distance_history.record(&snapshot);
            self.mass_aggregate_history.record(&snapshot);
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
        if self
            .ui_model
            .distance_probe_selection
            .is_some_and(|id| self.world.distance_probe(id).is_none())
        {
            self.ui_model.distance_probe_selection = None;
        }
        // A followed object that no longer exists must not leave the camera
        // pinned to a stale target, and the View panel's "Following: …"
        // indicator must not linger for something that is gone.
        if self
            .ui_model
            .following
            .is_some_and(|id| self.world.object(id).is_none())
        {
            self.ui_model.following = None;
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
        // Object identifiers are never reused, so a deleted object's
        // trajectory-display entry would otherwise linger for the rest of
        // the session — same reasoning as `BodyHistory::retain_objects` on
        // the runtime side.
        self.ui_model
            .object_trajectories
            .retain(|id, _| self.world.object(*id).is_some());
        // Each series is bounded, but the set of them is not: probe IDs are
        // never reused, so deleted probes would accumulate for the session.
        self.probe_history
            .retain_probes(|probe| self.world.probe(probe).is_some());
        self.distance_history
            .retain_probes(|probe| self.world.distance_probe(probe).is_some());
        self.mass_aggregate_history
            .retain_probes(|probe| self.world.mass_aggregate_probe(probe).is_some());
        self.ui_model
            .distance_probe_plots
            .retain(|probe| self.world.distance_probe(*probe).is_some());
        self.ui_model
            .distance_probe_series
            .retain(|probe, _| self.world.distance_probe(*probe).is_some());
        let scene_scale = self.scene_scale();
        if self.active_transform.is_some_and(|drag| {
            scene::selection_origin(&self.world, drag.target, scene_scale).is_none()
        }) {
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
            Some(CameraAction::ToggleFollow(id)) => {
                self.ui_model.following = if self.ui_model.following == Some(id) {
                    None
                } else {
                    Some(id)
                };
            }
            Some(CameraAction::SetDistance(distance)) => self.camera.set_distance(distance),
            Some(CameraAction::SetYaw(yaw)) => self.camera.set_yaw(yaw),
            Some(CameraAction::SetPitch(pitch)) => self.camera.set_pitch(pitch),
            None => {}
        }
    }

    /// Keep the camera's target locked onto the followed object's current
    /// position, every frame — so the object appears motionless in view
    /// while the rest of the world moves around it. Distance, yaw, and pitch
    /// are left untouched: orbiting or dollying while following adjusts the
    /// followed framing itself rather than being overridden by it.
    fn apply_camera_follow(&mut self, scene_scale: fieldcad_core::SceneScale) {
        let Some(id) = self.ui_model.following else {
            return;
        };
        let Some(object) = self.world.object(id) else {
            self.ui_model.following = None;
            return;
        };
        self.camera
            .set_target(scene_scale.to_render_vec3(object.transform.translation));
    }

    fn apply_viewport_gesture(
        &mut self,
        gesture: ViewportGesture,
        pixels_per_point: f32,
        mode: SimulationMode,
        queue_paused: bool,
        scene_scale: fieldcad_core::SceneScale,
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

        if self.ui_model.viewport_tool == ViewportTool::FieldBrush && mode == SimulationMode::Paused
        {
            if gesture.primary_pressed
                && let Some((plane, sample)) = self.field_brush_sample(pointer, scene_scale)
            {
                self.active_field_brush = Some(ActiveFieldBrushDrag {
                    plane,
                    samples: vec![sample],
                });
            }
            if gesture.primary_dragged
                && let Some((plane, sample)) = self.field_brush_sample(pointer, scene_scale)
                && let Some(active) = self.active_field_brush.as_mut()
                && active.plane == plane
                && active.samples.last().is_none_or(|last| {
                    last.distance(sample) >= self.ui_model.field_brush.radius_metres * 0.25
                })
            {
                active.samples.push(sample);
            }
            if gesture.primary_released
                && let Some(active) = self.active_field_brush.take()
                && let Some(channel) = self.ui_model.field_brush.channel.clone()
                && let Ok(strength) = self.brush_strength(&channel)
            {
                self.submit(
                    CommandPayload::ApplyFieldBrushStroke(FieldBrushStroke {
                        channel,
                        plane: active.plane,
                        samples: active.samples,
                        radius_metres: LengthMetres::from_si(
                            self.ui_model.field_brush.radius_metres,
                        ),
                        strength,
                        falloff: FieldBrushFalloff::SmoothCompact,
                    }),
                    "field painting",
                )?;
            }
        }

        // `visible_selection` rather than `scene_selection`: a hidden entity
        // has no handles on screen, so it must not start a drag either.
        if self.ui_model.viewport_tool == ViewportTool::Transform
            && gesture.primary_pressed
            && let (Some(selection), Some(pointer)) = (self.visible_selection(), pointer)
            && let Some(origin) = scene::selection_origin(&self.world, selection, scene_scale)
        {
            let picked_handle = scene::pick_transform_handle_with_display(
                &self.world,
                selection,
                &self.camera,
                self.viewport,
                pointer,
                self.ui_model.view.gizmo_display,
                pixels_per_point,
                scene_scale,
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
                    scene_scale,
                ) == Some(selection) =>
                {
                    Some(ManipulationConstraint::ViewPlane)
                }
                None => None,
            };
            if let Some(constraint) = constraint {
                // World-space, not the possibly-local stored fields: a
                // rotated attachment parent means `plane.normal`/
                // `field_box.rotation` alone would seed the drag in the
                // wrong frame — see `translate_selection`'s attached-object
                // handling for the same distinction.
                let plane_frame = match selection {
                    scene::SceneSelection::Plane(id) => {
                        self.world.planes().get(&id).and_then(|plane| {
                            self.world
                                .resolve_plane_frame(plane)
                                .ok()
                                .map(|(_, normal, u_axis)| PlaneFrame { normal, u_axis })
                        })
                    }
                    _ => None,
                };
                let box_frame = match selection {
                    scene::SceneSelection::Box(id) => {
                        self.world.boxes().get(&id).and_then(|field_box| {
                            self.world
                                .resolve_box_frame(field_box)
                                .ok()
                                .map(|(_, rotation)| BoxFrame { rotation })
                        })
                    }
                    _ => None,
                };
                self.active_transform = Some(ActiveTransformDrag {
                    target: selection,
                    constraint,
                    origin: scene_scale.to_world_vec3(origin),
                    plane_frame,
                    box_frame,
                });
                // Immediately, not at the end of the frame: the same frame that
                // starts the drag can already submit its first move.
                self.synchronize_edit_gesture(mode, queue_paused)?;
            }
        }

        if self.ui_model.viewport_tool == ViewportTool::Transform
            && gesture.primary_dragged
            && let (Some(active), Some(pointer)) = (self.active_transform, pointer)
        {
            match active.constraint {
                ManipulationConstraint::PlaneNormal => {
                    self.drag_plane_normal(active, pointer, pixels_per_point, scene_scale)?;
                }
                ManipulationConstraint::Rotate(handle) => {
                    self.drag_box_rotation(
                        active,
                        handle,
                        pointer,
                        pointer_delta,
                        pixels_per_point,
                        scene_scale,
                    )?;
                }
                ManipulationConstraint::Handle(handle) => {
                    let length = scene::selection_gizmo_length_with_display(
                        &self.world,
                        &self.camera,
                        self.viewport,
                        active.target,
                        self.ui_model.view.gizmo_display,
                        pixels_per_point,
                        scene_scale,
                    )
                    .ok_or_else(|| "selected entity no longer has a transform gizmo".to_owned())?;
                    if let Some(translation) = scene::constrained_translation(
                        handle,
                        &self.camera,
                        self.viewport,
                        pointer,
                        pointer_delta,
                        scene_scale.to_render_vec3(active.origin),
                        length,
                    ) && translation.length_squared() > 0.0
                    {
                        self.translate_selection(active, scene_scale.to_world_vec3(translation))?;
                    }
                }
                ManipulationConstraint::ViewPlane => {
                    if let Some(translation) = scene::view_plane_translation(
                        &self.camera,
                        self.viewport,
                        pointer,
                        pointer_delta,
                        scene_scale.to_render_vec3(active.origin),
                    ) && translation.length_squared() > 0.0
                    {
                        self.translate_selection(active, scene_scale.to_world_vec3(translation))?;
                    }
                }
            }
        }

        let drag_consumed = was_active || self.active_transform.is_some();
        if self.ui_model.viewport_tool != ViewportTool::FieldBrush
            && gesture.primary_clicked
            && !drag_consumed
            && let Some(pointer) = pointer
        {
            self.ui_model.set_scene_selection(scene::pick_scene(
                &self.world,
                self.scene_visibility(),
                &self.camera,
                self.viewport,
                pointer,
                scene_scale,
            ));
        }
        if gesture.primary_released {
            self.active_transform = None;
        }
        self.synchronize_edit_gesture(mode, queue_paused)
    }

    fn field_brush_sample(
        &self,
        pointer: Option<Vec2>,
        scene_scale: fieldcad_core::SceneScale,
    ) -> Option<(fieldcad_core::PlaneId, DVec2)> {
        let scene::SceneSelection::Plane(plane_id) = self.ui_model.scene_selection()? else {
            return None;
        };
        let plane = self.world.planes().get(&plane_id)?;
        let ray = self.camera.ray_from_viewport(pointer?, self.viewport)?;
        // A direction, not a length — cast as-is, never scale-converted.
        let normal = plane.normal.as_vec3();
        let denominator = ray.direction.dot(normal);
        if denominator.abs() < 1.0e-6 {
            return None;
        }
        let render_origin = scene_scale.to_render_vec3(plane.origin);
        let distance = (render_origin - ray.origin).dot(normal) / denominator;
        if distance < 0.0 {
            return None;
        }
        let hit = scene_scale.to_world_vec3(ray.origin + ray.direction * distance) - plane.origin;
        let (u, v) = plane.basis();
        Some((plane_id, DVec2::new(hit.dot(u), hit.dot(v))))
    }

    fn brush_strength(
        &self,
        channel: &fieldcad_core::ChannelId,
    ) -> Result<fieldcad_core::Quantity, String> {
        let schema = self
            .model()
            .field_systems()
            .into_iter()
            .flat_map(|system| system.channels)
            .find(|schema| &schema.id == channel)
            .ok_or_else(|| "selected field is no longer available".to_owned())?;
        fieldcad_core::Quantity::new(self.ui_model.field_brush.strength, schema.dimension())
            .map_err(|error| error.to_string())
    }

    /// What the 3D view is currently drawing, as the scene module sees it.
    ///
    /// One accessor, so drawing, gizmos, and hit-testing cannot be given
    /// different answers.
    fn scene_visibility(&self) -> scene::SceneVisibility {
        scene::SceneVisibility {
            objects: self.ui_model.view.objects,
            probes: self.ui_model.view.auxiliary_objects && self.ui_model.view.probes,
            planes: self.ui_model.view.auxiliary_objects && self.ui_model.view.planes,
            boxes: self.ui_model.view.auxiliary_objects && self.ui_model.view.boxes,
            spheres: self.ui_model.view.auxiliary_objects && self.ui_model.view.spheres,
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
        let world_command =
            match active.target {
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
                    let origin = match plane.attached_to {
                        None => next_origin,
                        Some(object) => {
                            let parent = self.world.object(object).ok_or_else(|| {
                                format!("attached object {object} no longer exists")
                            })?;
                            parent.transform.rotation.inverse()
                                * (next_origin - parent.transform.translation)
                        }
                    };
                    WorldCommand::SetPlane {
                        plane: plane_id,
                        spec: SlicePlaneSpec::from_plane(plane)
                            .with_origin(origin)
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
                            let parent = self.world.object(object).ok_or_else(|| {
                                format!("attached object {object} no longer exists")
                            })?;
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
                    let origin = match field_box.attached_to {
                        None => next_origin,
                        Some(object) => {
                            let parent = self.world.object(object).ok_or_else(|| {
                                format!("attached object {object} no longer exists")
                            })?;
                            parent.transform.rotation.inverse()
                                * (next_origin - parent.transform.translation)
                        }
                    };
                    WorldCommand::SetBox {
                        region: region_id,
                        spec: FieldBoxSpec::from_box(field_box)
                            .with_origin(origin)
                            .map_err(|error| error.to_string())?,
                    }
                }
                scene::SceneSelection::Sphere(sphere_id) => {
                    let sphere = self
                        .world
                        .spheres()
                        .get(&sphere_id)
                        .ok_or_else(|| format!("field sphere {sphere_id} no longer exists"))?;
                    let origin = match sphere.attached_to {
                        None => next_origin,
                        Some(object) => {
                            let parent = self.world.object(object).ok_or_else(|| {
                                format!("attached object {object} no longer exists")
                            })?;
                            parent.transform.rotation.inverse()
                                * (next_origin - parent.transform.translation)
                        }
                    };
                    WorldCommand::SetSphere {
                        sphere: sphere_id,
                        spec: FieldSphereSpec::from_sphere(sphere)
                            .with_origin(origin)
                            .map_err(|error| error.to_string())?,
                    }
                }
                // A mass-aggregate probe's anchor is repositioned every tick
                // from the live centroid (`adopt_world_commands`), never
                // authored — `gizmo::selection_origin` returns `None` for it,
                // so no drag gesture can ever reach here in practice.
                scene::SceneSelection::MassAggregateProbe(_) => {
                    return Err("a center of mass position is computed, not draggable".to_owned());
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
        pixels_per_point: f32,
        scene_scale: fieldcad_core::SceneScale,
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
            Some(active.preview(scene_scale)),
            self.ui_model.view.gizmo_display,
            pixels_per_point,
            scene_scale,
        )
        .ok_or_else(|| "selected plane no longer has a normal handle".to_owned())?;
        let render_origin = scene_scale.to_render_vec3(active.origin);
        let radius = tip.distance(render_origin);
        let Some(normal) = scene::dragged_plane_normal(
            &self.camera,
            self.viewport,
            pointer,
            render_origin,
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
        // `active.origin`/`normal`/`u_axis` are world-space (per `plane_frame`'s
        // drag-start seeding); an attached plane stores its frame local to the
        // parent, so convert back before writing — same trick as
        // `translate_selection`'s attached-object handling.
        let (origin, normal, u_axis) = match plane.attached_to {
            None => (active.origin, normal, u_axis),
            Some(object) => {
                let parent = self
                    .world
                    .object(object)
                    .ok_or_else(|| format!("attached object {object} no longer exists"))?;
                let inverse_rotation = parent.transform.rotation.inverse();
                (
                    inverse_rotation * (active.origin - parent.transform.translation),
                    inverse_rotation * normal,
                    inverse_rotation * u_axis,
                )
            }
        };
        let spec = SlicePlaneSpec::new(&plane.name, origin, normal)
            .and_then(|spec| spec.with_u_axis(u_axis))
            .and_then(|spec| spec.with_half_extent(plane.half_extent))
            .map(|spec| spec.with_visibility(plane.visible))
            .map(|spec| match plane.attached_to {
                Some(object) => spec.with_attached_to(object),
                None => spec,
            })
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

    #[allow(clippy::too_many_arguments)]
    fn drag_box_rotation(
        &mut self,
        active: ActiveTransformDrag,
        handle: TransformHandle,
        pointer: Vec2,
        pointer_delta: Vec2,
        pixels_per_point: f32,
        scene_scale: fieldcad_core::SceneScale,
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
        let origin = scene_scale.to_render_vec3(active.origin);
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
                let Some(radius) = scene::rotation_gizmo_radius_with_display(
                    &self.world,
                    &self.camera,
                    self.viewport,
                    active.target,
                    self.ui_model.view.gizmo_display,
                    pixels_per_point,
                    scene_scale,
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
        // `rotation` is world-space (per `box_frame`'s drag-start seeding);
        // convert back to the parent's local frame when attached, same as
        // `drag_plane_normal`.
        let rotation = match field_box.attached_to {
            None => rotation,
            Some(object) => {
                let parent = self
                    .world
                    .object(object)
                    .ok_or_else(|| format!("attached object {object} no longer exists"))?;
                parent.transform.rotation.inverse() * rotation
            }
        };
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

    /// While the current gesture is deferred (see [`EditGesture::deferred`]),
    /// this frame's pose is stashed rather than sent — nothing reaches the
    /// authoritative side until the gesture closes and submits exactly one
    /// `CommitWorld` (see `synchronize_edit_gesture`). Shared by
    /// `translate_selection`, `drag_plane_normal`, and `drag_box_rotation`,
    /// so every entity type and drag constraint gets the same treatment.
    fn submit_world_manipulation(
        &mut self,
        world_command: WorldCommand,
        operation: &str,
    ) -> Result<(), String> {
        if self.edit_gesture.is_some_and(|gesture| gesture.deferred) {
            self.pending_deferred_edit = Some(vec![world_command]);
            return Ok(());
        }
        self.submit(
            fieldcad_simulation::CommandPayload::CommitWorld(vec![world_command]),
            operation,
        )
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

    /// New/Save/Save As/Open — a rare, explicit, one-shot action, so a
    /// blocking native file dialog and a blocking scene-replace/save are
    /// both acceptable here the same way `apply_mcp_action`'s brief block on
    /// `Enable` already is.
    fn apply_app_action(&mut self, action: AppAction) {
        let result = match action {
            AppAction::NewScene { template } => {
                self.replace_session(SessionSource::New { template })
            }
            AppAction::SaveScene => self.save_scene(self.known_path.clone()),
            AppAction::SaveSceneAs => {
                match rfd::FileDialog::new()
                    .add_filter("Field CAD scene", &[fieldcad_scene_document::EXTENSION])
                    .set_directory(self.profile.last_directory_or_home())
                    .save_file()
                {
                    // Not every desktop-portal backend appends the selected
                    // filter's extension to a name typed without one (behavior
                    // varies by portal implementation) — enforced here instead
                    // of trusted from the dialog, so a saved file always
                    // matches the extension Open's own filter looks for.
                    Some(path) => self.save_scene(Some(ensure_scene_extension(path))),
                    None => Ok(()),
                }
            }
            AppAction::OpenScene => {
                match rfd::FileDialog::new()
                    .add_filter("Field CAD scene", &[fieldcad_scene_document::EXTENSION])
                    .set_directory(self.profile.last_directory_or_home())
                    .pick_file()
                {
                    Some(path) => self.replace_session(SessionSource::Load(path)),
                    None => Ok(()),
                }
            }
        };
        if let Err(error) = result {
            self.ui_model.command_error = Some(error);
        }
    }

    /// Replace the whole session in place: a new (empty or demo) scene, or
    /// one loaded from `path`. Rebuilds through [`build_session`] — the same
    /// construction path startup uses — then swaps it into the existing
    /// `Arc<Mutex<HeadlessServer>>` so the embedded MCP session (which holds
    /// a clone of that same `Arc`) observes the replacement too.
    fn replace_session(&mut self, source: SessionSource) -> Result<(), String> {
        let (compute_device, compute_queue) = self.renderer.compute_handles();
        let evaluator: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
            GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let gravity: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
            GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let maxwell: Arc<dyn MaxwellSolverBackend> =
            Arc::new(GpuMaxwellBackend::new(compute_device, compute_queue));
        let catalog = desktop_plugin_catalog(evaluator, gravity, maxwell);

        let (
            new_source,
            warnings,
            path,
            queue,
            view,
            playback_speed,
            probe_history,
            distance_history,
            mass_aggregate_history,
        ) = match source {
            SessionSource::New { template } => {
                let (source, warnings) = build_session(catalog, None, template)?;
                (source, warnings, None, None, None, None, None, None, None)
            }
            SessionSource::Load(path) => {
                let outcome = fieldcad_scene_document::load_newest_valid(&path)
                    .map_err(|error| error.to_string())?;
                let queue = outcome.document.queue.clone();
                let view = outcome.document.view.clone();
                let playback_speed = outcome.document.playback_speed;
                let probe_history = outcome.document.probe_history.clone();
                let distance_history = outcome.document.distance_history.clone();
                let mass_aggregate_history = outcome.document.mass_aggregate_history.clone();
                let (source, warnings) = build_session(catalog, Some(outcome.document), false)?;
                (
                    source,
                    warnings,
                    Some(path),
                    Some(queue),
                    Some(view),
                    Some(playback_speed),
                    Some(probe_history),
                    Some(distance_history),
                    Some(mass_aggregate_history),
                )
            }
        };

        {
            let mut model = lock_model(&self.data_source);
            model.replace_source(new_source);
            // Replay the saved paused-queue write-ahead log through the
            // ordinary command path: pause first if it was paused, then
            // resubmit each pending edit as an ordinary
            // `CommitWorld`/`ReconfigureDomain`, which lands back in the
            // queue unapplied because the queue is now paused. Never for
            // `NewScene`, whose `queue` is `None`.
            if let Some(queue) = queue {
                if queue.paused {
                    model
                        .submit(CommandPayload::PauseQueue)
                        .map_err(|error| error.to_string())?;
                }
                for payload in queue.pending {
                    model.submit(payload).map_err(|error| error.to_string())?;
                }
            }
            // Restore a loaded document's wall-clock playback rate the same
            // way — see `WindowState::new` for why this is a live command
            // rather than a `build_session` constructor argument.
            if let Some(speed) = playback_speed {
                model
                    .submit(CommandPayload::SetPlaybackSpeed(speed))
                    .map_err(|error| error.to_string())?;
            }
        }

        self.known_path = path.clone();
        self.window
            .set_title(&window_title(self.known_path.as_deref()));
        self.last_created_at = None;
        if let Some(path) = &path {
            self.profile.push_recent_file(path.clone());
        }

        // Everything below assumes ID/generation continuity with the
        // previous session and must not survive a session replace. A fresh
        // session's `run_generation` always starts at 0, which the *old*
        // session's may also still be — forcing a sentinel here makes
        // `refresh_world`'s generation-diff check reset the histories
        // unconditionally rather than relying on that coincidence not
        // occurring.
        self.run_generation = u64::MAX;
        self.region_geometry_cache.clear();
        self.active_transform = None;
        self.active_field_brush = None;
        self.edit_gesture = None;
        self.pending_deferred_edit = None;
        self.deferred_edits.clear();
        self.ui_model.select_world();
        self.ui_model.probe_plots.clear();
        self.ui_model.distance_probe_plots.clear();
        self.ui_model.distance_probe_series.clear();
        self.ui_model.mass_aggregate_probe_plots.clear();
        self.ui_model.mass_aggregate_probe_series.clear();
        self.ui_model.domain_draft = None;
        self.ui_model.command_error = if warnings.is_empty() {
            None
        } else {
            Some(format_resolve_warnings(&warnings))
        };

        // Restore the saved camera/follow/view-toggle/per-channel display
        // state on a Load, or reset it to a blank slate on New — either way,
        // before `refresh_world()` below: its cleanup pass prunes
        // `field_layers`/`following` entries whose plane/box/sphere/object ID
        // no longer exists live, and on a Load those IDs are exactly the ones
        // the just-loaded world was built from, so restoring first is safe.
        match view {
            Some(view) => {
                if let Some(camera) = &view.camera {
                    scene_view_state::restore_camera(&mut self.camera, camera);
                }
                self.ui_model.following = view.following;
                self.ui_model.view = view
                    .view_options
                    .map(scene_view_state::restore_view_options)
                    .unwrap_or_default();
                self.ui_model.field_layers = scene_view_state::restore_field_layers(view.channels);
                self.ui_model.object_trajectories =
                    scene_view_state::restore_object_trajectories(view.objects);
            }
            None => {
                self.camera = OrbitCamera::default();
                self.ui_model.following = None;
                self.ui_model.view = ui::ViewOptions::default();
                self.ui_model.field_layers.clear();
                self.ui_model.object_trajectories.clear();
            }
        }

        self.refresh_world();
        // After `refresh_world()`, not before: its generation-diff check
        // (triggered above by the `u64::MAX` sentinel) unconditionally
        // resets both histories to empty, which would otherwise discard a
        // restore landing here first.
        if let Some(state) = probe_history {
            self.probe_history =
                probe_history_state::restore_probe_history(state, self.probe_history.capacity());
        }
        if let Some(state) = distance_history {
            self.distance_history = probe_history_state::restore_distance_history(
                state,
                self.distance_history.capacity(),
            );
        }
        if let Some(state) = mass_aggregate_history {
            self.mass_aggregate_history = probe_history_state::restore_mass_aggregate_history(
                state,
                self.mass_aggregate_history.capacity(),
            );
        }
        self.last_saved_revision = Some(self.world.revision());
        Ok(())
    }

    /// Save the current session to `path`, falling back to Save As if no
    /// path is known yet (first save of a new scene). Only captures the
    /// document here — the actual disk write happens in the background (see
    /// [`SavingScene`], [`poll_save_task`](Self::poll_save_task)), so this
    /// returns before the save has completed. The File-menu Save/Save
    /// As/Open actions are disabled while `ui_model.save_in_progress`, so
    /// the guard against a second save overlapping the first only has to
    /// cover an action already queued this same frame.
    fn save_scene(&mut self, path: Option<PathBuf>) -> Result<(), String> {
        let Some(path) = path.or_else(|| self.known_path.clone()) else {
            self.apply_app_action(AppAction::SaveSceneAs);
            return Ok(());
        };
        if self.saving_scene.is_some() {
            return Ok(());
        }
        let inputs = {
            let mut model = lock_model(&self.data_source);
            let (world, queue) = model
                .capture_document()
                .map_err(|error| error.to_string())?;
            fieldcad_scene_document::SceneDocumentInputs {
                domain: model.domain(),
                time_step: model.time_step(),
                playback_speed: model.playback_speed(),
                scene_scale: model.scene_scale(),
                integration_scheme: model.integration_scheme(),
                field_systems: model.field_systems(),
                world,
                queue,
                view: scene_view_state::capture(
                    &self.camera,
                    self.ui_model.following,
                    &self.ui_model.view,
                    &self.ui_model.field_layers,
                    &self.ui_model.object_trajectories,
                ),
                probe_history: probe_history_state::capture_probe_history(&self.probe_history),
                distance_history: probe_history_state::capture_distance_history(
                    &self.distance_history,
                ),
                mass_aggregate_history: probe_history_state::capture_mass_aggregate_history(
                    &self.mass_aggregate_history,
                ),
            }
        };
        let document = fieldcad_scene_document::SceneDocument::capture(
            inputs,
            concat!("fieldcad-desktop/", env!("CARGO_PKG_VERSION")),
            self.last_created_at.clone(),
        );
        let created_at = document.metadata.created_at.clone();
        let revision = self.world.revision();
        let (sender, outcome) = mpsc::channel();
        let write_path = path.clone();
        std::thread::Builder::new()
            .name("fieldcad-scene-save".to_owned())
            .spawn(move || {
                let result = fieldcad_scene_document::save_to_path(&document, &write_path)
                    .map_err(|error| error.to_string());
                // Ignore a send failure: it only means the window closed
                // (and `outcome` was dropped) while the save was still
                // writing, not that anything went wrong with the save
                // itself — the file on disk is unaffected either way.
                let _ = sender.send(result);
            })
            .expect("spawn scene-save thread");
        self.saving_scene = Some(SavingScene {
            path,
            created_at,
            revision,
            outcome,
        });
        Ok(())
    }

    /// Fold a finished background save into `WindowState`, once its result
    /// lands — polled once per frame by `redraw`, never blocking: a save
    /// still writing is simply not there yet and `saving_scene` is left in
    /// place for the next frame's poll.
    fn poll_save_task(&mut self) {
        let Some(saving) = &self.saving_scene else {
            return;
        };
        match saving.outcome.try_recv() {
            Err(mpsc::TryRecvError::Empty) => {}
            Ok(Ok(())) => {
                let saving = self.saving_scene.take().expect("checked above");
                self.known_path = Some(saving.path.clone());
                self.window
                    .set_title(&window_title(self.known_path.as_deref()));
                // The revision captured alongside the document, not the
                // live one: a further edit made while this save was still
                // writing must still show as unsaved once this lands.
                self.last_saved_revision = Some(saving.revision);
                self.last_created_at = Some(saving.created_at);
                self.profile.push_recent_file(saving.path);
            }
            Ok(Err(error)) => {
                self.saving_scene = None;
                self.ui_model.command_error = Some(format!("save failed: {error}"));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // The save thread panicked before sending a result.
                self.saving_scene = None;
                self.ui_model.command_error =
                    Some("save failed: scene-save thread panicked".to_owned());
            }
        }
    }

    /// Enable or disable the embedded MCP server against this window's
    /// shared model. `Enable` blocks briefly (see `crate::mcp::enable`) —
    /// acceptable for a rare, explicit button click.
    fn apply_mcp_action(&mut self, action: McpAction) {
        match action {
            McpAction::Enable => {
                self.mcp = match mcp::enable(
                    self.data_source.clone(),
                    mcp_plugin_catalog_for(&self.renderer),
                ) {
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
    ///
    /// `queue_paused` decides, only at the moment a gesture *opens*, whether
    /// it runs deferred (see [`EditGesture::deferred`]) — any gesture
    /// [`Self::scene_is_being_edited`] reports while the mutation queue is
    /// explicitly paused, whatever the simulation's own mode: a viewport
    /// drag (object, box, sphere, plane, or probe) resubmits every
    /// pointer-moved frame, and a held or typed inspector control resubmits
    /// every frame it changes — either would flood a paused queue with
    /// per-frame commits alike, so both defer alike. The gesture's one
    /// deferred `CommitWorld` lands in `pending_mutations` and sits there
    /// until the queue resumes (`SessionCore::should_queue_mutation` in
    /// `fieldcad-simulation` holds a mutation for either reason — a
    /// `Running` tick boundary or an explicitly paused queue — not just the
    /// first), rather than the per-frame flood of *immediately applied*
    /// commits a live edit would otherwise submit while paused: the actual
    /// cost this exists to avoid, since a slow solver turns that flood into
    /// an unbounded, uncancellable backlog of in-flight solves. Closing a
    /// deferred gesture submits its one stashed
    /// [`WindowState::pending_deferred_edit`] here, since
    /// `EditGesture::transition` itself emits no commands for that case —
    /// the viewport-drag call sites stash through `submit_world_manipulation`,
    /// the inspector's through the `ui_frame.commands` loop in
    /// [`WindowState::redraw`], and either can be what's waiting here.
    fn synchronize_edit_gesture(
        &mut self,
        mode: SimulationMode,
        queue_paused: bool,
    ) -> Result<(), String> {
        let deferred = queue_paused && self.scene_is_being_edited();
        let was_deferred = self.edit_gesture.is_some_and(|gesture| gesture.deferred);
        let (mut next, commands) = EditGesture::transition(
            self.edit_gesture,
            self.scene_is_being_edited(),
            mode,
            deferred,
        );
        for payload in commands {
            // `AsyncLocalDataSource` never blocks: this always reports
            // `Submitted`, whatever the command's eventual outcome. A
            // `Pause` can still be rejected later (a paused mutation queue
            // with work held back) — record its id so the gesture can tell
            // whether it actually took once that rejection, if any, arrives
            // as an ordinary `CommandEvent::Failed`.
            let is_pause = matches!(payload, CommandPayload::Pause);
            let receipt = self
                .model()
                .submit(payload)
                .map_err(|error| format!("scene edit failed: {error}"))?;
            if is_pause && let Some(gesture) = &mut next {
                gesture.pause_command = Some(receipt.command);
            }
        }
        if was_deferred
            && next.is_none()
            && let Some(world_commands) = self.pending_deferred_edit.take()
        {
            let receipt = self
                .model()
                .submit(CommandPayload::CommitWorld(world_commands.clone()))
                .map_err(|error| format!("scene edit failed: {error}"))?;
            self.deferred_edits.push(DeferredEdit {
                command: receipt.command,
                world_commands,
            });
        }
        self.edit_gesture = next;
        Ok(())
    }

    fn focus_selection(&mut self) {
        let Some(selection) = self.ui_model.scene_selection() else {
            return;
        };
        let scene_scale = self.scene_scale();
        match selection {
            scene::SceneSelection::Object(id) => {
                let Some(object) = self.world.object(id) else {
                    return;
                };
                let (centre, radius) = object.bounding_sphere();
                self.camera.focus(
                    scene_scale.to_render_vec3(centre),
                    scene_scale.to_render(radius),
                );
            }
            scene::SceneSelection::Plane(id) => {
                let Some(plane) = self.world.planes().get(&id) else {
                    return;
                };
                self.camera.focus(
                    scene_scale.to_render_vec3(plane.origin),
                    scene_scale.to_render(plane.half_extent.length()),
                );
            }
            scene::SceneSelection::Probe(id) => {
                let Some(probe) = self.world.probe(id) else {
                    return;
                };
                if let Ok(position) = self.world.resolve_probe_position(probe) {
                    self.camera.focus(scene_scale.to_render_vec3(position), 0.2);
                }
            }
            scene::SceneSelection::Box(id) => {
                let Some(field_box) = self.world.boxes().get(&id) else {
                    return;
                };
                self.camera.focus(
                    scene_scale.to_render_vec3(field_box.origin),
                    scene_scale.to_render(field_box.half_extent.length()),
                );
            }
            scene::SceneSelection::Sphere(id) => {
                let Some(sphere) = self.world.spheres().get(&id) else {
                    return;
                };
                self.camera.focus(
                    scene_scale.to_render_vec3(sphere.origin),
                    scene_scale.to_render(sphere.radius),
                );
            }
            scene::SceneSelection::MassAggregateProbe(id) => {
                let Some(probe) = self.world.mass_aggregate_probe(id) else {
                    return;
                };
                if let Some(anchor) = self.world.object(probe.anchor) {
                    self.camera.focus(
                        scene_scale.to_render_vec3(anchor.transform.translation),
                        0.2,
                    );
                }
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
    /// The simulation was advancing when this gesture began, and so is
    /// resumed when it commits — *provided* the `Pause` that opened it is
    /// confirmed to have actually applied; see `pause_command`. A run the
    /// user had already paused stays paused.
    resume: bool,
    /// The id of this gesture's own `Pause` submission, while its outcome is
    /// still unknown. Submission through `AsyncLocalDataSource` never
    /// blocks — it always reports `Submitted` immediately — so a rejection
    /// (`reject_if_queue_paused`, when the mutation queue is paused with
    /// work still held back) is only discovered later, as an ordinary
    /// `CommandEvent::Failed`. `pause_rejected` clears `resume` when that
    /// happens, so a `Pause` that never took does not get an unconditional
    /// `Play` on commit — the sim was never actually stopped, so there is
    /// nothing to resume.
    pause_command: Option<CommandId>,
    /// This gesture — a viewport drag or a held/typed inspector control —
    /// opened while the mutation queue was explicitly paused (BE-16),
    /// whatever the simulation's own mode. The transport is never touched —
    /// no `Pause`/`Play`, no `SetInteractiveEdit` — because nothing is
    /// submitted per frame at all: the edit stays local, and the caller
    /// submits exactly one `CommitWorld` on close (see
    /// `synchronize_edit_gesture`), which the engine holds in
    /// `pending_mutations` until the queue resumes, however many per-frame
    /// commits a live edit would otherwise have submitted while paused.
    /// Decided once, at open, from the queue's state at that instant — it
    /// does not re-evaluate mid-gesture.
    deferred: bool,
}

impl EditGesture {
    /// What opening or closing a gesture asks of the source, and the state that
    /// leaves behind.
    ///
    /// Pure, because the ordering here is the whole contract and it is not worth
    /// standing up a window to check: the pause precedes the edits it brackets,
    /// the commit precedes the resume, and a run the user had already paused is
    /// given back exactly as it was found. `pause_command` is left unset here —
    /// the caller fills it in from the `Pause` submission's own receipt, since
    /// this function has no source to submit through.
    ///
    /// `deferred` short-circuits both arms to emit no commands at all: a
    /// deferred gesture never touches the transport, so there is nothing to
    /// bracket and nothing to hand back. The caller is responsible for
    /// submitting the gesture's single deferred `CommitWorld` itself when it
    /// detects the close (see `synchronize_edit_gesture`).
    fn transition(
        current: Option<Self>,
        editing: bool,
        mode: SimulationMode,
        deferred: bool,
    ) -> (Option<Self>, Vec<CommandPayload>) {
        match (editing, current) {
            (true, None) => {
                if deferred {
                    return (
                        Some(Self {
                            resume: false,
                            pause_command: None,
                            deferred: true,
                        }),
                        Vec::new(),
                    );
                }
                let resume = mode == SimulationMode::Running;
                let mut commands = Vec::new();
                if resume {
                    commands.push(CommandPayload::Pause);
                }
                commands.push(CommandPayload::SetInteractiveEdit(true));
                (
                    Some(Self {
                        resume,
                        pause_command: None,
                        deferred: false,
                    }),
                    commands,
                )
            }
            (false, Some(gesture)) => {
                if gesture.deferred {
                    return (None, Vec::new());
                }
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

    /// This gesture's own `Pause` was rejected: the run it meant to bracket
    /// never actually stopped, so closing the gesture must not submit an
    /// unconditional, unneeded `Play` on top of it.
    fn pause_rejected(&mut self, command: CommandId) {
        if self.pause_command == Some(command) {
            self.resume = false;
            self.pause_command = None;
        }
    }
}

/// A gesture's edit — a viewport drag's (object, box, sphere, plane, or
/// probe) or a held/typed inspector control's — submitted while the
/// mutation queue was paused, held in `pending_mutations` on the
/// authoritative side until the queue resumes, whatever the simulation's
/// own mode. Removed from [`WindowState::deferred_edits`] as soon as its
/// `command`'s outcome arrives as a `CommandEvent` — see
/// [`WindowState::redraw`].
#[derive(Clone, Debug, PartialEq)]
struct DeferredEdit {
    command: CommandId,
    world_commands: Vec<WorldCommand>,
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

#[derive(Clone, Debug)]
struct ActiveFieldBrushDrag {
    plane: fieldcad_core::PlaneId,
    samples: Vec<DVec2>,
}

impl ActiveTransformDrag {
    fn preview(self, scene_scale: fieldcad_core::SceneScale) -> scene::TransformPreview {
        scene::TransformPreview {
            origin: scene_scale.to_render_vec3(self.origin),
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

/// This host's plugin composition — one electric field, three candidate
/// models (electrostatics active by default; Newtonian gravity and Maxwell
/// composed but inactive) — each backed by this window's own GPU device.
///
/// Factored out of the old `create_local_data_source` so [`build_session`]
/// can reuse the exact same wiring for New Scene, Load, and startup, rather
/// than only the hardcoded demo scene construction that used to be the only
/// caller.
/// A [`mcp::PluginCatalog`] closure over this window's GPU device/queue, for
/// [`mcp::enable`]/[`mcp::enable_at`] — so `create_scene`/`open_scene`
/// called through the embedded MCP server build plugins with the same
/// evaluator backends the desktop's own File menu would have used, not the
/// standalone server's CPU-only catalog.
fn mcp_plugin_catalog_for(renderer: &ViewportRenderer) -> mcp::PluginCatalog {
    let (compute_device, compute_queue) = renderer.compute_handles();
    Arc::new(move || {
        let evaluator: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
            GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let gravity: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
            GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
        );
        let maxwell: Arc<dyn MaxwellSolverBackend> = Arc::new(GpuMaxwellBackend::new(
            compute_device.clone(),
            compute_queue.clone(),
        ));
        desktop_plugin_catalog(evaluator, gravity, maxwell)
    })
}

fn desktop_plugin_catalog(
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    gravity: Arc<dyn InverseSquareBatchEvaluator>,
    maxwell: Arc<dyn MaxwellSolverBackend>,
) -> Vec<PluginRegistration> {
    vec![
        PluginRegistration::with_default_configuration(Box::new(
            ElectrostaticsPlugin::with_evaluator(evaluator),
        )),
        PluginRegistration::with_default_configuration(Box::new(
            NewtonianGravityPlugin::with_evaluator(gravity),
        ))
        .with_enabled(false),
        PluginRegistration::with_default_configuration(Box::new(
            ElectromagnetismPlugin::with_backend(maxwell),
        ))
        .with_enabled(false),
    ]
}

/// The desktop's built-in demo scene: one positive point charge, a probe
/// recording it, and an XY slice plane. Content for New Scene's "Demo Scene"
/// variant and for the app's own startup (which keeps showing this scene by
/// default, per product decision, now that File > New offers an explicit
/// empty alternative too) — never for a loaded document, which brings its
/// own content.
fn demo_scene_commands() -> Result<Vec<WorldCommand>, String> {
    Ok(vec![
        WorldCommand::CreateObject(
            ObjectSpec::new("Positive point charge")
                .with_transform(
                    Transform::at(DVec3::new(0.0, 0.0, 0.6)).map_err(|e| e.to_string())?,
                )
                .with_shape(ObjectShape::point(0.15).map_err(|error| error.to_string())?)
                .with_component(
                    charge_component_id(),
                    charge_properties(ChargeCoulombs::from_si(1.0e-9))
                        .map_err(|error| error.to_string())?,
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
}

/// Appends `.fcscene` if `path` doesn't already end in it (case-insensitive),
/// so a name typed into the Save As dialog without an extension still
/// matches Open's own extension filter afterward.
fn ensure_scene_extension(path: PathBuf) -> PathBuf {
    let has_extension = path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case(fieldcad_scene_document::EXTENSION)
    });
    if has_extension {
        path
    } else {
        let mut name = path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        name.push(".");
        name.push(fieldcad_scene_document::EXTENSION);
        path.with_file_name(name)
    }
}

/// A fresh, session-scoped identity (US-01: "stable session identifier" —
/// stable *within* the session, never required to match a prior save's id).
/// Every call site used to hardcode `SessionId::from_u128(1)`; New Scene and
/// Load each need a session distinguishable from whatever came before.
fn fresh_session_id() -> SessionId {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(1);
    SessionId::from_u128(nanos)
}

/// Build a fresh runtime from either an empty world or a loaded document,
/// through one construction path — same domain/plugin wiring, same
/// `clear_edit_history()` discipline ADR 0024 requires of "a default scene,
/// later a loaded file." `demo_content` adds the built-in demo scene
/// (`demo_scene_commands`) before history is cleared; only meaningful when
/// `document` is `None` — a loaded document brings its own content instead.
///
/// Does not restore a saved document's paused command queue — that replay
/// happens one level up, after the caller has swapped this session into the
/// shared `HeadlessServer` (see `WindowState::replace_session`), since it
/// needs the session to already be reachable through the ordinary command
/// path rather than a bare `SimulationRuntime`.
fn build_session(
    catalog: Vec<PluginRegistration>,
    document: Option<fieldcad_scene_document::SceneDocument>,
    demo_content: bool,
) -> Result<
    (
        AsyncLocalDataSource,
        Vec<fieldcad_scene_document::ResolveWarning>,
    ),
    String,
> {
    let (domain, time_step, scene_scale, integration_scheme, world, plugins, warnings) =
        match document {
            None => {
                let domain = Domain::new(
                    DomainBounds::centred_cube(5.0).map_err(|error| error.to_string())?,
                    Resolution::uniform(32).map_err(|error| error.to_string())?,
                    BoundaryConditions::uniform(BoundaryCondition::Periodic),
                    Precision::F32,
                );
                let time_step = TimeStep::from_seconds(courant_limit(&domain) * 0.8)
                    .map_err(|error| error.to_string())?;
                (
                    domain,
                    time_step,
                    fieldcad_core::SceneScale::default(),
                    fieldcad_dynamics::IntegrationScheme::default(),
                    fieldcad_core::World::new(),
                    catalog,
                    Vec::new(),
                )
            }
            Some(doc) => {
                let (plugins, warnings) =
                    fieldcad_scene_document::resolve_plugins(catalog, &doc.field_systems)
                        .map_err(|error| error.to_string())?;
                (
                    doc.domain,
                    doc.time_step,
                    doc.scene_scale,
                    doc.integration_scheme,
                    fieldcad_core::World::from_document(doc.world),
                    plugins,
                    warnings,
                )
            }
        };
    let mut config = RuntimeConfig::new(domain, time_step, fresh_session_id())
        .with_world(world)
        .with_scene_scale(scene_scale)
        .with_integration_scheme(integration_scheme)
        .with_subscription(
            Subscription::PROBES_ONLY
                .with_planes(UVec2::splat(33))
                .with_domain_stride(8)
                .with_boxes(UVec3::splat(9))
                .with_spheres(9),
        );
    for plugin in plugins {
        config = config.with_plugin_registration(plugin);
    }
    let mut runtime = SimulationRuntime::new(config).map_err(|error| error.to_string())?;
    if demo_content {
        runtime
            .commit_world_commands(demo_scene_commands()?)
            .map_err(|error| error.to_string())?;
    }
    // The starting scene is where this session begins, not the user's first
    // edit. Without this the opening undo would empty the workspace.
    runtime.clear_edit_history();

    Ok((
        AsyncLocalDataSource::new(LocalDataSource::new(runtime)),
        warnings,
    ))
}

/// What [`WindowState::replace_session`] is building: a new scene (empty or
/// demo), or one loaded from a saved document.
enum SessionSource {
    New { template: bool },
    Load(PathBuf),
}

/// Surfaces a loaded document's plugin-version mismatches (minor/patch only
/// — a major mismatch is a hard [`fieldcad_scene_document::ResolveError`],
/// not a warning) as one human-readable message for `command_error`, the
/// same field an ordinary rejected command already reports through.
fn format_resolve_warnings(warnings: &[fieldcad_scene_document::ResolveWarning]) -> String {
    let mut message = String::from("Loaded with plugin version differences: ");
    for (index, warning) in warnings.iter().enumerate() {
        if index > 0 {
            message.push_str("; ");
        }
        message.push_str(&format!(
            "{} document={} linked={}",
            warning.plugin, warning.document_version, warning.linked_version
        ));
    }
    message
}

const HISTORY_SIZE: usize = 120;

/// A bounded, insertion-ordered time series.
///
/// Backed by a ring buffer for O(1) writes, but a UI panel wants to borrow an
/// oldest-to-newest slice without knowing about wraparound — so the ordered
/// view is rebuilt on every push instead. At `HISTORY_SIZE` samples that is
/// far cheaper than the bug it replaces: reading the raw ring buffer from
/// index 0 up to its length, which is only chronological before the buffer
/// first wraps and is scrambled every frame after.
struct History<const N: usize> {
    raw: [f32; N],
    ordered: [f32; N],
    index: usize,
    len: usize,
}

impl<const N: usize> Default for History<N> {
    fn default() -> Self {
        Self {
            raw: [0.0; N],
            ordered: [0.0; N],
            index: 0,
            len: 0,
        }
    }
}

impl<const N: usize> History<N> {
    fn push(&mut self, value: f32) {
        self.raw[self.index] = value;
        self.index = (self.index + 1) % N;
        self.len = (self.len + 1).min(N);

        let start = if self.len < N { 0 } else { self.index };
        for i in 0..self.len {
            self.ordered[i] = self.raw[(start + i) % N];
        }
    }

    /// Oldest first, newest last.
    fn ordered(&self) -> &[f32] {
        &self.ordered[..self.len]
    }
}

/// Process-level resource usage. Linux reads from `/proc/self/status` and
/// `/proc/self/stat`; other platforms return zeroed structs.
#[derive(Clone, Copy, Debug, Default)]
struct ProcessMetrics {
    rss_kb: u64,
    cpu_time_ms: f64,
}

#[cfg(target_os = "linux")]
fn read_process_metrics() -> ProcessMetrics {
    let rss_kb = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|val| val.parse::<u64>().ok())
        })
        .unwrap_or(0);

    // Fields 13 (utime) and 14 (stime) in clock ticks. CLK_TCK is 100 Hz
    // on all common Linux configurations — multiply by 10 for milliseconds.
    let cpu_time_ms = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split_whitespace().collect();
            if parts.len() > 14 {
                let utime = parts[13].parse::<u64>().ok().unwrap_or(0);
                let stime = parts[14].parse::<u64>().ok().unwrap_or(0);
                Some((utime + stime) as f64 * 10.0)
            } else {
                None
            }
        })
        .unwrap_or(0.0);

    ProcessMetrics {
        rss_kb,
        cpu_time_ms,
    }
}

#[cfg(not(target_os = "linux"))]
fn read_process_metrics() -> ProcessMetrics {
    ProcessMetrics::default()
}

struct FrameStats {
    previous_frame: Instant,
    smoothed_frame_ms: f32,
    frame_history: History<HISTORY_SIZE>,
    min_ms: f32,
    max_ms: f32,
    last_metrics_collection: Instant,
    metrics: ProcessMetrics,
    /// Sampled once per metrics collection, not once per render frame — a
    /// rendered frame can arrive far faster than `/proc` is worth reading,
    /// so this tracks the same cadence as `metrics` rather than the
    /// smoothed-frame-time history above.
    mem_history_mib: History<HISTORY_SIZE>,
    cpu_history_seconds: History<HISTORY_SIZE>,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            previous_frame: Instant::now(),
            smoothed_frame_ms: 0.0,
            frame_history: History::default(),
            min_ms: f32::MAX,
            max_ms: 0.0,
            last_metrics_collection: Instant::now(),
            metrics: ProcessMetrics::default(),
            mem_history_mib: History::default(),
            cpu_history_seconds: History::default(),
        }
    }
}

impl FrameStats {
    /// Returns wall-clock time since the previous redraw for simulation pacing.
    /// Idle time is deliberately not reported as render work.
    ///
    /// `metrics_interval` is the diagnostics panel's configured update rate —
    /// read fresh each call rather than cached, so dragging the panel's
    /// slider takes effect on the next frame instead of needing a restart.
    fn begin_frame(&mut self, metrics_interval: Duration) -> Duration {
        let now = Instant::now();
        let elapsed = now - self.previous_frame;
        self.previous_frame = now;

        if now - self.last_metrics_collection >= metrics_interval {
            self.metrics = read_process_metrics();
            self.last_metrics_collection = now;
            self.mem_history_mib
                .push(self.metrics.rss_kb as f32 / 1024.0);
            self.cpu_history_seconds
                .push((self.metrics.cpu_time_ms / 1000.0) as f32);
        }

        elapsed
    }

    fn finish_frame(&mut self, duration: Duration) {
        let elapsed_ms = duration.as_secs_f32() * 1_000.0;
        self.smoothed_frame_ms = if self.smoothed_frame_ms == 0.0 {
            elapsed_ms
        } else {
            self.smoothed_frame_ms * 0.9 + elapsed_ms * 0.1
        };

        self.frame_history.push(elapsed_ms);

        self.min_ms = if elapsed_ms < self.min_ms {
            elapsed_ms
        } else {
            self.min_ms
        };
        self.max_ms = if elapsed_ms > self.max_ms {
            elapsed_ms
        } else {
            self.max_ms
        };
    }
}

/// How long the compute thread took to finish each simulation tick.
///
/// Sampled once per *tick*, not once per render frame: `ComputeView` reports
/// the same `step_compute_ms` on every redraw between two ticks (a paused
/// simulation, or a tick slower than the frame rate), and pushing that into
/// history on every redraw would pad the plot with duplicate flat segments
/// instead of showing one point per step actually taken.
#[derive(Default)]
struct StepComputeStats {
    history: History<HISTORY_SIZE>,
    last_observed_tick: Option<u64>,
}

impl StepComputeStats {
    fn observe(&mut self, tick: u64, compute_ms: f32) {
        // `compute_ms <= 0.0` means no tick has actually run yet, whatever
        // `tick` itself reads as.
        if compute_ms <= 0.0 || self.last_observed_tick == Some(tick) {
            return;
        }
        self.last_observed_tick = Some(tick);
        self.history.push(compute_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::{EditGesture, FrameStats, StepComputeStats};
    use fieldcad_core::SimulationMode;
    use fieldcad_simulation::{CommandId, CommandPayload};
    use std::time::Duration;

    /// The order is the point: the pause has to arrive before the first edit of
    /// the gesture, and the resume after the commit that brings deferred systems
    /// current — otherwise the resumed run starts from a field nothing
    /// recomputed.
    #[test]
    fn a_running_simulation_is_suspended_for_an_edit_and_resumed_when_it_commits() {
        let (opened, commands) =
            EditGesture::transition(None, true, SimulationMode::Running, false);

        assert_eq!(
            commands,
            vec![
                CommandPayload::Pause,
                CommandPayload::SetInteractiveEdit(true)
            ]
        );

        // Nothing further while the gesture is held, however many frames it
        // spans.
        let (held, commands) = EditGesture::transition(opened, true, SimulationMode::Paused, false);
        assert_eq!(held, opened);
        assert!(commands.is_empty());

        let (closed, commands) =
            EditGesture::transition(held, false, SimulationMode::Paused, false);
        assert_eq!(closed, None);
        assert_eq!(
            commands,
            vec![
                CommandPayload::SetInteractiveEdit(false),
                CommandPayload::Play
            ]
        );
    }

    /// A plain-object drag opened while the mutation queue is paused (and
    /// the simulation still `Running`) never touches the transport: no
    /// `Pause`, no `SetInteractiveEdit` — the drag stays local until it
    /// closes, and the caller submits its one deferred `CommitWorld` itself
    /// (see `synchronize_edit_gesture`, not exercised by this pure-function
    /// test).
    #[test]
    fn a_deferred_gesture_opens_and_closes_without_touching_the_transport() {
        let (opened, commands) = EditGesture::transition(None, true, SimulationMode::Running, true);
        assert!(commands.is_empty());
        let opened = opened.expect("a deferred gesture still opens");
        assert!(opened.deferred);
        assert!(!opened.resume);
        assert_eq!(opened.pause_command, None);

        // Held across frames exactly as a live gesture would be — recomputing
        // `deferred` mid-drag is deliberately not this function's job.
        let (held, commands) =
            EditGesture::transition(Some(opened), true, SimulationMode::Running, false);
        assert_eq!(held, Some(opened));
        assert!(commands.is_empty());

        let (closed, commands) =
            EditGesture::transition(held, false, SimulationMode::Running, false);
        assert_eq!(closed, None);
        assert!(commands.is_empty());
    }

    /// UI-2 regression: `Pause` can be rejected after a gesture has already
    /// provisionally started on the assumption it would take —
    /// `reject_if_queue_paused` fires whenever the mutation queue is paused
    /// with work still held back, and `AsyncLocalDataSource` never blocks a
    /// submission to report that synchronously. Once the rejection arrives,
    /// closing the gesture must not submit an unconditional `Play`: the run
    /// it meant to bracket was never actually stopped.
    #[test]
    fn a_rejected_pause_does_not_resume_a_run_that_never_stopped() {
        let (opened, commands) =
            EditGesture::transition(None, true, SimulationMode::Running, false);
        assert_eq!(
            commands,
            vec![
                CommandPayload::Pause,
                CommandPayload::SetInteractiveEdit(true)
            ]
        );
        let mut opened = opened.unwrap();
        let pause_command = CommandId::new(7);
        opened.pause_command = Some(pause_command);

        opened.pause_rejected(pause_command);
        assert!(!opened.resume);

        // Held across frames exactly as an accepted pause would be.
        let (held, commands) =
            EditGesture::transition(Some(opened), true, SimulationMode::Running, false);
        assert_eq!(held, Some(opened));
        assert!(commands.is_empty());

        // Closing it asks only to leave interactive-edit mode — never `Play`,
        // since the run underneath was never actually paused.
        let (closed, commands) =
            EditGesture::transition(held, false, SimulationMode::Running, false);
        assert_eq!(closed, None);
        assert_eq!(commands, vec![CommandPayload::SetInteractiveEdit(false)]);
    }

    /// A rejection for some unrelated command must not disturb a gesture's
    /// own, still-pending or already-accepted pause.
    #[test]
    fn pause_rejected_ignores_an_unrelated_command_id() {
        let (opened, _) = EditGesture::transition(None, true, SimulationMode::Running, false);
        let mut opened = opened.unwrap();
        opened.pause_command = Some(CommandId::new(1));

        opened.pause_rejected(CommandId::new(2));

        assert!(opened.resume);
        assert_eq!(opened.pause_command, Some(CommandId::new(1)));
    }

    /// The gesture hands the transport back as it found it. Resuming a run the
    /// user had deliberately paused would make dragging a body start the
    /// simulation.
    #[test]
    fn editing_an_already_paused_simulation_leaves_it_paused() {
        let (opened, commands) = EditGesture::transition(None, true, SimulationMode::Paused, false);

        assert_eq!(commands, vec![CommandPayload::SetInteractiveEdit(true)]);

        let (closed, commands) =
            EditGesture::transition(opened, false, SimulationMode::Paused, false);

        assert_eq!(closed, None);
        assert_eq!(commands, vec![CommandPayload::SetInteractiveEdit(false)]);
    }

    #[test]
    fn no_edit_in_progress_asks_nothing_of_the_source() {
        let (gesture, commands) =
            EditGesture::transition(None, false, SimulationMode::Running, false);

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

    #[test]
    fn step_compute_history_records_one_sample_per_tick_not_per_redraw() {
        let mut stats = StepComputeStats::default();

        // A paused simulation, or one slower than the frame rate, reports the
        // same tick and the same compute time on every redraw in between —
        // that must not pad the history with duplicates.
        stats.observe(5, 1.5);
        stats.observe(5, 1.5);
        stats.observe(5, 1.5);
        assert_eq!(stats.history.ordered(), &[1.5]);

        stats.observe(6, 2.0);
        assert_eq!(stats.history.ordered(), &[1.5, 2.0]);

        // No tick has run yet — `compute_ms` is the sentinel default from
        // `SimulationRuntime::last_tick_compute_ms`, not a real sample.
        let mut fresh = StepComputeStats::default();
        fresh.observe(0, 0.0);
        assert!(fresh.history.ordered().is_empty());
    }

    mod field_layer_geometry_cache {
        use std::{collections::BTreeMap, sync::Arc};

        use fieldcad_core::{
            ChannelId, ChannelSchema, ChannelSnapshot, Dimension, Domain, FieldBatch, FieldColumn,
            FieldSnapshot, FieldValueKind, PlaneId, PlaneLattice, PluginId, SampleGeometry,
            SampleValidity, SessionId, SlicePlaneSpec, SnapshotCompleteness, SnapshotIdentity,
            World, WorldCommand, WorldRevision,
        };
        use glam::{DVec3, UVec2};

        use super::super::compute_field_layer_geometry;
        use crate::{scene, ui};

        /// A world holding one visible slice plane, so the batch id in the
        /// snapshot and the world's id agree — the same correspondence the
        /// runtime's own publication guarantees.
        fn world_with_plane() -> (World, PlaneId) {
            let mut world = World::new();
            let report = world
                .commit([WorldCommand::CreatePlane(
                    SlicePlaneSpec::new("Plane", DVec3::ZERO, DVec3::Z).unwrap(),
                )])
                .unwrap();
            (world, report.created_planes[0])
        }

        /// A world holding two visible slice planes.
        fn world_with_two_planes() -> (World, PlaneId, PlaneId) {
            let mut world = World::new();
            let report = world
                .commit([
                    WorldCommand::CreatePlane(
                        SlicePlaneSpec::new("Plane A", DVec3::ZERO, DVec3::Z).unwrap(),
                    ),
                    WorldCommand::CreatePlane(
                        SlicePlaneSpec::new("Plane B", DVec3::new(5.0, 0.0, 0.0), DVec3::Z)
                            .unwrap(),
                    ),
                ])
                .unwrap();
            (world, report.created_planes[0], report.created_planes[1])
        }

        fn plane_batch(plane: PlaneId, origin: DVec3, values: [DVec3; 4]) -> FieldBatch {
            let lattice = PlaneLattice::new(origin, DVec3::X, DVec3::Y, UVec2::splat(2));
            FieldBatch::new(
                SampleGeometry::Plane { plane, lattice },
                FieldColumn::vectors(values.to_vec()),
                vec![SampleValidity::Exact; 4],
            )
            .unwrap()
        }

        fn snapshot_of(
            sequence: u64,
            channel: ChannelId,
            batches: Vec<FieldBatch>,
        ) -> FieldSnapshot {
            FieldSnapshot {
                identity: SnapshotIdentity {
                    session: SessionId::from_u128(1),
                    sequence,
                    run_generation: 0,
                    world_revision: WorldRevision::INITIAL,
                    tick: 0,
                    time_seconds: 0.0,
                },
                completeness: SnapshotCompleteness::Complete,
                domain: Domain::centred_cube(4.0, 4).unwrap(),
                plugins: Arc::from([]),
                channels: BTreeMap::from([(
                    channel.clone(),
                    ChannelSnapshot {
                        schema: Arc::new(ChannelSchema {
                            id: channel,
                            display_name: "Vector".to_owned(),
                            value_kind: FieldValueKind::Vector(Dimension::ELECTRIC_FIELD),
                        }),
                        provider: PluginId::new("test").unwrap(),
                        batches: batches.into(),
                    },
                )]),
                diagnostics: Arc::from([]),
                distances: Arc::from([]),
                mass_aggregates: Arc::from([]),
            }
        }

        /// One vector channel publishing a single plane batch — just enough
        /// for [`scene::region_geometry`] to produce non-empty arrows.
        fn make_snapshot(sequence: u64, plane: PlaneId) -> (FieldSnapshot, ChannelId) {
            make_snapshot_with_values(sequence, plane, [DVec3::X; 4])
        }

        fn make_snapshot_with_values(
            sequence: u64,
            plane: PlaneId,
            values: [DVec3; 4],
        ) -> (FieldSnapshot, ChannelId) {
            let channel = ChannelId::new(PluginId::new("test").unwrap(), "vector").unwrap();
            let batch = plane_batch(plane, DVec3::ZERO, values);
            (snapshot_of(sequence, channel.clone(), vec![batch]), channel)
        }

        /// Two planes at different origins, publishing to the same channel —
        /// for proving one region's change does not invalidate a sibling's
        /// cache entry.
        fn two_plane_snapshot(
            sequence: u64,
            plane_a: PlaneId,
            origin_a: DVec3,
            plane_b: PlaneId,
            origin_b: DVec3,
        ) -> (FieldSnapshot, ChannelId) {
            let channel = ChannelId::new(PluginId::new("test").unwrap(), "vector").unwrap();
            let batches = vec![
                plane_batch(plane_a, origin_a, [DVec3::X; 4]),
                plane_batch(plane_b, origin_b, [DVec3::X; 4]),
            ];
            (snapshot_of(sequence, channel.clone(), batches), channel)
        }

        fn visible_layers(channel: &ChannelId) -> BTreeMap<ChannelId, ui::ChannelLayerSettings> {
            BTreeMap::from([(
                channel.clone(),
                ui::ChannelLayerSettings {
                    visible: true,
                    ..ui::ChannelLayerSettings::default()
                },
            )])
        }

        /// The regression this guards: a slice-plane drag republishes a
        /// snapshot on every pointer-move frame, but this region's own
        /// batch content — sample positions and the values sampled there —
        /// does not change just because the sequence number did. A cache
        /// keyed on batch content, not on `(session, sequence)`, must treat
        /// this as a hit. Poisoning the cached geometry with a vertex a
        /// real computation would never produce, then asserting it either
        /// survives or is discarded, tells reuse and invalidation apart in a
        /// way comparing two honest rebuilds never could — those always
        /// agree.
        #[test]
        fn identical_batch_content_across_a_sequence_bump_is_a_cache_hit() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;
            let key = (channel.clone(), scene::RegionId::Plane(plane));

            let (baseline, mut cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            let baseline_len = baseline.vector_lines.len();
            assert!(baseline_len > 0, "test setup: expected arrows");
            // `baseline` and the cache entry's `geometry` are the same
            // `Arc`; drop this reference to it so the cache's copy is
            // uniquely owned and poisonable below.
            drop(baseline);

            Arc::get_mut(
                &mut cache
                    .get_mut(&key)
                    .expect("plane has a cache entry")
                    .geometry,
            )
            .expect("freshly built, uniquely owned once `baseline` is dropped")
            .vector_lines
            .push(scene::ColoredVertex {
                position: glam::Vec3::splat(9_999.0),
                color: glam::Vec4::ZERO,
            });
            let poisoned_len = cache[&key].geometry.vector_lines.len();
            let poisoned_arc = Arc::clone(&cache[&key].geometry);

            // A higher snapshot sequence, but byte-identical batch content —
            // must still be a cache hit, not a rebuild.
            let (next_snapshot, _) = make_snapshot(1, plane);
            let (reused, cache) = compute_field_layer_geometry(
                cache,
                None,
                Some(&next_snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert_eq!(reused.vector_lines.len(), poisoned_len);
            assert!(
                Arc::ptr_eq(&cache[&key].geometry, &poisoned_arc),
                "identical batch content under a higher sequence number must reuse the \
                 cached region instead of rebuilding it — this is the whole point of \
                 per-region caching"
            );
        }

        /// A batch that genuinely differs (a moved plane, or new field
        /// values at the same position) must still force a rebuild — the
        /// cache must not become permanently stale once it starts comparing
        /// content instead of the snapshot sequence.
        #[test]
        fn a_batch_with_different_values_forces_a_rebuild() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;

            let (baseline, cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            let baseline_len = baseline.vector_lines.len();
            assert!(baseline_len > 0, "test setup: expected arrows");

            let (changed_snapshot, _) = make_snapshot_with_values(1, plane, [DVec3::Y; 4]);
            let (rebuilt, _) = compute_field_layer_geometry(
                cache,
                None,
                Some(&changed_snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert_eq!(
                rebuilt.vector_lines.len(),
                baseline_len,
                "same count, different content"
            );
            assert_ne!(
                rebuilt.vector_lines, baseline.vector_lines,
                "different field values at the same position must produce different arrows, \
                 not a stale reuse of the old direction"
            );
        }

        /// The property this whole redesign exists to add: a sibling
        /// region's own change (here, plane B moving, which also bumps the
        /// snapshot sequence both batches share) must not invalidate plane
        /// A's cache entry, since plane A's own batch is untouched.
        #[test]
        fn a_sibling_regions_own_change_does_not_invalidate_this_regions_cache() {
            let (world, plane_a, plane_b) = world_with_two_planes();
            let origin_a = DVec3::ZERO;
            let (snapshot, channel) =
                two_plane_snapshot(0, plane_a, origin_a, plane_b, DVec3::new(5.0, 0.0, 0.0));
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;
            let key_a = (channel.clone(), scene::RegionId::Plane(plane_a));

            let (_, cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            let arc_a_before = Arc::clone(
                &cache
                    .get(&key_a)
                    .expect("plane_a has a cache entry")
                    .geometry,
            );

            // plane_b moves (and the shared snapshot sequence bumps);
            // plane_a's own batch is byte-identical to before.
            let (moved_snapshot, _) =
                two_plane_snapshot(1, plane_a, origin_a, plane_b, DVec3::new(9.0, 0.0, 0.0));
            let (_, cache) = compute_field_layer_geometry(
                cache,
                None,
                Some(&moved_snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert!(
                Arc::ptr_eq(
                    &cache
                        .get(&key_a)
                        .expect("plane_a still has a cache entry")
                        .geometry,
                    &arc_a_before
                ),
                "plane_a's own batch did not change; its cached geometry must be reused \
                 verbatim even though plane_b — and therefore the snapshot sequence — did"
            );
        }

        /// Regression: [`scene::region_geometry`] returns three output
        /// fields — surface triangles, arrow lines, and flow ribbons — that
        /// are merged into one `FieldGeometry` across regions. Adding
        /// `flow_ribbons` to that struct is easy to do without also
        /// updating the merge, which silently drops every traced streamline
        /// while arrows keep working — exactly what shipped once already.
        #[test]
        fn flow_ribbons_survive_the_per_region_merge() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let mut layers = visible_layers(&channel);
            layers.get_mut(&channel).unwrap().planes.insert(
                plane,
                scene::PlaneLayerSettings {
                    flow_lines: scene::FlowLineDisplay::new(true, 5),
                    ..scene::PlaneLayerSettings::default()
                },
            );

            let (geometry, _) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                scene::SceneVisibility::ALL,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );

            assert!(
                !geometry.flow_ribbons.is_empty(),
                "a visible flow-line layer must reach the merged geometry, not just \
                 scene::region_geometry's own per-region output"
            );
        }

        /// Mirrors the batch-content case above for the layer settings:
        /// hiding a layer changes what is drawn without publishing a new
        /// snapshot, so it must invalidate the cache on its own.
        #[test]
        fn reuses_the_cache_until_the_layer_settings_change() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;
            let key = (channel.clone(), scene::RegionId::Plane(plane));

            let (_, mut cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            Arc::get_mut(
                &mut cache
                    .get_mut(&key)
                    .expect("plane has a cache entry")
                    .geometry,
            )
            .expect("freshly built, uniquely owned by this test")
            .vector_lines
            .push(scene::ColoredVertex {
                position: glam::Vec3::splat(9_999.0),
                color: glam::Vec4::ZERO,
            });

            let hidden_layers = BTreeMap::from([(
                channel.clone(),
                ui::ChannelLayerSettings {
                    visible: false,
                    ..ui::ChannelLayerSettings::default()
                },
            )]);
            let (rebuilt, new_cache) = compute_field_layer_geometry(
                cache,
                None,
                Some(&snapshot),
                &world.snapshot(),
                &hidden_layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert!(
                rebuilt.vector_lines.is_empty(),
                "a hidden layer must discard the stale cache and draw nothing"
            );
            assert!(
                !new_cache.contains_key(&key),
                "a channel no longer visible must not keep its regions' entries around"
            );
        }

        #[test]
        fn no_snapshot_and_no_cache_produces_empty_geometry_without_panicking() {
            let (world, _) = world_with_plane();
            let (geometry, new_cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                None,
                &world.snapshot(),
                &BTreeMap::new(),
                scene::SceneVisibility::ALL,
                &[],
                fieldcad_core::SceneScale::metre(),
            );
            assert!(geometry.surface_triangles.is_empty());
            assert!(geometry.vector_lines.is_empty());
            assert!(new_cache.is_empty());
        }

        /// Mirrors the layer-settings case above for the entity's own
        /// `visible` flag: hiding a plane changes what `region_geometry`
        /// draws, and until the toggle joins the cache key a retained
        /// snapshot keeps serving the hidden plane's arrows (UI-4).
        #[test]
        fn reuses_the_cache_until_entity_visibility_changes() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;
            let key = (channel.clone(), scene::RegionId::Plane(plane));

            let (baseline, mut cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert!(
                !baseline.vector_lines.is_empty(),
                "test setup: expected arrows"
            );
            // `baseline` and the cache entry's `geometry` are the same
            // `Arc`; drop this reference to it so the cache's copy is
            // uniquely owned and poisonable below.
            drop(baseline);
            Arc::get_mut(
                &mut cache
                    .get_mut(&key)
                    .expect("plane has a cache entry")
                    .geometry,
            )
            .expect("freshly built, uniquely owned once `baseline` is dropped")
            .vector_lines
            .push(scene::ColoredVertex {
                position: glam::Vec3::splat(9_999.0),
                color: glam::Vec4::ZERO,
            });
            let poisoned_len = cache[&key].geometry.vector_lines.len();

            // Same snapshot, same settings, same visibility: reused verbatim.
            let (reused, cache) = compute_field_layer_geometry(
                cache,
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert_eq!(reused.vector_lines.len(), poisoned_len);

            // Hide the plane in the believed world. Nothing republishes — the
            // snapshot and every layer setting are unchanged — yet the cache
            // must still invalidate, and the recomputed geometry is empty.
            let mut hidden = world;
            hidden
                .commit([WorldCommand::SetPlaneVisible {
                    plane,
                    visible: false,
                }])
                .unwrap();
            let (rebuilt, _) = compute_field_layer_geometry(
                cache,
                None,
                Some(&snapshot),
                &hidden.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert!(
                rebuilt.vector_lines.is_empty(),
                "a hidden plane must discard the stale cache and draw nothing"
            );
        }

        /// The property the top-level merge cache exists to add: two calls
        /// with byte-identical inputs must return the *same* `Arc`, not a
        /// fresh rebuild that happens to contain the same values — a
        /// redraw that changes nothing (an animated flow line's shader-only
        /// scroll, most commonly) must cost a refcount bump, not a copy of
        /// every visible region's geometry.
        #[test]
        fn nothing_changed_reuses_the_previous_merged_geometry_verbatim() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;

            let (baseline, cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );

            let (reused, _) = compute_field_layer_geometry(
                cache,
                Some(Arc::clone(&baseline)),
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );

            assert!(
                Arc::ptr_eq(&reused, &baseline),
                "identical inputs across two calls must reuse the previous merged \
                 geometry verbatim, not rebuild and re-copy an identical result"
            );
        }

        /// The merge cache must not paper over a real change: once any
        /// region's own cache misses, the previous merged `Arc` is stale and
        /// must not be handed back.
        #[test]
        fn a_changed_region_forces_a_fresh_merged_geometry() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;

            let (baseline, cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );

            let (changed_snapshot, _) = make_snapshot_with_values(1, plane, [DVec3::Y; 4]);
            let (rebuilt, _) = compute_field_layer_geometry(
                cache,
                Some(Arc::clone(&baseline)),
                Some(&changed_snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );

            assert!(
                !Arc::ptr_eq(&rebuilt, &baseline),
                "a region that actually changed must not serve the stale previous merge"
            );
            assert_ne!(
                rebuilt.vector_lines, baseline.vector_lines,
                "the rebuilt geometry must reflect the new field values"
            );
        }

        /// Mirrors the change-forces-a-rebuild case above for a region
        /// disappearing entirely (hidden layer) rather than one of its
        /// inputs changing — `unchanged`'s `cache.is_empty()` check, not its
        /// `any_rebuilt` check, is what has to catch this: no region is
        /// individually rebuilt here, the visible set just shrank.
        #[test]
        fn a_region_going_hidden_forces_a_fresh_merged_geometry() {
            let (world, plane) = world_with_plane();
            let (snapshot, channel) = make_snapshot(0, plane);
            let layers = visible_layers(&channel);
            let show = scene::SceneVisibility::ALL;

            let (baseline, cache) = compute_field_layer_geometry(
                BTreeMap::new(),
                None,
                Some(&snapshot),
                &world.snapshot(),
                &layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );
            assert!(
                !baseline.vector_lines.is_empty(),
                "test setup: expected arrows"
            );

            let hidden_layers = BTreeMap::from([(
                channel.clone(),
                ui::ChannelLayerSettings {
                    visible: false,
                    ..ui::ChannelLayerSettings::default()
                },
            )]);
            let (rebuilt, _) = compute_field_layer_geometry(
                cache,
                Some(Arc::clone(&baseline)),
                Some(&snapshot),
                &world.snapshot(),
                &hidden_layers,
                show,
                std::slice::from_ref(&channel),
                fieldcad_core::SceneScale::metre(),
            );

            assert!(
                !Arc::ptr_eq(&rebuilt, &baseline),
                "a region that disappeared from the visible set must not serve the \
                 stale previous merge, which still included it"
            );
            assert!(rebuilt.vector_lines.is_empty());
        }
    }
}
