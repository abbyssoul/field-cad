//! Analytic Newtonian gravity over the shared inverse-square superposition
//! kernel — see `fieldcad-superposition`'s module doc. Newton's law of
//! gravitation and Coulomb's law are the same functional form with a
//! different coupling constant and an opposite sign; this plugin is the
//! thin, gravity-specific adapter over that shared kernel, mirroring
//! `plugins/electrostatics`.

use std::sync::Arc;

use fieldcad_core::quantities::{MassKg, SiScalar};
use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, CoupledSource, DiagnosticSeverity, Dimension,
    Domain, FieldColumn, FieldValueKind, GradientColumn, ObjectIndex, PluginId, PluginVersion,
    SampleGeometry, SolverDiagnostic, WorldSnapshot,
};
use fieldcad_plugin_api::{
    ChannelHandle, DynamicBody, EquationSystemPlugin, EquationSystemSolver, PluginError,
    PluginMetadata, SampleCache, SampledColumn, SolverContext, SolverKind,
};
use fieldcad_sources::{collect_gravity_sources, mass_component_schemas};
use fieldcad_superposition::{
    CpuInverseSquareEvaluator, InverseSquareBatchEvaluator, InverseSquareSample,
    InverseSquareSource,
};
use glam::DVec3;

pub const PLUGIN_ID: &str = "fieldcad.gravity";
/// Newton's gravitational constant in m³·kg⁻¹·s⁻² (CODATA 2018).
pub const GRAVITATIONAL_CONSTANT: f64 = 6.674_30e-11;
pub const GRAVITATIONAL_ACCELERATION: &str = "gravitational-acceleration";
pub const GRAVITATIONAL_POTENTIAL: &str = "gravitational-potential";
pub const GRAVITATIONAL_ACCELERATION_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const GRAVITATIONAL_POTENTIAL_HANDLE: ChannelHandle = ChannelHandle::new(1);
const POTENTIAL_DIMENSION: Dimension = Dimension::new(0, 2, -2, 0, 0, 0, 0);
/// Retains the small set of geometries one runtime publication samples
/// (each visible plane, box, sphere, the probe set) so multiple channels
/// over the same geometry share one evaluation.
const SAMPLE_CACHE_CAPACITY: usize = 16;

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}
pub fn gravitational_acceleration_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), GRAVITATIONAL_ACCELERATION).expect("static channel ID is valid")
}
pub fn gravitational_potential_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), GRAVITATIONAL_POTENTIAL).expect("static channel ID is valid")
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

/// `CoupledSource<MassKg>` → the shared, coupling-value-agnostic source
/// shape `fieldcad-superposition`'s kernel (and any GPU evaluator built over
/// it) actually operates on. Public so a GPU evaluator can build its own
/// source buffer from the same mapping this crate's CPU reference uses,
/// rather than duplicating it.
pub fn inverse_square_source(source: &CoupledSource<MassKg>) -> InverseSquareSource {
    InverseSquareSource {
        position: source.position,
        strength: source.coupling_value.into_si(),
        distribution: source.distribution,
    }
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
/// see `fieldcad-superposition`'s `InverseSquareBatchEvaluator`, shared with
/// `plugins/electrostatics`.
pub struct NewtonianGravityPlugin {
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
}

impl Default for NewtonianGravityPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl NewtonianGravityPlugin {
    pub fn new() -> Self {
        Self {
            evaluator: Arc::new(CpuInverseSquareEvaluator),
        }
    }

    pub fn with_evaluator(evaluator: Arc<dyn InverseSquareBatchEvaluator>) -> Self {
        Self { evaluator }
    }
}

impl EquationSystemPlugin for NewtonianGravityPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 1, 0),
            display_name: "Newtonian gravity".to_owned(),
            description:
                "Analytic Newtonian gravitational acceleration and potential with superposition"
                    .to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        channels()
    }
    fn component_schemas(&self) -> Vec<ComponentSchema> {
        mass_component_schemas()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        // A snapshot's precision metadata must describe the numbers it
        // actually carries, or an `f32` interactive result is
        // indistinguishable from the `f64` oracle it is checked against —
        // matching `plugins/electrostatics`.
        if context.domain.precision() != self.evaluator.precision() {
            return Err(PluginError::InvalidConfiguration(format!(
                "gravity evaluator produces {}, but the domain declares {}",
                self.evaluator.precision().label(),
                context.domain.precision().label()
            )));
        }
        let sources = sources(context.world)?;
        let inverse_square_sources = sources
            .as_slice()
            .iter()
            .map(inverse_square_source)
            .collect();
        Ok(Box::new(NewtonianGravitySolver {
            domain: *context.domain,
            sources,
            inverse_square_sources,
            world_revision: context.world.revision(),
            evaluator: Arc::clone(&self.evaluator),
            cache: SampleCache::new(SAMPLE_CACHE_CAPACITY),
        }))
    }
}

/// Collect the world's mass sources, dropping zero-mass ones before they
/// reach the solver's indexes.
///
/// The returned `ObjectIndex` and the solver's `inverse_square_sources`
/// buffer are built from the same filtered list, so position `i` in one is
/// position `i` in the other — the alignment `add_forces`' index-based
/// exclusion relies on. Dropping zero-mass sources here is observable only
/// through sources whose field and force contributions are exactly zero
/// (the kernel skips them anyway); it also keeps the reported source count
/// to contributing sources.
fn sources(world: &WorldSnapshot) -> Result<ObjectIndex<CoupledSource<MassKg>>, PluginError> {
    collect_gravity_sources(world)
        .map(|collected| {
            ObjectIndex::new(
                collected
                    .into_iter()
                    .filter(|source| source.coupling_value.into_si() != 0.0)
                    .collect(),
            )
        })
        .map_err(|error| PluginError::UnsupportedWorld(error.to_string()))
}

struct NewtonianGravitySolver {
    domain: Domain,
    sources: ObjectIndex<CoupledSource<MassKg>>,
    /// Rebuilt with the object-indexed sources on creation/world changes;
    /// this is the cache-local input shape the shared evaluator expects,
    /// converted once per world change rather than on every channel read.
    inverse_square_sources: Vec<InverseSquareSource>,
    world_revision: fieldcad_core::WorldRevision,
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    cache: SampleCache<InverseSquareSample>,
}

impl EquationSystemSolver for NewtonianGravitySolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }
    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        sources(world).map(|_| ())
    }
    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.sources = sources(world)?;
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
            GRAVITATIONAL_ACCELERATION_HANDLE => {
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
            GRAVITATIONAL_POTENTIAL_HANDLE => {
                let column = SampledColumn::new(
                    FieldColumn::scalars(samples.iter().map(|sample| sample.potential).collect()),
                    validity,
                );
                // ∇Φ = −g: the potential's gradient is exactly minus the
                // acceleration this solver already computed, so no separate
                // math is needed — only whether `gradients.is_some()` still
                // gates it, to keep both channels' gradient availability
                // consistent for the same evaluator.
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

    fn add_forces(&self, bodies: &[DynamicBody], out: &mut [DVec3]) -> Result<(), PluginError> {
        for (body, out_force) in bodies.iter().zip(out) {
            // One lookup decides both questions: a body absent from the
            // filtered index (not a source, or zero mass) neither exerts
            // nor feels gravity.
            let Some(excluded) = self.sources.index_of(body.object) else {
                continue;
            };
            // `strength` is `coupling_value.into_si()` by construction —
            // the same number as the source's mass, without touching the
            // second source array.
            let mass = self.inverse_square_sources[excluded].strength;
            let acceleration = self.acceleration_excluding(excluded, body.position)?;
            *out_force += acceleration * mass;
        }
        Ok(())
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: plugin_id(),
            severity: DiagnosticSeverity::Info,
            code: "newtonian-gravity-source-count".to_owned(),
            message: format!(
                "{} mass source(s), {} batched evaluator, world revision {}",
                self.sources.len(),
                self.evaluator.precision().label(),
                self.world_revision
            ),
        }]
    }
}

impl NewtonianGravitySolver {
    /// The gravitational acceleration at `position` from every source
    /// except the one at index `excluded` of the solver's positionally
    /// aligned source slices.
    ///
    /// Evaluated over the precomputed `inverse_square_sources` buffer via
    /// the kernel's slice-based exclusion — the same items in the same
    /// order `ObjectIndex::iter_excluding` yields, without re-mapping each
    /// source per body per tick.
    fn acceleration_excluding(
        &self,
        excluded: usize,
        position: DVec3,
    ) -> Result<DVec3, PluginError> {
        fieldcad_superposition::field_excluding_at(
            -GRAVITATIONAL_CONSTANT,
            &self.inverse_square_sources,
            excluded,
            position,
        )
        .ok_or_else(|| {
            PluginError::Solver(
                "gravitational acceleration overflowed to a non-finite value".to_owned(),
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
                self.evaluator
                    .evaluate(
                        -GRAVITATIONAL_CONSTANT,
                        &self.inverse_square_sources,
                        &self.domain,
                        geometry,
                    )
                    .map_err(PluginError::Solver)
            },
            |out| {
                self.evaluator
                    .evaluate_into(
                        -GRAVITATIONAL_CONSTANT,
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
    use super::*;
    use fieldcad_core::quantities::kilogram;
    use fieldcad_core::{
        BoundaryConditions, DomainBounds, ObjectShape, ObjectSpec, Precision, ProbeId, Resolution,
        StepContext, TimeStep, Transform, World, WorldCommand,
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

    /// Mirrors `plugins/electrostatics`' equivalent test — the CPU
    /// evaluator now reports a closed-form Jacobian, and gravity's `sample`
    /// must attach it the same way electrostatics' does.
    #[test]
    fn the_gravitational_acceleration_channel_publishes_its_jacobian() {
        let solver = solver();
        let geometry = SampleGeometry::probes(vec![ProbeId::new(0)], vec![DVec3::X]).unwrap();

        let column = solver
            .sample(GRAVITATIONAL_ACCELERATION_HANDLE, &geometry)
            .unwrap();

        match column.gradient {
            Some(GradientColumn::Vector(jacobians)) => assert_eq!(jacobians.len(), geometry.len()),
            other => panic!("expected a Jacobian per sample, got {other:?}"),
        }
    }

    #[test]
    fn the_potential_channel_publishes_minus_the_acceleration_as_its_gradient() {
        let solver = solver();
        let geometry = SampleGeometry::probes(vec![ProbeId::new(0)], vec![DVec3::X]).unwrap();

        let acceleration_column = solver
            .sample(GRAVITATIONAL_ACCELERATION_HANDLE, &geometry)
            .unwrap();
        let potential_column = solver
            .sample(GRAVITATIONAL_POTENTIAL_HANDLE, &geometry)
            .unwrap();

        let FieldColumn::Vector(accelerations) = acceleration_column.values else {
            panic!("expected a vector field column");
        };
        let Some(GradientColumn::Scalar(gradients)) = potential_column.gradient else {
            panic!("expected the potential channel to publish a gradient");
        };

        assert_eq!(accelerations.len(), gradients.len());
        for (acceleration, gradient) in accelerations.iter().zip(gradients.iter()) {
            assert!((*gradient - (-*acceleration)).length() < 1.0e-12);
        }
    }

    /// Mirrors `plugins/electrostatics`' equivalent test: this is the
    /// behavior change `docs/tasks/unify-inverse-square-sample-and-evaluator.md`
    /// made deliberate — a mismatched domain/evaluator precision must now be
    /// rejected at `create_solver` instead of silently quantized.
    #[test]
    fn create_solver_rejects_a_domain_precision_mismatch() {
        let plugin = NewtonianGravityPlugin::new();
        let world = World::new();
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(4).unwrap(),
            BoundaryConditions::default(),
            Precision::F32,
        );

        assert!(matches!(
            plugin.create_solver(SolverContext {
                configuration: &Default::default(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(1.0).unwrap(),
                },
                cancellation: Default::default(),
            }),
            Err(PluginError::InvalidConfiguration(_))
        ));
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

        for (body, force) in bodies.iter().zip(&forces) {
            let mass = collected
                .iter()
                .find(|source| source.object == body.object)
                .unwrap()
                .coupling_value
                .into_si();
            let expected_field = fieldcad_superposition::field_excluding(
                -GRAVITATIONAL_CONSTANT,
                collected
                    .iter()
                    .filter(|source| source.object != body.object)
                    .map(inverse_square_source),
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
