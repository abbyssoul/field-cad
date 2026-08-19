//! A contract fixture, not a physical model.
//!
//! This plugin exposes two closed-form functions with values that are trivial to
//! predict by hand, so runtime behaviour — revision handling, batching, validity,
//! diagnostics — can be tested without a solver's numerical error confusing the
//! result. It is deliberately not electrostatics.

use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, ComponentTypeId, DiagnosticSeverity, Dimension,
    Domain, FieldColumn, FieldValueKind, PluginId, PluginVersion, PropertyBag, PropertyId,
    PropertyKind, PropertySchema, PropertyValue, Quantity, SampleGeometry, SampleValidity,
    SolverDiagnostic, UndefinedReason, WorldRevision, WorldSnapshot,
};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginConfigurationSchema,
    PluginError, PluginMetadata, SampledColumn, SolverContext, SolverKind, SolverStepOutcome,
};

pub const PLUGIN_ID: &str = "fieldcad.test-field";
pub const SCALAR_CHANNEL: &str = "linear-scalar";
pub const VECTOR_CHANNEL: &str = "position-vector";
const GAIN_PROPERTY: &str = "gain";
const LIVE_GAIN_COMPONENT: &str = "live-gain";
const LIVE_GAIN_PROPERTY: &str = "gain";

/// Handles must match the order of [`TestFieldPlugin::channels`].
pub const SCALAR_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const VECTOR_HANDLE: ChannelHandle = ChannelHandle::new(1);

/// Samples closer to the origin than this are reported undefined, exercising the
/// singularity path that a real point-source solver needs.
pub const UNDEFINED_RADIUS: f64 = 0.05;

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}

pub fn scalar_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), SCALAR_CHANNEL).expect("static channel ID is valid")
}

pub fn vector_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), VECTOR_CHANNEL).expect("static channel ID is valid")
}

pub fn gain_property_id() -> PropertyId {
    PropertyId::new(GAIN_PROPERTY).expect("static property ID is valid")
}

/// Object component used only by expression contract tests to demonstrate
/// pre-tick live observation binding without opting a physical schema in.
pub fn live_gain_component_id() -> ComponentTypeId {
    ComponentTypeId::new(plugin_id(), LIVE_GAIN_COMPONENT).expect("static component ID is valid")
}

/// Required dimensionless property carried by [`live_gain_component_id`].
pub fn live_gain_property_id() -> PropertyId {
    PropertyId::new(LIVE_GAIN_PROPERTY).expect("static property ID is valid")
}

/// Schema for the test-only live-binding contract component.
pub fn live_gain_component_schema() -> ComponentSchema {
    ComponentSchema {
        id: live_gain_component_id(),
        display_name: "Test live gain".to_owned(),
        properties: vec![PropertySchema {
            id: live_gain_property_id(),
            display_name: "Live gain".to_owned(),
            description: Some(
                "Dimensionless test fixture multiplier that may follow a live distance expression"
                    .to_owned(),
            ),
            kind: PropertyKind::Scalar(Dimension::DIMENSIONLESS),
            required: true,
            live_binding: true,
            default_value: Some(PropertyValue::Scalar(
                Quantity::new(1.0, Dimension::DIMENSIONLESS)
                    .expect("static finite quantity is valid"),
            )),
            relevant_when: None,
        }],
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TestFieldPlugin;

impl EquationSystemPlugin for TestFieldPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 3, 0),
            display_name: "Analytic test field".to_owned(),
            description: "Known scalar and vector functions used to validate the runtime"
                .to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        vec![
            ChannelSchema {
                id: scalar_channel_id(),
                display_name: "Linear scalar".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::LENGTH),
            },
            ChannelSchema {
                id: vector_channel_id(),
                display_name: "Position vector".to_owned(),
                value_kind: FieldValueKind::Vector(Dimension::LENGTH),
            },
        ]
    }

    fn configuration_schema(&self) -> PluginConfigurationSchema {
        PluginConfigurationSchema {
            properties: vec![PropertySchema {
                id: gain_property_id(),
                display_name: "Gain".to_owned(),
                description: None,
                kind: PropertyKind::Scalar(Dimension::DIMENSIONLESS),
                required: true,
                live_binding: false,
                default_value: None,
                relevant_when: None,
            }],
        }
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        vec![live_gain_component_schema()]
    }

    fn default_configuration(&self) -> PropertyBag {
        [(
            gain_property_id(),
            PropertyValue::Scalar(
                Quantity::new(1.0, Dimension::DIMENSIONLESS)
                    .expect("static finite quantity is valid"),
            ),
        )]
        .into_iter()
        .collect()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        self.configuration_schema()
            .validate(context.configuration)?;
        let gain = context
            .configuration
            .scalar(&gain_property_id())
            .ok_or_else(|| {
                PluginError::InvalidConfiguration("gain must be a dimensionless scalar".to_owned())
            })?;

        Ok(Box::new(TestFieldSolver {
            configured_gain: gain,
            gain: gain * world_live_gain(context.world)?,
            domain: *context.domain,
            time_seconds: context.initial_step.time_seconds,
            world_revision: context.world.revision(),
        }))
    }
}

struct TestFieldSolver {
    configured_gain: f64,
    gain: f64,
    domain: Domain,
    time_seconds: f64,
    world_revision: WorldRevision,
}

impl TestFieldSolver {
    fn validity_at(&self, position: glam::DVec3) -> SampleValidity {
        if position.length() < UNDEFINED_RADIUS {
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        } else if self.domain.bounds().contains(position) {
            SampleValidity::Exact
        } else {
            SampleValidity::Undefined(UndefinedReason::OutsideDomain)
        }
    }
}

fn world_live_gain(world: &WorldSnapshot) -> Result<f64, PluginError> {
    let component = live_gain_component_id();
    let mut fixtures = world.objects_with(&component);
    let Some((_, properties)) = fixtures.next() else {
        return Ok(1.0);
    };
    if fixtures.next().is_some() {
        return Err(PluginError::UnsupportedWorld(
            "at most one object may carry the test live-gain component".to_owned(),
        ));
    }
    properties.scalar(&live_gain_property_id()).ok_or_else(|| {
        PluginError::UnsupportedWorld("test live-gain must be a dimensionless scalar".to_owned())
    })
}

impl EquationSystemSolver for TestFieldSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        world_live_gain(world).map(|_| ())
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.gain = self.configured_gain * world_live_gain(world)?;
        self.world_revision = world.revision();
        Ok(())
    }

    fn step(
        &mut self,
        context: fieldcad_core::StepContext,
    ) -> Result<SolverStepOutcome, PluginError> {
        self.time_seconds = context.time_seconds;
        Ok(SolverStepOutcome::default())
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let count = geometry.len();
        let mut validity = Vec::with_capacity(count);

        match channel {
            SCALAR_HANDLE => {
                let mut values = Vec::with_capacity(count);
                for position in geometry.positions() {
                    values.push(self.gain * (position.x + 2.0 * position.y + 3.0 * position.z));
                    validity.push(self.validity_at(position));
                }
                Ok(SampledColumn::new(FieldColumn::scalars(values), validity))
            }
            VECTOR_HANDLE => {
                let mut values = Vec::with_capacity(count);
                for position in geometry.positions() {
                    values.push(self.gain * position);
                    validity.push(self.validity_at(position));
                }
                Ok(SampledColumn::new(FieldColumn::vectors(values), validity))
            }
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: plugin_id(),
            severity: DiagnosticSeverity::Info,
            code: "observed-world-revision".to_owned(),
            message: format!(
                "world revision {}; simulation time {:.6} s",
                self.world_revision, self.time_seconds
            ),
        }]
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectSpec, ProbeId, StepContext, TimeStep, World, WorldCommand};
    use glam::DVec3;

    use super::*;

    fn solver() -> Box<dyn EquationSystemSolver> {
        let plugin = TestFieldPlugin;
        let domain = Domain::centred_cube(10.0, 8).unwrap();
        let world = World::new();
        plugin
            .create_solver(SolverContext {
                configuration: &plugin.default_configuration(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(0.1).unwrap(),
                },
                cancellation: fieldcad_plugin_api::SolverCancellation::default(),
            })
            .unwrap()
    }

    fn live_gain_properties(value: f64) -> PropertyBag {
        [(
            live_gain_property_id(),
            PropertyValue::Scalar(
                Quantity::new(value, Dimension::DIMENSIONLESS).expect("finite fixture gain"),
            ),
        )]
        .into_iter()
        .collect()
    }

    fn points(positions: Vec<DVec3>) -> SampleGeometry {
        let ids = (0..positions.len() as u64).map(ProbeId::new).collect();
        SampleGeometry::probes(ids, positions).unwrap()
    }

    #[test]
    fn analytic_channels_have_known_values() {
        let solver = solver();
        let geometry = points(vec![DVec3::new(1.0, 2.0, 3.0)]);

        let scalar = solver.sample(SCALAR_HANDLE, &geometry).unwrap();
        assert_eq!(scalar.values, FieldColumn::scalars(vec![14.0]));

        let vector = solver.sample(VECTOR_HANDLE, &geometry).unwrap();
        assert_eq!(
            vector.values,
            FieldColumn::vectors(vec![DVec3::new(1.0, 2.0, 3.0)])
        );
    }

    #[test]
    fn one_call_evaluates_a_whole_batch() {
        let solver = solver();
        let geometry = points(vec![DVec3::X, DVec3::Y, DVec3::Z]);

        let column = solver.sample(SCALAR_HANDLE, &geometry).unwrap();

        assert_eq!(column.values, FieldColumn::scalars(vec![1.0, 2.0, 3.0]));
        assert_eq!(column.len(), geometry.len());
    }

    #[test]
    fn samples_outside_the_domain_and_inside_the_source_are_undefined() {
        let solver = solver();
        let geometry = points(vec![DVec3::ZERO, DVec3::splat(100.0), DVec3::X]);

        let column = solver.sample(SCALAR_HANDLE, &geometry).unwrap();

        assert_eq!(
            column.validity[0],
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius)
        );
        assert_eq!(
            column.validity[1],
            SampleValidity::Undefined(UndefinedReason::OutsideDomain)
        );
        assert_eq!(column.validity[2], SampleValidity::Exact);
    }

    #[test]
    fn unknown_channel_handles_are_rejected() {
        let solver = solver();

        assert!(matches!(
            solver.sample(ChannelHandle::new(7), &points(vec![DVec3::ZERO])),
            Err(PluginError::UnknownChannel(7))
        ));
    }

    #[test]
    fn declared_channel_order_matches_the_published_handles() {
        let channels = TestFieldPlugin.channels();

        assert_eq!(channels[SCALAR_HANDLE.index()].id, scalar_channel_id());
        assert_eq!(channels[VECTOR_HANDLE.index()].id, vector_channel_id());
    }

    #[test]
    fn live_gain_schema_is_explicitly_live_bindable() {
        let schema = live_gain_component_schema();
        assert_eq!(schema.id, live_gain_component_id());
        assert!(schema.properties[0].required);
        assert!(schema.properties[0].live_binding);
        assert_eq!(
            schema.properties[0].kind,
            PropertyKind::Scalar(Dimension::DIMENSIONLESS)
        );
        assert_eq!(
            TestFieldPlugin.metadata().version,
            PluginVersion::new(0, 3, 0)
        );
    }

    #[test]
    fn world_notifications_reread_the_optional_live_gain() {
        let plugin = TestFieldPlugin;
        let domain = Domain::centred_cube(10.0, 8).unwrap();
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(live_gain_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("fixture")
                        .with_component(live_gain_component_id(), live_gain_properties(2.0)),
                ),
            ])
            .unwrap();
        let mut solver = plugin
            .create_solver(SolverContext {
                configuration: &plugin.default_configuration(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(0.1).unwrap(),
                },
                cancellation: Default::default(),
            })
            .unwrap();
        assert_eq!(
            solver
                .sample(SCALAR_HANDLE, &points(vec![DVec3::X]))
                .unwrap()
                .values,
            FieldColumn::scalars(vec![2.0])
        );

        let object = *world.snapshot().objects().keys().next().unwrap();
        world
            .commit([WorldCommand::AttachComponent {
                object,
                component: live_gain_component_id(),
                properties: live_gain_properties(3.0),
            }])
            .unwrap();
        solver.on_world_changed(&world.snapshot()).unwrap();
        assert_eq!(
            solver
                .sample(SCALAR_HANDLE, &points(vec![DVec3::X]))
                .unwrap()
                .values,
            FieldColumn::scalars(vec![3.0])
        );
    }

    #[test]
    fn a_second_live_gain_object_is_rejected() {
        let mut world = World::new();
        world
            .commit([WorldCommand::RegisterComponentSchema(
                live_gain_component_schema(),
            )])
            .unwrap();
        for name in ["first", "second"] {
            world
                .commit([WorldCommand::CreateObject(
                    ObjectSpec::new(name)
                        .with_component(live_gain_component_id(), live_gain_properties(1.0)),
                )])
                .unwrap();
        }
        assert!(matches!(
            world_live_gain(&world.snapshot()),
            Err(PluginError::UnsupportedWorld(_))
        ));
    }
}
