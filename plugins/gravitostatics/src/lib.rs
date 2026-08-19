//! Analytic Newtonian gravity over the shared inverse-square superposition
//! kernel — see `fieldcad-superposition`'s module doc. Newton's law of
//! gravitation and Coulomb's law are the same functional form with a
//! different coupling constant and an opposite sign; this plugin is the
//! thin, gravity-specific adapter over that shared kernel, the shared
//! solver skeleton, and the shared plugin wrapper
//! (`fieldcad-superposition-solver`), mirroring `plugins/electrostatics`.

use fieldcad_core::quantities::MassKg;
use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, CoupledSource, Dimension, FieldValueKind, PluginId,
    PluginVersion, PropertyId, Quantity, WorldSnapshot,
};
use fieldcad_plugin_api::{ExportedVariable, PluginMetadata};
use fieldcad_sources::{collect_gravity_sources, mass_component_schemas};
use fieldcad_superposition::InverseSquareSample;
use fieldcad_superposition_solver::{InverseSquareCoupling, InverseSquarePlugin};
use glam::DVec3;

pub const PLUGIN_ID: &str = "fieldcad.gravity";

/// Newton's gravitational constant in m³·kg⁻¹·s⁻² (CODATA 2018).
pub const GRAVITATIONAL_CONSTANT: f64 = 6.674_30e-11;

pub const GRAVITATIONAL_ACCELERATION: &str = "gravitational-acceleration";
pub const GRAVITATIONAL_POTENTIAL: &str = "gravitational-potential";
/// The one generic `CoupledSource<T>` → `InverseSquareSource` mapping,
/// under this plugin's own name: a GPU evaluator builds its source buffer
/// from the same mapping the CPU reference uses.
pub use fieldcad_superposition_solver::coupled_inverse_square_source as inverse_square_source;
/// The solver skeleton owns the handle ordering; these aliases keep the
/// plugin's own names for the same values, so what `channels()` advertises
/// and what the skeleton's `sample` matches cannot drift apart.
pub use fieldcad_superposition_solver::{
    FIELD_CHANNEL_HANDLE as GRAVITATIONAL_ACCELERATION_HANDLE,
    POTENTIAL_CHANNEL_HANDLE as GRAVITATIONAL_POTENTIAL_HANDLE,
};
const POTENTIAL_DIMENSION: Dimension = Dimension::new(0, 2, -2, 0, 0, 0, 0);

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}
pub fn gravitational_acceleration_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), GRAVITATIONAL_ACCELERATION).expect("static channel ID is valid")
}
pub fn gravitational_potential_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), GRAVITATIONAL_POTENTIAL).expect("static channel ID is valid")
}

/// Evaluate the superposed gravitational field and potential at a single
/// position from all given mass sources.
pub fn evaluate_sources(sources: &[CoupledSource<MassKg>], position: DVec3) -> InverseSquareSample {
    fieldcad_superposition::evaluate_sources(
        -GRAVITATIONAL_CONSTANT,
        sources.iter().map(inverse_square_source),
        position,
    )
}

/// Analytic, static Newtonian gravity over a pluggable batched evaluator —
/// the shared plugin wrapper, parameterized by this crate's coupling.
pub type NewtonianGravityPlugin = InverseSquarePlugin<GravityCoupling>;

/// Gravity as an [`InverseSquareCoupling`]: everything Newtonian gravity
/// differs from electrostatics by, and nothing else — the solver and the
/// plugin wrapper are shared, in `fieldcad-superposition-solver`.
pub struct GravityCoupling;

impl InverseSquareCoupling for GravityCoupling {
    type Strength = MassKg;
    const COUPLING_SIGN: f64 = -1.0;
    const SYSTEM_LABEL: &str = "gravity";
    const NON_FINITE_MESSAGE: &str = "gravitational acceleration overflowed to a non-finite value";
    const DIAGNOSTIC_CODE: &str = "newtonian-gravity-source-count";
    const SOURCE_NOUN: &str = "mass";

    fn metadata() -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 1, 0),
            display_name: "Newtonian gravity".to_owned(),
            description: "Analytic Newtonian gravitational field and potential with superposition"
                .to_owned(),
        }
    }

    fn coupling_constant() -> ExportedVariable {
        ExportedVariable {
            property: PropertyId::new("G").expect("static property id is valid"),
            display_name: "Gravitational constant".to_owned(),
            description: Some(
                "Newton's gravitational constant, CODATA 2018 (m^3 kg^-1 s^-2)".to_owned(),
            ),
            default_value: Quantity::new(
                GRAVITATIONAL_CONSTANT,
                Dimension::new(-1, 3, -2, 0, 0, 0, 0),
            )
            .expect("static dimension is valid"),
        }
    }

    fn channels() -> Vec<ChannelSchema> {
        vec![
            ChannelSchema {
                id: gravitational_acceleration_channel_id(),
                display_name: "Gravitational acceleration g".to_owned(),
                value_kind: FieldValueKind::Vector(Dimension::ACCELERATION),
            },
            ChannelSchema {
                id: gravitational_potential_channel_id(),
                display_name: "Gravitational potential Φ".to_owned(),
                value_kind: FieldValueKind::Scalar(POTENTIAL_DIMENSION),
            },
        ]
    }

    fn component_schemas() -> Vec<ComponentSchema> {
        mass_component_schemas()
    }

    fn collect_sources(world: &WorldSnapshot) -> Result<Vec<CoupledSource<MassKg>>, String> {
        collect_gravity_sources(world).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::quantities::{SiScalar, kilogram};
    use fieldcad_core::{
        Domain, FieldColumn, ObjectShape, ObjectSpec, ProbeId, SampleGeometry, StepContext,
        TimeStep, Transform, World, WorldCommand,
    };
    use fieldcad_plugin_api::{
        DynamicBody, EquationSystemPlugin, EquationSystemSolver, SolverContext,
    };
    use fieldcad_sources::{
        gravitational_mass_component_id, independent_gravitational_mass_properties,
        inertial_mass_component_id, inertial_mass_properties, linked_gravitational_mass_properties,
    };

    fn solver() -> Box<dyn EquationSystemSolver> {
        let plugin = NewtonianGravityPlugin::new();
        let mut world = World::new();
        world
            .commit(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema),
            )
            .unwrap();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::default())
                    .with_shape(ObjectShape::point(0.01).unwrap())
                    .with_component(
                        inertial_mass_component_id(),
                        inertial_mass_properties(MassKg::new::<kilogram>(2.0e10)).unwrap(),
                    )
                    .with_component(
                        gravitational_mass_component_id(),
                        linked_gravitational_mass_properties(),
                    ),
            )])
            .unwrap();
        let domain = Domain::centred_cube(4.0, 8).unwrap();
        plugin
            .create_solver(SolverContext {
                configuration: &Default::default(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(1.0).unwrap(),
                },
                cancellation: Default::default(),
            })
            .unwrap()
    }

    #[test]
    fn publishes_acceleration_and_potential_channels() {
        let solver = solver();
        let geometry = SampleGeometry::probes(vec![ProbeId::new(0)], vec![DVec3::X]).unwrap();
        let acceleration = solver
            .sample(GRAVITATIONAL_ACCELERATION_HANDLE, &geometry)
            .unwrap();
        let potential = solver
            .sample(GRAVITATIONAL_POTENTIAL_HANDLE, &geometry)
            .unwrap();
        let FieldColumn::Vector(acceleration_values) = acceleration.values else {
            panic!("expected vector field");
        };
        let FieldColumn::Scalar(potential_values) = potential.values else {
            panic!("expected scalar field");
        };
        assert!(acceleration_values[0].x < 0.0);
        assert!(potential_values[0] < 0.0);
    }

    /// PH-2 regression: a body grazing one source's exclusion radius must not
    /// lose gravity from every *other* source too. Two-body pull plus a
    /// small third body grazing the sample point — before the fix,
    /// `evaluate_sources` returned a whole-sample `Undefined` on the first
    /// source in range without visiting the primary, and `forces()` mapped
    /// that to zero.
    #[test]
    fn a_body_grazing_one_sources_exclusion_radius_still_feels_the_others() {
        let plugin = NewtonianGravityPlugin::new();
        let mut world = World::new();
        world
            .commit(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema),
            )
            .unwrap();

        let primary = ObjectSpec::new("primary")
            .with_transform(Transform::at_finite(DVec3::new(-10.0, 0.0, 0.0)))
            .with_shape(ObjectShape::point(0.01).unwrap())
            .with_component(
                inertial_mass_component_id(),
                inertial_mass_properties(MassKg::new::<kilogram>(1.0e12)).unwrap(),
            )
            .with_component(
                gravitational_mass_component_id(),
                linked_gravitational_mass_properties(),
            );
        // Small and irrelevant except for its exclusion radius, which the
        // sample point (the origin) sits well inside.
        let grazing = ObjectSpec::new("grazing")
            .with_transform(Transform::at_finite(DVec3::new(1.0, 0.0, 0.0)))
            .with_shape(ObjectShape::point(2.0).unwrap())
            .with_component(
                inertial_mass_component_id(),
                inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
            )
            .with_component(
                gravitational_mass_component_id(),
                linked_gravitational_mass_properties(),
            );
        let body = ObjectSpec::new("body")
            .with_transform(Transform::default())
            .with_shape(ObjectShape::point(0.01).unwrap())
            .with_component(
                inertial_mass_component_id(),
                inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
            )
            .with_component(
                gravitational_mass_component_id(),
                linked_gravitational_mass_properties(),
            );

        let report = world
            .commit([
                WorldCommand::CreateObject(primary),
                WorldCommand::CreateObject(grazing),
                WorldCommand::CreateObject(body),
            ])
            .unwrap();
        let body_id = report.created_objects[2];

        let domain = Domain::centred_cube(40.0, 8).unwrap();
        let solver = plugin
            .create_solver(SolverContext {
                configuration: &Default::default(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(1.0).unwrap(),
                },
                cancellation: Default::default(),
            })
            .unwrap();

        let mut forces = [DVec3::ZERO];
        solver
            .add_forces(
                &[DynamicBody {
                    object: body_id,
                    inertial_mass_kg: MassKg::new::<kilogram>(1.0),
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                }],
                &mut forces,
            )
            .unwrap();

        assert!(
            forces[0].x < 0.0,
            "a body 1m inside a small grazing source's exclusion radius must \
             still feel the distant primary's pull; got {:?}",
            forces[0]
        );
    }

    /// Phase 2 parity: `add_forces` over the solver's precomputed,
    /// index-aligned buffer must equal the manual superposition it
    /// replaced — map every collected source, exclude the body's own by
    /// object id, sum via the kernel — bit-for-bit, over exterior points
    /// and a sphere interior alike.
    #[test]
    fn add_forces_matches_manual_superposition_bit_for_bit() {
        let plugin = NewtonianGravityPlugin::new();
        let mut world = World::new();
        world
            .commit(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema),
            )
            .unwrap();
        world
            .commit([
                WorldCommand::CreateObject(
                    ObjectSpec::new("heavy")
                        .with_transform(Transform::at_finite(DVec3::new(-2.0, 0.0, 0.0)))
                        .with_shape(ObjectShape::point(0.01).unwrap())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(4.0e12)).unwrap(),
                        )
                        .with_component(
                            gravitational_mass_component_id(),
                            linked_gravitational_mass_properties(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("sphere")
                        .with_transform(Transform::at_finite(DVec3::new(3.0, 1.0, 0.0)))
                        .with_shape(ObjectShape::sphere(2.0).unwrap())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(6.0e12)).unwrap(),
                        )
                        .with_component(
                            gravitational_mass_component_id(),
                            linked_gravitational_mass_properties(),
                        ),
                ),
                // Inside the sphere's radius, so its interior formula is
                // on the parity path too.
                WorldCommand::CreateObject(
                    ObjectSpec::new("light")
                        .with_transform(Transform::at_finite(DVec3::new(1.5, 0.5, 0.0)))
                        .with_shape(ObjectShape::point(0.01).unwrap())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(2.0e10)).unwrap(),
                        )
                        .with_component(
                            gravitational_mass_component_id(),
                            linked_gravitational_mass_properties(),
                        ),
                ),
            ])
            .unwrap();

        let snapshot = world.snapshot();
        let domain = Domain::centred_cube(8.0, 4).unwrap();
        let solver = plugin
            .create_solver(SolverContext {
                configuration: &Default::default(),
                domain: &domain,
                world: &snapshot,
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(1.0).unwrap(),
                },
                cancellation: Default::default(),
            })
            .unwrap();

        let collected = fieldcad_sources::collect_gravity_sources(&snapshot).unwrap();
        let bodies: Vec<_> = collected
            .iter()
            .map(|source| DynamicBody {
                object: source.object,
                inertial_mass_kg: source.coupling_value,
                position: source.position,
                velocity: Default::default(),
            })
            .collect();
        let mut forces = vec![DVec3::ZERO; bodies.len()];
        solver.add_forces(&bodies, &mut forces).unwrap();

        let inverse_square_sources: Vec<_> = collected.iter().map(inverse_square_source).collect();
        for (body, force) in bodies.iter().zip(&forces) {
            let mass = collected
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
                -GRAVITATIONAL_CONSTANT,
                &inverse_square_sources,
                excluded,
                body.position,
            )
            .unwrap();
            assert_eq!(
                *force,
                expected_field * mass,
                "force on {:?} diverged from manual superposition",
                body.object
            );
        }
    }

    /// A zero-gravitational-mass object is collected (its mass component is
    /// valid) but must be inert in both directions: filtered from the
    /// solver's source indexes, so it exerts nothing (identical forces in a
    /// world without it) and feels nothing (zero force of its own).
    #[test]
    fn a_zero_gravitational_mass_object_neither_exerts_nor_feels_gravity() {
        fn object(name: &str, inertial_kg: f64, gravitational_kg: Option<f64>) -> ObjectSpec {
            let spec = ObjectSpec::new(name)
                .with_shape(ObjectShape::point(0.01).unwrap())
                .with_component(
                    inertial_mass_component_id(),
                    inertial_mass_properties(MassKg::new::<kilogram>(inertial_kg)).unwrap(),
                );
            match gravitational_kg {
                Some(kg) => spec.with_component(
                    gravitational_mass_component_id(),
                    independent_gravitational_mass_properties(MassKg::new::<kilogram>(kg)).unwrap(),
                ),
                None => spec.with_component(
                    gravitational_mass_component_id(),
                    linked_gravitational_mass_properties(),
                ),
            }
        }

        fn solver_for(world: &World) -> Box<dyn EquationSystemSolver> {
            let domain = Domain::centred_cube(8.0, 4).unwrap();
            NewtonianGravityPlugin::new()
                .create_solver(SolverContext {
                    configuration: &Default::default(),
                    domain: &domain,
                    world: &world.snapshot(),
                    initial_step: StepContext {
                        tick: 0,
                        time_seconds: 0.0,
                        time_step: TimeStep::from_seconds(1.0).unwrap(),
                    },
                    cancellation: Default::default(),
                })
                .unwrap()
        }

        fn force_on_body(solver: &dyn EquationSystemSolver, body: DynamicBody) -> DVec3 {
            let mut out = [DVec3::ZERO];
            solver.add_forces(&[body], &mut out).unwrap();
            out[0]
        }

        let register = mass_component_schemas()
            .into_iter()
            .map(WorldCommand::RegisterComponentSchema);

        let mut with_zero = World::new();
        let report = with_zero
            .commit(
                register.clone().chain([
                    WorldCommand::CreateObject(
                        object("primary", 1.0e12, None)
                            .with_transform(Transform::at_finite(DVec3::new(-3.0, 0.0, 0.0))),
                    ),
                    WorldCommand::CreateObject(
                        object("zero", 1.0, Some(0.0))
                            .with_transform(Transform::at_finite(DVec3::new(0.5, 0.0, 0.0))),
                    ),
                    WorldCommand::CreateObject(
                        object("body", 1.0, None)
                            .with_transform(Transform::at_finite(DVec3::new(1.0, 0.0, 0.0))),
                    ),
                ]),
            )
            .unwrap();
        let zero_id = report.created_objects[1];
        let body_id = report.created_objects[2];

        let mut without_zero = World::new();
        let without_zero_report = without_zero
            .commit(
                register.chain([
                    WorldCommand::CreateObject(
                        object("primary", 1.0e12, None)
                            .with_transform(Transform::at_finite(DVec3::new(-3.0, 0.0, 0.0))),
                    ),
                    WorldCommand::CreateObject(
                        object("body", 1.0, None)
                            .with_transform(Transform::at_finite(DVec3::new(1.0, 0.0, 0.0))),
                    ),
                ]),
            )
            .unwrap();
        let without_zero_body_id = without_zero_report.created_objects[1];

        let body = |id, position| DynamicBody {
            object: id,
            inertial_mass_kg: MassKg::new::<kilogram>(1.0),
            position,
            velocity: Default::default(),
        };
        let with_zero_solver = solver_for(&with_zero);
        let without_zero_solver = solver_for(&without_zero);

        assert_eq!(
            force_on_body(
                with_zero_solver.as_ref(),
                body(body_id, DVec3::new(1.0, 0.0, 0.0))
            ),
            force_on_body(
                without_zero_solver.as_ref(),
                body(without_zero_body_id, DVec3::new(1.0, 0.0, 0.0))
            ),
            "the zero-mass object must not exert gravity"
        );
        assert_eq!(
            force_on_body(
                with_zero_solver.as_ref(),
                body(zero_id, DVec3::new(0.5, 0.0, 0.0))
            ),
            DVec3::ZERO,
            "the zero-mass object must not feel gravity"
        );
    }
}
