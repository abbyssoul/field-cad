//! The authoritative, headless simulation runtime.
//!
//! In local mode this runs in the desktop process; in remote mode the same type
//! runs inside the compute service. Nothing here knows which.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use fieldcad_core::{
    ChannelId, ChannelSchema, ChannelSnapshot, ClockSnapshot, CommitReport, Domain, FieldBatch,
    PluginId, PluginProvenance, PropertyBag, SampleGeometry, SamplingError, SchemaError, SessionId,
    SimulationClock, SimulationMode, SnapshotCompleteness, SnapshotIdentity, StepContext, TimeStep,
    World, WorldCommand, WorldError, WorldRevision, WorldSnapshot,
};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginError, PluginMetadata,
    SolverContext,
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

pub struct PluginRegistration {
    pub plugin: Box<dyn EquationSystemPlugin>,
    pub configuration: PropertyBag,
}

impl PluginRegistration {
    pub fn with_default_configuration(plugin: Box<dyn EquationSystemPlugin>) -> Self {
        let configuration = plugin.default_configuration();
        Self {
            plugin,
            configuration,
        }
    }
}

struct PluginSlot {
    metadata: PluginMetadata,
    /// Index in this vector is the plugin's [`ChannelHandle`].
    channels: Vec<Arc<ChannelSchema>>,
    solver: Box<dyn EquationSystemSolver>,
}

impl PluginSlot {
    fn handles(&self) -> impl Iterator<Item = (ChannelHandle, &Arc<ChannelSchema>)> {
        self.channels
            .iter()
            .enumerate()
            .map(|(index, schema)| (ChannelHandle::new(index as u16), schema))
    }
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
    latest: Arc<fieldcad_core::FieldSnapshot>,
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
}

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
        }
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
        } = config;

        let mut plugin_ids = BTreeSet::new();
        let mut channel_ids = BTreeSet::new();
        let mut prepared = Vec::with_capacity(plugins.len());

        // Schema registration goes through the command boundary like any other
        // edit, so the revision a solver is initialized against already includes
        // every schema it can see.
        for registration in &plugins {
            let metadata = registration.plugin.metadata();
            if !plugin_ids.insert(metadata.id.clone()) {
                return Err(RuntimeError::DuplicatePlugin(metadata.id));
            }
            let mut schema_commands = Vec::new();
            for component in registration.plugin.component_schemas() {
                if component.id.plugin() != &metadata.id {
                    return Err(RuntimeError::ForeignComponent {
                        plugin: metadata.id.clone(),
                        component: component.id,
                    });
                }
                schema_commands.push(WorldCommand::RegisterComponentSchema(component));
            }
            world.commit(schema_commands)?;
        }

        for registration in plugins {
            let metadata = registration.plugin.metadata();
            registration
                .plugin
                .configuration_schema()
                .validate(&registration.configuration)?;

            let declared = registration.plugin.channels();
            if declared.len() > usize::from(u16::MAX) {
                return Err(RuntimeError::TooManyChannels(metadata.id));
            }
            let mut channels = Vec::with_capacity(declared.len());
            for channel in declared {
                if channel.id.plugin() != &metadata.id {
                    return Err(RuntimeError::ForeignChannel {
                        plugin: metadata.id.clone(),
                        channel: channel.id,
                    });
                }
                if !channel_ids.insert(channel.id.clone()) {
                    return Err(RuntimeError::DuplicateChannel(channel.id));
                }
                channels.push(Arc::new(channel));
            }

            let world_snapshot = world.snapshot();
            let solver = registration.plugin.create_solver(SolverContext {
                configuration: &registration.configuration,
                domain: &domain,
                world: &world_snapshot,
            })?;
            solver.validate_world(&world_snapshot)?;
            solver.validate_time_step(time_step)?;
            prepared.push(PluginSlot {
                metadata,
                channels,
                solver,
            });
        }

        let mut runtime = Self {
            world,
            clock: SimulationClock::new(time_step),
            domain,
            subscription,
            sampling_budget,
            session,
            next_sequence: 0,
            plugins: prepared,
            latest: Arc::new(empty_snapshot(session, domain)),
        };
        let world_snapshot = runtime.world.snapshot();
        for slot in &mut runtime.plugins {
            slot.solver.on_world_changed(&world_snapshot)?;
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
        for slot in &self.plugins {
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
            .any(|slot| slot.solver.kind().advances_with_time())
    }

    pub fn play(&mut self) {
        self.clock.play();
    }

    pub fn pause(&mut self) {
        self.clock.pause();
    }

    pub fn set_time_step(&mut self, time_step: TimeStep) -> Result<(), RuntimeError> {
        for slot in &self.plugins {
            slot.solver.validate_time_step(time_step)?;
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
        for slot in &mut self.plugins {
            if slot.solver.kind().advances_with_time() {
                slot.solver.step(context)?;
            }
        }
        self.publish_snapshot(SamplingPolicy::TimeDependentOnly)
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
        let mut candidate = self.world.clone();
        let report = candidate.commit(commands)?;
        if report.revision == self.world.revision() {
            return Ok(report);
        }

        let candidate_snapshot = candidate.snapshot();
        for slot in &self.plugins {
            slot.solver.validate_world(&candidate_snapshot)?;
        }

        self.world = candidate;
        for slot in &mut self.plugins {
            slot.solver.on_world_changed(&candidate_snapshot)?;
        }
        self.publish_snapshot(SamplingPolicy::All)?;
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

        for slot in &self.plugins {
            provenance.push(PluginProvenance {
                id: slot.metadata.id.clone(),
                version: slot.metadata.version,
            });
            diagnostics.extend(slot.solver.diagnostics());

            if sampling == SamplingPolicy::TimeDependentOnly
                && !slot.solver.kind().advances_with_time()
            {
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
                    let column = slot.solver.sample(handle, &geometry)?;
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
    #[error("plugin '{0}' was registered more than once")]
    DuplicatePlugin(PluginId),
    #[error("field channel '{0}' was registered more than once")]
    DuplicateChannel(ChannelId),
    #[error("channel '{channel}' is not owned by plugin '{plugin}'")]
    ForeignChannel {
        plugin: PluginId,
        channel: ChannelId,
    },
    #[error("component '{component}' is not owned by plugin '{plugin}'")]
    ForeignComponent {
        plugin: PluginId,
        component: fieldcad_core::ComponentTypeId,
    },
    #[error("plugin '{0}' declares more channels than a channel handle can address")]
    TooManyChannels(PluginId),
    #[error("single-step is only valid while the simulation is paused")]
    CannotStepWhileRunning,
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
            Self::DuplicatePlugin(_) => "duplicate-plugin",
            Self::DuplicateChannel(_) => "duplicate-channel",
            Self::ForeignChannel { .. } => "foreign-channel",
            Self::ForeignComponent { .. } => "foreign-component",
            Self::TooManyChannels(_) => "too-many-channels",
            Self::CannotStepWhileRunning => "cannot-step-while-running",
            Self::InvalidSamplingBudget => "invalid-sampling-budget",
            Self::InvalidSubscription(_) => "invalid-subscription",
            Self::SamplingBudgetExceeded { .. } => "sampling-budget-exceeded",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
