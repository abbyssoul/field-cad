//! The equation-system plugin contract.
//!
//! A plugin declares what it observes (field channels), what it needs from
//! objects (component schemas), and how it is configured; a solver is one
//! plugin's mutable state for one simulation session.
//!
//! The trait deliberately contains no rendering or window-system types. A future
//! remote host can expose the same schemas and snapshots over a transport
//! without loading this Rust object into the visualizer process.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use fieldcad_core::quantities::{LengthMetres, MassKg};
use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, Domain, FieldColumn, GradientColumn, ObjectId,
    PlaneId, PluginId, PluginVersion, PropertyBag, PropertySchema, Quantity, SampleGeometry,
    SampleValidity, SchemaError, SolverDiagnostic, StepContext, TimeStep, Transform, Velocity,
    WorldSnapshot, validate_properties,
};
use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

// Not `Eq`: a property's declared default may carry an `f64` magnitude.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

/// The radial profile used by a numerical field brush.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldBrushFalloff {
    /// A smooth, compact bump: it reaches zero exactly at the brush radius.
    #[default]
    SmoothCompact,
}

/// Serializable user intent for one field-painting gesture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldBrushStroke {
    pub channel: ChannelId,
    pub plane: PlaneId,
    /// Plane-local `(u, v)` centres, in metres, sampled along one drag.
    pub samples: Vec<DVec2>,
    pub radius_metres: LengthMetres,
    pub strength: Quantity,
    pub falloff: FieldBrushFalloff,
}

/// A brush stroke resolved against the authoritative plane at execution time.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFieldBrushStroke {
    pub stroke: FieldBrushStroke,
    pub centres: Vec<DVec3>,
    pub direction: DVec3,
}

/// One channel's values over one geometry, as produced by a solver.
///
/// Values are columnar: the channel already declares dimension and shape once,
/// so repeating them per sample costs memory and buys nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct SampledColumn {
    pub values: FieldColumn,
    pub validity: Vec<SampleValidity>,
    /// The channel's spatial derivative at each sample, if this solver can
    /// report one. Most cannot or do not bother — `None` means every
    /// consumer falls back to today's plain trilinear/bilinear
    /// reconstruction, the same optional-capability idiom the rest of this
    /// trait uses (see [`EquationSystemSolver::time_step_limit`]).
    pub gradient: Option<GradientColumn>,
}

/// One body the dynamics system will move, as a field system sees it.
///
/// Deliberately carries only kinematics and inertia. A plugin's own coupling
/// charge — electric charge, gravitational mass — is read from the world it
/// already adopted in `on_world_changed`, so this type never has to name every
/// quantity a future field might couple to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicBody {
    pub object: ObjectId,
    /// The inertia a total force will be divided by. Always finite and positive.
    pub inertial_mass_kg: MassKg,
    pub position: glam::DVec3,
    pub velocity: glam::DVec3,
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
        Self {
            values,
            validity,
            gradient: None,
        }
    }

    /// Every value was evaluated exactly at its requested position.
    pub fn exact(values: FieldColumn) -> Self {
        let validity = vec![SampleValidity::Exact; values.len()];
        Self {
            values,
            validity,
            gradient: None,
        }
    }

    pub fn exact_scalars(values: Vec<f64>) -> Self {
        Self::exact(FieldColumn::scalars(values))
    }

    pub fn exact_vectors(values: Vec<glam::DVec3>) -> Self {
        Self::exact(FieldColumn::vectors(values))
    }

    /// Report a per-sample spatial derivative alongside the values already
    /// set. Most solvers never call this; the runtime is what actually
    /// validates it (`FieldBatch::with_gradient`), matching how this type
    /// never validated `values`/`validity` either.
    pub fn with_gradient(mut self, gradient: GradientColumn) -> Self {
        self.gradient = Some(gradient);
        self
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

    /// Largest stable fixed step this solver can recommend for its currently
    /// configured numerical domain. `None` means the solver has no finite
    /// domain-derived limit to offer; it still validates candidates through
    /// [`Self::validate_time_step`].
    fn time_step_limit(&self) -> Option<TimeStep> {
        None
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

    /// Vector channels this evolving solver permits direct numerical edits to.
    /// Analytic solvers deliberately advertise none.
    fn mutable_vector_channels(&self) -> &[ChannelHandle] {
        &[]
    }

    /// Apply one resolved brush stroke. The runtime has already checked that
    /// the channel belongs to this solver and is a declared mutable channel.
    fn apply_field_brush_stroke(
        &mut self,
        _stroke: &ResolvedFieldBrushStroke,
    ) -> Result<(), PluginError> {
        Err(PluginError::Solver(
            "this solver does not support numerical field painting".to_owned(),
        ))
    }

    /// Add this system's force on each dynamic body into `out`, in newtons.
    ///
    /// `out` has one entry per body, in the order given, already zeroed by
    /// the caller for this call. A plugin adds its own contribution into it
    /// (`out[i] += force`) rather than overwriting or returning one, so the
    /// runtime can sum every enabled system's contribution into the same
    /// buffer without allocating a fresh one per plugin per tick. A system
    /// that exerts no force on a body simply adds nothing to that entry,
    /// rather than special-casing it; a system that exerts no force on any
    /// body at all is exactly today's default trait body: a no-op.
    ///
    /// This is where coupling happens. Each field system converts its own field
    /// and its own coupling charge into a force — `qE` for an electric field,
    /// `m_g·g` for a gravitational one — and the dynamics system, which knows
    /// about neither, sums them and divides by inertia. A new field becomes
    /// dynamically coupled by implementing this and nothing else.
    ///
    /// Forces are evaluated at the body's *current* position and velocity, so a
    /// velocity-dependent force such as `qv×B` is expressible; note that
    /// collapsing it into a single vector here is what costs the exact magnetic
    /// rotation a Boris push would have given ([ADR 0022]).
    ///
    /// `out.len()` always equals `bodies.len()` — the caller sizes it once,
    /// before any plugin is asked to add to it, and it cannot change
    /// afterward: it is a slice, not a `Vec`, precisely so a plugin has no
    /// way to answer with the wrong number of forces.
    ///
    /// [ADR 0022]: ../../../docs/adr/0022-dynamics-is-a-first-party-system.md
    fn add_forces(&self, bodies: &[DynamicBody], out: &mut [DVec3]) -> Result<(), PluginError> {
        let _ = (bodies, out);
        Ok(())
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

/// A small, size-bounded memoization of the last few sample geometries a
/// solver was asked to evaluate, keyed by structural equality on
/// [`SampleGeometry`] and evicted oldest-first once full.
///
/// A runtime publication samples the same handful of geometries (each
/// visible plane, box, sphere, and the probe set) once per channel; without
/// this, an analytic solver with several channels over the same geometry
/// redoes the same evaluation once per channel, every tick.
struct SampleCacheEntry<T> {
    geometry: SampleGeometry,
    samples: Arc<[T]>,
    /// Set by [`SampleCache::clear`]; a moved source invalidates the
    /// *values* here without changing `geometry` itself (the geometry a
    /// runtime publication samples — a plane, box, sphere, or probe set —
    /// is stable tick to tick even while its sources move), so the entry's
    /// buffer is worth keeping and refilling rather than dropping.
    stale: bool,
}

pub struct SampleCache<T> {
    capacity: usize,
    entries: Mutex<VecDeque<SampleCacheEntry<T>>>,
}

impl<T> SampleCache<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(VecDeque::new()),
        }
    }

    /// The cached samples for `geometry`.
    ///
    /// A warm hit returns the existing `Arc` directly, running neither
    /// closure. A stale entry (same geometry, invalidated by [`Self::clear`])
    /// is refilled in place via `refresh` whenever the cache is still the
    /// sole owner of its buffer — the common case, since nothing else holds
    /// onto a `samples_for` result past its own call — so a moved source
    /// costs a recompute but not a reallocation. `compute` is the
    /// allocating fallback: a geometry never seen before, or the rare case
    /// where the existing buffer is still shared and can't be reused.
    pub fn get_or_try_insert_with(
        &self,
        geometry: &SampleGeometry,
        compute: impl FnOnce() -> Result<Vec<T>, PluginError>,
        refresh: impl FnOnce(&mut [T]) -> Result<(), PluginError>,
    ) -> Result<Arc<[T]>, PluginError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| PluginError::Solver("sample cache poisoned".to_owned()))?;
        if let Some(entry) = entries.iter_mut().find(|entry| &entry.geometry == geometry) {
            if !entry.stale {
                return Ok(Arc::clone(&entry.samples));
            }
            if let Some(slice) = Arc::get_mut(&mut entry.samples) {
                refresh(slice)?;
            } else {
                entry.samples = compute()?.into();
            }
            entry.stale = false;
            return Ok(Arc::clone(&entry.samples));
        }
        let samples: Arc<[T]> = compute()?.into();
        if entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(SampleCacheEntry {
            geometry: geometry.clone(),
            samples: Arc::clone(&samples),
            stale: false,
        });
        Ok(samples)
    }

    /// Invalidate every cached entry's *values* — call when the world
    /// changes and cached samples would no longer reflect it. Buffers are
    /// kept, not dropped: [`Self::get_or_try_insert_with`] refills them in
    /// place on the next request for the same geometry rather than
    /// reallocating.
    pub fn clear(&mut self) -> Result<(), PluginError> {
        for entry in self
            .entries
            .get_mut()
            .map_err(|_| PluginError::Solver("sample cache poisoned".to_owned()))?
        {
            entry.stale = true;
        }
        Ok(())
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
    use fieldcad_core::{Dimension, ProbeId, PropertyId, PropertyKind, PropertyValue, Quantity};

    use super::*;

    fn probe_geometry(value: f64) -> SampleGeometry {
        SampleGeometry::probes(
            vec![ProbeId::new(1), ProbeId::new(2)],
            vec![DVec3::X * value, DVec3::Y * value],
        )
        .unwrap()
    }

    #[test]
    fn a_warm_hit_runs_neither_closure() {
        let cache = SampleCache::<f64>::new(4);
        let geometry = probe_geometry(1.0);
        cache
            .get_or_try_insert_with(
                &geometry,
                || Ok(vec![1.0, 2.0]),
                |_| panic!("refresh must not run on the very first insert"),
            )
            .unwrap();

        let hit = cache
            .get_or_try_insert_with(
                &geometry,
                || panic!("compute must not run on a warm hit"),
                |_| panic!("refresh must not run on a warm hit"),
            )
            .unwrap();

        assert_eq!(&*hit, [1.0, 2.0]);
    }

    #[test]
    fn a_stale_hit_refreshes_the_same_buffer_instead_of_reallocating() {
        let mut cache = SampleCache::<f64>::new(4);
        let geometry = probe_geometry(1.0);
        let first = cache
            .get_or_try_insert_with(&geometry, || Ok(vec![1.0, 2.0]), |_| unreachable!())
            .unwrap();
        let original_allocation = Arc::as_ptr(&first);
        // A real `samples_for` never lets its `Arc` clone outlive the call
        // that produced it — drop this one the same way, so the cache is
        // the sole owner again by the time it's asked to refresh.
        drop(first);

        cache.clear().unwrap();

        let refreshed = cache
            .get_or_try_insert_with(
                &geometry,
                || panic!("compute must not run once a stale buffer can be reused in place"),
                |samples| {
                    samples[0] = 3.0;
                    samples[1] = 4.0;
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(
            Arc::as_ptr(&refreshed),
            original_allocation,
            "a stale hit should refill the existing allocation, not replace it"
        );
        assert_eq!(&*refreshed, [3.0, 4.0]);
    }

    #[test]
    fn configuration_validation_rejects_wrong_dimensions() {
        let id = PropertyId::new("gain").unwrap();
        let schema = PluginConfigurationSchema {
            properties: vec![PropertySchema {
                id: id.clone(),
                display_name: "Gain".to_owned(),
                description: None,
                kind: PropertyKind::Scalar(Dimension::DIMENSIONLESS),
                required: true,
                default_value: None,
                relevant_when: None,
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
    fn sampled_column_gradient_defaults_to_none() {
        let column = SampledColumn::exact_scalars(vec![1.0]);
        assert!(column.gradient.is_none());

        let with_gradient = SampledColumn::exact_vectors(vec![glam::DVec3::X])
            .with_gradient(GradientColumn::Vector(vec![glam::DMat3::IDENTITY].into()));
        assert!(with_gradient.gradient.is_some());
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
