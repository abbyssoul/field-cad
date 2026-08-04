//! The authoritative, headless simulation runtime.
//!
//! In local mode this runs in the desktop process; in remote mode the same type
//! runs inside the compute service. Nothing here knows which.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use fieldcad_core::{
    ChannelId, ChannelSchema, ChannelSnapshot, ClockSnapshot, CommitReport, ComponentSchema,
    ComponentTypeId, DiagnosticSeverity, Domain, FieldBatch, ObjectId, PluginId, PluginProvenance,
    PropertyBag, SampleGeometry, SamplingError, SchemaError, SessionId, SimulationClock,
    SimulationMode, SnapshotCompleteness, SnapshotIdentity, SolverDiagnostic, StepContext,
    TimeStep, World, WorldCheckpoint, WorldCommand, WorldError, WorldRevision, WorldSnapshot,
};
use fieldcad_dynamics::{self as dynamics, DynamicsError};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginConfigurationSchema,
    PluginError, PluginMetadata, SolverCancellation, SolverContext,
};
use glam::UVec2;

/// What the runtime should sample when it publishes a snapshot.
///
/// This is a visualization concern, not a physical one: changing it changes how
/// densely a result is observed, never the result itself. Keeping it separate
/// from [`Domain`] is what makes that invariant checkable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscription {
    /// Sample every probe that requested each channel.
    pub probes: bool,
    /// Sample each visible slice plane at this many points along u and v.
    pub planes: Option<UVec2>,
    /// Sample the whole domain on a lattice decimated by this stride.
    pub domain_stride: Option<u32>,
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
        }
    }
}

impl Subscription {
    pub const PROBES_ONLY: Self = Self {
        probes: true,
        planes: None,
        domain_stride: None,
    };

    pub fn with_planes(mut self, counts: UVec2) -> Self {
        self.planes = Some(counts);
        self
    }

    pub fn with_domain_stride(mut self, stride: u32) -> Self {
        self.domain_stride = Some(stride);
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
    /// What the edit was, in the user's words, for the control that offers it.
    label: String,
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
        self.undo.push_back(HistoryEntry { checkpoint, label });
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
pub struct FieldSystemStatus {
    pub plugin: PluginMetadata,
    pub channels: Vec<ChannelSchema>,
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
    subscription: Subscription,
    sampling_budget: SamplingBudget,
    session: SessionId,
    next_sequence: u64,
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
}

/// Everything needed to stand up a runtime.
pub struct RuntimeConfig {
    pub world: World,
    pub domain: Domain,
    pub time_step: TimeStep,
    pub session: SessionId,
    pub subscription: Subscription,
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
            subscription,
            sampling_budget,
            session,
            next_sequence: 0,
            plugins: prepared,
            cancellation,
            latest: Arc::new(empty_snapshot(session, domain)),
            interactive_edit: None,
            history: EditHistory::new(undo_depth),
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
            label: entry.label.clone(),
        };
        if let Err(error) = self.adopt_checkpoint(&entry.checkpoint) {
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
            self.publish_snapshot(SamplingPolicy::All)?;
        }
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

    fn apply_tick(&mut self, context: StepContext) -> Result<(), RuntimeError> {
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
        let bodies: Vec<_> = dynamics::collect_dynamic_bodies(&world)?
            .into_iter()
            .filter(|body| !kinematic_owners.contains_key(&body.object))
            .collect();
        let mut contributions = Vec::new();
        for slot in self.plugins.iter().filter(|slot| slot.enabled) {
            contributions.push(slot.solver().forces(&bodies)?);
        }
        let total_forces = dynamics::accumulate_forces(bodies.len(), &contributions)?;

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
        let carried: Vec<_> = dynamics::collect_carried_bodies(&world)?
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
                    batches.push(FieldBatch::new(geometry, column.values, column.validity)?);
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulationStatus {
    pub clock: ClockSnapshot,
    pub world_revision: WorldRevision,
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
                    ParticleTemplate::Electron,
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
                        ParticleTemplate::Proton,
                        false,
                        DVec3::new(-0.25, 0.0, 0.0),
                        DVec3::new(0.0, -1.0e5, 0.0),
                        0.01,
                    )
                    .unwrap(),
                ),
                WorldCommand::CreateObject(
                    template_particle_spec(
                        ParticleTemplate::Electron,
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
}
