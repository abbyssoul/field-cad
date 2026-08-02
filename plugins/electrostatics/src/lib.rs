//! Analytic electrostatics for point sources and uniformly charged spheres.
//!
//! This is the first physical equation-system plugin. The CPU `f64` evaluator
//! is deliberately small and explicit: it is the correctness oracle for every
//! later parallel or GPU implementation.

use std::sync::{Arc, Mutex};

use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, ComponentTypeId, DiagnosticSeverity, Dimension,
    Domain, FieldColumn, FieldValueKind, ObjectShape, PluginId, PluginVersion, Precision,
    PropertyBag, PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity, QuantityError,
    SampleGeometry, SampleValidity, SolverDiagnostic, UndefinedReason, WorldObject, WorldSnapshot,
};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginError, PluginMetadata,
    SampledColumn, SolverContext, SolverKind,
};
use glam::DVec3;

pub const PLUGIN_ID: &str = "fieldcad.electrostatics";
pub const CHARGE_COMPONENT: &str = "charge-source";
pub const CHARGE_PROPERTY: &str = "charge";
pub const ELECTRIC_FIELD_CHANNEL: &str = "electric-field";
pub const ELECTRIC_POTENTIAL_CHANNEL: &str = "electric-potential";

/// Coulomb constant in N·m²/C² (CODATA conventional value used by the oracle).
pub const COULOMB_CONSTANT: f64 = 8.987_551_792_3e9;

pub const ELECTRIC_FIELD_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const ELECTRIC_POTENTIAL_HANDLE: ChannelHandle = ChannelHandle::new(1);

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}

pub fn charge_component_id() -> ComponentTypeId {
    ComponentTypeId::new(plugin_id(), CHARGE_COMPONENT).expect("static component ID is valid")
}

pub fn charge_property_id() -> PropertyId {
    PropertyId::new(CHARGE_PROPERTY).expect("static property ID is valid")
}

pub fn electric_field_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), ELECTRIC_FIELD_CHANNEL).expect("static channel ID is valid")
}

pub fn electric_potential_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), ELECTRIC_POTENTIAL_CHANNEL).expect("static channel ID is valid")
}

pub fn charge_properties(coulombs: f64) -> Result<PropertyBag, QuantityError> {
    Ok([(
        charge_property_id(),
        PropertyValue::Scalar(Quantity::new(coulombs, Dimension::CHARGE)?),
    )]
    .into_iter()
    .collect())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChargeDistribution {
    /// An ideal point source. Values at or inside `exclusion_radius` are
    /// deliberately undefined rather than clamped.
    Point { exclusion_radius: f64 },
    /// A solid sphere with uniform volume charge density.
    UniformSphere { radius: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChargeSource {
    pub position: DVec3,
    pub charge_coulombs: f64,
    pub distribution: ChargeDistribution,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ElectrostaticSample {
    pub electric_field: DVec3,
    pub potential: f64,
    pub validity: SampleValidity,
}

/// Host-provided parallel evaluator for one complete sample geometry.
///
/// The plugin defines this narrow, renderer-free seam while the application
/// host owns concrete GPU device/queue access and resource budgets. Results
/// return through ordinary snapshot columns, so a local GPU is an implementation
/// detail of compute and the visualizer remains interchangeable with a remote
/// data source.
pub trait ElectrostaticBatchEvaluator: Send + Sync {
    /// Numerical representation written into the returned snapshot columns.
    fn precision(&self) -> Precision;

    /// Evaluate both electrostatic channels in one dispatch/readback.
    fn evaluate(
        &self,
        sources: &[ChargeSource],
        domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<ElectrostaticSample>, String>;
}

impl ElectrostaticSample {
    fn undefined(reason: UndefinedReason) -> Self {
        Self {
            electric_field: DVec3::ZERO,
            potential: 0.0,
            validity: SampleValidity::Undefined(reason),
        }
    }
}

/// Evaluate the superposed electrostatic field and potential in SI units.
pub fn evaluate_sources(sources: &[ChargeSource], position: DVec3) -> ElectrostaticSample {
    let mut electric_field = DVec3::ZERO;
    let mut potential = 0.0;

    for source in sources {
        if source.charge_coulombs == 0.0 {
            continue;
        }
        let displacement = position - source.position;
        let distance_squared = displacement.length_squared();
        let distance = distance_squared.sqrt();

        let (field_contribution, potential_contribution) = match source.distribution {
            ChargeDistribution::Point { exclusion_radius } => {
                if distance <= exclusion_radius {
                    return ElectrostaticSample::undefined(UndefinedReason::InsideSourceRadius);
                }
                let inverse_distance = distance.recip();
                (
                    COULOMB_CONSTANT
                        * source.charge_coulombs
                        * displacement
                        * inverse_distance.powi(3),
                    COULOMB_CONSTANT * source.charge_coulombs * inverse_distance,
                )
            }
            ChargeDistribution::UniformSphere { radius } if distance < radius => {
                let radius_cubed = radius.powi(3);
                (
                    COULOMB_CONSTANT * source.charge_coulombs * displacement / radius_cubed,
                    COULOMB_CONSTANT * source.charge_coulombs / (2.0 * radius)
                        * (3.0 - distance_squared / radius.powi(2)),
                )
            }
            ChargeDistribution::UniformSphere { .. } => {
                let inverse_distance = distance.recip();
                (
                    COULOMB_CONSTANT
                        * source.charge_coulombs
                        * displacement
                        * inverse_distance.powi(3),
                    COULOMB_CONSTANT * source.charge_coulombs * inverse_distance,
                )
            }
        };

        electric_field += field_contribution;
        potential += potential_contribution;
        if !electric_field.is_finite() || !potential.is_finite() {
            return ElectrostaticSample::undefined(UndefinedReason::NumericalOverflow);
        }
    }

    ElectrostaticSample {
        electric_field,
        potential,
        validity: SampleValidity::Exact,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ElectrostaticsPlugin;

impl ElectrostaticsPlugin {
    pub fn with_evaluator(
        evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
    ) -> AcceleratedElectrostaticsPlugin {
        AcceleratedElectrostaticsPlugin { evaluator }
    }
}

/// Electrostatics plugin configured with a host-owned batched evaluator.
pub struct AcceleratedElectrostaticsPlugin {
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
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
            ChannelSchema {
                id: electric_field_channel_id(),
                display_name: "Electric field".to_owned(),
                value_kind: FieldValueKind::Vector(Dimension::ELECTRIC_FIELD),
            },
            ChannelSchema {
                id: electric_potential_channel_id(),
                display_name: "Electric potential".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::ELECTRIC_POTENTIAL),
            },
        ]
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        vec![ComponentSchema {
            id: charge_component_id(),
            display_name: "Charge source".to_owned(),
            properties: vec![PropertySchema {
                id: charge_property_id(),
                display_name: "Charge".to_owned(),
                kind: PropertyKind::Scalar(Dimension::CHARGE),
                required: true,
            }],
        }]
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        Ok(Box::new(ElectrostaticsSolver {
            sources: collect_sources(context.world)?,
            world_revision: context.world.revision(),
        }))
    }
}

impl EquationSystemPlugin for AcceleratedElectrostaticsPlugin {
    fn metadata(&self) -> PluginMetadata {
        ElectrostaticsPlugin.metadata()
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        ElectrostaticsPlugin.channels()
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        ElectrostaticsPlugin.component_schemas()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        if context.domain.precision() != self.evaluator.precision() {
            return Err(PluginError::InvalidConfiguration(format!(
                "electrostatics evaluator produces {}, but the domain declares {}",
                self.evaluator.precision().label(),
                context.domain.precision().label()
            )));
        }
        Ok(Box::new(AcceleratedElectrostaticsSolver {
            domain: *context.domain,
            sources: collect_sources(context.world)?,
            world_revision: context.world.revision(),
            evaluator: Arc::clone(&self.evaluator),
            cache: Mutex::new(Vec::new()),
        }))
    }
}

struct ElectrostaticsSolver {
    sources: Vec<ChargeSource>,
    world_revision: fieldcad_core::WorldRevision,
}

struct AcceleratedElectrostaticsSolver {
    domain: Domain,
    sources: Vec<ChargeSource>,
    world_revision: fieldcad_core::WorldRevision,
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
    /// Runtime publication asks for E and V separately. Retain the small set of
    /// geometries from this publication so both channels share one GPU dispatch.
    cache: Mutex<Vec<(SampleGeometry, Arc<[ElectrostaticSample]>)>>,
}

impl EquationSystemSolver for ElectrostaticsSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        collect_sources(world).map(|_| ())
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.sources = collect_sources(world)?;
        self.world_revision = world.revision();
        Ok(())
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let mut validity = Vec::with_capacity(geometry.len());

        match channel {
            ELECTRIC_FIELD_HANDLE => {
                let mut values = Vec::with_capacity(geometry.len());
                for position in geometry.positions() {
                    let sample = self.evaluate(position);
                    values.push(sample.electric_field);
                    validity.push(sample.validity);
                }
                Ok(SampledColumn::new(FieldColumn::vectors(values), validity))
            }
            ELECTRIC_POTENTIAL_HANDLE => {
                let mut values = Vec::with_capacity(geometry.len());
                for position in geometry.positions() {
                    let sample = self.evaluate(position);
                    values.push(sample.potential);
                    validity.push(sample.validity);
                }
                Ok(SampledColumn::new(FieldColumn::scalars(values), validity))
            }
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: plugin_id(),
            severity: DiagnosticSeverity::Info,
            code: "electrostatic-source-count".to_owned(),
            message: format!(
                "{} charge source(s), world revision {}",
                self.sources.len(),
                self.world_revision
            ),
        }]
    }
}

impl ElectrostaticsSolver {
    fn evaluate(&self, position: DVec3) -> ElectrostaticSample {
        evaluate_sources(&self.sources, position)
    }
}

impl EquationSystemSolver for AcceleratedElectrostaticsSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        collect_sources(world).map(|_| ())
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.sources = collect_sources(world)?;
        self.world_revision = world.revision();
        self.cache
            .get_mut()
            .map_err(|_| PluginError::Solver("electrostatics GPU cache is poisoned".to_owned()))?
            .clear();
        Ok(())
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let samples = self.samples_for(geometry)?;
        let validity = samples.iter().map(|sample| sample.validity).collect();
        match channel {
            ELECTRIC_FIELD_HANDLE => Ok(SampledColumn::new(
                FieldColumn::vectors(samples.iter().map(|sample| sample.electric_field).collect()),
                validity,
            )),
            ELECTRIC_POTENTIAL_HANDLE => Ok(SampledColumn::new(
                FieldColumn::scalars(samples.iter().map(|sample| sample.potential).collect()),
                validity,
            )),
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: plugin_id(),
            severity: DiagnosticSeverity::Info,
            code: "electrostatic-gpu-source-count".to_owned(),
            message: format!(
                "{} charge source(s), {} batched evaluator, world revision {}",
                self.sources.len(),
                self.evaluator.precision().label(),
                self.world_revision
            ),
        }]
    }
}

impl AcceleratedElectrostaticsSolver {
    fn samples_for(
        &self,
        geometry: &SampleGeometry,
    ) -> Result<Arc<[ElectrostaticSample]>, PluginError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| PluginError::Solver("electrostatics GPU cache is poisoned".to_owned()))?;
        if let Some((_, samples)) = cache.iter().find(|(cached, _)| cached == geometry) {
            return Ok(Arc::clone(samples));
        }

        let evaluated = self
            .evaluator
            .evaluate(&self.sources, &self.domain, geometry)
            .map_err(PluginError::Solver)?;
        if evaluated.len() != geometry.len() {
            return Err(PluginError::Solver(format!(
                "batched evaluator returned {} samples for a geometry of length {}",
                evaluated.len(),
                geometry.len()
            )));
        }
        let evaluated: Arc<[ElectrostaticSample]> = evaluated.into();
        // A subscription currently has probes plus any number of planes and one
        // grid. Bound stale entries left by density changes without complicating
        // the plugin contract with publication lifecycle callbacks.
        if cache.len() >= 16 {
            cache.remove(0);
        }
        cache.push((geometry.clone(), Arc::clone(&evaluated)));
        Ok(evaluated)
    }
}

pub fn collect_sources(world: &WorldSnapshot) -> Result<Vec<ChargeSource>, PluginError> {
    world
        .objects_with(&charge_component_id())
        .map(|(object, properties)| source_from_object(object, properties))
        .collect()
}

fn source_from_object(
    object: &WorldObject,
    properties: &PropertyBag,
) -> Result<ChargeSource, PluginError> {
    let charge_coulombs = properties.scalar(&charge_property_id()).ok_or_else(|| {
        PluginError::UnsupportedWorld(format!(
            "object '{}' has a charge component without a scalar charge",
            object.name
        ))
    })?;
    let distribution = match object.shape {
        Some(ObjectShape::Point { radius }) => ChargeDistribution::Point {
            exclusion_radius: radius,
        },
        Some(ObjectShape::Sphere { radius }) if radius > 0.0 => {
            ChargeDistribution::UniformSphere { radius }
        }
        Some(ObjectShape::Sphere { .. }) => {
            return Err(PluginError::UnsupportedWorld(format!(
                "charged sphere '{}' must have a positive radius",
                object.name
            )));
        }
        _ => {
            return Err(PluginError::UnsupportedWorld(format!(
                "charged object '{}' must use a point or sphere shape",
                object.name
            )));
        }
    };
    Ok(ChargeSource {
        position: object.transform.translation,
        charge_coulombs,
        distribution,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fieldcad_core::{
        BoundaryConditions, DomainBounds, ObjectSpec, PlaneLattice, Resolution, Transform, World,
        WorldCommand,
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
        ChargeSource {
            position,
            charge_coulombs,
            distribution: ChargeDistribution::Point {
                exclusion_radius: radius,
            },
        }
    }

    #[test]
    fn a_positive_point_charge_matches_coulombs_law() {
        let sample = evaluate_sources(&[point(DVec3::ZERO, 2.0e-9, 0.01)], DVec3::X);

        relative_eq(sample.electric_field.x, COULOMB_CONSTANT * 2.0e-9, 1.0e-14);
        relative_eq(sample.potential, COULOMB_CONSTANT * 2.0e-9, 1.0e-14);
        assert_eq!(sample.electric_field.y, 0.0);
        assert_eq!(sample.validity, SampleValidity::Exact);
    }

    #[test]
    fn field_direction_and_potential_follow_charge_sign() {
        let positive = evaluate_sources(&[point(DVec3::ZERO, 1.0e-9, 0.0)], DVec3::X);
        let negative = evaluate_sources(&[point(DVec3::ZERO, -1.0e-9, 0.0)], DVec3::X);

        assert!(positive.electric_field.x > 0.0);
        assert!(positive.potential > 0.0);
        assert!(negative.electric_field.x < 0.0);
        assert!(negative.potential < 0.0);
    }

    #[test]
    fn point_field_has_inverse_square_falloff() {
        let source = point(DVec3::ZERO, 1.0e-9, 0.0);
        let near = evaluate_sources(&[source], DVec3::X)
            .electric_field
            .length();
        let far = evaluate_sources(&[source], DVec3::X * 2.0)
            .electric_field
            .length();

        relative_eq(far / near, 0.25, 1.0e-14);
    }

    #[test]
    fn superposition_cancels_symmetric_fields_and_adds_potential() {
        let charge = 1.0e-9;
        let sample = evaluate_sources(
            &[point(-DVec3::X, charge, 0.0), point(DVec3::X, charge, 0.0)],
            DVec3::ZERO,
        );

        assert_eq!(sample.electric_field, DVec3::ZERO);
        relative_eq(sample.potential, 2.0 * COULOMB_CONSTANT * charge, 1.0e-14);
    }

    #[test]
    fn point_source_exclusion_is_explicit() {
        let sample = evaluate_sources(&[point(DVec3::ZERO, 1.0, 0.1)], DVec3::X * 0.05);

        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        );
        assert_eq!(sample.electric_field, DVec3::ZERO);
        assert_eq!(sample.potential, 0.0);
    }

    #[test]
    fn uniformly_charged_sphere_is_finite_and_continuous_at_its_surface() {
        let charge = 2.0e-9;
        let radius = 0.5;
        let source = ChargeSource {
            position: DVec3::ZERO,
            charge_coulombs: charge,
            distribution: ChargeDistribution::UniformSphere { radius },
        };
        let centre = evaluate_sources(&[source], DVec3::ZERO);
        let surface = evaluate_sources(&[source], DVec3::X * radius);

        assert_eq!(centre.electric_field, DVec3::ZERO);
        relative_eq(
            centre.potential,
            1.5 * COULOMB_CONSTANT * charge / radius,
            1.0e-14,
        );
        relative_eq(
            surface.electric_field.x,
            COULOMB_CONSTANT * charge / radius.powi(2),
            1.0e-14,
        );
        relative_eq(
            surface.potential,
            COULOMB_CONSTANT * charge / radius,
            1.0e-14,
        );
    }

    #[test]
    fn plugin_rejects_charged_objects_without_a_supported_shape() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(
                    ElectrostaticsPlugin.component_schemas().remove(0),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("invalid")
                        .with_transform(Transform::at(DVec3::ZERO).unwrap())
                        .with_component(charge_component_id(), charge_properties(1.0).unwrap()),
                ),
            ])
            .unwrap();
        let domain = Domain::centred_cube(2.0, 8).unwrap();

        assert!(matches!(
            ElectrostaticsPlugin.create_solver(SolverContext {
                configuration: &PropertyBag::default(),
                domain: &domain,
                world: &world.snapshot(),
            }),
            Err(PluginError::UnsupportedWorld(_))
        ));
    }

    struct CountingEvaluator {
        calls: AtomicUsize,
    }

    impl ElectrostaticBatchEvaluator for CountingEvaluator {
        fn precision(&self) -> Precision {
            Precision::F32
        }

        fn evaluate(
            &self,
            _sources: &[ChargeSource],
            _domain: &Domain,
            geometry: &SampleGeometry,
        ) -> Result<Vec<ElectrostaticSample>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![
                ElectrostaticSample {
                    electric_field: DVec3::X,
                    potential: 2.0,
                    validity: SampleValidity::Exact,
                };
                geometry.len()
            ])
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
            }),
            Err(PluginError::InvalidConfiguration(_))
        ));
    }
}
