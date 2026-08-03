//! The equation-system plugin contract.
//!
//! A plugin declares what it observes (field channels), what it needs from
//! objects (component schemas), and how it is configured; a solver is one
//! plugin's mutable state for one simulation session.
//!
//! The trait deliberately contains no rendering or window-system types. A future
//! remote host can expose the same schemas and snapshots over a transport
//! without loading this Rust object into the visualizer process.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use fieldcad_core::{
    ChannelSchema, ComponentSchema, Domain, FieldColumn, ObjectId, PluginId, PluginVersion,
    PropertyBag, PropertySchema, SampleGeometry, SampleValidity, SchemaError, SolverDiagnostic,
    StepContext, TimeStep, Transform, Velocity, WorldSnapshot, validate_properties,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginMetadata {
    pub id: PluginId,
    pub version: PluginVersion,
    pub display_name: String,
    pub description: String,
}

/// Whether an equation system evolves in time.
///
/// An analytic system such as electrostatics has no state to advance: its result
/// depends only on the world. Saying so lets the runtime avoid republishing an
/// unchanged result on every tick, and stops an analytic plugin from having to
/// implement a `step` it does not have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SolverKind {
    /// Re-evaluated from the world; ticks do not change the result.
    #[default]
    Analytic,
    /// Carries state that a fixed-step advance evolves.
    TimeStepped,
}

impl SolverKind {
    pub const fn advances_with_time(self) -> bool {
        matches!(self, Self::TimeStepped)
    }
}

/// An index into the channel list a plugin declared from `channels()`.
///
/// Channels are addressed by handle rather than by `ChannelId` on the sampling
/// path. `ChannelId` is a plugin-namespaced string: correct for serialization,
/// persistence, and the UI, but comparing one per sample means allocating and
/// comparing strings in the hottest loop the application has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelHandle(u16);

impl ChannelHandle {
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginConfigurationSchema {
    pub properties: Vec<PropertySchema>,
}

impl PluginConfigurationSchema {
    /// Configuration is a property bag like any other, so it reuses the core's
    /// single validation implementation rather than repeating it.
    pub fn validate(&self, values: &PropertyBag) -> Result<(), SchemaError> {
        validate_properties(&self.properties, values)
    }
}

/// What a solver needs in order to be created.
pub struct SolverContext<'a> {
    pub configuration: &'a PropertyBag,
    /// The finite region and numerical configuration the solver represents.
    pub domain: &'a Domain,
    /// The world as of the revision the solver is being initialized against.
    pub world: &'a WorldSnapshot,
    /// The scene time at which this solver becomes active.
    ///
    /// A field system can be enabled after other systems have already advanced
    /// the scene clock. Initializing against this context prevents a fresh
    /// solver from publishing a time-zero field under a later snapshot time.
    pub initial_step: StepContext,
    /// Session-lifetime cooperative cancellation, used by asynchronous GPU
    /// completion and other work that may be waiting outside Rust code.
    pub cancellation: SolverCancellation,
}

#[derive(Clone, Debug, Default)]
pub struct SolverCancellation(Arc<AtomicBool>);

impl SolverCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// One channel's values over one geometry, as produced by a solver.
///
/// Values are columnar: the channel already declares dimension and shape once,
/// so repeating them per sample costs memory and buys nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct SampledColumn {
    pub values: FieldColumn,
    pub validity: Vec<SampleValidity>,
}

/// One authoritative object pose and velocity produced by a solver tick.
///
/// This deliberately exposes only kinematics rather than arbitrary world
/// commands. The runtime remains the sole writer of the world, validates the
/// complete result atomically, and can reject two equation systems attempting
/// to advance the same object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectKinematicsUpdate {
    pub object: ObjectId,
    pub transform: Transform,
    pub velocity: Velocity,
}

/// Observable world changes produced while advancing one equation system.
///
/// Most equation systems only evolve private field state and return the empty
/// outcome. Particle-coupled systems use this Interface to publish motion
/// without receiving mutable access to the world.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SolverStepOutcome {
    pub object_kinematics: Vec<ObjectKinematicsUpdate>,
}

impl SampledColumn {
    pub fn new(values: FieldColumn, validity: Vec<SampleValidity>) -> Self {
        Self { values, validity }
    }

    /// Every value was evaluated exactly at its requested position.
    pub fn exact(values: FieldColumn) -> Self {
        let validity = vec![SampleValidity::Exact; values.len()];
        Self { values, validity }
    }

    pub fn exact_scalars(values: Vec<f64>) -> Self {
        Self::exact(FieldColumn::scalars(values))
    }

    pub fn exact_vectors(values: Vec<glam::DVec3>) -> Self {
        Self::exact(FieldColumn::vectors(values))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Describes an equation system and creates isolated solver instances.
pub trait EquationSystemPlugin: Send + Sync {
    fn metadata(&self) -> PluginMetadata;

    /// The channels this plugin publishes. Order defines `ChannelHandle` values
    /// and must be stable for the lifetime of a plugin version.
    fn channels(&self) -> Vec<ChannelSchema>;

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        Vec::new()
    }

    fn configuration_schema(&self) -> PluginConfigurationSchema {
        PluginConfigurationSchema::default()
    }

    fn default_configuration(&self) -> PropertyBag {
        PropertyBag::default()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError>;
}

/// Mutable state owned by one simulation session for one equation system.
pub trait EquationSystemSolver: Send {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    /// Report whether this solver can represent `world`, without changing any
    /// state.
    ///
    /// The runtime calls this on a candidate world *before* adopting it, so a
    /// rejected edit leaves the committed world untouched rather than accepting
    /// an edit that the solver then refuses.
    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        let _ = world;
        Ok(())
    }

    /// Report whether the numerical method can advance with `time_step`.
    ///
    /// The runtime calls this before adopting an initial or edited `dt`, which
    /// gives explicit schemes a place to enforce stability limits atomically.
    /// Analytic solvers normally accept every positive core-validated value.
    fn validate_time_step(&self, time_step: TimeStep) -> Result<(), PluginError> {
        let _ = time_step;
        Ok(())
    }

    /// Adopt a world that `validate_world` has already accepted.
    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError>;

    /// Objects whose canonical pose and velocity this solver may advance.
    ///
    /// The runtime checks ownership conflicts and object existence before any
    /// solver mutates its tick state. A solver may return fewer updates from a
    /// particular tick, but it may not update an object it did not declare.
    fn kinematic_objects(&self) -> &[ObjectId] {
        &[]
    }

    /// Advance internal state by one fixed step. Analytic solvers need not
    /// implement this.
    fn step(&mut self, context: StepContext) -> Result<SolverStepOutcome, PluginError> {
        let _ = context;
        Ok(SolverStepOutcome::default())
    }

    /// Evaluate one channel over a batch of positions.
    ///
    /// The returned column must have one entry per position in `geometry`; the
    /// runtime checks that once per batch rather than once per value.
    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError>;

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        Vec::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error("plugin configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("this plugin does not publish a channel with handle {0}")]
    UnknownChannel(usize),
    #[error("the world cannot be represented by this equation system: {0}")]
    UnsupportedWorld(String),
    #[error("equation solver failed: {0}")]
    Solver(String),
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{Dimension, PropertyId, PropertyKind, PropertyValue, Quantity};

    use super::*;

    #[test]
    fn configuration_validation_rejects_wrong_dimensions() {
        let id = PropertyId::new("gain").unwrap();
        let schema = PluginConfigurationSchema {
            properties: vec![PropertySchema {
                id: id.clone(),
                display_name: "Gain".to_owned(),
                kind: PropertyKind::Scalar(Dimension::DIMENSIONLESS),
                required: true,
            }],
        };
        let values: PropertyBag = [(
            id,
            PropertyValue::Scalar(Quantity::new(2.0, Dimension::MASS).unwrap()),
        )]
        .into_iter()
        .collect();

        assert!(matches!(
            schema.validate(&values),
            Err(SchemaError::ValueMismatch { .. })
        ));
    }

    #[test]
    fn exact_columns_carry_one_validity_flag_per_value() {
        let column = SampledColumn::exact_scalars(vec![1.0, 2.0, 3.0]);

        assert_eq!(column.len(), 3);
        assert_eq!(column.validity.len(), 3);
        assert!(column.validity.iter().all(|flag| flag.is_usable()));
    }

    #[test]
    fn analytic_is_the_default_solver_kind() {
        assert!(!SolverKind::default().advances_with_time());
        assert!(SolverKind::TimeStepped.advances_with_time());
    }

    #[test]
    fn solver_cancellation_is_shared_across_the_session() {
        let worker = SolverCancellation::default();
        let controller = worker.clone();

        assert!(!worker.is_cancelled());
        controller.cancel();
        assert!(worker.is_cancelled());
    }
}
