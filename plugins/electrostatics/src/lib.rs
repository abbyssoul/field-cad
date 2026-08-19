//! Analytic electrostatics for point sources and uniformly charged spheres.
//!
//! This is the first physical equation-system plugin. The CPU `f64` evaluator
//! is deliberately small and explicit: it is the correctness oracle for every
//! later parallel or GPU implementation. The solver and the plugin wrapper
//! are shared with gravity, in `fieldcad-superposition-solver`; this crate
//! owns the identity, schemas, constants, and the `ChargeSource` coupling.

use fieldcad_core::quantities::ChargeCoulombs;
use fieldcad_core::{
    ChannelSchema, ComponentSchema, CoupledSource, PluginId, PluginVersion, WorldSnapshot,
};
pub use fieldcad_electromagnetic_sources::{
    ChargeSource, charge_component_id, charge_properties, charge_property_id,
    collect_charge_sources as collect_sources,
};
use fieldcad_electromagnetic_sources::{
    charge_component_schema, electric_field_channel_schema, electric_potential_channel_schema,
};
use fieldcad_plugin_api::PluginMetadata;
use fieldcad_superposition::InverseSquareSample;
use fieldcad_superposition_solver::{InverseSquareCoupling, InverseSquarePlugin};
use glam::DVec3;

#[cfg(test)]
use fieldcad_core::{Domain, Precision, PropertyBag, SampleGeometry, SampleValidity};
#[cfg(test)]
use std::sync::Arc;

pub const PLUGIN_ID: &str = "fieldcad.electrostatics";

/// Coulomb constant in N·m²/C² (CODATA conventional value used by the oracle).
pub const COULOMB_CONSTANT: f64 = 8.987_551_792_3e9;

/// The solver skeleton owns the handle ordering; these aliases keep the
/// plugin's own names for the same values, so what `channels()` advertises
/// and what the skeleton's `sample` matches cannot drift apart.
pub use fieldcad_superposition_solver::{
    FIELD_CHANNEL_HANDLE as ELECTRIC_FIELD_HANDLE,
    POTENTIAL_CHANNEL_HANDLE as ELECTRIC_POTENTIAL_HANDLE,
};

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}

/// The electric field this system computes is *the* electric field, not this
/// plugin's own. Re-exported so callers need not know which module owns the
/// name, and so a future third model of the same field is a drop-in.
pub use fieldcad_electromagnetic_sources::{
    electric_field_channel_id, electric_potential_channel_id,
};

/// The one generic `CoupledSource<T>` → `InverseSquareSource` mapping,
/// under this plugin's own name: a GPU evaluator builds its source buffer
/// from the same mapping the CPU reference uses.
pub use fieldcad_superposition_solver::coupled_inverse_square_source as inverse_square_source;

/// Evaluate the superposed electrostatic field and potential in SI units.
pub fn evaluate_sources(sources: &[ChargeSource], position: DVec3) -> InverseSquareSample {
    fieldcad_superposition::evaluate_sources(
        COULOMB_CONSTANT,
        sources.iter().map(inverse_square_source),
        position,
    )
}

/// Analytic electrostatics over a pluggable batched evaluator — the shared
/// plugin wrapper, parameterized by this crate's coupling.
pub type ElectrostaticsPlugin = InverseSquarePlugin<ElectrostaticsCoupling>;

/// Electrostatics as an [`InverseSquareCoupling`]: everything Coulomb's law
/// differs from Newtonian gravity by, and nothing else — the solver and
/// the plugin wrapper are shared, in `fieldcad-superposition-solver`.
pub struct ElectrostaticsCoupling;

impl InverseSquareCoupling for ElectrostaticsCoupling {
    type Strength = ChargeCoulombs;
    const COUPLING_CONSTANT: f64 = COULOMB_CONSTANT;
    const SYSTEM_LABEL: &str = "electrostatics";
    const NON_FINITE_MESSAGE: &str = "electrostatic force evaluation produced a non-finite field";
    const DIAGNOSTIC_CODE: &str = "electrostatic-source-count";
    const SOURCE_NOUN: &str = "charge";

    fn metadata() -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 1, 0),
            display_name: "Electrostatics".to_owned(),
            description: "Analytic Coulomb field and potential with superposition".to_owned(),
        }
    }

    fn channels() -> Vec<ChannelSchema> {
        vec![
            electric_field_channel_schema(),
            electric_potential_channel_schema(),
        ]
    }

    fn component_schemas() -> Vec<ComponentSchema> {
        vec![charge_component_schema()]
    }

    fn collect_sources(
        world: &WorldSnapshot,
    ) -> Result<Vec<CoupledSource<ChargeCoulombs>>, String> {
        collect_sources(world).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fieldcad_core::quantities::{ChargeCoulombs, MassKg, SiScalar, coulomb, kilogram};
    use fieldcad_core::{
        ChargeDistribution, ObjectId, ObjectShape, ObjectSpec, PlaneLattice, Transform,
        UndefinedReason, Velocity, World, WorldCommand,
    };
    use fieldcad_plugin_api::{
        DynamicBody, EquationSystemPlugin, EquationSystemSolver, PluginError, SolverContext,
    };
    use fieldcad_superposition::{
        CpuInverseSquareEvaluator, InverseSquareBatchEvaluator, InverseSquareSource,
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

    /// PH-3 regression: `field_excluding_at` used to collapse a uniformly
    /// charged sphere's interior to the same "excluded, contributes
    /// nothing" treatment as a point source's exclusion radius, even
    /// though `evaluate_sources` has always had the correct finite
    /// interior formula right next to it — a body dragged inside a charged
    /// sphere felt exactly zero force from it. Fixed by sharing one
    /// implementation with `plugins/gravitostatics`, which already got this right
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

    /// Phase 2 parity: `add_forces` over the solver's precomputed,
    /// index-aligned buffer must equal the manual superposition it
    /// replaced — map every collected source, exclude the body's own by
    /// object id, sum via the kernel — bit-for-bit, over exterior points
    /// and a sphere interior alike.
    #[test]
    fn add_forces_matches_manual_superposition_bit_for_bit() {
        let plugin = ElectrostaticsPlugin::new();
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("positive")
                        .with_transform(Transform::at_finite(DVec3::new(-1.0, 0.0, 0.0)))
                        .with_shape(ObjectShape::point(0.01).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(2.0e-9)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("sphere")
                        .with_transform(Transform::at_finite(DVec3::new(1.0, 0.5, 0.0)))
                        .with_shape(ObjectShape::sphere(1.5).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(-3.0e-9)).unwrap(),
                        ),
                ),
                // Inside the sphere's radius, so its interior formula is on
                // the parity path too.
                WorldCommand::CreateObject(
                    ObjectSpec::new("probe")
                        .with_transform(Transform::at_finite(DVec3::new(1.2, 0.4, 0.0)))
                        .with_shape(ObjectShape::point(0.01).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        ),
                ),
            ])
            .unwrap();

        let snapshot = world.snapshot();
        let domain = Domain::centred_cube(8.0, 4).unwrap();
        let solver = plugin
            .create_solver(SolverContext {
                configuration: &PropertyBag::default(),
                domain: &domain,
                world: &snapshot,
                initial_step: fieldcad_core::StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
                },
                cancellation: fieldcad_plugin_api::SolverCancellation::default(),
            })
            .unwrap();

        let collected = collect_sources(&snapshot).unwrap();
        let bodies: Vec<_> = collected
            .iter()
            .map(|source| DynamicBody {
                object: source.object,
                inertial_mass_kg: MassKg::new::<kilogram>(1.0),
                position: source.position,
                velocity: Default::default(),
            })
            .collect();
        let mut forces = vec![DVec3::ZERO; bodies.len()];
        solver.add_forces(&bodies, &mut forces).unwrap();

        let inverse_square_sources: Vec<_> = collected.iter().map(inverse_square_source).collect();
        for (body, force) in bodies.iter().zip(&forces) {
            let charge = collected
                .iter()
                .find(|source| source.object == body.object)
                .unwrap()
                .coupling_value
                .into_si();
            let excluded = collected
                .iter()
                .position(|source| source.object == body.object)
                .unwrap();
            let expected_field = fieldcad_superposition::field_excluding_at(
                COULOMB_CONSTANT,
                &inverse_square_sources,
                excluded,
                body.position,
            )
            .unwrap();
            assert_eq!(
                *force,
                expected_field * charge,
                "force on {:?} diverged from manual superposition",
                body.object
            );
        }
    }

    /// A zero-charge object is collected (zero charge is a valid property)
    /// but must be inert in both directions: filtered from the solver's
    /// source indexes, so it exerts nothing (identical forces in a world
    /// without it) and feels nothing (zero force of its own).
    #[test]
    fn a_zero_charge_object_neither_exerts_nor_feels_a_force() {
        fn charged(name: &str, coulombs: f64, position: DVec3) -> ObjectSpec {
            ObjectSpec::new(name)
                .with_transform(Transform::at_finite(position))
                .with_shape(ObjectShape::point(0.01).unwrap())
                .with_component(
                    charge_component_id(),
                    charge_properties(ChargeCoulombs::new::<coulomb>(coulombs)).unwrap(),
                )
        }

        fn solver_for(world: &World) -> Box<dyn EquationSystemSolver> {
            let domain = Domain::centred_cube(8.0, 4).unwrap();
            ElectrostaticsPlugin::new()
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
                .unwrap()
        }

        fn force_on_body(solver: &dyn EquationSystemSolver, body: DynamicBody) -> DVec3 {
            let mut out = [DVec3::ZERO];
            solver.add_forces(&[body], &mut out).unwrap();
            out[0]
        }

        let body = |id, position| DynamicBody {
            object: id,
            inertial_mass_kg: MassKg::new::<kilogram>(1.0),
            position,
            velocity: Default::default(),
        };

        let mut with_zero = World::new();
        let with_zero_report = with_zero
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(charged("primary", 2.0e-9, DVec3::new(-3.0, 0.0, 0.0))),
                WorldCommand::CreateObject(charged("zero", 0.0, DVec3::new(0.5, 0.0, 0.0))),
                WorldCommand::CreateObject(charged("probe", 1.0e-9, DVec3::new(1.0, 0.0, 0.0))),
            ])
            .unwrap();
        let zero_id = with_zero_report.created_objects[1];
        let probe_id = with_zero_report.created_objects[2];

        let mut without_zero = World::new();
        let without_zero_report = without_zero
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(charged("primary", 2.0e-9, DVec3::new(-3.0, 0.0, 0.0))),
                WorldCommand::CreateObject(charged("probe", 1.0e-9, DVec3::new(1.0, 0.0, 0.0))),
            ])
            .unwrap();
        let without_zero_probe_id = without_zero_report.created_objects[1];

        let with_zero_solver = solver_for(&with_zero);
        let without_zero_solver = solver_for(&without_zero);

        assert_eq!(
            force_on_body(
                with_zero_solver.as_ref(),
                body(probe_id, DVec3::new(1.0, 0.0, 0.0))
            ),
            force_on_body(
                without_zero_solver.as_ref(),
                body(without_zero_probe_id, DVec3::new(1.0, 0.0, 0.0))
            ),
            "the zero-charge object must not exert a field"
        );
        assert_eq!(
            force_on_body(
                with_zero_solver.as_ref(),
                body(zero_id, DVec3::new(0.5, 0.0, 0.0))
            ),
            DVec3::ZERO,
            "the zero-charge object must not feel a force"
        );
    }
}
