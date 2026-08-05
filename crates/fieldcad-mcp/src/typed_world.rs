//! The typed, schema-discoverable MCP request DSL for authoring the world.
//!
//! `commit_world` (this crate's original slice) accepts raw
//! [`fieldcad_core::WorldCommand`] JSON: a client has to reverse-engineer its
//! serde shape and gets only an authority-side rejection when it guesses
//! wrong. Every type here is MCP-facing vocabulary instead — stable operation
//! names, explicit geometry fields rather than `glam` serialization, `{
//! plugin, name }` component/channel references, and a tagged property-value
//! shape whose dimension is supplied by the discovered component schema
//! rather than submitted by the client (so it cannot disagree with the
//! declared property). See `docs/tasks/typed-world-mutation-dsl.md`.
//!
//! [`WorldEditParam`] converts to exactly one [`WorldCommand`]; a transaction
//! is a `Vec<WorldEditParam>`, submitted the same way `commit_world` submits
//! its raw commands — one atomic [`CommandPayload::CommitWorld`]. Conversion
//! validates identifiers, finite geometry, and component-property kinds
//! against the schemas already registered in the world; the runtime remains
//! the final validator for everything conversion cannot know (whether an
//! entity ID actually resolves, cross-entity invariants such as a probe's
//! attachment target).
//!
//! Deliberately not yet covered (left for a later increment of the same
//! task): optimistic-concurrency `expected_revision`, and allocated-ID
//! reporting for a transaction queued behind a running tick boundary — see
//! [`fieldcad_simulation::CommandReceipt::created`].

use std::collections::BTreeMap;

use fieldcad_core::{
    BoxId, ChannelId, ComponentSchema, ComponentTypeId, FieldBoxSpec, FieldSphereSpec, ObjectId,
    ObjectShape, ObjectSpec, PlaneId, PluginId, ProbeId, ProbePosition, ProbeSpec, PropertyBag,
    PropertyId, PropertyKind, PropertyValue, Quantity, SlicePlaneSpec, SphereId, Transform,
    VectorQuantity, Velocity, WorldCommand, WorldError,
};
use glam::{DQuat, DVec2, DVec3};
use rmcp::schemars;
use serde::Deserialize;

fn default_true() -> bool {
    true
}

/// An explicit `{x, y, z}` request shape, kept separate from `glam`'s own
/// serialization (a bare 3-array) so the MCP schema names each field.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
pub struct Vec3Param {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl From<Vec3Param> for DVec3 {
    fn from(value: Vec3Param) -> Self {
        DVec3::new(value.x, value.y, value.z)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
pub struct Vec2Param {
    pub x: f64,
    pub y: f64,
}

impl From<Vec2Param> for DVec2 {
    fn from(value: Vec2Param) -> Self {
        DVec2::new(value.x, value.y)
    }
}

/// A rotation quaternion, `x, y, z, w`. Defaults to identity wherever a field
/// omits it.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
pub struct QuatParam {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

impl Default for QuatParam {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        }
    }
}

impl From<QuatParam> for DQuat {
    fn from(value: QuatParam) -> Self {
        DQuat::from_xyzw(value.x, value.y, value.z, value.w)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
pub struct TransformParam {
    #[serde(default)]
    pub translation: Vec3Param,
    #[serde(default)]
    pub rotation: QuatParam,
}

impl TryFrom<TransformParam> for Transform {
    type Error = WorldError;

    fn try_from(value: TransformParam) -> Result<Self, Self::Error> {
        Transform::new(value.translation.into(), value.rotation.into())
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
pub struct VelocityParam {
    #[serde(default)]
    pub linear: Vec3Param,
    #[serde(default)]
    pub angular: Vec3Param,
}

impl TryFrom<VelocityParam> for Velocity {
    type Error = WorldError;

    fn try_from(value: VelocityParam) -> Result<Self, Self::Error> {
        Velocity::new(value.linear.into(), value.angular.into())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObjectShapeParam {
    /// A point source with a declared radius inside which the analytic field
    /// is undefined rather than merely large.
    Point {
        radius_m: f64,
    },
    Sphere {
        radius_m: f64,
    },
    Box {
        half_extent_m: Vec3Param,
    },
}

impl TryFrom<ObjectShapeParam> for ObjectShape {
    type Error = WorldError;

    fn try_from(value: ObjectShapeParam) -> Result<Self, Self::Error> {
        match value {
            ObjectShapeParam::Point { radius_m } => ObjectShape::point(radius_m),
            ObjectShapeParam::Sphere { radius_m } => ObjectShape::sphere(radius_m),
            ObjectShapeParam::Box { half_extent_m } => ObjectShape::boxed(half_extent_m.into()),
        }
    }
}

/// A plugin-namespaced component reference, as reported by `get_world`'s
/// `component_schemas` or `list_component_schemas`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ComponentRefParam {
    pub plugin: String,
    pub name: String,
}

impl ComponentRefParam {
    fn resolve(self) -> Result<ComponentTypeId, String> {
        let plugin = PluginId::new(self.plugin).map_err(|error| error.to_string())?;
        ComponentTypeId::new(plugin, self.name).map_err(|error| error.to_string())
    }
}

/// A plugin-namespaced channel reference, as reported by `list_field_systems`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ChannelRefParam {
    pub plugin: String,
    pub name: String,
}

impl ChannelRefParam {
    pub(crate) fn resolve(self) -> Result<ChannelId, String> {
        let plugin = PluginId::new(self.plugin).map_err(|error| error.to_string())?;
        ChannelId::new(plugin, self.name).map_err(|error| error.to_string())
    }
}

/// A component-property value. Deliberately carries no dimension: the
/// component schema (`list_component_schemas`) supplies it for `scalar` and
/// `vector`, so a client cannot submit a dimension that disagrees with the
/// declared property.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertyValueParam {
    Scalar {
        si_value: f64,
    },
    Vector {
        x: f64,
        y: f64,
        z: f64,
    },
    Boolean {
        value: bool,
    },
    Text {
        value: String,
    },
    /// One of the options the component schema declares for this property.
    Choice {
        value: String,
    },
}

impl PropertyValueParam {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::Scalar { .. } => "scalar",
            Self::Vector { .. } => "vector",
            Self::Boolean { .. } => "boolean",
            Self::Text { .. } => "text",
            Self::Choice { .. } => "choice",
        }
    }
}

fn describe_property_kind(kind: &PropertyKind) -> String {
    match kind {
        PropertyKind::Scalar(dimension) => format!("scalar ({})", dimension.unit_symbol()),
        PropertyKind::Vector(dimension) => format!("vector ({})", dimension.unit_symbol()),
        PropertyKind::Boolean => "boolean".to_owned(),
        PropertyKind::Text => "text".to_owned(),
        PropertyKind::Choice(options) => format!("choice of [{}]", options.join(", ")),
    }
}

fn convert_property_value(
    component: &ComponentTypeId,
    property_id: &PropertyId,
    kind: &PropertyKind,
    param: PropertyValueParam,
) -> Result<PropertyValue, String> {
    let kind_name = param.kind_name();
    match (kind, param) {
        (PropertyKind::Scalar(dimension), PropertyValueParam::Scalar { si_value }) => {
            Quantity::new(si_value, *dimension)
                .map(PropertyValue::Scalar)
                .map_err(|error| {
                    format!("component '{component}' property '{property_id}': {error}")
                })
        }
        (PropertyKind::Vector(dimension), PropertyValueParam::Vector { x, y, z }) => {
            VectorQuantity::new(DVec3::new(x, y, z), *dimension)
                .map(PropertyValue::Vector)
                .map_err(|error| {
                    format!("component '{component}' property '{property_id}': {error}")
                })
        }
        (PropertyKind::Boolean, PropertyValueParam::Boolean { value }) => {
            Ok(PropertyValue::Boolean(value))
        }
        (PropertyKind::Text, PropertyValueParam::Text { value }) => Ok(PropertyValue::Text(value)),
        (PropertyKind::Choice(options), PropertyValueParam::Choice { value }) => {
            if options.contains(&value) {
                Ok(PropertyValue::Choice(value))
            } else {
                Err(format!(
                    "component '{component}' property '{property_id}': '{value}' is not one of [{}]",
                    options.join(", ")
                ))
            }
        }
        (kind, _) => Err(format!(
            "component '{component}' property '{property_id}': expected {}, got {kind_name}",
            describe_property_kind(kind)
        )),
    }
}

fn convert_properties(
    schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
    component: &ComponentTypeId,
    properties: BTreeMap<String, PropertyValueParam>,
) -> Result<PropertyBag, String> {
    let schema = schemas.get(component).ok_or_else(|| {
        format!(
            "component schema '{component}' is not registered; call list_component_schemas to discover valid components"
        )
    })?;
    let mut bag = PropertyBag::default();
    for (name, value) in properties {
        let property_id = PropertyId::new(name.clone())
            .map_err(|error| format!("component '{component}' property '{name}': {error}"))?;
        let property = schema
            .properties
            .iter()
            .find(|candidate| candidate.id == property_id)
            .ok_or_else(|| {
                format!(
                    "component '{component}' has no property '{property_id}'; valid properties: [{}]",
                    schema
                        .properties
                        .iter()
                        .map(|candidate| candidate.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let converted = convert_property_value(component, &property_id, &property.kind, value)?;
        bag.insert(property_id, converted);
    }
    Ok(bag)
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ComponentAttachParam {
    pub component: ComponentRefParam,
    pub properties: BTreeMap<String, PropertyValueParam>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProbePositionParam {
    World { position: Vec3Param },
    Attached { object: u64, offset: Vec3Param },
}

impl From<ProbePositionParam> for ProbePosition {
    fn from(value: ProbePositionParam) -> Self {
        match value {
            ProbePositionParam::World { position } => ProbePosition::World(position.into()),
            ProbePositionParam::Attached { object, offset } => ProbePosition::Attached {
                object: ObjectId::new(object),
                offset: offset.into(),
            },
        }
    }
}

/// One typed world-mutation operation. Entity references are stable numeric
/// IDs (from `get_world`, or a previous transaction's
/// [`fieldcad_simulation::CommandReceipt::created`]); component/channel
/// references are `{ plugin, name }` pairs.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WorldEditParam {
    CreateObject {
        name: String,
        #[serde(default)]
        transform: Option<TransformParam>,
        #[serde(default)]
        velocity: Option<VelocityParam>,
        #[serde(default)]
        shape: Option<ObjectShapeParam>,
        #[serde(default = "default_true")]
        visible: bool,
        #[serde(default)]
        pinned: bool,
        #[serde(default)]
        components: Vec<ComponentAttachParam>,
    },
    RemoveObject {
        object: u64,
    },
    SetObjectName {
        object: u64,
        name: String,
    },
    SetTransform {
        object: u64,
        transform: TransformParam,
    },
    SetVelocity {
        object: u64,
        velocity: VelocityParam,
    },
    SetShape {
        object: u64,
        #[serde(default)]
        shape: Option<ObjectShapeParam>,
    },
    SetObjectVisible {
        object: u64,
        visible: bool,
    },
    /// Hand authority over an object's motion to the user, or back to solvers.
    SetObjectPinned {
        object: u64,
        pinned: bool,
    },
    AttachComponent {
        object: u64,
        component: ComponentRefParam,
        properties: BTreeMap<String, PropertyValueParam>,
    },
    DetachComponent {
        object: u64,
        component: ComponentRefParam,
    },
    CreatePlane {
        name: String,
        origin: Vec3Param,
        normal: Vec3Param,
        #[serde(default)]
        half_extent: Option<Vec2Param>,
        #[serde(default)]
        u_axis: Option<Vec3Param>,
        #[serde(default = "default_true")]
        visible: bool,
    },
    SetPlaneName {
        plane: u64,
        name: String,
    },
    SetPlane {
        plane: u64,
        name: String,
        origin: Vec3Param,
        normal: Vec3Param,
        #[serde(default)]
        half_extent: Option<Vec2Param>,
        #[serde(default)]
        u_axis: Option<Vec3Param>,
        #[serde(default = "default_true")]
        visible: bool,
    },
    SetPlaneVisible {
        plane: u64,
        visible: bool,
    },
    RemovePlane {
        plane: u64,
    },
    CreateBox {
        name: String,
        origin: Vec3Param,
        half_extent: Vec3Param,
        #[serde(default)]
        rotation: QuatParam,
        #[serde(default = "default_true")]
        visible: bool,
    },
    SetBoxName {
        region: u64,
        name: String,
    },
    SetBox {
        region: u64,
        name: String,
        origin: Vec3Param,
        half_extent: Vec3Param,
        #[serde(default)]
        rotation: QuatParam,
        #[serde(default = "default_true")]
        visible: bool,
    },
    SetBoxVisible {
        region: u64,
        visible: bool,
    },
    RemoveBox {
        region: u64,
    },
    CreateSphere {
        name: String,
        origin: Vec3Param,
        radius_m: f64,
        #[serde(default = "default_true")]
        visible: bool,
    },
    SetSphereName {
        sphere: u64,
        name: String,
    },
    SetSphere {
        sphere: u64,
        name: String,
        origin: Vec3Param,
        radius_m: f64,
        #[serde(default = "default_true")]
        visible: bool,
    },
    SetSphereVisible {
        sphere: u64,
        visible: bool,
    },
    RemoveSphere {
        sphere: u64,
    },
    CreateProbe {
        name: String,
        position: ProbePositionParam,
        #[serde(default)]
        channels: Vec<ChannelRefParam>,
        #[serde(default = "default_true")]
        visible: bool,
        #[serde(default)]
        history_capacity: Option<usize>,
    },
    SetProbeName {
        probe: u64,
        name: String,
    },
    SetProbePosition {
        probe: u64,
        position: ProbePositionParam,
    },
    SetProbeChannels {
        probe: u64,
        channels: Vec<ChannelRefParam>,
    },
    SetProbeVisible {
        probe: u64,
        visible: bool,
    },
    RemoveProbe {
        probe: u64,
    },
}

/// One atomic `edit_world` transaction: typed commands discovered from tool
/// schemas rather than reverse-engineered `WorldCommand` serde shapes.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditWorldParams {
    /// Applied in order, as one atomic transaction. Component-property kinds
    /// are validated against schemas already registered in the world before
    /// submission; discover them with `list_component_schemas`.
    pub commands: Vec<WorldEditParam>,
}

fn describe_world_error(error: WorldError) -> String {
    error.to_string()
}

/// Convert one typed operation into the authoritative [`WorldCommand`] it
/// describes, validating identifiers, finite geometry, and component-property
/// kinds against `schemas` (the world's currently registered component
/// schemas). The runtime remains the final validator for everything this
/// cannot know from a schema alone — whether an entity ID actually resolves,
/// or cross-entity invariants such as a probe's attachment target existing.
pub fn into_world_command(
    schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
    param: WorldEditParam,
) -> Result<WorldCommand, String> {
    let command = match param {
        WorldEditParam::CreateObject {
            name,
            transform,
            velocity,
            shape,
            visible,
            pinned,
            components,
        } => {
            let mut spec = ObjectSpec::new(name)
                .with_visibility(visible)
                .with_pinned(pinned);
            if let Some(transform) = transform {
                let transform: Transform = transform.try_into().map_err(describe_world_error)?;
                spec = spec.with_transform(transform);
            }
            if let Some(velocity) = velocity {
                let velocity: Velocity = velocity.try_into().map_err(describe_world_error)?;
                spec = spec.with_velocity(velocity);
            }
            if let Some(shape) = shape {
                let shape: ObjectShape = shape.try_into().map_err(describe_world_error)?;
                spec = spec.with_shape(shape);
            }
            for attach in components {
                let component = attach.component.resolve()?;
                let bag = convert_properties(schemas, &component, attach.properties)?;
                spec = spec.with_component(component, bag);
            }
            WorldCommand::CreateObject(spec)
        }
        WorldEditParam::RemoveObject { object } => {
            WorldCommand::RemoveObject(ObjectId::new(object))
        }
        WorldEditParam::SetObjectName { object, name } => WorldCommand::SetObjectName {
            object: ObjectId::new(object),
            name,
        },
        WorldEditParam::SetTransform { object, transform } => WorldCommand::SetTransform {
            object: ObjectId::new(object),
            transform: transform.try_into().map_err(describe_world_error)?,
        },
        WorldEditParam::SetVelocity { object, velocity } => WorldCommand::SetVelocity {
            object: ObjectId::new(object),
            velocity: velocity.try_into().map_err(describe_world_error)?,
        },
        WorldEditParam::SetShape { object, shape } => WorldCommand::SetShape {
            object: ObjectId::new(object),
            shape: shape
                .map(TryInto::try_into)
                .transpose()
                .map_err(describe_world_error)?,
        },
        WorldEditParam::SetObjectVisible { object, visible } => WorldCommand::SetObjectVisible {
            object: ObjectId::new(object),
            visible,
        },
        WorldEditParam::SetObjectPinned { object, pinned } => WorldCommand::SetObjectPinned {
            object: ObjectId::new(object),
            pinned,
        },
        WorldEditParam::AttachComponent {
            object,
            component,
            properties,
        } => {
            let component = component.resolve()?;
            let bag = convert_properties(schemas, &component, properties)?;
            WorldCommand::AttachComponent {
                object: ObjectId::new(object),
                component,
                properties: bag,
            }
        }
        WorldEditParam::DetachComponent { object, component } => WorldCommand::DetachComponent {
            object: ObjectId::new(object),
            component: component.resolve()?,
        },
        WorldEditParam::CreatePlane {
            name,
            origin,
            normal,
            half_extent,
            u_axis,
            visible,
        } => {
            let mut spec = SlicePlaneSpec::new(name, origin.into(), normal.into())
                .map_err(describe_world_error)?;
            if let Some(half_extent) = half_extent {
                spec = spec
                    .with_half_extent(half_extent.into())
                    .map_err(describe_world_error)?;
            }
            if let Some(u_axis) = u_axis {
                spec = spec
                    .with_u_axis(u_axis.into())
                    .map_err(describe_world_error)?;
            }
            WorldCommand::CreatePlane(spec.with_visibility(visible))
        }
        WorldEditParam::SetPlaneName { plane, name } => WorldCommand::SetPlaneName {
            plane: PlaneId::new(plane),
            name,
        },
        WorldEditParam::SetPlane {
            plane,
            name,
            origin,
            normal,
            half_extent,
            u_axis,
            visible,
        } => {
            let mut spec = SlicePlaneSpec::new(name, origin.into(), normal.into())
                .map_err(describe_world_error)?;
            if let Some(half_extent) = half_extent {
                spec = spec
                    .with_half_extent(half_extent.into())
                    .map_err(describe_world_error)?;
            }
            if let Some(u_axis) = u_axis {
                spec = spec
                    .with_u_axis(u_axis.into())
                    .map_err(describe_world_error)?;
            }
            WorldCommand::SetPlane {
                plane: PlaneId::new(plane),
                spec: spec.with_visibility(visible),
            }
        }
        WorldEditParam::SetPlaneVisible { plane, visible } => WorldCommand::SetPlaneVisible {
            plane: PlaneId::new(plane),
            visible,
        },
        WorldEditParam::RemovePlane { plane } => WorldCommand::RemovePlane(PlaneId::new(plane)),
        WorldEditParam::CreateBox {
            name,
            origin,
            half_extent,
            rotation,
            visible,
        } => {
            let spec = FieldBoxSpec::new(name, origin.into(), half_extent.into())
                .map_err(describe_world_error)?
                .with_rotation(rotation.into())
                .map_err(describe_world_error)?
                .with_visibility(visible);
            WorldCommand::CreateBox(spec)
        }
        WorldEditParam::SetBoxName { region, name } => WorldCommand::SetBoxName {
            region: BoxId::new(region),
            name,
        },
        WorldEditParam::SetBox {
            region,
            name,
            origin,
            half_extent,
            rotation,
            visible,
        } => {
            let spec = FieldBoxSpec::new(name, origin.into(), half_extent.into())
                .map_err(describe_world_error)?
                .with_rotation(rotation.into())
                .map_err(describe_world_error)?
                .with_visibility(visible);
            WorldCommand::SetBox {
                region: BoxId::new(region),
                spec,
            }
        }
        WorldEditParam::SetBoxVisible { region, visible } => WorldCommand::SetBoxVisible {
            region: BoxId::new(region),
            visible,
        },
        WorldEditParam::RemoveBox { region } => WorldCommand::RemoveBox(BoxId::new(region)),
        WorldEditParam::CreateSphere {
            name,
            origin,
            radius_m,
            visible,
        } => {
            let spec = FieldSphereSpec::new(name, origin.into(), radius_m)
                .map_err(describe_world_error)?
                .with_visibility(visible);
            WorldCommand::CreateSphere(spec)
        }
        WorldEditParam::SetSphereName { sphere, name } => WorldCommand::SetSphereName {
            sphere: SphereId::new(sphere),
            name,
        },
        WorldEditParam::SetSphere {
            sphere,
            name,
            origin,
            radius_m,
            visible,
        } => {
            let spec = FieldSphereSpec::new(name, origin.into(), radius_m)
                .map_err(describe_world_error)?
                .with_visibility(visible);
            WorldCommand::SetSphere {
                sphere: SphereId::new(sphere),
                spec,
            }
        }
        WorldEditParam::SetSphereVisible { sphere, visible } => WorldCommand::SetSphereVisible {
            sphere: SphereId::new(sphere),
            visible,
        },
        WorldEditParam::RemoveSphere { sphere } => {
            WorldCommand::RemoveSphere(SphereId::new(sphere))
        }
        WorldEditParam::CreateProbe {
            name,
            position,
            channels,
            visible,
            history_capacity,
        } => {
            let channels = channels
                .into_iter()
                .map(ChannelRefParam::resolve)
                .collect::<Result<Vec<_>, _>>()?;
            let mut spec = match ProbePosition::from(position) {
                ProbePosition::World(position) => ProbeSpec::at(name, position, channels),
                ProbePosition::Attached { object, offset } => {
                    ProbeSpec::attached(name, object, offset, channels)
                }
            };
            spec = spec.with_visibility(visible);
            if let Some(capacity) = history_capacity {
                spec = spec.with_history_capacity(capacity);
            }
            WorldCommand::CreateProbe(spec)
        }
        WorldEditParam::SetProbeName { probe, name } => WorldCommand::SetProbeName {
            probe: ProbeId::new(probe),
            name,
        },
        WorldEditParam::SetProbePosition { probe, position } => WorldCommand::SetProbePosition {
            probe: ProbeId::new(probe),
            position: position.into(),
        },
        WorldEditParam::SetProbeChannels { probe, channels } => {
            let channels = channels
                .into_iter()
                .map(ChannelRefParam::resolve)
                .collect::<Result<Vec<_>, _>>()?;
            WorldCommand::SetProbeChannels {
                probe: ProbeId::new(probe),
                channels,
            }
        }
        WorldEditParam::SetProbeVisible { probe, visible } => WorldCommand::SetProbeVisible {
            probe: ProbeId::new(probe),
            visible,
        },
        WorldEditParam::RemoveProbe { probe } => WorldCommand::RemoveProbe(ProbeId::new(probe)),
    };
    Ok(command)
}
