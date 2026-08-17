//! Analytic electrostatics for point sources and uniformly charged spheres.
//!
//! This is the first physical equation-system plugin. The CPU `f64` evaluator
//! is deliberately small and explicit: it is the correctness oracle for every
//! later parallel or GPU implementation.

use std::sync::Arc;

use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    ChannelSchema, ComponentSchema, DiagnosticSeverity, Domain, FieldColumn, GradientColumn,
    ObjectId, ObjectIndex, PluginId, PluginVersion, SampleGeometry, SolverDiagnostic,
    WorldSnapshot,
};
pub use fieldcad_electromagnetic_sources::{
    ChargeSource, charge_component_id, charge_properties, charge_property_id,
    collect_charge_sources as collect_sources,
};
use fieldcad_electromagnetic_sources::{
    charge_component_schema, electric_field_channel_schema, electric_potential_channel_schema,
};
use fieldcad_plugin_api::{
    ChannelHandle, DynamicBody, EquationSystemPlugin, EquationSystemSolver, PluginError,
    PluginMetadata, SampleCache, SampledColumn, SolverContext, SolverKind,
};
use fieldcad_superposition::{
    CpuInverseSquareEvaluator, InverseSquareBatchEvaluator, InverseSquareSample,
    InverseSquareSource,
};
use glam::DVec3;

#[cfg(test)]
use fieldcad_core::{Precision, PropertyBag, SampleValidity};

pub const PLUGIN_ID: &str = "fieldcad.electrostatics";

/// Coulomb constant in N·m²/C² (CODATA conventional value used by the oracle).
pub const COULOMB_CONSTANT: f64 = 8.987_551_792_3e9;

pub const ELECTRIC_FIELD_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const ELECTRIC_POTENTIAL_HANDLE: ChannelHandle = ChannelHandle::new(1);

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}

/// The electric field this system computes is *the* electric field, not this
/// plugin's own. Re-exported so callers need not know which module owns the
/// name, and so a future third model of the same field is a drop-in.
pub use fieldcad_electromagnetic_sources::{
    electric_field_channel_id, electric_potential_channel_id,
};

/// `ChargeSource` → the shared, coupling-value-agnostic source shape
/// `fieldcad-superposition`'s kernel (and any GPU evaluator built over it)
/// actually operates on. Public so a GPU evaluator can build its own source
/// buffer from the same mapping this crate's CPU reference uses, rather than
/// duplicating it.
pub fn inverse_square_source(source: &ChargeSource) -> InverseSquareSource {
    InverseSquareSource {
        position: source.position,
        strength: source.coupling_value.into_si(),
        distribution: source.distribution,
    }
}

/// Evaluate the superposed electrostatic field and potential in SI units.
pub fn evaluate_sources(sources: &[ChargeSource], position: DVec3) -> InverseSquareSample {
    fieldcad_superposition::evaluate_sources(
        COULOMB_CONSTANT,
        sources.iter().map(inverse_square_source),
        position,
    )
}

/// Analytic electrostatics over a pluggable batched evaluator.
pub struct ElectrostaticsPlugin {
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
}

impl Default for ElectrostaticsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ElectrostaticsPlugin {
    /// Backed by the reference `f64` evaluator.
    pub fn new() -> Self {
        Self {
            evaluator: Arc::new(CpuInverseSquareEvaluator),
        }
    }

    /// Backed by a host-owned evaluator, typically a `wgpu` compute backend.
    pub fn with_evaluator(evaluator: Arc<dyn InverseSquareBatchEvaluator>) -> Self {
        Self { evaluator }
    }
}

impl EquationSystemPlugin for ElectrostaticsPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 1, 0),
            display_name: "Electrostatics".to_owned(),
            description: "Analytic Coulomb field and potential with superposition".to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        vec![
            electric_field_channel_schema(),
            electric_potential_channel_schema(),
        ]
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        vec![charge_component_schema()]
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        // A snapshot's precision metadata must describe the numbers it actually
        // carries, or an `f32` interactive result is indistinguishable from the
        // `f64` oracle it is checked against.
        if context.domain.precision() != self.evaluator.precision() {
            return Err(PluginError::InvalidConfiguration(format!(
                "electrostatics evaluator produces {}, but the domain declares {}",
                self.evaluator.precision().label(),
                context.domain.precision().label()
            )));
        }
        let sources = ObjectIndex::new(
            collect_sources(context.world)
                .map_err(|error| PluginError::UnsupportedWorld(error.to_string()))?,
        );
        let inverse_square_sources = sources
            .as_slice()
            .iter()
            .map(inverse_square_source)
            .collect();
        Ok(Box::new(ElectrostaticsSolver {
            domain: *context.domain,
            sources,
            inverse_square_sources,
            world_revision: context.world.revision(),
            evaluator: Arc::clone(&self.evaluator),
            cache: SampleCache::new(SAMPLE_CACHE_CAPACITY),
        }))
    }
}

/// A subscription currently has probes plus any number of planes and one
/// grid. Bounds stale entries left by density changes without complicating
/// the plugin contract with publication lifecycle callbacks.
const SAMPLE_CACHE_CAPACITY: usize = 16;

struct ElectrostaticsSolver {
    domain: Domain,
    sources: ObjectIndex<ChargeSource>,
    /// Rebuilt with the object-indexed sources on creation/world changes;
    /// this is the cache-local input shape the shared evaluator expects,
    /// converted once per world change rather than on every channel read.
    inverse_square_sources: Vec<InverseSquareSource>,
    world_revision: fieldcad_core::WorldRevision,
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    /// Runtime publication asks for E and V separately. Retain the small set of
    /// geometries from this publication so both channels share one evaluation.
    cache: SampleCache<InverseSquareSample>,
}

impl EquationSystemSolver for ElectrostaticsSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        collect_sources(world)
            .map(|_| ())
            .map_err(|error| PluginError::UnsupportedWorld(error.to_string()))
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.sources = ObjectIndex::new(
            collect_sources(world)
                .map_err(|error| PluginError::UnsupportedWorld(error.to_string()))?,
        );
        self.inverse_square_sources = self
            .sources
            .as_slice()
            .iter()
            .map(inverse_square_source)
            .collect();
        self.world_revision = world.revision();
        self.cache.clear()
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let samples = self.samples_for(geometry)?;
        let validity = samples.iter().map(|sample| sample.validity).collect();
        // A gradient is published only if *every* sample in the batch
        // reported one — the rest of the pipeline treats "does this batch
        // carry a gradient" as one per-batch decision, not a per-point one.
        let gradients = samples
            .iter()
            .map(|sample| sample.gradient)
            .collect::<Option<Vec<_>>>();
        match channel {
            ELECTRIC_FIELD_HANDLE => {
                let column = SampledColumn::new(
                    FieldColumn::vectors(samples.iter().map(|sample| sample.field).collect()),
                    validity,
                );
                Ok(match gradients {
                    Some(jacobians) => {
                        column.with_gradient(GradientColumn::Vector(jacobians.into()))
                    }
                    None => column,
                })
            }
            ELECTRIC_POTENTIAL_HANDLE => {
                let column = SampledColumn::new(
                    FieldColumn::scalars(samples.iter().map(|sample| sample.potential).collect()),
                    validity,
                );
                // ∇φ = −E: the potential's gradient is exactly minus the
                // field this solver already computed, so no separate math is
                // needed — only whether `gradients.is_some()` still gates it,
                // to keep both channels' gradient availability consistent
                // for the same evaluator.
                Ok(match gradients {
                    Some(_) => column.with_gradient(GradientColumn::Scalar(
                        samples.iter().map(|sample| -sample.field).collect(),
                    )),
                    None => column,
                })
            }
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    /// Add the Coulomb force on each dynamic body into `out`: `F = qE`.
    ///
    /// Note the field, not the potential. `qφ` is the potential *energy* in
    /// joules; the force is the charge times the field, which is minus the
    /// gradient of that potential. The evaluator already returns `E`, so this is
    /// the one multiplication.
    ///
    /// A body's own charge is excluded from the field acting on it — a point
    /// charge does not accelerate itself, and its own Coulomb singularity would
    /// dominate everything else if it were included.
    fn add_forces(&self, bodies: &[DynamicBody], out: &mut [DVec3]) -> Result<(), PluginError> {
        for (body, out_force) in bodies.iter().zip(out) {
            let charge = self
                .sources
                .get(body.object)
                .map_or(0.0, |source| source.coupling_value.into_si());
            if charge == 0.0 {
                // Uncharged: this field does not act on it at all.
                continue;
            }
            let field = self.field_excluding(body.object, body.position)?;
            *out_force += field * charge;
        }
        Ok(())
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: plugin_id(),
            severity: DiagnosticSeverity::Info,
            code: "electrostatic-source-count".to_owned(),
            message: format!(
                "{} charge source(s), {} batched evaluator, world revision {}",
                self.sources.len(),
                self.evaluator.precision().label(),
                self.world_revision
            ),
        }]
    }
}

impl ElectrostaticsSolver {
    /// The electric field at `position` from every source except `object`.
    ///
    /// Evaluated directly rather than through the batched evaluator, because the
    /// source list differs per body and the evaluator's contract is one field
    /// for one geometry from *all* sources.
    fn field_excluding(&self, object: ObjectId, position: DVec3) -> Result<DVec3, PluginError> {
        fieldcad_superposition::field_excluding(
            COULOMB_CONSTANT,
            self.sources
                .iter_excluding(object)
                .map(inverse_square_source),
            position,
        )
        .ok_or_else(|| {
            PluginError::Solver(
                "electrostatic force evaluation produced a non-finite field".to_owned(),
            )
        })
    }

    fn samples_for(
        &self,
        geometry: &SampleGeometry,
    ) -> Result<Arc<[InverseSquareSample]>, PluginError> {
        self.cache.get_or_try_insert_with(
            geometry,
            || {
                let evaluated = self
                    .evaluator
                    .evaluate(
                        COULOMB_CONSTANT,
                        &self.inverse_square_sources,
                        &self.domain,
                        geometry,
                    )
                    .map_err(PluginError::Solver)?;
                if evaluated.len() != geometry.len() {
                    return Err(PluginError::Solver(format!(
                        "batched evaluator returned {} samples for a geometry of length {}",
                        evaluated.len(),
                        geometry.len()
                    )));
                }
                Ok(evaluated)
            },
            |out| {
                self.evaluator
                    .evaluate_into(
                        COULOMB_CONSTANT,
                        &self.inverse_square_sources,
                        &self.domain,
                        geometry,
                        out,
                    )
                    .map_err(PluginError::Solver)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fieldcad_core::quantities::{ChargeCoulombs, MassKg, SiScalar, coulomb, kilogram};
    use fieldcad_core::{
        BoundaryConditions, ChargeDistribution, DomainBounds, ObjectId, ObjectShape, ObjectSpec,
        PlaneLattice, Resolution, Transform, UndefinedReason, Velocity, World, WorldCommand,
    };
    use glam::UVec2;

    use super::*;

    fn relative_eq(actual: f64, expected: f64, relative_tolerance: f64) {
        let scale = expected.abs().max(1.0);
        assert!(
            (actual - expected).abs() <= relative_tolerance * scale,
            "expected {expected:e}, received {actual:e}"
        );
    }

    fn point(position: DVec3, charge_coulombs: f64, radius: f64) -> ChargeSource {
        ChargeSource::new(
            ObjectId::new(0),
            position,
            Velocity::default(),
            ChargeCoulombs::new::<coulomb>(charge_coulombs),
            ChargeDistribution::Point {
                exclusion_radius: radius,
            },
        )
    }

    #[test]
    fn a_positive_point_charge_matches_coulombs_law() {
        let sample = evaluate_sources(&[point(DVec3::ZERO, 2.0e-9, 0.01)], DVec3::X);

        relative_eq(sample.field.x, COULOMB_CONSTANT * 2.0e-9, 1.0e-14);
        relative_eq(sample.potential, COULOMB_CONSTANT * 2.0e-9, 1.0e-14);
        assert_eq!(sample.field.y, 0.0);
        assert_eq!(sample.validity, SampleValidity::Exact);
    }

    #[test]
    fn field_direction_and_potential_follow_charge_sign() {
        let positive = evaluate_sources(&[point(DVec3::ZERO, 1.0e-9, 0.0)], DVec3::X);
        let negative = evaluate_sources(&[point(DVec3::ZERO, -1.0e-9, 0.0)], DVec3::X);

        assert!(positive.field.x > 0.0);
        assert!(positive.potential > 0.0);
        assert!(negative.field.x < 0.0);
        assert!(negative.potential < 0.0);
    }

    #[test]
    fn point_field_has_inverse_square_falloff() {
        let source = point(DVec3::ZERO, 1.0e-9, 0.0);
        let near = evaluate_sources(&[source], DVec3::X).field.length();
        let far = evaluate_sources(&[source], DVec3::X * 2.0).field.length();

        relative_eq(far / near, 0.25, 1.0e-14);
    }

    #[test]
    fn superposition_cancels_symmetric_fields_and_adds_potential() {
        let charge = 1.0e-9;
        let sample = evaluate_sources(
            &[point(-DVec3::X, charge, 0.0), point(DVec3::X, charge, 0.0)],
            DVec3::ZERO,
        );

        assert_eq!(sample.field, DVec3::ZERO);
        relative_eq(sample.potential, 2.0 * COULOMB_CONSTANT * charge, 1.0e-14);
    }

    #[test]
    fn point_source_exclusion_is_explicit() {
        let sample = evaluate_sources(&[point(DVec3::ZERO, 1.0, 0.1)], DVec3::X * 0.05);

        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        );
        assert_eq!(sample.field, DVec3::ZERO);
        assert_eq!(sample.potential, 0.0);
    }

    #[test]
    fn uniformly_charged_sphere_is_finite_and_continuous_at_its_surface() {
        let charge_q = ChargeCoulombs::new::<coulomb>(2.0e-9);
        let charge = charge_q.into_si();
        let radius = 0.5;
        let source = ChargeSource::new(
            ObjectId::new(0),
            DVec3::ZERO,
            Velocity::default(),
            charge_q,
            ChargeDistribution::UniformSphere { radius },
        );
        let centre = evaluate_sources(&[source], DVec3::ZERO);
        let surface = evaluate_sources(&[source], DVec3::X * radius);

        assert_eq!(centre.field, DVec3::ZERO);
        relative_eq(
            centre.potential,
            1.5 * COULOMB_CONSTANT * charge / radius,
            1.0e-14,
        );
        relative_eq(
            surface.field.x,
            COULOMB_CONSTANT * charge / radius.powi(2),
            1.0e-14,
        );
        relative_eq(
            surface.potential,
            COULOMB_CONSTANT * charge / radius,
            1.0e-14,
        );
    }

    /// Build a world holding one charged object with the given shape, and try
    /// to create a solver against it.
    fn solver_for_charged_shape(
        shape: Option<ObjectShape>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        let mut world = World::new();
        let mut spec = ObjectSpec::new("charged")
            .with_transform(Transform::at_finite(DVec3::ZERO))
            .with_component(
                charge_component_id(),
                charge_properties(ChargeCoulombs::new::<coulomb>(1.0)).unwrap(),
            );
        if let Some(shape) = shape {
            spec = spec.with_shape(shape);
        }
        world
            .commit([
                WorldCommand::RegisterComponentSchema(
                    ElectrostaticsPlugin::new().component_schemas().remove(0),
                ),
                WorldCommand::CreateObject(spec),
            ])
            .unwrap();
        let domain = Domain::centred_cube(2.0, 8).unwrap();

        ElectrostaticsPlugin::new().create_solver(SolverContext {
            configuration: &PropertyBag::default(),
            domain: &domain,
            world: &world.snapshot(),
            initial_step: fieldcad_core::StepContext {
                tick: 0,
                time_seconds: 0.0,
                time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
            },
            cancellation: fieldcad_plugin_api::SolverCancellation::default(),
        })
    }

    #[test]
    fn a_charged_object_with_no_shape_is_a_point_charge() {
        // Composing an object means creating it bare and attaching charge
        // afterwards. That intermediate object has no shape and must still be
        // solvable, or the authoring flow cannot reach a valid world.
        assert!(solver_for_charged_shape(None).is_ok());
    }

    #[test]
    fn plugin_rejects_charged_objects_without_a_supported_shape() {
        // A box is genuinely unsupported: there is no closed-form field for a
        // uniformly charged cuboid in this solver.
        assert!(matches!(
            solver_for_charged_shape(Some(ObjectShape::boxed(DVec3::ONE).unwrap())),
            Err(PluginError::UnsupportedWorld(_))
        ));
    }

    /// PH-3 regression: `field_excluding` used to collapse a uniformly
    /// charged sphere's interior to the same "excluded, contributes
    /// nothing" treatment as a point source's exclusion radius, even
    /// though `evaluate_sources` has always had the correct finite
    /// interior formula right next to it — a body dragged inside a charged
    /// sphere felt exactly zero force from it. Fixed by sharing one
    /// implementation with `plugins/gravity`, which already got this right
    /// for gravity (PH-2/PH-19).
    #[test]
    fn a_body_inside_a_charged_sphere_feels_its_finite_interior_field() {
        let mut world = World::new();
        let schema = ElectrostaticsPlugin::new().component_schemas().remove(0);
        let report = world
            .commit([
                WorldCommand::RegisterComponentSchema(schema),
                WorldCommand::CreateObject(
                    ObjectSpec::new("sphere")
                        .with_transform(Transform::default())
                        .with_shape(ObjectShape::sphere(1.0).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(2.0e-9)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("probe charge")
                        .with_transform(Transform::at_finite(DVec3::X * 0.4))
                        .with_shape(ObjectShape::point(0.01).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        ),
                ),
            ])
            .unwrap();
        let probe_id = report.created_objects[1];

        let domain = Domain::centred_cube(4.0, 8).unwrap();
        let mut solver = ElectrostaticsPlugin::new()
            .create_solver(SolverContext {
                configuration: &PropertyBag::default(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: fieldcad_core::StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
                },
                cancellation: fieldcad_plugin_api::SolverCancellation::default(),
            })
            .unwrap();
        solver.on_world_changed(&world.snapshot()).unwrap();

        let mut forces = [DVec3::ZERO];
        solver
            .add_forces(
                &[DynamicBody {
                    object: probe_id,
                    inertial_mass_kg: MassKg::new::<kilogram>(1.0),
                    position: DVec3::X * 0.4,
                    velocity: DVec3::ZERO,
                }],
                &mut forces,
            )
            .unwrap();

        assert!(forces[0].x.is_finite());
        assert!(
            forces[0].x > 0.0,
            "a positive probe charge inside a positively charged sphere must \
             feel a real outward force from it, got {:?}",
            forces[0]
        );
    }

    struct CountingEvaluator {
        calls: AtomicUsize,
    }

    impl InverseSquareBatchEvaluator for CountingEvaluator {
        fn precision(&self) -> Precision {
            Precision::F32
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
                    validity: SampleValidity::Exact,
                };
                geometry.len()
            ])
        }
    }

    #[test]
    fn every_evaluator_backing_declares_the_same_contract() {
        // One plugin type means the schemas a host validates against cannot
        // depend on which evaluator was injected. Two plugin types previously
        // could, and silently did.
        let reference = ElectrostaticsPlugin::new();
        let accelerated = ElectrostaticsPlugin::with_evaluator(Arc::new(CountingEvaluator {
            calls: AtomicUsize::new(0),
        }));

        assert_eq!(reference.metadata(), accelerated.metadata());
        assert_eq!(reference.channels(), accelerated.channels());
        assert_eq!(
            reference.component_schemas(),
            accelerated.component_schemas()
        );
        assert_eq!(
            reference.configuration_schema(),
            accelerated.configuration_schema()
        );
        assert_eq!(
            reference.default_configuration(),
            accelerated.default_configuration()
        );
    }

    #[test]
    fn the_reference_evaluator_agrees_with_the_analytic_oracle() {
        let sources = [
            point(DVec3::ZERO, 1.5e-9, 0.05),
            point(DVec3::X, -0.8e-9, 0.05),
        ];
        let geometry = SampleGeometry::Plane {
            plane: fieldcad_core::PlaneId::new(0),
            lattice: PlaneLattice::new(
                DVec3::new(-1.0, -1.0, 0.0),
                DVec3::new(0.5, 0.0, 0.0),
                DVec3::new(0.0, 0.5, 0.0),
                UVec2::splat(4),
            ),
        };
        let domain = Domain::centred_cube(4.0, 8).unwrap();
        let inverse_square_sources: Vec<_> = sources.iter().map(inverse_square_source).collect();

        let batched = CpuInverseSquareEvaluator
            .evaluate(
                COULOMB_CONSTANT,
                &inverse_square_sources,
                &domain,
                &geometry,
            )
            .unwrap();

        assert_eq!(batched.len(), geometry.len());
        for (batched, position) in batched.iter().zip(geometry.positions()) {
            assert_eq!(*batched, evaluate_sources(&sources, position));
        }
    }

    #[test]
    fn accelerated_channels_share_one_batch_evaluation() {
        let evaluator = Arc::new(CountingEvaluator {
            calls: AtomicUsize::new(0),
        });
        let plugin = ElectrostaticsPlugin::with_evaluator(evaluator.clone());
        let world = World::new().snapshot();
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(4).unwrap(),
            BoundaryConditions::default(),
            Precision::F32,
        );
        let configuration = PropertyBag::default();
        let mut solver = plugin
            .create_solver(SolverContext {
                configuration: &configuration,
                domain: &domain,
                world: &world,
                initial_step: fieldcad_core::StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
                },
                cancellation: fieldcad_plugin_api::SolverCancellation::default(),
            })
            .unwrap();
        solver.on_world_changed(&world).unwrap();
        let geometry = SampleGeometry::Plane {
            plane: fieldcad_core::PlaneId::new(0),
            lattice: PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(3)),
        };

        solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();
        solver.sample(ELECTRIC_POTENTIAL_HANDLE, &geometry).unwrap();

        assert_eq!(evaluator.calls.load(Ordering::Relaxed), 1);
    }

    /// A plane offset from the origin, comfortably outside a default-radius
    /// point charge's exclusion zone, for tests that need every sample to
    /// come back `Exact` rather than `Undefined`.
    fn plane_away_from_the_origin() -> SampleGeometry {
        SampleGeometry::Plane {
            plane: fieldcad_core::PlaneId::new(0),
            lattice: PlaneLattice::new(
                DVec3::new(-1.0, -1.0, 0.5),
                DVec3::new(0.5, 0.0, 0.0),
                DVec3::new(0.0, 0.5, 0.0),
                UVec2::splat(3),
            ),
        }
    }

    #[test]
    fn the_electric_field_channel_publishes_its_jacobian() {
        let solver = solver_for_charged_shape(None).unwrap();
        let geometry = plane_away_from_the_origin();

        let column = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();

        match column.gradient {
            Some(GradientColumn::Vector(jacobians)) => assert_eq!(jacobians.len(), geometry.len()),
            other => panic!("expected a Jacobian per sample, got {other:?}"),
        }
    }

    #[test]
    fn the_potential_channel_publishes_minus_the_field_as_its_gradient() {
        let solver = solver_for_charged_shape(None).unwrap();
        let geometry = plane_away_from_the_origin();

        let field_column = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();
        let potential_column = solver.sample(ELECTRIC_POTENTIAL_HANDLE, &geometry).unwrap();

        let FieldColumn::Vector(fields) = field_column.values else {
            panic!("expected a vector field column");
        };
        let Some(GradientColumn::Scalar(gradients)) = potential_column.gradient else {
            panic!("expected the potential channel to publish a gradient");
        };

        assert_eq!(fields.len(), gradients.len());
        for (field, gradient) in fields.iter().zip(gradients.iter()) {
            assert!((*gradient - (-*field)).length() < 1.0e-12);
        }
    }

    #[test]
    fn accelerated_evaluator_precision_must_match_snapshot_metadata() {
        let evaluator = Arc::new(CountingEvaluator {
            calls: AtomicUsize::new(0),
        });
        let plugin = ElectrostaticsPlugin::with_evaluator(evaluator);
        let world = World::new().snapshot();
        let domain = Domain::centred_cube(2.0, 4).unwrap();
        let configuration = PropertyBag::default();

        assert!(matches!(
            plugin.create_solver(SolverContext {
                configuration: &configuration,
                domain: &domain,
                world: &world,
                initial_step: fieldcad_core::StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
                },
                cancellation: fieldcad_plugin_api::SolverCancellation::default(),
            }),
            Err(PluginError::InvalidConfiguration(_))
        ));
    }
}
