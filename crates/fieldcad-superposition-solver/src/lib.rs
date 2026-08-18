//! The shared equation-system solver skeleton over
//! `fieldcad-superposition`'s inverse-square kernel.
//!
//! `fieldcad-superposition` owns the mathematics and deliberately carries
//! no plugin types; each equation-system plugin owns its identity,
//! schemas, and constants but has no solver logic of its own. This crate
//! is the layer between: one [`EquationSystemSolver`] implementation —
//! sampling with per-geometry caching, index-based force exclusion, the
//! evaluator precision gate, and the batch-length verification —
//! parameterized by an [`InverseSquareCoupling`] so gravity and
//! electrostatics (and any future inverse-square coupling) share it
//! rather than mirror it. The correctness detail that motivated this
//! extraction — the batch-length check only one of the two mirrored
//! solvers carried — now exists exactly once, here, for every coupling.

use std::marker::PhantomData;
use std::sync::Arc;

use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    ChannelSchema, ComponentSchema, CoupledSource, DiagnosticSeverity, Domain, FieldColumn,
    GradientColumn, ObjectIndex, SampleGeometry, SampleValidity, SolverDiagnostic, WorldRevision,
    WorldSnapshot,
};
use fieldcad_plugin_api::{
    ChannelHandle, DynamicBody, EquationSystemPlugin, EquationSystemSolver, PluginError,
    PluginMetadata, SampleCache, SampledColumn, SolverContext, SolverKind,
};
use fieldcad_superposition::{
    CpuInverseSquareEvaluator, InverseSquareBatchEvaluator, InverseSquareSample,
    InverseSquareSource,
};
use glam::{DMat3, DVec3};

/// One geometry's evaluation plus every column both channels publish,
/// derived once per evaluation rather than once per channel read.
///
/// A runtime publication asks for the field and the potential channels
/// over the same geometry, and unchanged-world reads ask again; deriving
/// the shared columns at evaluation time makes each read an `Arc` clone
/// instead of an O(samples) allocation pass. `samples` is retained only
/// so a stale-refill can `evaluate_into` it in place, reusing its
/// allocation; nothing reads it after derivation.
struct GeometrySamples {
    samples: Vec<InverseSquareSample>,
    validity: Arc<[SampleValidity]>,
    /// `Some` only when *every* sample reported a gradient — the
    /// per-batch capability decision the published columns share.
    jacobians: Option<Arc<[DMat3]>>,
    field_values: Arc<[DVec3]>,
    potential_values: Arc<[f64]>,
    /// ∇φ = −field: the potential channel's gradient column.
    negated_fields: Arc<[DVec3]>,
}

impl GeometrySamples {
    fn evaluate(
        evaluator: &dyn InverseSquareBatchEvaluator,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Self, PluginError> {
        let samples = evaluator
            .evaluate(coupling_constant, sources, domain, geometry)
            .map_err(PluginError::Solver)?;
        // An injected evaluator that cannot meet the batch contract returns
        // `Err`, never a partial result — this check makes a wrong-length
        // success impossible to publish downstream, where a length mismatch
        // would surface as a generic `FieldBatch` error with no solver
        // context.
        if samples.len() != geometry.len() {
            return Err(PluginError::Solver(format!(
                "batched evaluator returned {} samples for a geometry of length {}",
                samples.len(),
                geometry.len()
            )));
        }
        Ok(Self::from_samples(samples))
    }

    fn refill(
        &mut self,
        evaluator: &dyn InverseSquareBatchEvaluator,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<(), PluginError> {
        evaluator
            .evaluate_into(
                coupling_constant,
                sources,
                domain,
                geometry,
                &mut self.samples,
            )
            .map_err(PluginError::Solver)?;
        *self = Self::from_samples(std::mem::take(&mut self.samples));
        Ok(())
    }

    /// One pass over `samples`, not five: each column used to be its own
    /// `.iter().map(...).collect()`, so a geometry's samples were read once
    /// per column instead of once total. `jacobians` optimistically fills
    /// with `DMat3::ZERO` for a missing gradient rather than allocating a
    /// second time once every sample turns out to have one — the batch-wide
    /// capability gate ([`GeometrySamples`]'s doc comment) means an
    /// evaluator either reports a gradient for every sample or none at all,
    /// so the discard case this trades against is not the common one.
    fn from_samples(samples: Vec<InverseSquareSample>) -> Self {
        let mut validity = Vec::with_capacity(samples.len());
        let mut jacobians = Vec::with_capacity(samples.len());
        let mut field_values = Vec::with_capacity(samples.len());
        let mut potential_values = Vec::with_capacity(samples.len());
        let mut negated_fields = Vec::with_capacity(samples.len());
        let mut every_sample_has_a_gradient = true;
        for sample in &samples {
            validity.push(sample.validity);
            jacobians.push(sample.gradient.unwrap_or(DMat3::ZERO));
            every_sample_has_a_gradient &= sample.gradient.is_some();
            field_values.push(sample.field);
            potential_values.push(sample.potential);
            negated_fields.push(-sample.field);
        }
        Self {
            samples,
            validity: validity.into(),
            jacobians: every_sample_has_a_gradient.then(|| jacobians.into()),
            field_values: field_values.into(),
            potential_values: potential_values.into(),
            negated_fields: negated_fields.into(),
        }
    }
}

/// Handle of the vector-valued field channel every coupling publishes
/// first — acceleration `g` for gravity, the electric field `E` for
/// electrostatics. A plugin's own handle constants are re-exports of
/// these, so what its channel schema advertises and what its solver
/// matches cannot drift apart.
pub const FIELD_CHANNEL_HANDLE: ChannelHandle = ChannelHandle::new(0);
/// Handle of the scalar potential channel every coupling publishes
/// second — `Φ` for gravity, `φ` ( volts) for electrostatics.
pub const POTENTIAL_CHANNEL_HANDLE: ChannelHandle = ChannelHandle::new(1);

/// `CoupledSource<T>` → the shared, coupling-agnostic source shape the
/// kernel (and any GPU evaluator built over it) actually operates on,
/// generic over the coupling quantity — charge, mass, or any future
/// `SiScalar` coupling. The plugins re-export this under their own
/// `inverse_square_source` names, so an injected evaluator builds its
/// source buffer from the same mapping the CPU reference uses, exactly
/// once.
pub fn coupled_inverse_square_source<T: SiScalar>(
    source: &CoupledSource<T>,
) -> InverseSquareSource {
    InverseSquareSource {
        position: source.position,
        strength: source.coupling_value.into_si(),
        distribution: source.distribution,
    }
}

/// A subscription currently has probes plus any number of planes and one
/// grid. Bounds stale entries left by density changes without complicating
/// the plugin contract with publication lifecycle callbacks.
const SAMPLE_CACHE_CAPACITY: usize = 16;

/// Everything one inverse-square coupling law differs from another by.
///
/// Implementations live in the plugins: the coupling constant with its
/// sign (−G for gravity, +k for electrostatics), the world-reading source
/// collection, the identity and schemas, and the wording of the few
/// plugin-facing strings. Everything else — the solver and the plugin
/// wrapper — is shared.
pub trait InverseSquareCoupling: Send + Sync + 'static {
    /// The coupling quantity carried by one source — mass for gravity,
    /// charge for electrostatics. Every coupling's world-collected source
    /// is `CoupledSource<Self::Strength>`, never a bespoke shape: naming
    /// just the quantity (not `IdentifiedByObject`, which `CoupledSource<T>`
    /// already implements for every `T`) is what lets
    /// `inverse_square_source`/`strength` below live here once, as default
    /// methods, instead of as an identical two-line impl per plugin.
    type Strength: SiScalar + Send;

    /// The coupling constant in SI, sign included.
    const COUPLING_CONSTANT: f64;
    /// The equation system's own name, for the evaluator/domain precision
    /// mismatch error.
    const SYSTEM_LABEL: &'static str;
    /// The solver error message when a body's summed field overflows to a
    /// non-finite value.
    const NON_FINITE_MESSAGE: &'static str;
    /// The info diagnostic's stability code (e.g.
    /// `newtonian-gravity-source-count`).
    const DIAGNOSTIC_CODE: &'static str;
    /// How one source is called in prose (e.g. `mass`, `charge`), for the
    /// info diagnostic.
    const SOURCE_NOUN: &'static str;

    /// The coupling's plugin identity and display metadata. The single
    /// source of the plugin id — diagnostics report `metadata().id`, so
    /// identity cannot drift between the two.
    fn metadata() -> PluginMetadata;
    /// The channels this coupling publishes, in field-then-potential
    /// handle order ([`FIELD_CHANNEL_HANDLE`], then
    /// [`POTENTIAL_CHANNEL_HANDLE`]).
    fn channels() -> Vec<ChannelSchema>;
    /// The component schemas the coupling's sources are authored against.
    fn component_schemas() -> Vec<ComponentSchema>;

    /// Collect every source the world carries for this coupling, in
    /// deterministic object order. Errors as a displayable string;
    /// the skeleton wraps it in `UnsupportedWorld`.
    fn collect_sources(world: &WorldSnapshot)
    -> Result<Vec<CoupledSource<Self::Strength>>, String>;

    /// The same mapping this coupling's CPU reference exposes publicly, so
    /// the solver's precomputed buffer is built exactly like the oracle's
    /// inputs. The default is exactly [`coupled_inverse_square_source`];
    /// every coupling's source is a plain `CoupledSource<Self::Strength>`,
    /// so there is nothing plugin-specific left to override.
    fn inverse_square_source(source: &CoupledSource<Self::Strength>) -> InverseSquareSource {
        coupled_inverse_square_source(source)
    }

    /// The coupling value in SI — the number zero-strength filtering and
    /// the force law both use. Same reasoning as
    /// [`Self::inverse_square_source`]: nothing plugin-specific to override.
    fn strength(source: &CoupledSource<Self::Strength>) -> f64 {
        source.coupling_value.into_si()
    }
}

/// The one analytic inverse-square solver, shared by every coupling.
///
/// Holds the object-indexed sources, the positionally aligned
/// `inverse_square_sources` buffer (built from the same filtered list, so
/// position `i` in one is position `i` in the other — the alignment
/// index-based force exclusion relies on), the injected batch evaluator,
/// and the per-geometry sample cache.
pub struct InverseSquareSolver<C: InverseSquareCoupling> {
    domain: Domain,
    sources: CouplingSources<C>,
    /// Rebuilt with the object-indexed sources on creation/world changes;
    /// this is the cache-local input shape the shared evaluator expects,
    /// converted once per world change rather than on every channel read.
    inverse_square_sources: Vec<InverseSquareSource>,
    world_revision: WorldRevision,
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    /// Runtime publication asks for the field and the potential separately,
    /// and unchanged-world reads ask again. Each geometry's evaluation and
    /// every column both channels derive from it live in one entry, so a
    /// read is an `Arc` clone, not an O(samples) derivation.
    cache: SampleCache<GeometrySamples>,
}

impl<C: InverseSquareCoupling> InverseSquareSolver<C> {
    /// Build a solver for `context`'s world over the injected evaluator.
    ///
    /// Rejects an evaluator whose precision disagrees with the domain's: a
    /// snapshot's precision metadata must describe the numbers it actually
    /// carries, or an `f32` interactive result is indistinguishable from
    /// the `f64` oracle it is checked against.
    pub fn new(
        context: SolverContext<'_>,
        evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    ) -> Result<Self, PluginError> {
        if context.domain.precision() != evaluator.precision() {
            return Err(PluginError::InvalidConfiguration(format!(
                "{} evaluator produces {}, but the domain declares {}",
                C::SYSTEM_LABEL,
                evaluator.precision().label(),
                context.domain.precision().label()
            )));
        }
        let (sources, inverse_square_sources) = load_sources::<C>(context.world)?;
        Ok(Self {
            domain: *context.domain,
            sources,
            inverse_square_sources,
            world_revision: context.world.revision(),
            evaluator,
            cache: SampleCache::new(SAMPLE_CACHE_CAPACITY),
        })
    }
}

/// A coupling's object-indexed, zero-strength-filtered sources — the shape
/// held by [`InverseSquareSolver`] and returned by [`load_sources`].
type CouplingSources<C> = ObjectIndex<CoupledSource<<C as InverseSquareCoupling>::Strength>>;

/// Collect the coupling's sources, dropping zero-strength ones before they
/// reach the solver's indexes, and derive the positionally aligned
/// `inverse_square_sources` buffer from that same filtered list — shared by
/// [`InverseSquareSolver::new`] and `on_world_changed`, the only two places
/// the world is (re-)read.
///
/// Position `i` in the `ObjectIndex` is position `i` in the buffer — the
/// alignment index-based force exclusion relies on. Dropping zero-strength
/// sources here is observable only through sources whose field and force
/// contributions are exactly zero (the kernel skips them anyway); it also
/// keeps the reported source count to contributing sources.
fn load_sources<C: InverseSquareCoupling>(
    world: &WorldSnapshot,
) -> Result<(CouplingSources<C>, Vec<InverseSquareSource>), PluginError> {
    let sources = C::collect_sources(world)
        .map(|collected| {
            ObjectIndex::new(
                collected
                    .into_iter()
                    .filter(|source| C::strength(source) != 0.0)
                    .collect(),
            )
        })
        .map_err(PluginError::UnsupportedWorld)?;
    let inverse_square_sources = sources
        .as_slice()
        .iter()
        .map(C::inverse_square_source)
        .collect();
    Ok((sources, inverse_square_sources))
}

impl<C: InverseSquareCoupling> EquationSystemSolver for InverseSquareSolver<C> {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        C::collect_sources(world)
            .map(|_| ())
            .map_err(PluginError::UnsupportedWorld)
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        (self.sources, self.inverse_square_sources) = load_sources::<C>(world)?;
        self.world_revision = world.revision();
        self.cache.clear()
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let bundle = self.samples_for(geometry)?;
        let entry = &bundle[0];
        match channel {
            FIELD_CHANNEL_HANDLE => Ok(SampledColumn::from_shared_parts(
                FieldColumn::Vector(Arc::clone(&entry.field_values)),
                Arc::clone(&entry.validity),
                entry
                    .jacobians
                    .as_ref()
                    .map(|jacobians| GradientColumn::Vector(Arc::clone(jacobians))),
            )),
            POTENTIAL_CHANNEL_HANDLE => Ok(SampledColumn::from_shared_parts(
                FieldColumn::Scalar(Arc::clone(&entry.potential_values)),
                Arc::clone(&entry.validity),
                // ∇φ = −field: published under the same batch-wide gradient
                // capability gate as the field channel's Jacobian, so both
                // channels' gradient availability stays consistent for the
                // same evaluator.
                entry
                    .jacobians
                    .as_ref()
                    .map(|_| GradientColumn::Scalar(Arc::clone(&entry.negated_fields))),
            )),
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    fn add_forces(&self, bodies: &[DynamicBody], out: &mut [DVec3]) -> Result<(), PluginError> {
        // A body absent from the filtered index (not a source, or zero
        // coupling) neither exerts nor feels this force — `None`, skipped
        // by the kernel. One call per tick; the force law itself lives in
        // the kernel, once, with its tests.
        fieldcad_superposition::add_forces_excluding_into(
            C::COUPLING_CONSTANT,
            &self.inverse_square_sources,
            bodies
                .iter()
                .map(|body| (self.sources.index_of(body.object), body.position)),
            out,
        )
        .map_err(|error| match error {
            fieldcad_superposition::AddForcesError::NonFinite { .. } => {
                PluginError::Solver(C::NON_FINITE_MESSAGE.to_owned())
            }
        })
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: C::metadata().id,
            severity: DiagnosticSeverity::Info,
            code: C::DIAGNOSTIC_CODE.to_owned(),
            message: format!(
                "{} {} source(s), {} batched evaluator, world revision {}",
                self.sources.len(),
                C::SOURCE_NOUN,
                self.evaluator.precision().label(),
                self.world_revision
            ),
        }]
    }
}

impl<C: InverseSquareCoupling> InverseSquareSolver<C> {
    /// The evaluation and every derived column for `geometry`, as one
    /// length-1 memoized bundle.
    ///
    /// `SampleCache` is keyed and length-checked per its original
    /// per-sample-point contract; wrapping the whole geometry's evaluation
    /// in one `GeometrySamples` means the cache's own length guard (`entry
    /// length == geometry.len()`) only re-admits the in-place `refresh`
    /// path when `geometry.len() == 1` (a single probe) — every other
    /// geometry takes the `compute` fallback on a stale hit, paying one
    /// fresh evaluation exactly as it always did pre-memoization. What
    /// Phase 6 buys either way: repeat reads of the same evaluation (the
    /// field and potential channels of one publish, or repeated reads of
    /// an unchanged world) are `Arc` clones, not re-derivations.
    fn samples_for(
        &self,
        geometry: &SampleGeometry,
    ) -> Result<Arc<[GeometrySamples]>, PluginError> {
        self.cache.get_or_try_insert_with(
            geometry,
            || {
                let bundle = GeometrySamples::evaluate(
                    self.evaluator.as_ref(),
                    C::COUPLING_CONSTANT,
                    &self.inverse_square_sources,
                    &self.domain,
                    geometry,
                )?;
                Ok(vec![bundle])
            },
            |slice| {
                slice[0].refill(
                    self.evaluator.as_ref(),
                    C::COUPLING_CONSTANT,
                    &self.inverse_square_sources,
                    &self.domain,
                    geometry,
                )
            },
        )
    }
}

/// The equation-system plugin wrapper shared by every coupling: it carries
/// only the injected batch evaluator. Identity, schemas, and constants
/// come from the [`InverseSquareCoupling`]; a plugin crate names the
/// instantiation through a public type alias over its coupling (gravity's
/// `NewtonianGravityPlugin`, electrostatics' `ElectrostaticsPlugin`).
pub struct InverseSquarePlugin<C: InverseSquareCoupling> {
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    coupling: PhantomData<C>,
}

impl<C: InverseSquareCoupling> Default for InverseSquarePlugin<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: InverseSquareCoupling> InverseSquarePlugin<C> {
    /// Backed by the reference `f64` evaluator.
    pub fn new() -> Self {
        Self {
            evaluator: Arc::new(CpuInverseSquareEvaluator),
            coupling: PhantomData,
        }
    }

    /// Backed by a host-owned evaluator, typically a `wgpu` compute
    /// backend.
    pub fn with_evaluator(evaluator: Arc<dyn InverseSquareBatchEvaluator>) -> Self {
        Self {
            evaluator,
            coupling: PhantomData,
        }
    }
}

impl<C: InverseSquareCoupling> EquationSystemPlugin for InverseSquarePlugin<C> {
    fn metadata(&self) -> PluginMetadata {
        C::metadata()
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        C::channels()
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        C::component_schemas()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        Ok(Box::new(InverseSquareSolver::<C>::new(
            context,
            Arc::clone(&self.evaluator),
        )?))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fieldcad_core::{
        CoupledSource, ObjectSpec, PluginId, PluginVersion, Precision, ProbeId, Transform, World,
        WorldCommand,
    };
    use fieldcad_plugin_api::SolverCancellation;

    use super::*;

    /// A minimal coupling over unitless point sources, for contract tests
    /// that are about the skeleton, not any physics.
    struct ToyCoupling;

    impl InverseSquareCoupling for ToyCoupling {
        type Strength = f64;
        const COUPLING_CONSTANT: f64 = 1.0;
        const SYSTEM_LABEL: &str = "toy";
        const NON_FINITE_MESSAGE: &str = "toy coupling overflowed";
        const DIAGNOSTIC_CODE: &str = "toy-source-count";
        const SOURCE_NOUN: &str = "unit";

        fn metadata() -> PluginMetadata {
            PluginMetadata {
                id: PluginId::new("fieldcad.toy").expect("static plugin ID is valid"),
                version: PluginVersion::new(0, 1, 0),
                display_name: "Toy coupling".to_owned(),
                description: "Unit-strength sources for skeleton contract tests".to_owned(),
            }
        }

        fn channels() -> Vec<ChannelSchema> {
            Vec::new()
        }

        fn component_schemas() -> Vec<ComponentSchema> {
            Vec::new()
        }

        fn collect_sources(world: &WorldSnapshot) -> Result<Vec<CoupledSource<f64>>, String> {
            // Every object is a unit-strength source at its transform.
            Ok(world
                .objects()
                .values()
                .map(|object| {
                    CoupledSource::new(
                        object.id,
                        object.transform.translation,
                        Default::default(),
                        1.0,
                        fieldcad_core::ChargeDistribution::Point {
                            exclusion_radius: 0.0,
                        },
                    )
                })
                .collect())
        }

        // `inverse_square_source`/`strength` are the trait's default
        // methods: a toy `f64`-strength coupling has nothing plugin-
        // specific to add, which is the point of this test double.
    }

    /// Counts `evaluate` calls; returns canned samples sized to the
    /// geometry. `precision` lets tests choose either side of the gate.
    struct CountingEvaluator {
        calls: AtomicUsize,
        precision: Precision,
    }

    impl InverseSquareBatchEvaluator for CountingEvaluator {
        fn precision(&self) -> Precision {
            self.precision
        }

        fn evaluate(
            &self,
            _coupling_constant: f64,
            _sources: &[InverseSquareSource],
            _domain: &Domain,
            geometry: &SampleGeometry,
        ) -> Result<Vec<InverseSquareSample>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![
                InverseSquareSample {
                    field: DVec3::X,
                    potential: 2.0,
                    gradient: None,
                    validity: fieldcad_core::SampleValidity::Exact,
                };
                geometry.len()
            ])
        }
    }

    /// An evaluator whose success violates the batch contract's length
    /// guarantee — the exact misbehavior the skeleton must catch.
    struct WrongLengthEvaluator;

    impl InverseSquareBatchEvaluator for WrongLengthEvaluator {
        fn precision(&self) -> Precision {
            Precision::F64
        }

        fn evaluate(
            &self,
            _coupling_constant: f64,
            _sources: &[InverseSquareSource],
            _domain: &Domain,
            _geometry: &SampleGeometry,
        ) -> Result<Vec<InverseSquareSample>, String> {
            Ok(Vec::new())
        }
    }

    fn toy_world() -> World {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("unit").with_transform(Transform::at_finite(DVec3::X)),
            )])
            .expect("a bare object is authorable");
        world
    }

    fn toy_solver(
        evaluator: Arc<dyn InverseSquareBatchEvaluator>,
        world: &WorldSnapshot,
        domain: &Domain,
    ) -> Box<dyn EquationSystemSolver> {
        Box::new(
            InverseSquareSolver::<ToyCoupling>::new(
                SolverContext {
                    configuration: &Default::default(),
                    domain,
                    world,
                    initial_step: fieldcad_core::StepContext {
                        tick: 0,
                        time_seconds: 0.0,
                        time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
                    },
                    cancellation: SolverCancellation::default(),
                },
                evaluator,
            )
            .expect("toy coupling composes a valid solver"),
        )
    }

    fn f64_domain() -> Domain {
        Domain::centred_cube(2.0, 4).unwrap()
    }

    fn probe_geometry() -> SampleGeometry {
        // Off the toy source's own position (+X), so a real evaluator's
        // samples come back `Exact` rather than excluded.
        SampleGeometry::probes(vec![ProbeId::new(0)], vec![DVec3::new(0.5, 0.2, 0.1)]).unwrap()
    }

    #[test]
    fn both_channels_share_one_batch_evaluation() {
        let evaluator = Arc::new(CountingEvaluator {
            calls: AtomicUsize::new(0),
            precision: Precision::F64,
        });
        let world = toy_world();
        let solver = toy_solver(evaluator.clone(), &world.snapshot(), &f64_domain());
        let geometry = probe_geometry();

        solver.sample(FIELD_CHANNEL_HANDLE, &geometry).unwrap();
        solver.sample(POTENTIAL_CHANNEL_HANDLE, &geometry).unwrap();

        assert_eq!(evaluator.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_world_change_invalidates_the_cached_evaluation() {
        let evaluator = Arc::new(CountingEvaluator {
            calls: AtomicUsize::new(0),
            precision: Precision::F64,
        });
        let mut world = toy_world();
        let domain = f64_domain();
        let mut solver = toy_solver(evaluator.clone(), &world.snapshot(), &domain);
        let geometry = probe_geometry();

        solver.sample(FIELD_CHANNEL_HANDLE, &geometry).unwrap();
        assert_eq!(evaluator.calls.load(Ordering::Relaxed), 1);

        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("another unit").with_transform(Transform::at_finite(-DVec3::X)),
            )])
            .unwrap();
        solver
            .on_world_changed(&world.snapshot())
            .expect("the changed world is representable");
        solver.sample(FIELD_CHANNEL_HANDLE, &geometry).unwrap();
        assert_eq!(
            evaluator.calls.load(Ordering::Relaxed),
            2,
            "a changed world must force a re-evaluation, not reuse the cache"
        );
    }

    #[test]
    fn an_evaluator_precision_mismatch_is_rejected_at_creation() {
        let evaluator = Arc::new(CountingEvaluator {
            calls: AtomicUsize::new(0),
            precision: Precision::F32,
        });
        let world = toy_world();

        assert!(matches!(
            InverseSquareSolver::<ToyCoupling>::new(
                SolverContext {
                    configuration: &Default::default(),
                    domain: &f64_domain(),
                    world: &world.snapshot(),
                    initial_step: fieldcad_core::StepContext {
                        tick: 0,
                        time_seconds: 0.0,
                        time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
                    },
                    cancellation: SolverCancellation::default(),
                },
                evaluator,
            ),
            Err(PluginError::InvalidConfiguration(_))
        ));
    }

    /// The correctness gap this crate exists to close: a misbehaving
    /// evaluator's wrong-length success must fail here, with solver
    /// context, for every coupling — not surface as a generic downstream
    /// `FieldBatch` length error in whichever plugin forgot the check.
    #[test]
    fn a_wrong_length_batch_is_rejected_with_solver_context() {
        let world = toy_world();
        let solver = toy_solver(
            Arc::new(WrongLengthEvaluator),
            &world.snapshot(),
            &f64_domain(),
        );

        match solver.sample(FIELD_CHANNEL_HANDLE, &probe_geometry()) {
            Err(PluginError::Solver(message)) => assert!(
                message.contains("returned 0 samples for a geometry of length 1"),
                "expected the length mismatch to be named, got: {message}"
            ),
            other => panic!("expected a solver error, got {other:?}"),
        }
    }

    #[test]
    fn diagnostics_report_the_couplings_identity_and_count() {
        let world = toy_world();
        let solver = toy_solver(
            Arc::new(CountingEvaluator {
                calls: AtomicUsize::new(0),
                precision: Precision::F64,
            }),
            &world.snapshot(),
            &f64_domain(),
        );

        let diagnostics = solver.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "toy-source-count");
        assert_eq!(
            diagnostics[0].message,
            "1 unit source(s), f64 batched evaluator, world revision 1"
        );
    }

    /// The gradient plumbing in `sample` is skeleton code, so this contract
    /// lives here once — driven by the real CPU evaluator, whose samples
    /// carry the closed-form Jacobians — rather than mirrored per plugin.
    /// The potential channel's gradient must be exactly minus the field the
    /// same samples produced: the skeleton maps the same buffers.
    #[test]
    fn the_field_and_potential_channels_publish_gradients_over_the_cpu_evaluator() {
        let world = toy_world();
        let solver = toy_solver(
            Arc::new(CpuInverseSquareEvaluator),
            &world.snapshot(),
            &f64_domain(),
        );
        let geometry = probe_geometry();

        let field_column = solver.sample(FIELD_CHANNEL_HANDLE, &geometry).unwrap();
        let FieldColumn::Vector(fields) = field_column.values else {
            panic!("expected a vector field column");
        };
        match field_column.gradient {
            Some(GradientColumn::Vector(jacobians)) => assert_eq!(jacobians.len(), geometry.len()),
            other => panic!("expected a Jacobian per sample, got {other:?}"),
        }

        let potential_column = solver.sample(POTENTIAL_CHANNEL_HANDLE, &geometry).unwrap();
        let Some(GradientColumn::Scalar(gradients)) = potential_column.gradient else {
            panic!("expected the potential channel to publish a gradient");
        };
        assert_eq!(fields.len(), gradients.len());
        for (field, gradient) in fields.iter().zip(gradients.iter()) {
            assert_eq!(*gradient, -*field);
        }
    }

    /// Pins the Phase 6 memoization itself: both channels of one geometry,
    /// and successive reads of an unchanged world, must hand out the exact
    /// same `validity`/values `Arc` — not equal contents, the same
    /// allocation — or a read is doing an O(samples) derivation it doesn't
    /// need to.
    #[test]
    fn repeated_reads_of_one_geometry_share_the_same_validity_and_value_buffers() {
        let world = toy_world();
        let solver = toy_solver(
            Arc::new(CpuInverseSquareEvaluator),
            &world.snapshot(),
            &f64_domain(),
        );
        let geometry = probe_geometry();

        let field_first = solver.sample(FIELD_CHANNEL_HANDLE, &geometry).unwrap();
        let field_second = solver.sample(FIELD_CHANNEL_HANDLE, &geometry).unwrap();
        assert!(
            Arc::ptr_eq(&field_first.validity, &field_second.validity),
            "two reads of an unchanged geometry must share one validity buffer"
        );
        let FieldColumn::Vector(first_values) = &field_first.values else {
            panic!("expected a vector field column");
        };
        let FieldColumn::Vector(second_values) = &field_second.values else {
            panic!("expected a vector field column");
        };
        assert!(
            Arc::ptr_eq(first_values, second_values),
            "two reads of an unchanged geometry must share one values buffer"
        );

        let potential = solver.sample(POTENTIAL_CHANNEL_HANDLE, &geometry).unwrap();
        assert!(
            Arc::ptr_eq(&field_first.validity, &potential.validity),
            "the field and potential channels of one geometry must share one validity buffer"
        );
    }
}
