//! The authoritative, headless simulation runtime.
//!
//! In local mode this runs in the desktop process; in remote mode the same type
//! runs inside the compute service. Nothing here knows which.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    ChannelId, ChannelSchema, ChannelSnapshot, ClockSnapshot, CommitReport, ComponentSchema,
    ComponentTypeId, DiagnosticSeverity, Domain, FieldBatch, ObjectId, PluginId, PluginProvenance,
    PropertyBag, SampleGeometry, SamplingError, SceneScale, SchemaError, SessionId,
    SimulationClock, SimulationMode, SnapshotCompleteness, SnapshotIdentity, SolverDiagnostic,
    StepContext, TimeStep, World, WorldCheckpoint, WorldCommand, WorldError, WorldRevision,
    WorldSnapshot,
};
use fieldcad_dynamics::{self as dynamics, DynamicsError};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, FieldBrushStroke,
    PluginConfigurationSchema, PluginError, PluginMetadata, ResolvedFieldBrushStroke,
    SolverCancellation, SolverContext,
};
use glam::{DVec2, DVec3, UVec2, UVec3};
use serde::{Deserialize, Serialize};

/// What the runtime should sample when it publishes a snapshot.
///
/// This is a visualization concern, not a physical one: changing it changes how
/// densely a result is observed, never the result itself. Keeping it separate
/// from [`Domain`] is what makes that invariant checkable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// Sample every probe that requested each channel.
    pub probes: bool,
    /// Sample each visible slice plane at this many points along u and v.
    pub planes: Option<UVec2>,
    /// Sample the whole domain on a lattice decimated by this stride.
    pub domain_stride: Option<u32>,
    /// Sample each visible field box at this many points along u, v, and w.
    pub boxes: Option<UVec3>,
    /// Sample each visible field sphere's bounding cube at this many points
    /// per axis.
    pub spheres: Option<u32>,
}

/// Host-owned limit on presentation sampling work requested in one snapshot.
///
/// The same validated limit applies to local UI commands and future remote
/// clients; a widget range is not a security or memory boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SamplingBudget {
    pub max_plane_samples_per_axis: u32,
    pub max_samples_per_snapshot: u64,
}

impl Default for SamplingBudget {
    fn default() -> Self {
        Self {
            max_plane_samples_per_axis: 1_024,
            max_samples_per_snapshot: 16 * 1_024 * 1_024,
        }
    }
}

impl Default for Subscription {
    fn default() -> Self {
        Self {
            probes: true,
            planes: None,
            domain_stride: None,
            boxes: None,
            spheres: None,
        }
    }
}

impl Subscription {
    pub const PROBES_ONLY: Self = Self {
        probes: true,
        planes: None,
        domain_stride: None,
        boxes: None,
        spheres: None,
    };

    pub fn with_planes(mut self, counts: UVec2) -> Self {
        self.planes = Some(counts);
        self
    }

    pub fn with_domain_stride(mut self, stride: u32) -> Self {
        self.domain_stride = Some(stride);
        self
    }

    pub fn with_boxes(mut self, counts: UVec3) -> Self {
        self.boxes = Some(counts);
        self
    }

    pub fn with_spheres(mut self, counts_per_axis: u32) -> Self {
        self.spheres = Some(counts_per_axis);
        self
    }
}

/// Converts elapsed wall-clock time into whole fixed ticks.
///
/// A slow solver must not silently change the numerical time step, so this only
/// ever emits whole `dt` ticks. If the runtime cannot keep up it drops the
/// backlog and says so rather than growing an unbounded debt or stretching `dt`.
///
/// The remainder is carried in `Duration`, i.e. integer nanoseconds. Carrying it
/// as `f64` seconds compounds a rounding error on every poll, which shows up as
/// an occasional dropped or doubled tick — a pacing bug that looks like a
/// physics bug.
#[derive(Clone, Copy, Debug)]
pub struct TickPacer {
    accumulated: Duration,
    max_ticks_per_poll: u32,
}

impl Default for TickPacer {
    fn default() -> Self {
        Self::with_max_ticks(8)
    }
}

impl TickPacer {
    pub const fn with_max_ticks(max_ticks_per_poll: u32) -> Self {
        Self {
            accumulated: Duration::ZERO,
            max_ticks_per_poll: if max_ticks_per_poll == 0 {
                1
            } else {
                max_ticks_per_poll
            },
        }
    }

    /// How many whole ticks are owed for `elapsed`, and whether a backlog was
    /// discarded to get there.
    pub fn ticks_due(&mut self, elapsed: Duration, step: TimeStep) -> TickDemand {
        self.accumulated = self.accumulated.saturating_add(elapsed);

        let step_nanos = Duration::from_secs_f64(step.seconds()).as_nanos();
        if step_nanos == 0 {
            // A sub-nanosecond `dt` cannot be paced against a wall clock. Run the
            // budget and start clean rather than dividing by zero.
            self.accumulated = Duration::ZERO;
            return TickDemand {
                ticks: self.max_ticks_per_poll,
                fell_behind: true,
            };
        }

        let owed = self.accumulated.as_nanos() / step_nanos;
        let consumed = owed * step_nanos;
        self.accumulated = self.accumulated.saturating_sub(Duration::new(
            u64::try_from(consumed / 1_000_000_000).unwrap_or(u64::MAX),
            (consumed % 1_000_000_000) as u32,
        ));

        let owed = u32::try_from(owed).unwrap_or(u32::MAX);
        let ticks = owed.min(self.max_ticks_per_poll);
        TickDemand {
            ticks,
            fell_behind: owed > ticks,
        }
    }

    /// Hands back the budget for ticks that were demanded but never run:
    /// a flush rejection vetoes a cycle after `ticks_due` has already been
    /// paid for it, and the vetoed ticks must stay owed rather than vanish.
    /// Only the demanded (capped) ticks can be returned — backlog beyond the
    /// per-poll budget was already discarded by `ticks_due`, by design.
    pub fn return_ticks(&mut self, ticks: u32, step: TimeStep) {
        let step = Duration::from_secs_f64(step.seconds());
        self.accumulated = self
            .accumulated
            .saturating_add(step.checked_mul(ticks).unwrap_or(Duration::MAX));
    }

    pub fn reset(&mut self) {
        self.accumulated = Duration::ZERO;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickDemand {
    pub ticks: u32,
    pub fell_behind: bool,
}

/// Undo and redo over authored scene edits.
///
/// Entries are whole captured scenes rather than inverse commands. Inverting a
/// command is not generally possible without inventing information — undoing a
/// removal has to put back an object with the identifier it had, which a
/// `CreateObject` cannot do — and the cost that usually argues against
/// snapshots does not apply here: a [`WorldCheckpoint`] is a reference-counted
/// pointer, so an entry costs a pointer plus a label, and the scene it refers
/// to is shared with every other entry that did not change it. Field data is
/// not in the world, so nothing large is retained.
///
/// The stack is bounded anyway, because a session is long and a bound that is
/// never reached costs nothing to have.
struct EditHistory {
    /// Scenes as they stood *before* each recorded edit, oldest first.
    undo: VecDeque<HistoryEntry>,
    /// Scenes undone away from, newest last. Cleared by any new edit.
    redo: VecDeque<HistoryEntry>,
    depth: usize,
    /// Whether the interactive edit in progress has already recorded its entry.
    ///
    /// A drag submits an edit every frame. Without this, undo would step back
    /// one mouse position at a time and be useless for the gesture a user
    /// actually made. The gesture is the unit (ADR 0023), so the first commit
    /// inside one records the scene it started from and the rest join it.
    gesture_recorded: bool,
}

struct HistoryEntry {
    checkpoint: WorldCheckpoint,
    /// Numerical configuration is included only for a domain edit. Ordinary
    /// scene undo deliberately leaves playback settings alone.
    numerical: Option<NumericalCheckpoint>,
    /// What the edit was, in the user's words, for the control that offers it.
    label: String,
    /// A direct numerical edit is inverted by reapplying its signed strength.
    brush: Option<FieldBrushStroke>,
}

#[derive(Clone, Copy)]
struct NumericalCheckpoint {
    domain: Domain,
    time_step: TimeStep,
}

impl EditHistory {
    fn new(depth: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            depth,
            gesture_recorded: false,
        }
    }

    /// Record the scene an edit is about to replace.
    fn record(&mut self, checkpoint: WorldCheckpoint, label: String, coalesce: bool) {
        if coalesce && self.gesture_recorded {
            return;
        }
        self.gesture_recorded = coalesce;
        // A new edit is a new branch; what was undone away from is now
        // unreachable and must not be offered as though it still followed on.
        self.redo.clear();
        if self.depth == 0 {
            return;
        }
        self.undo.push_back(HistoryEntry {
            checkpoint,
            numerical: None,
            label,
            brush: None,
        });
        while self.undo.len() > self.depth {
            self.undo.pop_front();
        }
    }

    fn record_domain(&mut self, checkpoint: WorldCheckpoint, numerical: NumericalCheckpoint) {
        self.gesture_recorded = false;
        self.redo.clear();
        if self.depth == 0 {
            return;
        }
        self.undo.push_back(HistoryEntry {
            checkpoint,
            numerical: Some(numerical),
            label: "Change numerical domain".to_owned(),
            brush: None,
        });
        while self.undo.len() > self.depth {
            self.undo.pop_front();
        }
    }

    fn record_brush(&mut self, checkpoint: WorldCheckpoint, stroke: FieldBrushStroke) {
        self.gesture_recorded = false;
        self.redo.clear();
        if self.depth == 0 {
            return;
        }
        self.undo.push_back(HistoryEntry {
            checkpoint,
            numerical: None,
            label: "Paint field".to_owned(),
            brush: Some(stroke),
        });
        while self.undo.len() > self.depth {
            self.undo.pop_front();
        }
    }

    /// Begin a new coalescing window, whether or not the last one recorded.
    const fn begin_gesture(&mut self) {
        self.gesture_recorded = false;
    }

    /// Forget everything.
    ///
    /// Used when the world stops being the one the entries describe — a solver
    /// tick that moves a body replaces the authored scene with a computed one,
    /// and "the scene before my edit" no longer names anything that exists.
    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.gesture_recorded = false;
    }

    fn status(&self) -> EditHistoryStatus {
        EditHistoryStatus {
            undo: self.undo.back().map(|entry| entry.label.clone()),
            redo: self.redo.back().map(|entry| entry.label.clone()),
            undo_depth: self.undo.len(),
            redo_depth: self.redo.len(),
        }
    }
}

/// What the edit history currently offers, for a control that presents it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditHistoryStatus {
    /// The next edit undo would reverse, named. `None` when there is none.
    pub undo: Option<String>,
    /// The next edit redo would reapply, named.
    pub redo: Option<String>,
    pub undo_depth: usize,
    pub redo_depth: usize,
}

impl EditHistoryStatus {
    pub const fn can_undo(&self) -> bool {
        self.undo.is_some()
    }

    pub const fn can_redo(&self) -> bool {
        self.redo.is_some()
    }
}

pub struct PluginRegistration {
    pub plugin: Box<dyn EquationSystemPlugin>,
    pub configuration: PropertyBag,
    pub enabled: bool,
    pub realtime: bool,
}

impl PluginRegistration {
    pub fn with_default_configuration(plugin: Box<dyn EquationSystemPlugin>) -> Self {
        let configuration = plugin.default_configuration();
        Self {
            plugin,
            configuration,
            enabled: true,
            realtime: true,
        }
    }

    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub const fn with_realtime(mut self, realtime: bool) -> Self {
        self.realtime = realtime;
        self
    }
}

struct PluginSlot {
    metadata: PluginMetadata,
    /// Index in this vector is the plugin's [`ChannelHandle`].
    channels: Vec<Arc<ChannelSchema>>,
    configuration_schema: PluginConfigurationSchema,
    plugin: Box<dyn EquationSystemPlugin>,
    configuration: PropertyBag,
    /// Solver memory exists only while this field system is active. Recreating
    /// it on activation initializes the system at the current scene time rather
    /// than resuming stale state after inactive ticks.
    solver: Option<Box<dyn EquationSystemSolver>>,
    enabled: bool,
    /// Whether this system reacts to every intermediate value of an interactive
    /// edit. When false it keeps its last complete result for the duration of
    /// the gesture and is brought current once, at the boundary.
    realtime: bool,
}

impl PluginSlot {
    fn declares(&self, channel: &ChannelId) -> bool {
        self.channels.iter().any(|schema| &schema.id == channel)
    }

    fn handles(&self) -> impl Iterator<Item = (ChannelHandle, &Arc<ChannelSchema>)> {
        self.channels
            .iter()
            .enumerate()
            .map(|(index, schema)| (ChannelHandle::new(index as u16), schema))
    }

    fn solver(&self) -> &dyn EquationSystemSolver {
        self.solver
            .as_deref()
            .expect("an enabled field system always owns a solver")
    }

    fn solver_mut(&mut self) -> &mut dyn EquationSystemSolver {
        self.solver
            .as_deref_mut()
            .expect("an enabled field system always owns a solver")
    }
}

/// One equation system available to a simulation scene.
///
/// This catalog is deliberately independent of snapshot provenance: an
/// inactive system must remain discoverable in the scene inspector even though
/// it publishes no channels. Component schemas are registered separately on
/// the world and likewise remain available while the system is inactive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldSystemStatus {
    pub plugin: PluginMetadata,
    pub channels: Vec<ChannelSchema>,
    /// Vector channels the active numerical solver accepts direct painting for.
    /// Empty for analytical and inactive systems.
    pub mutable_vector_channels: Vec<ChannelId>,
    /// The declared settings and their authoritative values. Keeping these in
    /// the source-owned catalog lets both local and future remote scenes report
    /// exactly which numerical scenario is active.
    pub configuration_schema: PluginConfigurationSchema,
    pub configuration: PropertyBag,
    pub enabled: bool,
    /// Whether this system recomputes on every intermediate value while an
    /// interactive edit is in progress, or waits for the edit to be committed.
    pub realtime: bool,
}

/// Owns solver memory for one session and publishes immutable field snapshots.
pub struct SimulationRuntime {
    world: World,
    clock: SimulationClock,
    domain: Domain,
    /// How many metres one render/camera unit represents. Purely a
    /// presentation concern for the desktop viewport — see
    /// [`CommandPayload::SetSceneScale`](crate::source::CommandPayload::SetSceneScale).
    scene_scale: SceneScale,
    subscription: Subscription,
    sampling_budget: SamplingBudget,
    session: SessionId,
    next_sequence: u64,
    /// Identifies a fresh numerical run within one long-lived source session.
    /// It changes when solver state is discarded for a domain reconfiguration.
    run_generation: u64,
    plugins: Vec<PluginSlot>,
    cancellation: SolverCancellation,
    latest: Arc<fieldcad_core::FieldSnapshot>,
    /// The world revision an interactive edit started from, while one is in
    /// progress.
    ///
    /// An interactive edit is a scene edit that spans frames — a viewport drag,
    /// or an inspector control held down — and its intermediate values are
    /// authored, not physical. Systems that opted out of realtime update ignore
    /// them and are brought current once, when the gesture ends. Keeping the
    /// starting revision here is what lets that final catch-up be skipped when
    /// the gesture changed nothing.
    interactive_edit: Option<WorldRevision>,
    history: EditHistory,
    /// The dynamics system's summed force on every body it advanced at the
    /// most recent tick.
    ///
    /// A byproduct of `apply_tick` that used to be computed and discarded
    /// immediately after `dynamics::integrate` consumed it; retaining it costs
    /// nothing extra and is what lets an inspector show "what force is this
    /// body feeling right now" without a second computation path. Bodies a
    /// solver kinematically owns (its own pusher, not summed forces — see
    /// `fieldcad_dynamics`'s module docs) and pinned/carried bodies have no
    /// entry, because neither has a force this system computed for it.
    last_forces: BTreeMap<ObjectId, DVec3>,
    /// Wall-clock time `apply_tick` took to complete its most recent tick,
    /// in milliseconds — everything a fixed step actually costs: force
    /// collection, every time-stepped solver's own advance, dynamics
    /// integration, and the snapshot it publishes. Zero until the first tick
    /// runs. Presentation only, like `last_forces`: nothing here reads this
    /// to decide a physical result, only to tell a user whether their
    /// machine can keep up with the configured dt.
    last_tick_compute_ms: f32,
}

/// Everything needed to stand up a runtime.
pub struct RuntimeConfig {
    pub world: World,
    pub domain: Domain,
    pub time_step: TimeStep,
    pub session: SessionId,
    pub subscription: Subscription,
    /// How many metres one render/camera unit represents. Defaults to
    /// [`SceneScale::metre`].
    pub scene_scale: SceneScale,
    pub sampling_budget: SamplingBudget,
    pub plugins: Vec<PluginRegistration>,
    /// How many authored edits can be stepped back through.
    pub undo_depth: usize,
}

/// Deep enough that a session's worth of authoring is reachable, shallow enough
/// to be a bound rather than a promise to retain everything.
pub const DEFAULT_UNDO_DEPTH: usize = 128;

impl RuntimeConfig {
    pub fn new(domain: Domain, time_step: TimeStep, session: SessionId) -> Self {
        Self {
            world: World::new(),
            domain,
            time_step,
            session,
            subscription: Subscription::default(),
            scene_scale: SceneScale::default(),
            sampling_budget: SamplingBudget::default(),
            plugins: Vec::new(),
            undo_depth: DEFAULT_UNDO_DEPTH,
        }
    }

    pub const fn with_undo_depth(mut self, undo_depth: usize) -> Self {
        self.undo_depth = undo_depth;
        self
    }

    pub fn with_world(mut self, world: World) -> Self {
        self.world = world;
        self
    }

    pub fn with_subscription(mut self, subscription: Subscription) -> Self {
        self.subscription = subscription;
        self
    }

    pub const fn with_scene_scale(mut self, scene_scale: SceneScale) -> Self {
        self.scene_scale = scene_scale;
        self
    }

    pub fn with_sampling_budget(mut self, sampling_budget: SamplingBudget) -> Self {
        self.sampling_budget = sampling_budget;
        self
    }

    pub fn with_plugin(mut self, plugin: Box<dyn EquationSystemPlugin>) -> Self {
        self.plugins
            .push(PluginRegistration::with_default_configuration(plugin));
        self
    }

    pub fn with_plugin_registration(mut self, registration: PluginRegistration) -> Self {
        self.plugins.push(registration);
        self
    }
}

impl SimulationRuntime {
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let RuntimeConfig {
            mut world,
            domain,
            time_step,
            session,
            subscription,
            scene_scale,
            sampling_budget,
            plugins,
            undo_depth,
        } = config;

        let mut plugin_ids = BTreeSet::new();
        let mut channel_ids: BTreeMap<ChannelId, (PluginId, Arc<ChannelSchema>)> = BTreeMap::new();
        let mut prepared = Vec::with_capacity(plugins.len());
        let mut component_schemas: BTreeMap<ComponentTypeId, (PluginId, ComponentSchema)> =
            BTreeMap::new();

        // A physical property can be consumed by several equation systems, so
        // component identity belongs to the shared domain Module rather than
        // whichever plugin happens to register first. Identical contributions
        // compose; incompatible definitions are rejected before any solver is
        // created.
        for registration in &plugins {
            let metadata = registration.plugin.metadata();
            if !plugin_ids.insert(metadata.id.clone()) {
                return Err(RuntimeError::DuplicatePlugin(metadata.id));
            }
            for component in registration.plugin.component_schemas() {
                if let Some((first_plugin, first_schema)) = component_schemas.get(&component.id) {
                    if first_schema != &component {
                        return Err(RuntimeError::ConflictingComponentSchema {
                            component: component.id,
                            first_plugin: first_plugin.clone(),
                            second_plugin: metadata.id.clone(),
                        });
                    }
                } else {
                    component_schemas
                        .insert(component.id.clone(), (metadata.id.clone(), component));
                }
            }
        }

        // Schema registration goes through the command Interface like any
        // other edit, so the revision a solver is initialized against already
        // includes every schema it can see.
        if !component_schemas.is_empty() {
            world.commit(
                component_schemas
                    .into_values()
                    .map(|(_, schema)| WorldCommand::RegisterComponentSchema(schema)),
            )?;
        }

        let clock = SimulationClock::new(time_step);
        let cancellation = SolverCancellation::default();
        for registration in plugins {
            let metadata = registration.plugin.metadata();
            let configuration_schema = registration.plugin.configuration_schema();
            configuration_schema.validate(&registration.configuration)?;

            let declared = registration.plugin.channels();
            if declared.len() > usize::from(u16::MAX) {
                return Err(RuntimeError::TooManyChannels(metadata.id));
            }
            let mut channels = Vec::with_capacity(declared.len());
            for channel in declared {
                // A field channel names a physical quantity, so several systems
                // may declare the same one — that is what makes them models of
                // one field rather than two fields with the same name. The rule
                // is the one shared component schemas already use (ADR 0017):
                // identical declarations compose, incompatible ones are rejected
                // before any solver is created.
                match channel_ids.get(&channel.id) {
                    Some((first_plugin, first)) if first.as_ref() != &channel => {
                        return Err(RuntimeError::ConflictingChannelSchema {
                            channel: channel.id,
                            first_plugin: first_plugin.clone(),
                            second_plugin: metadata.id.clone(),
                        });
                    }
                    Some((_, first)) => channels.push(Arc::clone(first)),
                    None => {
                        let shared = Arc::new(channel);
                        channel_ids.insert(
                            shared.id.clone(),
                            (metadata.id.clone(), Arc::clone(&shared)),
                        );
                        channels.push(shared);
                    }
                }
            }

            let world_snapshot = world.snapshot();
            let solver = if registration.enabled {
                let solver = registration.plugin.create_solver(SolverContext {
                    configuration: &registration.configuration,
                    domain: &domain,
                    world: &world_snapshot,
                    initial_step: clock.snapshot().step,
                    cancellation: cancellation.clone(),
                })?;
                solver.validate_world(&world_snapshot)?;
                solver.validate_time_step(time_step)?;
                Some(solver)
            } else {
                None
            };
            prepared.push(PluginSlot {
                metadata,
                channels,
                configuration_schema,
                plugin: registration.plugin,
                configuration: registration.configuration,
                solver,
                enabled: registration.enabled,
                realtime: registration.realtime,
            });
        }

        let mut runtime = Self {
            world,
            clock,
            domain,
            scene_scale,
            subscription,
            sampling_budget,
            session,
            next_sequence: 0,
            run_generation: 0,
            plugins: prepared,
            cancellation,
            latest: Arc::new(empty_snapshot(session, domain)),
            interactive_edit: None,
            history: EditHistory::new(undo_depth),
            last_forces: BTreeMap::new(),
            last_tick_compute_ms: 0.0,
        };
        runtime.check_single_provider_per_field()?;
        let world_snapshot = runtime.world.snapshot();
        for slot in runtime.plugins.iter_mut().filter(|slot| slot.enabled) {
            slot.solver_mut().on_world_changed(&world_snapshot)?;
        }
        runtime.validate_subscription(runtime.subscription)?;
        runtime.publish_snapshot(SamplingPolicy::All)?;
        Ok(runtime)
    }

    pub fn world_snapshot(&self) -> WorldSnapshot {
        self.world.snapshot()
    }

    pub fn clock_snapshot(&self) -> ClockSnapshot {
        self.clock.snapshot()
    }

    pub const fn domain(&self) -> &Domain {
        &self.domain
    }

    pub const fn scene_scale(&self) -> SceneScale {
        self.scene_scale
    }

    /// Adopt a new render/camera scale. Purely a presentation setting: it
    /// never touches `world`, `domain`, or solver state, and does not enter
    /// undo history.
    pub const fn set_scene_scale(&mut self, scene_scale: SceneScale) {
        self.scene_scale = scene_scale;
    }

    pub const fn run_generation(&self) -> u64 {
        self.run_generation
    }

    pub const fn subscription(&self) -> Subscription {
        self.subscription
    }

    /// Every equation system composed into this scene, including inactive
    /// systems and the channels they would publish when enabled.
    pub fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.plugins
            .iter()
            .map(|slot| FieldSystemStatus {
                plugin: slot.metadata.clone(),
                channels: slot
                    .channels
                    .iter()
                    .map(|channel| channel.as_ref().clone())
                    .collect(),
                mutable_vector_channels: if slot.enabled {
                    slot.solver()
                        .mutable_vector_channels()
                        .iter()
                        .filter_map(|handle| {
                            slot.channels
                                .get(handle.index())
                                .map(|schema| schema.id.clone())
                        })
                        .collect()
                } else {
                    Vec::new()
                },
                configuration_schema: slot.configuration_schema.clone(),
                configuration: slot.configuration.clone(),
                enabled: slot.enabled,
                realtime: slot.realtime,
            })
            .collect()
    }

    /// Whether an interactive edit is currently in progress.
    pub const fn is_editing(&self) -> bool {
        self.interactive_edit.is_some()
    }

    /// What undo and redo currently offer.
    pub fn edit_history(&self) -> EditHistoryStatus {
        self.history.status()
    }

    /// Treat the current scene as where this session starts.
    ///
    /// Setting a session up — a default scene, a loaded file — authors through
    /// the same command path a user's edits take, which is what keeps validation
    /// and provenance uniform. It should not also be offered as something to
    /// step back through: the first undo of a session emptying the workspace is
    /// not a feature.
    pub fn clear_edit_history(&mut self) {
        self.history.clear();
    }

    /// Step back to the scene as it stood before the most recent authored edit.
    ///
    /// A no-op when there is nothing recorded, so a shortcut pressed one time too
    /// many is not an error to report.
    pub fn undo(&mut self) -> Result<(), RuntimeError> {
        self.step_history(HistoryDirection::Undo)
    }

    /// Reapply the most recently undone edit.
    pub fn redo(&mut self) -> Result<(), RuntimeError> {
        self.step_history(HistoryDirection::Redo)
    }

    fn step_history(&mut self, direction: HistoryDirection) -> Result<(), RuntimeError> {
        // An undo is defined against a scene. While the clock is advancing, the
        // scene it refers to is being replaced underneath it, and the boundary
        // the restored world would land on is whichever tick happened to be next
        // — which is not a boundary the user chose. Pausing is the same one
        // click that single-stepping already requires.
        if self.clock.snapshot().mode == SimulationMode::Running {
            return Err(RuntimeError::CannotEditHistoryWhileRunning);
        }
        let source = match direction {
            HistoryDirection::Undo => &mut self.history.undo,
            HistoryDirection::Redo => &mut self.history.redo,
        };
        let Some(entry) = source.pop_back() else {
            return Ok(());
        };

        // The scene being left is what the opposite direction returns to. Take
        // it before adopting, and only commit the swap once adoption succeeded,
        // so a restored world an active solver refuses leaves the history
        // exactly as it was.
        let leaving = HistoryEntry {
            checkpoint: self.world.checkpoint(),
            numerical: entry.numerical.map(|_| NumericalCheckpoint {
                domain: self.domain,
                time_step: self.clock.time_step(),
            }),
            label: entry.label.clone(),
            brush: entry.brush.clone(),
        };
        let result = if let Some(stroke) = &entry.brush {
            let stroke = if direction == HistoryDirection::Undo {
                Self::inverted_brush(stroke)?
            } else {
                stroke.clone()
            };
            self.apply_field_brush_stroke_inner(stroke)
        } else {
            self.adopt_history_entry(&entry)
        };
        if let Err(error) = result {
            match direction {
                HistoryDirection::Undo => self.history.undo.push_back(entry),
                HistoryDirection::Redo => self.history.redo.push_back(entry),
            }
            return Err(error);
        }
        match direction {
            HistoryDirection::Undo => self.history.redo.push_back(leaving),
            HistoryDirection::Redo => self.history.undo.push_back(leaving),
        }
        // Nothing new was authored, so the coalescing window is meaningless now.
        self.history.begin_gesture();
        self.publish_snapshot(SamplingPolicy::All)
    }

    fn adopt_history_entry(&mut self, entry: &HistoryEntry) -> Result<(), RuntimeError> {
        self.adopt_checkpoint(&entry.checkpoint)?;
        if let Some(numerical) = entry.numerical {
            self.reconfigure_domain_inner(numerical.domain, Some(numerical.time_step))?;
        }
        Ok(())
    }

    /// Validate and adopt a captured scene, on the same terms as an edit.
    ///
    /// Restoring is not privileged: a scene that was valid when it was captured
    /// can have stopped being representable since — a field system enabled in the
    /// meantime may reject it — so every active solver sees the candidate before
    /// the world moves (ADR 0007).
    fn adopt_checkpoint(&mut self, checkpoint: &WorldCheckpoint) -> Result<(), RuntimeError> {
        let mut candidate = self.world.clone();
        if candidate.restore(checkpoint) == self.world.revision() {
            return Ok(());
        }

        let candidate_snapshot = candidate.snapshot();
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            slot.solver().validate_world(&candidate_snapshot)?;
        }

        let editing = self.is_editing();
        self.world = candidate;
        for slot in self
            .plugins
            .iter_mut()
            .filter(|slot| slot.enabled && (slot.realtime || !editing))
        {
            slot.solver_mut().on_world_changed(&candidate_snapshot)?;
        }
        Ok(())
    }

    /// Choose whether one equation system follows every intermediate value of an
    /// interactive edit.
    ///
    /// This is a cost/latency choice, not a physical one: a system that waits
    /// computes the same result from the same committed world, just once instead
    /// of once per frame of a drag. It is what keeps a scene draggable when an
    /// analytic evaluator is expensive enough that recomputing it per frame
    /// makes the viewport unusable.
    pub fn set_field_system_realtime(
        &mut self,
        plugin: &PluginId,
        realtime: bool,
    ) -> Result<(), RuntimeError> {
        let index = self
            .plugins
            .iter()
            .position(|slot| &slot.metadata.id == plugin)
            .ok_or_else(|| RuntimeError::UnknownPlugin(plugin.clone()))?;
        if self.plugins[index].realtime == realtime {
            return Ok(());
        }
        self.plugins[index].realtime = realtime;

        // Becoming realtime part-way through a gesture means this system has
        // been ignoring world edits it now claims to follow. Catch it up rather
        // than leaving it silently behind until the gesture ends.
        if realtime && self.is_editing() && self.plugins[index].enabled {
            let world = self.world.snapshot();
            self.plugins[index].solver_mut().on_world_changed(&world)?;
        }
        // Every branch above changes what `field_systems()` reports (the
        // `realtime` flag itself), not just the mid-gesture catch-up, so
        // every branch must publish — a consumer that caches
        // `field_systems()` behind the published sequence would otherwise
        // show a stale toggle until an unrelated publication happened to
        // come along.
        self.publish_snapshot(SamplingPolicy::All)?;
        Ok(())
    }

    /// Open or close an interactive edit.
    ///
    /// Closing one is the commit boundary: every system that deferred is shown
    /// the committed world and republishes, so what is on screen when the user
    /// lets go is computed from the values they let go of.
    pub fn set_interactive_edit(&mut self, editing: bool) -> Result<(), RuntimeError> {
        if editing {
            if self.interactive_edit.is_none() {
                self.interactive_edit = Some(self.world.revision());
                // One gesture is one undo step, so this opens the window the
                // gesture's edits coalesce into.
                self.history.begin_gesture();
            }
            return Ok(());
        }

        let Some(started_at) = self.interactive_edit.take() else {
            return Ok(());
        };
        if started_at == self.world.revision() {
            // A gesture that committed nothing — a drag that never moved, or a
            // value typed and left unchanged — leaves nothing to recompute.
            return Ok(());
        }

        let world = self.world.snapshot();
        let mut deferred = false;
        for slot in self
            .plugins
            .iter_mut()
            .filter(|slot| slot.enabled && !slot.realtime)
        {
            slot.solver_mut().on_world_changed(&world)?;
            deferred = true;
        }
        if deferred {
            self.publish_snapshot(SamplingPolicy::All)?;
        }
        Ok(())
    }

    pub fn cancellation(&self) -> SolverCancellation {
        self.cancellation.clone()
    }

    /// Which active system, if any, computes `channel`.
    pub fn field_provider(&self, channel: &ChannelId) -> Option<PluginId> {
        self.provider_slot(channel)
            .map(|slot| slot.metadata.id.clone())
    }

    fn provider_slot(&self, channel: &ChannelId) -> Option<&PluginSlot> {
        self.plugins
            .iter()
            .find(|slot| slot.enabled && slot.declares(channel))
    }

    /// Reject a composition in which one field would be computed twice.
    ///
    /// Two active models of one field are not two fields: they would publish
    /// contradictory values under one identity and, worse, each contribute the
    /// force their own field exerts, so a charge would feel `qE` twice from two
    /// disagreeing models of the same interaction.
    fn check_single_provider_per_field(&self) -> Result<(), RuntimeError> {
        let mut providers: BTreeMap<&ChannelId, &PluginId> = BTreeMap::new();
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            for schema in &slot.channels {
                if let Some(first) = providers.insert(&schema.id, &slot.metadata.id) {
                    return Err(RuntimeError::ConflictingFieldProvider {
                        channel: schema.id.clone(),
                        active_plugin: first.clone(),
                        requested_plugin: slot.metadata.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Choose which equation system computes `channel`, or none.
    ///
    /// The scene has one of each field, so choosing a model for it is one
    /// operation rather than a disable followed by an enable: the intermediate
    /// state, in which nothing computes the field the user is asking about,
    /// is not a state anything should observe or be left stranded in.
    pub fn set_field_model(
        &mut self,
        channel: &ChannelId,
        provider: Option<&PluginId>,
    ) -> Result<(), RuntimeError> {
        let known = self
            .plugins
            .iter()
            .any(|slot| slot.channels.iter().any(|schema| &schema.id == channel));
        if !known {
            return Err(RuntimeError::UnknownFieldChannel(channel.clone()));
        }
        if let Some(requested) = provider
            && !self
                .plugins
                .iter()
                .any(|slot| &slot.metadata.id == requested && slot.declares(channel))
        {
            return Err(RuntimeError::UnknownFieldChannel(channel.clone()));
        }

        let current = self.field_provider(channel);
        if current.as_ref() == provider.cloned().as_ref() {
            return Ok(());
        }

        // A system computes all of its fields or none of them, because one
        // solver couples them: Maxwell cannot advance `E` without `B`. Choosing
        // it as the model of one field therefore chooses it for the rest, and
        // every system it overlaps with stands down. Refusing instead would
        // leave a field whose only model overlaps an active one unreachable
        // from its own control.
        let displaced: Vec<PluginId> = match provider {
            Some(requested) => {
                let claimed: Vec<ChannelId> = self
                    .plugins
                    .iter()
                    .find(|slot| &slot.metadata.id == requested)
                    .map(|slot| {
                        slot.channels
                            .iter()
                            .map(|schema| schema.id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                self.plugins
                    .iter()
                    .filter(|slot| slot.enabled && &slot.metadata.id != requested)
                    .filter(|slot| claimed.iter().any(|channel| slot.declares(channel)))
                    .map(|slot| slot.metadata.id.clone())
                    .collect()
            }
            None => current.into_iter().collect(),
        };

        // Stand the old models down first, so the new one is validated against a
        // composition it can actually join.
        for plugin in &displaced {
            self.set_field_system_enabled(plugin, false)?;
        }
        let Some(requested) = provider else {
            return Ok(());
        };
        if let Err(error) = self.set_field_system_enabled(requested, true) {
            // Put back what was computing these fields. A refused choice must
            // not cost the user the models they already had.
            for plugin in &displaced {
                self.set_field_system_enabled(plugin, true)?;
            }
            return Err(error);
        }
        Ok(())
    }

    /// Enable or disable one equation system without unregistering the object
    /// properties it contributed to the world.
    ///
    /// Re-enabling first validates and adopts the current scene and time step,
    /// so edits made while the system was inactive cannot silently introduce an
    /// invalid simulation state.
    pub fn set_field_system_enabled(
        &mut self,
        plugin: &PluginId,
        enabled: bool,
    ) -> Result<(), RuntimeError> {
        let index = self
            .plugins
            .iter()
            .position(|slot| &slot.metadata.id == plugin)
            .ok_or_else(|| RuntimeError::UnknownPlugin(plugin.clone()))?;
        if self.plugins[index].enabled == enabled {
            return Ok(());
        }

        if enabled {
            // A second model of a field this scene already computes is refused
            // rather than silently replacing the first: which model computes a
            // field is the user's choice, not a consequence of activation order.
            for schema in &self.plugins[index].channels {
                if let Some(active) = self.provider_slot(&schema.id) {
                    return Err(RuntimeError::ConflictingFieldProvider {
                        channel: schema.id.clone(),
                        active_plugin: active.metadata.id.clone(),
                        requested_plugin: plugin.clone(),
                    });
                }
            }
            let world = self.world.snapshot();
            let mut solver = self.plugins[index].plugin.create_solver(SolverContext {
                configuration: &self.plugins[index].configuration,
                domain: &self.domain,
                world: &world,
                initial_step: self.clock.snapshot().step,
                cancellation: self.cancellation.clone(),
            })?;
            solver.validate_world(&world)?;
            solver.validate_time_step(self.clock.time_step())?;
            solver.on_world_changed(&world)?;

            // An inactive system's channels do not count against the current
            // sampling budget. Include them before activation so the command is
            // rejected rather than publishing an unexpectedly oversized frame.
            self.plugins[index].solver = Some(solver);
            self.plugins[index].enabled = true;
            if let Err(error) = self.validate_subscription(self.subscription) {
                self.plugins[index].enabled = false;
                self.plugins[index].solver = None;
                return Err(error);
            }
        } else {
            self.plugins[index].enabled = false;
        }

        if let Err(error) = self.publish_snapshot(SamplingPolicy::All) {
            self.plugins[index].enabled = !enabled;
            if enabled {
                self.plugins[index].solver = None;
            }
            return Err(error);
        }
        if !enabled {
            self.plugins[index].solver = None;
        }
        Ok(())
    }

    /// Change what is sampled. This never changes a computed value, only how
    /// densely it is observed, so it does not advance the world revision.
    pub fn set_subscription(&mut self, subscription: Subscription) -> Result<(), RuntimeError> {
        if subscription == self.subscription {
            return Ok(());
        }
        self.validate_subscription(subscription)?;
        let previous = std::mem::replace(&mut self.subscription, subscription);
        if let Err(error) = self.publish_snapshot(SamplingPolicy::All) {
            // Publication builds its candidate off to the side and changes
            // `latest` only at the end, so restoring this field makes the
            // rejected command completely unobservable.
            self.subscription = previous;
            return Err(error);
        }
        Ok(())
    }

    fn validate_subscription(&self, subscription: Subscription) -> Result<(), RuntimeError> {
        if self.sampling_budget.max_plane_samples_per_axis == 0
            || self.sampling_budget.max_samples_per_snapshot == 0
        {
            return Err(RuntimeError::InvalidSamplingBudget);
        }
        if let Some(counts) = subscription.planes {
            if counts.min_element() == 0 {
                return Err(RuntimeError::InvalidSubscription(
                    "plane counts must be non-zero when plane sampling is enabled".to_owned(),
                ));
            }
            if counts.max_element() > self.sampling_budget.max_plane_samples_per_axis {
                return Err(RuntimeError::InvalidSubscription(format!(
                    "plane counts exceed the per-axis limit of {}",
                    self.sampling_budget.max_plane_samples_per_axis
                )));
            }
        }
        if subscription.domain_stride == Some(0) {
            return Err(RuntimeError::InvalidSubscription(
                "domain stride must be non-zero when grid sampling is enabled".to_owned(),
            ));
        }
        if let Some(counts) = subscription.boxes {
            if counts.min_element() == 0 {
                return Err(RuntimeError::InvalidSubscription(
                    "box counts must be non-zero when box sampling is enabled".to_owned(),
                ));
            }
            if counts.max_element() > self.sampling_budget.max_plane_samples_per_axis {
                return Err(RuntimeError::InvalidSubscription(format!(
                    "box counts exceed the per-axis limit of {}",
                    self.sampling_budget.max_plane_samples_per_axis
                )));
            }
        }
        if let Some(density) = subscription.spheres {
            if density == 0 {
                return Err(RuntimeError::InvalidSubscription(
                    "sphere density must be non-zero when sphere sampling is enabled".to_owned(),
                ));
            }
            if density > self.sampling_budget.max_plane_samples_per_axis {
                return Err(RuntimeError::InvalidSubscription(format!(
                    "sphere density exceeds the per-axis limit of {}",
                    self.sampling_budget.max_plane_samples_per_axis
                )));
            }
        }

        let world = self.world.snapshot();
        let mut requested = 0_u64;
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            for schema in &slot.channels {
                if subscription.probes {
                    requested = requested.saturating_add(
                        world
                            .probes()
                            .values()
                            .filter(|probe| probe.channels.contains(&schema.id))
                            .count() as u64,
                    );
                }
                if let Some(counts) = subscription.planes {
                    let planes = world
                        .planes()
                        .values()
                        .filter(|plane| plane.visible)
                        .count() as u64;
                    requested = requested.saturating_add(
                        planes
                            .saturating_mul(u64::from(counts.x))
                            .saturating_mul(u64::from(counts.y)),
                    );
                }
                if let Some(stride) = subscription.domain_stride {
                    requested = requested
                        .saturating_add(self.domain.decimated_lattice(stride).len() as u64);
                }
                if let Some(counts) = subscription.boxes {
                    let boxes = world
                        .boxes()
                        .values()
                        .filter(|region| region.visible)
                        .count() as u64;
                    requested = requested.saturating_add(
                        boxes
                            .saturating_mul(u64::from(counts.x))
                            .saturating_mul(u64::from(counts.y))
                            .saturating_mul(u64::from(counts.z)),
                    );
                }
                if let Some(density) = subscription.spheres {
                    let per_sphere = u64::from(density)
                        .saturating_mul(u64::from(density))
                        .saturating_mul(u64::from(density));
                    let spheres = world
                        .spheres()
                        .values()
                        .filter(|sphere| sphere.visible)
                        .count() as u64;
                    requested = requested.saturating_add(spheres.saturating_mul(per_sphere));
                }
                if requested > self.sampling_budget.max_samples_per_snapshot {
                    return Err(RuntimeError::SamplingBudgetExceeded {
                        requested,
                        limit: self.sampling_budget.max_samples_per_snapshot,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn latest_snapshot(&self) -> Arc<fieldcad_core::FieldSnapshot> {
        Arc::clone(&self.latest)
    }

    /// The dynamics system's summed force on `object` as of the most recent
    /// tick, if it was one of the bodies that system advanced. `None` for an
    /// object with no mass, a pinned/carried body, a body a solver moves with
    /// its own pusher, or before the first tick.
    pub fn body_force(&self, object: ObjectId) -> Option<DVec3> {
        self.last_forces.get(&object).copied()
    }

    /// Every body force from the most recent tick, for a source that captures
    /// its whole state wholesale (see `async_source::SourceState`) rather than
    /// querying one object at a time.
    pub fn body_forces(&self) -> BTreeMap<ObjectId, DVec3> {
        self.last_forces.clone()
    }

    /// Wall-clock milliseconds `apply_tick` took to complete the most recent
    /// tick. Zero before the first tick runs.
    pub fn last_tick_compute_ms(&self) -> f32 {
        self.last_tick_compute_ms
    }

    /// True if any registered solver evolves in time. When false, ticks cannot
    /// change a result and the runtime need not republish one.
    pub fn has_time_dependent_solver(&self) -> bool {
        self.plugins
            .iter()
            .any(|slot| slot.enabled && slot.solver().kind().advances_with_time())
    }

    pub fn play(&mut self) {
        self.clock.play();
    }

    pub fn pause(&mut self) {
        self.clock.pause();
    }

    pub fn set_time_step(&mut self, time_step: TimeStep) -> Result<(), RuntimeError> {
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            slot.solver().validate_time_step(time_step)?;
        }
        self.clock.set_time_step(time_step);
        Ok(())
    }

    /// Atomically replace the numerical lattice and rebuild every active solver
    /// from the current authored world. A changed lattice invalidates all
    /// evolved state, so the replacement always starts paused at tick/time zero.
    pub fn reconfigure_domain(&mut self, domain: Domain) -> Result<(), RuntimeError> {
        let previous_domain = self.domain;
        let previous_time_step = self.clock.time_step();
        let before = self.world.checkpoint();
        self.reconfigure_domain_inner(domain, None)?;
        if domain != previous_domain {
            self.history.record_domain(
                before,
                NumericalCheckpoint {
                    domain: previous_domain,
                    time_step: previous_time_step,
                },
            );
        }
        self.publish_snapshot(SamplingPolicy::All)
    }

    fn reconfigure_domain_inner(
        &mut self,
        domain: Domain,
        requested_time_step: Option<TimeStep>,
    ) -> Result<(), RuntimeError> {
        if self.is_editing() {
            return Err(RuntimeError::CannotReconfigureDomainWhileEditing);
        }
        if domain == self.domain {
            return Ok(());
        }

        let world = self.world.snapshot();
        let initial_step = SimulationClock::new(self.clock.time_step()).snapshot().step;
        let mut replacements: Vec<Option<Box<dyn EquationSystemSolver>>> = self
            .plugins
            .iter()
            .map(|slot| {
                if !slot.enabled {
                    return Ok(None);
                }
                let mut solver = slot.plugin.create_solver(SolverContext {
                    configuration: &slot.configuration,
                    domain: &domain,
                    world: &world,
                    initial_step,
                    cancellation: self.cancellation.clone(),
                })?;
                solver.validate_world(&world)?;
                solver.on_world_changed(&world)?;
                Ok(Some(solver))
            })
            .collect::<Result<_, PluginError>>()?;

        let current_step = requested_time_step.unwrap_or(self.clock.time_step());
        let current_is_valid = replacements
            .iter()
            .flatten()
            .all(|solver| solver.validate_time_step(current_step).is_ok());
        let time_step = if current_is_valid {
            current_step
        } else {
            let limit = replacements
                .iter()
                .flatten()
                .filter_map(|solver| solver.time_step_limit())
                .min_by(|left, right| left.partial_cmp(right).expect("finite time steps"))
                .ok_or(RuntimeError::NoSafeTimeStepForDomain {
                    current: current_step,
                })?;
            TimeStep::from_seconds(limit.seconds() * 0.8)
                .expect("a positive finite solver limit has a positive 80% margin")
        };
        for solver in replacements.iter().flatten() {
            solver.validate_time_step(time_step)?;
        }
        self.validate_subscription(self.subscription)?;

        self.domain = domain;
        self.clock.reset(time_step);
        self.run_generation = self.run_generation.saturating_add(1);
        self.last_forces.clear();
        for (slot, replacement) in self.plugins.iter_mut().zip(replacements.drain(..)) {
            slot.solver = replacement;
        }
        Ok(())
    }

    pub fn step_once(&mut self) -> Result<(), RuntimeError> {
        let context = self
            .clock
            .step_once()
            .ok_or(RuntimeError::CannotStepWhileRunning)?;
        self.apply_tick(context)
    }

    /// Advance one tick if running. Returns whether a tick was taken.
    pub fn advance_running(&mut self) -> Result<bool, RuntimeError> {
        let Some(context) = self.clock.advance_running() else {
            return Ok(false);
        };
        self.apply_tick(context)?;
        Ok(true)
    }

    /// Times `apply_tick_inner` and records the result regardless of outcome
    /// — a tick that errored out still cost wall-clock time, and hiding that
    /// would make a solver bug look like a free tick in the history.
    fn apply_tick(&mut self, context: StepContext) -> Result<(), RuntimeError> {
        let started = Instant::now();
        let result = self.apply_tick_inner(context);
        self.last_tick_compute_ms = started.elapsed().as_secs_f64() as f32 * 1_000.0;
        result
    }

    fn apply_tick_inner(&mut self, context: StepContext) -> Result<(), RuntimeError> {
        // Resolve motion ownership before any solver advances. Discovering a
        // conflict after a field/particle integrator mutated its private state
        // would leave that state ahead of the authoritative world.
        let world = self.world.snapshot();
        let mut kinematic_owners: BTreeMap<ObjectId, PluginId> = BTreeMap::new();
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            if !slot.solver().kind().advances_with_time() {
                continue;
            }
            for &object in slot.solver().kinematic_objects() {
                if world.object(object).is_none() {
                    return Err(RuntimeError::UnknownKinematicObject {
                        plugin: slot.metadata.id.clone(),
                        object,
                    });
                }
                if let Some(first_plugin) = kinematic_owners.get(&object) {
                    return Err(RuntimeError::ConflictingObjectKinematics {
                        object,
                        first_plugin: first_plugin.clone(),
                        second_plugin: slot.metadata.id.clone(),
                    });
                }
                kinematic_owners.insert(object, slot.metadata.id.clone());
            }
        }

        // Gather the dynamics system's inputs before anything advances, so every
        // field system is asked about the same instant. A body a solver has
        // claimed through `kinematic_objects` is excluded: that solver
        // integrates it with a scheme of its own, and two integrators moving one
        // body would each be right about a different trajectory.
        let (all_dynamic, all_carried) = dynamics::collect_bodies(&world)?;
        let bodies: Vec<_> = all_dynamic
            .into_iter()
            .filter(|body| !kinematic_owners.contains_key(&body.object))
            .collect();
        let mut contributions = Vec::new();
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            contributions.push(slot.solver().forces(&bodies)?);
        }
        let total_forces = dynamics::accumulate_forces(bodies.len(), &contributions)?;
        self.last_forces = bodies
            .iter()
            .zip(&total_forces)
            .map(|(body, force)| (body.object, *force))
            .collect();

        let mut kinematics = BTreeMap::new();
        for slot in self.plugins.iter_mut().filter(|slot| slot.enabled) {
            if slot.solver().kind().advances_with_time() {
                let plugin = slot.metadata.id.clone();
                for update in slot.solver_mut().step(context)?.object_kinematics {
                    if kinematic_owners.get(&update.object) != Some(&plugin) {
                        return Err(RuntimeError::UndeclaredObjectKinematics {
                            plugin,
                            object: update.object,
                        });
                    }
                    if kinematics.insert(update.object, update).is_some() {
                        return Err(RuntimeError::DuplicateObjectKinematics {
                            plugin,
                            object: update.object,
                        });
                    }
                }
            }
        }

        // The dynamics system moves everything else: sum of forces over inertia.
        let seconds = context.time_step.seconds();
        for update in dynamics::integrate(&bodies, &total_forces, seconds)? {
            kinematics.insert(update.object, update);
        }
        // Pinned bodies with an authored velocity are carried at exactly that
        // velocity, integrating nothing.
        let carried: Vec<_> = all_carried
            .into_iter()
            .filter(|body| !kinematic_owners.contains_key(&body.object))
            .collect();
        for update in dynamics::carry(&carried, seconds)? {
            kinematics.insert(update.object, update);
        }

        if kinematics.is_empty() {
            return self.publish_snapshot(SamplingPolicy::TimeDependentOnly);
        }

        // A solver has moved a body, so the world is no longer the authored
        // scene the history describes. "The scene before my edit" would name a
        // state the simulation has already left, and restoring it would drag
        // every integrated body back to where it was without rewinding the
        // clock. Motion is undone by not running it, which is what pause and
        // step are for.
        self.history.clear();

        let mut commands = Vec::with_capacity(kinematics.len() * 2);
        for update in kinematics.into_values() {
            commands.push(WorldCommand::SetTransform {
                object: update.object,
                transform: update.transform,
            });
            commands.push(WorldCommand::SetVelocity {
                object: update.object,
                velocity: update.velocity,
            });
        }
        self.adopt_world_commands(commands)?;

        // Object motion can change analytic fields as well as time-stepped
        // fields, so a kinematic tick must republish every active system.
        self.publish_snapshot(SamplingPolicy::All)
    }

    /// Apply a batch of world edits.
    ///
    /// The candidate world is validated by every solver *before* it is adopted,
    /// so a rejected edit leaves the committed world and every solver exactly as
    /// they were. Accepting an edit that a solver then refuses would leave the
    /// runtime advertising a revision nothing had computed.
    pub fn commit_world_commands(
        &mut self,
        commands: Vec<WorldCommand>,
    ) -> Result<CommitReport, RuntimeError> {
        // Captured before the attempt and kept only if it succeeded: a rejected
        // edit changed nothing, so offering to undo it would step the user back
        // past an edit they did make.
        let before = self.world.checkpoint();
        let label = WorldCommand::batch_label(&commands);
        let coalesce = self.is_editing();

        let report = self.adopt_world_commands(commands)?;
        if report.revision != before.captured_at() {
            self.history.record(before, label, coalesce);
        }
        self.publish_snapshot(SamplingPolicy::All)?;
        Ok(report)
    }

    /// Apply one paused, solver-owned numerical field edit.
    pub fn apply_field_brush_stroke(
        &mut self,
        stroke: FieldBrushStroke,
    ) -> Result<(), RuntimeError> {
        if self.clock.snapshot().mode == SimulationMode::Running {
            return Err(RuntimeError::CannotPaintFieldWhileRunning);
        }
        let before = self.world.checkpoint();
        self.apply_field_brush_stroke_inner(stroke.clone())?;
        self.history.record_brush(before, stroke);
        self.publish_snapshot(SamplingPolicy::All)
    }

    fn inverted_brush(stroke: &FieldBrushStroke) -> Result<FieldBrushStroke, RuntimeError> {
        let strength =
            fieldcad_core::Quantity::new(-stroke.strength.si_value(), stroke.strength.dimension())
                .map_err(|error| RuntimeError::InvalidFieldBrush(error.to_string()))?;
        Ok(FieldBrushStroke {
            strength,
            ..stroke.clone()
        })
    }

    fn apply_field_brush_stroke_inner(
        &mut self,
        stroke: FieldBrushStroke,
    ) -> Result<(), RuntimeError> {
        if !stroke.radius_metres.is_finite()
            || stroke.radius_metres.into_si() <= 0.0
            || stroke.samples.is_empty()
        {
            return Err(RuntimeError::InvalidFieldBrush(
                "a stroke needs finite positive radius and at least one sample".to_owned(),
            ));
        }
        let world = self.world.snapshot();
        let plane = world
            .planes()
            .get(&stroke.plane)
            .ok_or(RuntimeError::UnknownBrushPlane(stroke.plane))?;
        let slot = self
            .provider_slot(&stroke.channel)
            .ok_or_else(|| RuntimeError::UnknownFieldChannel(stroke.channel.clone()))?;
        let index = self
            .plugins
            .iter()
            .position(|candidate| candidate.metadata.id == slot.metadata.id)
            .expect("provider slot belongs to plugin list");
        let handle = self.plugins[index]
            .handles()
            .find_map(|(handle, schema)| (schema.id == stroke.channel).then_some(handle))
            .expect("provider declares requested channel");
        let schema = &self.plugins[index].channels[handle.index()];
        if !matches!(schema.value_kind, fieldcad_core::FieldValueKind::Vector(_))
            || schema.dimension() != stroke.strength.dimension()
        {
            return Err(RuntimeError::InvalidFieldBrush(
                "strength must use the selected vector field's native dimension".to_owned(),
            ));
        }
        if !self.plugins[index]
            .solver()
            .mutable_vector_channels()
            .contains(&handle)
        {
            return Err(RuntimeError::FieldIsReadOnly(stroke.channel));
        }
        let (u, v) = plane.basis();
        let centres = stroke
            .samples
            .iter()
            .map(|sample: &DVec2| plane.origin + u * sample.x + v * sample.y)
            .collect();
        let resolved = ResolvedFieldBrushStroke {
            stroke,
            centres,
            direction: plane.normal,
        };
        self.plugins[index]
            .solver_mut()
            .apply_field_brush_stroke(&resolved)?;
        Ok(())
    }

    /// Validate and adopt a world edit without choosing a publication policy.
    /// Both authored commands and solver-produced kinematics cross this same
    /// Interface, keeping one authoritative world mutation path.
    fn adopt_world_commands(
        &mut self,
        commands: Vec<WorldCommand>,
    ) -> Result<CommitReport, RuntimeError> {
        let mut candidate = self.world.clone();
        let report = candidate.commit(commands)?;
        if report.revision == self.world.revision() {
            return Ok(report);
        }

        let candidate_snapshot = candidate.snapshot();
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            slot.solver().validate_world(&candidate_snapshot)?;
        }

        // Validation above is unconditional — an edit a solver cannot represent
        // is rejected whether or not that solver is following this gesture. What
        // a non-realtime system skips is the *work*: it is not shown the
        // intermediate worlds it would only have to recompute from again.
        let editing = self.is_editing();
        self.world = candidate;
        for slot in self
            .plugins
            .iter_mut()
            .filter(|slot| slot.enabled && (slot.realtime || !editing))
        {
            slot.solver_mut().on_world_changed(&candidate_snapshot)?;
        }
        Ok(report)
    }

    /// The geometries this subscription asks for, given the current world.
    fn geometries(&self, world: &WorldSnapshot, channel: &ChannelId) -> Vec<SampleGeometry> {
        let mut geometries = Vec::new();

        if self.subscription.probes {
            let mut ids = Vec::new();
            let mut positions = Vec::new();
            for probe in world.probes().values() {
                if !probe.channels.contains(channel) {
                    continue;
                }
                if let Ok(position) = world.resolve_probe_position(probe) {
                    ids.push(probe.id);
                    positions.push(position);
                }
            }
            if !ids.is_empty()
                && let Ok(geometry) = SampleGeometry::probes(ids, positions)
            {
                geometries.push(geometry);
            }
        }

        if let Some(counts) = self.subscription.planes {
            for plane in world.planes().values().filter(|plane| plane.visible) {
                geometries.push(SampleGeometry::Plane {
                    plane: plane.id,
                    lattice: plane.lattice(counts),
                });
            }
        }

        if let Some(stride) = self.subscription.domain_stride {
            geometries.push(SampleGeometry::Grid(self.domain.decimated_lattice(stride)));
        }

        if let Some(counts) = self.subscription.boxes {
            for region in world.boxes().values().filter(|region| region.visible) {
                geometries.push(SampleGeometry::Box {
                    region: region.id,
                    lattice: region.lattice(counts),
                });
            }
        }

        if let Some(density) = self.subscription.spheres {
            for sphere in world.spheres().values().filter(|sphere| sphere.visible) {
                geometries.push(SampleGeometry::Sphere {
                    region: sphere.id,
                    lattice: sphere.lattice(density),
                });
            }
        }

        geometries
    }

    fn publish_snapshot(&mut self, sampling: SamplingPolicy) -> Result<(), RuntimeError> {
        let world = self.world.snapshot();
        let clock = self.clock.snapshot();
        let mut channels = BTreeMap::new();
        let mut diagnostics = Vec::new();
        let mut provenance = Vec::with_capacity(self.plugins.len());

        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            provenance.push(PluginProvenance {
                id: slot.metadata.id.clone(),
                version: slot.metadata.version,
            });
            diagnostics.extend(slot.solver().diagnostics());

            // Two reasons to republish what was already computed rather than
            // compute it again: a tick cannot change an analytic result, and a
            // system that opted out of realtime update is deliberately holding
            // its result for the duration of an interactive edit.
            let unchanged_by_tick = sampling == SamplingPolicy::TimeDependentOnly
                && !slot.solver().kind().advances_with_time();
            let deferred = self.is_editing() && !slot.realtime;
            if deferred {
                diagnostics.push(SolverDiagnostic {
                    plugin: slot.metadata.id.clone(),
                    severity: DiagnosticSeverity::Info,
                    code: "deferred-during-edit".to_owned(),
                    message: format!(
                        "'{}' is showing its last complete result: realtime update is off and a \
                         scene edit is in progress",
                        slot.metadata.display_name
                    ),
                });
            }
            if unchanged_by_tick || deferred {
                for schema in &slot.channels {
                    if let Some(previous) = self.latest.channels.get(&schema.id) {
                        channels.insert(schema.id.clone(), previous.clone());
                    }
                }
                continue;
            }

            for (handle, schema) in slot.handles() {
                let mut batches = Vec::new();
                for geometry in self.geometries(&world, &schema.id) {
                    let column = slot.solver().sample(handle, &geometry)?;
                    // Shape and length are checked once per batch here, rather
                    // than once per value at every site that reads the batch.
                    if !column.values.matches(schema.value_kind) {
                        return Err(SchemaError::ChannelColumnMismatch {
                            channel: schema.id.clone(),
                            expected: schema.value_kind,
                        }
                        .into());
                    }
                    let mut batch = FieldBatch::new(geometry, column.values, column.validity)?;
                    if let Some(gradient) = column.gradient {
                        batch = batch.with_gradient(gradient)?;
                    }
                    batches.push(batch);
                }
                if batches.is_empty() {
                    continue;
                }
                channels.insert(
                    schema.id.clone(),
                    ChannelSnapshot {
                        schema: Arc::clone(schema),
                        provider: slot.metadata.id.clone(),
                        batches: batches.into(),
                    },
                );
            }
        }

        self.latest = Arc::new(fieldcad_core::FieldSnapshot {
            identity: SnapshotIdentity {
                session: self.session,
                sequence: self.next_sequence,
                run_generation: self.run_generation,
                world_revision: world.revision(),
                tick: clock.tick(),
                time_seconds: clock.time_seconds(),
            },
            completeness: SnapshotCompleteness::Complete,
            domain: self.domain,
            plugins: provenance.into(),
            channels,
            diagnostics: diagnostics.into(),
        });
        self.next_sequence += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SamplingPolicy {
    All,
    TimeDependentOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryDirection {
    Undo,
    Redo,
}

fn empty_snapshot(session: SessionId, domain: Domain) -> fieldcad_core::FieldSnapshot {
    fieldcad_core::FieldSnapshot {
        identity: SnapshotIdentity {
            session,
            sequence: 0,
            run_generation: 0,
            world_revision: WorldRevision::INITIAL,
            tick: 0,
            time_seconds: 0.0,
        },
        completeness: SnapshotCompleteness::Partial,
        domain,
        plugins: Arc::from([]),
        channels: BTreeMap::new(),
        diagnostics: Arc::from([]),
    }
}

/// Clock and world state as one value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationStatus {
    pub clock: ClockSnapshot,
    pub world_revision: WorldRevision,
    pub run_generation: u64,
}

impl SimulationStatus {
    pub const fn mode(self) -> SimulationMode {
        self.clock.mode
    }

    pub const fn tick(self) -> u64 {
        self.clock.tick()
    }

    pub const fn time_seconds(self) -> f64 {
        self.clock.time_seconds()
    }

    pub const fn time_step(self) -> TimeStep {
        self.clock.time_step()
    }
}

impl SimulationRuntime {
    pub fn status(&self) -> SimulationStatus {
        SimulationStatus {
            clock: self.clock.snapshot(),
            world_revision: self.world.revision(),
            run_generation: self.run_generation,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error(transparent)]
    World(#[from] WorldError),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Sampling(#[from] SamplingError),
    #[error(transparent)]
    Dynamics(#[from] DynamicsError),
    #[error("plugin '{0}' was registered more than once")]
    DuplicatePlugin(PluginId),
    #[error("plugin '{0}' is not registered in this scene")]
    UnknownPlugin(PluginId),
    #[error(
        "plugins '{first_plugin}' and '{second_plugin}' declare incompatible schemas for field channel '{channel}'"
    )]
    ConflictingChannelSchema {
        channel: ChannelId,
        first_plugin: PluginId,
        second_plugin: PluginId,
    },
    #[error(
        "field '{channel}' is already computed by '{active_plugin}'; a field has one model at a time, so deactivate that system or choose '{requested_plugin}' as its model"
    )]
    ConflictingFieldProvider {
        channel: ChannelId,
        active_plugin: PluginId,
        requested_plugin: PluginId,
    },
    #[error("no equation system in this scene computes field '{0}'")]
    UnknownFieldChannel(ChannelId),
    #[error(
        "plugins '{first_plugin}' and '{second_plugin}' declare incompatible schemas for component '{component}'"
    )]
    ConflictingComponentSchema {
        component: ComponentTypeId,
        first_plugin: PluginId,
        second_plugin: PluginId,
    },
    #[error(
        "plugins '{first_plugin}' and '{second_plugin}' both attempted to advance object '{object}' in one tick"
    )]
    ConflictingObjectKinematics {
        object: ObjectId,
        first_plugin: PluginId,
        second_plugin: PluginId,
    },
    #[error("plugin '{plugin}' claims motion authority for missing object '{object}'")]
    UnknownKinematicObject { plugin: PluginId, object: ObjectId },
    #[error("plugin '{plugin}' produced undeclared kinematics for object '{object}'")]
    UndeclaredObjectKinematics { plugin: PluginId, object: ObjectId },
    #[error("plugin '{plugin}' produced more than one kinematic update for object '{object}'")]
    DuplicateObjectKinematics { plugin: PluginId, object: ObjectId },
    #[error("plugin '{0}' declares more channels than a channel handle can address")]
    TooManyChannels(PluginId),
    #[error("single-step is only valid while the simulation is paused")]
    CannotStepWhileRunning,
    #[error("undo and redo are only valid while the simulation is paused")]
    CannotEditHistoryWhileRunning,
    #[error("field painting is only valid while the simulation is paused")]
    CannotPaintFieldWhileRunning,
    #[error("slice plane '{0}' no longer exists")]
    UnknownBrushPlane(fieldcad_core::PlaneId),
    #[error("field '{0}' is read-only; its active solver does not accept numerical painting")]
    FieldIsReadOnly(ChannelId),
    #[error("invalid field brush: {0}")]
    InvalidFieldBrush(String),
    #[error(
        "cannot reconfigure the numerical domain while an interactive scene edit is in progress"
    )]
    CannotReconfigureDomainWhileEditing,
    #[error(
        "the current time step {current:?} is invalid for the proposed domain and no active solver reported a safe replacement"
    )]
    NoSafeTimeStepForDomain { current: TimeStep },
    #[error("invalid sampling budget")]
    InvalidSamplingBudget,
    #[error("invalid subscription: {0}")]
    InvalidSubscription(String),
    #[error("subscription requests {requested} samples, exceeding the limit of {limit}")]
    SamplingBudgetExceeded { requested: u64, limit: u64 },
}

impl RuntimeError {
    /// A stable, machine-readable label for reporting across a transport.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Plugin(_) => "plugin",
            Self::World(_) => "world",
            Self::Schema(_) => "schema",
            Self::Sampling(_) => "sampling",
            Self::Dynamics(_) => "dynamics",
            Self::DuplicatePlugin(_) => "duplicate-plugin",
            Self::UnknownPlugin(_) => "unknown-plugin",
            Self::ConflictingChannelSchema { .. } => "conflicting-channel-schema",
            Self::ConflictingFieldProvider { .. } => "conflicting-field-provider",
            Self::UnknownFieldChannel(_) => "unknown-field-channel",
            Self::ConflictingComponentSchema { .. } => "conflicting-component-schema",
            Self::ConflictingObjectKinematics { .. } => "conflicting-object-kinematics",
            Self::UnknownKinematicObject { .. } => "unknown-kinematic-object",
            Self::UndeclaredObjectKinematics { .. } => "undeclared-object-kinematics",
            Self::DuplicateObjectKinematics { .. } => "duplicate-object-kinematics",
            Self::TooManyChannels(_) => "too-many-channels",
            Self::CannotStepWhileRunning => "cannot-step-while-running",
            Self::CannotEditHistoryWhileRunning => "cannot-edit-history-while-running",
            Self::CannotPaintFieldWhileRunning => "cannot-paint-field-while-running",
            Self::UnknownBrushPlane(_) => "unknown-brush-plane",
            Self::FieldIsReadOnly(_) => "field-read-only",
            Self::InvalidFieldBrush(_) => "invalid-field-brush",
            Self::CannotReconfigureDomainWhileEditing => "cannot-reconfigure-domain-while-editing",
            Self::NoSafeTimeStepForDomain { .. } => "no-safe-time-step-for-domain",
            Self::InvalidSamplingBudget => "invalid-sampling-budget",
            Self::InvalidSubscription(_) => "invalid-subscription",
            Self::SamplingBudgetExceeded { .. } => "sampling-budget-exceeded",
        }
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, DomainBounds, ObjectSpec, PluginVersion, Precision,
        Resolution, Transform, Velocity,
    };
    use fieldcad_plugin_api::{
        ObjectKinematicsUpdate, SampledColumn, SolverKind, SolverStepOutcome,
    };
    use glam::DVec3;

    use super::*;

    struct MotionPlugin {
        id: PluginId,
        object: ObjectId,
        component_schema: Option<ComponentSchema>,
    }

    impl MotionPlugin {
        fn new(id: &str, object: ObjectId) -> Self {
            Self {
                id: PluginId::new(id).unwrap(),
                object,
                component_schema: None,
            }
        }

        fn with_component_schema(mut self, component_schema: ComponentSchema) -> Self {
            self.component_schema = Some(component_schema);
            self
        }
    }

    impl EquationSystemPlugin for MotionPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                id: self.id.clone(),
                version: PluginVersion::new(0, 1, 0),
                display_name: "Motion test".to_owned(),
                description: "Exercises solver-produced object kinematics".to_owned(),
            }
        }

        fn channels(&self) -> Vec<ChannelSchema> {
            Vec::new()
        }

        fn component_schemas(&self) -> Vec<ComponentSchema> {
            self.component_schema.clone().into_iter().collect()
        }

        fn create_solver(
            &self,
            _context: SolverContext<'_>,
        ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
            Ok(Box::new(MotionSolver {
                object: self.object,
            }))
        }
    }

    struct MotionSolver {
        object: ObjectId,
    }

    impl EquationSystemSolver for MotionSolver {
        fn kind(&self) -> SolverKind {
            SolverKind::TimeStepped
        }

        fn on_world_changed(&mut self, _world: &WorldSnapshot) -> Result<(), PluginError> {
            Ok(())
        }

        fn kinematic_objects(&self) -> &[ObjectId] {
            std::slice::from_ref(&self.object)
        }

        fn step(&mut self, context: StepContext) -> Result<SolverStepOutcome, PluginError> {
            Ok(SolverStepOutcome {
                object_kinematics: vec![ObjectKinematicsUpdate {
                    object: self.object,
                    transform: Transform::at(DVec3::X * context.time_seconds).unwrap(),
                    velocity: Velocity::new(DVec3::X, DVec3::ZERO).unwrap(),
                }],
            })
        }

        fn sample(
            &self,
            channel: ChannelHandle,
            _geometry: &SampleGeometry,
        ) -> Result<SampledColumn, PluginError> {
            Err(PluginError::UnknownChannel(channel.index()))
        }
    }

    /// A single-channel plugin publishing a constant scalar field, optionally
    /// with a constant gradient — just enough to exercise `publish_snapshot`'s
    /// gradient plumbing without a real solver's complexity.
    struct FieldPlugin {
        id: PluginId,
        channel: ChannelId,
        report_gradient: bool,
    }

    impl EquationSystemPlugin for FieldPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                id: self.id.clone(),
                version: PluginVersion::new(0, 1, 0),
                display_name: "Field test".to_owned(),
                description: "Exercises gradient publishing".to_owned(),
            }
        }

        fn channels(&self) -> Vec<ChannelSchema> {
            vec![ChannelSchema {
                id: self.channel.clone(),
                display_name: "Test field".to_owned(),
                value_kind: fieldcad_core::FieldValueKind::Scalar(
                    fieldcad_core::Dimension::DIMENSIONLESS,
                ),
            }]
        }

        fn create_solver(
            &self,
            _context: SolverContext<'_>,
        ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
            Ok(Box::new(FieldSolver {
                report_gradient: self.report_gradient,
            }))
        }
    }

    struct FieldSolver {
        report_gradient: bool,
    }

    impl EquationSystemSolver for FieldSolver {
        fn on_world_changed(&mut self, _world: &WorldSnapshot) -> Result<(), PluginError> {
            Ok(())
        }

        fn sample(
            &self,
            _channel: ChannelHandle,
            geometry: &SampleGeometry,
        ) -> Result<SampledColumn, PluginError> {
            let column = SampledColumn::exact_scalars(vec![1.0; geometry.len()]);
            Ok(if self.report_gradient {
                column.with_gradient(fieldcad_core::GradientColumn::Scalar(
                    vec![DVec3::X; geometry.len()].into(),
                ))
            } else {
                column
            })
        }
    }

    fn motion_runtime(
        plugins: impl IntoIterator<Item = Box<dyn EquationSystemPlugin>>,
    ) -> (SimulationRuntime, ObjectId) {
        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("particle"))])
            .unwrap();
        let object = report.created_objects[0];
        let mut config = RuntimeConfig::new(
            Domain::centred_cube(2.0, 4).unwrap(),
            TimeStep::from_seconds(0.25).unwrap(),
            SessionId::from_u128(0x6),
        )
        .with_world(world);
        for plugin in plugins {
            config = config.with_plugin(plugin);
        }
        (SimulationRuntime::new(config).unwrap(), object)
    }

    /// A runtime with one [`FieldPlugin`] and a probe subscribed to its
    /// channel, so `publish_snapshot` actually calls `sample()` once during
    /// construction without needing plane/box/sphere setup.
    fn field_runtime(report_gradient: bool, session: u128) -> (SimulationRuntime, ChannelId) {
        let plugin_id = PluginId::new(format!("field-test-{session:x}")).unwrap();
        let channel = ChannelId::new(plugin_id.clone(), "value").unwrap();
        let plugin = FieldPlugin {
            id: plugin_id,
            channel: channel.clone(),
            report_gradient,
        };

        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(fieldcad_core::ProbeSpec::at(
                "probe",
                DVec3::ZERO,
                vec![channel.clone()],
            ))])
            .unwrap();
        let config = RuntimeConfig::new(
            Domain::centred_cube(2.0, 4).unwrap(),
            TimeStep::from_seconds(0.25).unwrap(),
            SessionId::from_u128(session),
        )
        .with_world(world)
        .with_plugin(Box::new(plugin));

        (SimulationRuntime::new(config).unwrap(), channel)
    }

    #[test]
    fn a_solver_that_reports_a_gradient_publishes_one() {
        let (runtime, channel) = field_runtime(true, 0x100);

        let snapshot = runtime.latest_snapshot();
        let batch = &snapshot.channels[&channel].batches[0];
        assert!(batch.gradient().is_some());
    }

    #[test]
    fn a_solver_that_does_not_report_a_gradient_still_publishes_fine() {
        let (runtime, channel) = field_runtime(false, 0x101);

        let snapshot = runtime.latest_snapshot();
        let batch = &snapshot.channels[&channel].batches[0];
        assert!(batch.gradient().is_none());
    }

    #[test]
    fn pacer_emits_whole_ticks_and_keeps_the_remainder() {
        let step = TimeStep::from_seconds(0.1).unwrap();
        let mut pacer = TickPacer::default();

        let demand = pacer.ticks_due(Duration::from_millis(250), step);
        assert_eq!(demand.ticks, 2);
        assert!(!demand.fell_behind);

        // The leftover 50 ms carries forward rather than being rounded away.
        let demand = pacer.ticks_due(Duration::from_millis(50), step);
        assert_eq!(demand.ticks, 1);
    }

    #[test]
    fn the_remainder_does_not_drift_over_many_polls() {
        // 0.1 s is not representable in binary, so an f64 accumulator loses a
        // tick here roughly every other poll.
        let step = TimeStep::from_seconds(0.1).unwrap();
        let mut pacer = TickPacer::with_max_ticks(64);

        let mut total = 0;
        for _ in 0..1_000 {
            total += pacer.ticks_due(Duration::from_millis(25), step).ticks;
        }

        // 1000 polls of 25 ms is 25 s, which is exactly 250 ticks of 0.1 s.
        assert_eq!(total, 250);
    }

    #[test]
    fn returned_ticks_are_owed_again() {
        let step = TimeStep::from_seconds(0.1).unwrap();
        let mut pacer = TickPacer::default();

        let demand = pacer.ticks_due(Duration::from_millis(350), step);
        assert_eq!(demand.ticks, 3);

        // The vetoed ticks come back in full; the 50 ms sub-tick remainder
        // is carried through both calls, not double-counted.
        pacer.return_ticks(demand.ticks, step);
        let demand = pacer.ticks_due(Duration::ZERO, step);
        assert_eq!(demand.ticks, 3);

        pacer.return_ticks(0, step);
        assert_eq!(pacer.ticks_due(Duration::ZERO, step).ticks, 0);
    }

    #[test]
    fn domain_reconfiguration_resets_and_is_undoable() {
        let (mut runtime, _) = motion_runtime([]);
        let original = *runtime.domain();
        let replacement = Domain::centred_cube(3.0, 6).unwrap();

        runtime.reconfigure_domain(replacement).unwrap();
        let after_change = runtime.status();
        assert_eq!(*runtime.domain(), replacement);
        assert_eq!(after_change.tick(), 0);
        assert_eq!(after_change.mode(), SimulationMode::Paused);
        assert_eq!(after_change.run_generation, 1);
        assert!(runtime.edit_history().can_undo());

        runtime.undo().unwrap();
        let after_undo = runtime.status();
        assert_eq!(*runtime.domain(), original);
        assert_eq!(after_undo.tick(), 0);
        assert_eq!(after_undo.mode(), SimulationMode::Paused);
        assert_eq!(after_undo.run_generation, 2);

        runtime.redo().unwrap();
        assert_eq!(*runtime.domain(), replacement);
        assert_eq!(runtime.status().run_generation, 3);
    }

    #[test]
    fn a_completed_tick_records_positive_wall_clock_compute_time() {
        // The regression this guards: `apply_tick`'s early-return path (no
        // kinematics to adopt, see `apply_tick_inner`) used to skip whatever
        // bookkeeping sat after it — a timer wrapped around the wrong span
        // would silently under-report every tick that took that path.
        let (mut runtime, _) = motion_runtime([]);
        assert_eq!(runtime.last_tick_compute_ms(), 0.0, "no tick has run yet");

        runtime.step_once().unwrap();

        assert!(
            runtime.last_tick_compute_ms() > 0.0,
            "a completed tick must report measurable compute time"
        );
    }

    #[test]
    fn a_runtime_that_falls_behind_drops_the_backlog_instead_of_stretching_dt() {
        let step = TimeStep::from_seconds(0.001).unwrap();
        let mut pacer = TickPacer::with_max_ticks(4);

        let demand = pacer.ticks_due(Duration::from_secs(1), step);

        assert_eq!(demand.ticks, 4);
        assert!(demand.fell_behind);
        // The next poll starts clean: the missed 996 ticks are gone, and dt is
        // untouched.
        assert_eq!(pacer.ticks_due(Duration::ZERO, step).ticks, 0);
    }

    #[test]
    fn solver_produced_kinematics_cross_the_authoritative_world_interface() {
        let placeholder = ObjectId::new(0);
        let (mut runtime, object) =
            motion_runtime([
                Box::new(MotionPlugin::new("fieldcad.motion-a", placeholder))
                    as Box<dyn EquationSystemPlugin>,
            ]);
        assert_eq!(object, placeholder);
        let before_revision = runtime.world_snapshot().revision();

        runtime.step_once().unwrap();

        let world = runtime.world_snapshot();
        let particle = world.object(object).unwrap();
        assert_eq!(particle.transform.translation, DVec3::X * 0.25);
        assert_eq!(particle.velocity.linear, DVec3::X);
        assert_eq!(world.revision(), before_revision.next());
        assert_eq!(
            runtime.latest_snapshot().identity.world_revision,
            world.revision()
        );
    }

    #[test]
    fn two_solvers_cannot_advance_the_same_object() {
        let object = ObjectId::new(0);
        let (mut runtime, _) = motion_runtime([
            Box::new(MotionPlugin::new("fieldcad.motion-a", object))
                as Box<dyn EquationSystemPlugin>,
            Box::new(MotionPlugin::new("fieldcad.motion-b", object))
                as Box<dyn EquationSystemPlugin>,
        ]);
        let revision = runtime.world_snapshot().revision();

        let error = runtime.step_once().unwrap_err();

        assert_eq!(error.code(), "conflicting-object-kinematics");
        assert_eq!(runtime.world_snapshot().revision(), revision);
    }

    #[test]
    fn incompatible_shared_component_schema_contributions_are_rejected() {
        let component = ComponentTypeId::new(
            PluginId::new("fieldcad.shared-test").unwrap(),
            "particle-property",
        )
        .unwrap();
        let first = ComponentSchema {
            id: component.clone(),
            display_name: "First definition".to_owned(),
            properties: Vec::new(),
        };
        let second = ComponentSchema {
            id: component,
            display_name: "Incompatible definition".to_owned(),
            properties: Vec::new(),
        };
        let config = RuntimeConfig::new(
            Domain::centred_cube(2.0, 4).unwrap(),
            TimeStep::from_seconds(0.25).unwrap(),
            SessionId::from_u128(0x7),
        )
        .with_plugin(Box::new(
            MotionPlugin::new("fieldcad.schema-a", ObjectId::new(0)).with_component_schema(first),
        ))
        .with_plugin(Box::new(
            MotionPlugin::new("fieldcad.schema-b", ObjectId::new(0)).with_component_schema(second),
        ));

        let error = match SimulationRuntime::new(config) {
            Ok(_) => panic!("incompatible schemas must not compose"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "conflicting-component-schema");
    }

    #[test]
    fn maxwell_particle_edits_are_interventions_but_solver_motion_is_not() {
        use fieldcad_electromagnetism::{ElectromagnetismPlugin, courant_limit};
        use fieldcad_particles::{ParticleTemplate, template_particle_spec};

        let domain = Domain::new(
            DomainBounds::centred_cube(1.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.5).unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(0x66))
                .with_plugin(Box::new(ElectromagnetismPlugin::new())),
        )
        .unwrap();
        let report = runtime
            .commit_world_commands(vec![WorldCommand::CreateObject(
                template_particle_spec(
                    ParticleTemplate::Catalog("Electron"),
                    true,
                    DVec3::ZERO,
                    DVec3::X * 1.0e8,
                    0.01,
                )
                .unwrap(),
            )])
            .unwrap();
        let particle = report.created_objects[0];

        runtime.step_once().unwrap();
        let after_solver_step = runtime
            .world_snapshot()
            .object(particle)
            .unwrap()
            .transform
            .translation;
        assert!(after_solver_step.x > 0.0);
        assert!(coupling_diagnostic(&runtime).contains("interventions 0"));

        runtime
            .commit_world_commands(vec![WorldCommand::SetTransform {
                object: particle,
                transform: Transform::at(DVec3::new(0.2, 0.0, 0.0)).unwrap(),
            }])
            .unwrap();
        assert!(coupling_diagnostic(&runtime).contains("interventions 1"));

        runtime.step_once().unwrap();
        assert!(coupling_diagnostic(&runtime).contains("interventions 1"));
    }

    #[test]
    fn proton_electron_maxwell_baseline_is_reproducible_without_hidden_species_rules() {
        let mut first = proton_electron_runtime(0x67);
        let mut second = proton_electron_runtime(0x68);

        for _ in 0..8 {
            first.step_once().unwrap();
            second.step_once().unwrap();
        }

        let first_world = first.world_snapshot();
        let second_world = second.world_snapshot();
        let first_kinematics: Vec<_> = first_world
            .objects()
            .values()
            .map(|object| (object.transform, object.velocity))
            .collect();
        let second_kinematics: Vec<_> = second_world
            .objects()
            .values()
            .map(|object| (object.transform, object.velocity))
            .collect();
        assert_eq!(first_kinematics, second_kinematics);
        assert!(first_kinematics.iter().all(|(transform, velocity)| {
            transform.translation.is_finite() && velocity.linear.is_finite()
        }));
        assert!(coupling_diagnostic(&first).contains("charge 0.000000e0 C"));
    }

    fn proton_electron_runtime(session: u128) -> SimulationRuntime {
        use fieldcad_electromagnetism::{ElectromagnetismPlugin, courant_limit};
        use fieldcad_particles::{ParticleTemplate, template_particle_spec};

        let domain = Domain::new(
            DomainBounds::centred_cube(1.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.5).unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(session))
                .with_plugin(Box::new(ElectromagnetismPlugin::new())),
        )
        .unwrap();
        runtime
            .commit_world_commands(vec![
                WorldCommand::CreateObject(
                    template_particle_spec(
                        ParticleTemplate::Catalog("Proton"),
                        false,
                        DVec3::new(-0.25, 0.0, 0.0),
                        DVec3::new(0.0, -1.0e5, 0.0),
                        0.01,
                    )
                    .unwrap(),
                ),
                WorldCommand::CreateObject(
                    template_particle_spec(
                        ParticleTemplate::Catalog("Electron"),
                        false,
                        DVec3::new(0.25, 0.0, 0.0),
                        DVec3::new(0.0, 1.0e5, 0.0),
                        0.01,
                    )
                    .unwrap(),
                ),
            ])
            .unwrap();
        runtime
    }

    fn coupling_diagnostic(runtime: &SimulationRuntime) -> String {
        runtime
            .latest_snapshot()
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "particle-coupling-conservation")
            .map(|diagnostic| diagnostic.message.clone())
            .expect("coupled Maxwell solver publishes conservation diagnostics")
    }

    #[test]
    fn set_subscription_rejects_invalid_box_and_sphere_counts() {
        let (mut runtime, _object) = motion_runtime([]);

        assert!(matches!(
            runtime.set_subscription(Subscription {
                boxes: Some(UVec3::new(0, 4, 4)),
                ..runtime.subscription()
            }),
            Err(RuntimeError::InvalidSubscription(_))
        ));
        assert!(matches!(
            runtime.set_subscription(Subscription {
                spheres: Some(0),
                ..runtime.subscription()
            }),
            Err(RuntimeError::InvalidSubscription(_))
        ));
    }

    #[test]
    fn box_and_sphere_subscriptions_publish_their_geometry() {
        use fieldcad_core::{FieldBoxSpec, FieldSphereSpec};

        let (mut runtime, _object) = motion_runtime([]);
        let report = runtime
            .commit_world_commands(vec![
                WorldCommand::CreateBox(
                    FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(1.0)).unwrap(),
                ),
                WorldCommand::CreateSphere(FieldSphereSpec::new("ball", DVec3::ZERO, 1.0).unwrap()),
            ])
            .unwrap();
        runtime
            .set_subscription(Subscription {
                boxes: Some(UVec3::splat(3)),
                spheres: Some(3),
                ..runtime.subscription()
            })
            .unwrap();

        let channel = ChannelId::new(PluginId::new("test").unwrap(), "unused").unwrap();
        let world = runtime.world_snapshot();
        let geometries = runtime.geometries(&world, &channel);

        let box_geometry = geometries
            .iter()
            .find(|geometry| {
                matches!(geometry, SampleGeometry::Box { region, .. } if *region == report.created_boxes[0])
            })
            .expect("a visible box publishes its own geometry");
        assert_eq!(box_geometry.len(), 27);

        let sphere_geometry = geometries
            .iter()
            .find(|geometry| {
                matches!(geometry, SampleGeometry::Sphere { region, .. } if *region == report.created_spheres[0])
            })
            .expect("a visible sphere publishes its own geometry");
        assert_eq!(sphere_geometry.len(), 27);
    }

    /// A presentation setting, like `Subscription`: adopting it must not
    /// touch the domain, rebuild solver state, or advance the world.
    #[test]
    fn set_scene_scale_updates_only_its_own_accessor() {
        let domain = Domain::centred_cube(2.0, 4).unwrap();
        let mut runtime = SimulationRuntime::new(RuntimeConfig::new(
            domain,
            TimeStep::from_seconds(0.25).unwrap(),
            SessionId::from_u128(0x9),
        ))
        .unwrap();

        assert_eq!(runtime.scene_scale(), fieldcad_core::SceneScale::metre());
        let revision_before = runtime.world_snapshot().revision();
        let domain_before = *runtime.domain();

        runtime.set_scene_scale(fieldcad_core::SceneScale::nanometre());

        assert_eq!(
            runtime.scene_scale(),
            fieldcad_core::SceneScale::nanometre()
        );
        assert_eq!(*runtime.domain(), domain_before);
        assert_eq!(runtime.world_snapshot().revision(), revision_before);
    }
}
