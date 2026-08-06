//! The shared electromagnetic model: what its inputs and its fields are called.
//!
//! Neither end of an equation system belongs to one numerical model. Charge is a
//! property of a world object, and the electric field is a property of the
//! scene — a scene has one, however it is computed. This module owns both stable
//! schemas so that electrostatics and time-domain electromagnetism describe the
//! same physics with the same names, without depending on one another.
//!
//! A channel declared here is a *physical field*, not a plugin's output. Any
//! equation system may declare it, at most one active system computes it, and
//! which one is a choice of model rather than a second field (ADR 0025).
//! Channels that only make sense for one method — an FDTD divergence residual,
//! say — stay in that plugin's own namespace, because they are diagnostics of a
//! discretization rather than quantities the world has.

use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, ComponentTypeId, Dimension, FieldValueKind,
    ObjectId, PluginId, PointOrSphere, PointOrSphereError, PropertyBag, PropertyId, PropertyKind,
    PropertySchema, PropertyValue, Quantity, QuantityError, Velocity, WorldObject, WorldSnapshot,
};
use glam::DVec3;

pub const SCHEMA_NAMESPACE: &str = "fieldcad.electromagnetic-sources";
pub const CHARGE_COMPONENT: &str = "charge-source";
pub const CHARGE_PROPERTY: &str = "charge";

/// Namespace for the electromagnetic fields themselves, distinct from the
/// sources that produce them and from any plugin that computes them.
pub const FIELD_NAMESPACE: &str = "fieldcad.electromagnetic-field";
pub const ELECTRIC_FIELD_CHANNEL: &str = "electric-field";
pub const MAGNETIC_FIELD_CHANNEL: &str = "magnetic-flux-density";
pub const ELECTRIC_POTENTIAL_CHANNEL: &str = "electric-potential";

pub fn schema_namespace_id() -> PluginId {
    PluginId::new(SCHEMA_NAMESPACE).expect("static schema namespace is valid")
}

pub fn field_namespace_id() -> PluginId {
    PluginId::new(FIELD_NAMESPACE).expect("static field namespace is valid")
}

fn field_channel_id(name: &str) -> ChannelId {
    ChannelId::new(field_namespace_id(), name).expect("static channel ID is valid")
}

pub fn electric_field_channel_id() -> ChannelId {
    field_channel_id(ELECTRIC_FIELD_CHANNEL)
}

pub fn magnetic_field_channel_id() -> ChannelId {
    field_channel_id(MAGNETIC_FIELD_CHANNEL)
}

pub fn electric_potential_channel_id() -> ChannelId {
    field_channel_id(ELECTRIC_POTENTIAL_CHANNEL)
}

/// The schema every provider of `E` must declare, character for character.
///
/// Returned whole rather than as an identifier so two systems cannot describe
/// one field with different names or units and have the difference show up as a
/// composition failure at startup instead of as a shared constant.
pub fn electric_field_channel_schema() -> ChannelSchema {
    ChannelSchema {
        id: electric_field_channel_id(),
        display_name: "Electric field E".to_owned(),
        value_kind: FieldValueKind::Vector(Dimension::ELECTRIC_FIELD),
    }
}

pub fn magnetic_field_channel_schema() -> ChannelSchema {
    ChannelSchema {
        id: magnetic_field_channel_id(),
        display_name: "Magnetic field B".to_owned(),
        value_kind: FieldValueKind::Vector(Dimension::MAGNETIC_FLUX_DENSITY),
    }
}

pub fn electric_potential_channel_schema() -> ChannelSchema {
    ChannelSchema {
        id: electric_potential_channel_id(),
        display_name: "Electric potential".to_owned(),
        value_kind: FieldValueKind::Scalar(Dimension::ELECTRIC_POTENTIAL),
    }
}

pub fn charge_component_id() -> ComponentTypeId {
    ComponentTypeId::new(schema_namespace_id(), CHARGE_COMPONENT)
        .expect("static component ID is valid")
}

pub fn charge_property_id() -> PropertyId {
    PropertyId::new(CHARGE_PROPERTY).expect("static property ID is valid")
}

pub fn charge_component_schema() -> ComponentSchema {
    ComponentSchema {
        id: charge_component_id(),
        display_name: "Charge source".to_owned(),
        properties: vec![PropertySchema {
            id: charge_property_id(),
            display_name: "Charge".to_owned(),
            kind: PropertyKind::Scalar(Dimension::CHARGE),
            required: true,
            default_value: None,
            relevant_when: None,
        }],
    }
}

pub fn charge_properties(coulombs: f64) -> Result<PropertyBag, QuantityError> {
    Ok([(
        charge_property_id(),
        PropertyValue::Scalar(Quantity::new(coulombs, Dimension::CHARGE)?),
    )]
    .into_iter()
    .collect())
}

/// The variants mirror [`fieldcad_mass_sources::MassDistribution`] because
/// the geometry question is the same one — [`PointOrSphere`] answers it
/// once, shared by both — the quantity spread over that geometry is what
/// differs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChargeDistribution {
    Point { exclusion_radius: f64 },
    UniformSphere { radius: f64 },
}

impl From<PointOrSphere> for ChargeDistribution {
    fn from(value: PointOrSphere) -> Self {
        match value {
            PointOrSphere::Point { exclusion_radius } => Self::Point { exclusion_radius },
            PointOrSphere::UniformSphere { radius } => Self::UniformSphere { radius },
        }
    }
}

impl From<ChargeDistribution> for PointOrSphere {
    fn from(value: ChargeDistribution) -> Self {
        match value {
            ChargeDistribution::Point { exclusion_radius } => Self::Point { exclusion_radius },
            ChargeDistribution::UniformSphere { radius } => Self::UniformSphere { radius },
        }
    }
}

/// The exclusion radius given to a charged object with no authored shape.
pub const DEFAULT_POINT_RADIUS: f64 = fieldcad_core::DEFAULT_PROXY_RADIUS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChargeSource {
    /// Stable authored identity retained for deposition and particle coupling.
    pub object: ObjectId,
    pub position: DVec3,
    pub velocity: Velocity,
    pub charge_coulombs: f64,
    pub distribution: ChargeDistribution,
}

/// Extract every supported authored charge in deterministic object-ID order.
pub fn collect_charge_sources(
    world: &WorldSnapshot,
) -> Result<Vec<ChargeSource>, ChargeSourceError> {
    world
        .objects_with(&charge_component_id())
        .map(|(object, properties)| source_from_object(object, properties))
        .collect()
}

fn source_from_object(
    object: &WorldObject,
    properties: &PropertyBag,
) -> Result<ChargeSource, ChargeSourceError> {
    let charge_coulombs = properties.scalar(&charge_property_id()).ok_or_else(|| {
        ChargeSourceError::InvalidCharge {
            object: object.name.clone(),
        }
    })?;
    let distribution = PointOrSphere::from_shape(object.shape, DEFAULT_POINT_RADIUS)
        .map_err(|error| match error {
            PointOrSphereError::NonPositiveSphere => ChargeSourceError::NonPositiveSphere {
                object: object.name.clone(),
            },
            PointOrSphereError::UnsupportedShape => ChargeSourceError::UnsupportedShape {
                object: object.name.clone(),
            },
        })?
        .into();
    Ok(ChargeSource {
        object: object.id,
        position: object.transform.translation,
        velocity: object.velocity,
        charge_coulombs,
        distribution,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChargeSourceError {
    #[error("object '{object}' has a charge component without a scalar charge")]
    InvalidCharge { object: String },
    #[error("charged sphere '{object}' must have a positive radius")]
    NonPositiveSphere { object: String },
    #[error("charged object '{object}' must use a point or sphere shape")]
    UnsupportedShape { object: String },
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectShape, ObjectSpec, Transform, World, WorldCommand};

    use super::*;

    #[test]
    fn point_and_sphere_objects_become_solver_neutral_charge_sources() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("point")
                        .with_transform(Transform::at(DVec3::X).unwrap())
                        .with_shape(ObjectShape::point(0.1).unwrap())
                        .with_component(charge_component_id(), charge_properties(2.0).unwrap()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("sphere")
                        .with_shape(ObjectShape::sphere(0.5).unwrap())
                        .with_component(charge_component_id(), charge_properties(-3.0).unwrap()),
                ),
            ])
            .unwrap();

        let sources = collect_charge_sources(&world.snapshot()).unwrap();

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].object, ObjectId::new(0));
        assert_eq!(sources[0].position, DVec3::X);
        assert_eq!(sources[0].velocity, Velocity::default());
        assert_eq!(sources[0].charge_coulombs, 2.0);
        assert_eq!(
            sources[1].distribution,
            ChargeDistribution::UniformSphere { radius: 0.5 }
        );
    }

    #[test]
    fn a_shapeless_gizmo_given_charge_is_a_point_charge() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("gizmo")
                        .with_component(charge_component_id(), charge_properties(1.0).unwrap()),
                ),
            ])
            .unwrap();

        let sources = collect_charge_sources(&world.snapshot()).unwrap();

        assert_eq!(
            sources[0].distribution,
            ChargeDistribution::Point {
                exclusion_radius: DEFAULT_POINT_RADIUS
            }
        );
    }

    #[test]
    fn unsupported_geometry_is_rejected_once_for_every_consumer() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("box")
                        .with_shape(ObjectShape::boxed(DVec3::ONE).unwrap())
                        .with_component(charge_component_id(), charge_properties(1.0).unwrap()),
                ),
            ])
            .unwrap();

        assert_eq!(
            collect_charge_sources(&world.snapshot()),
            Err(ChargeSourceError::UnsupportedShape {
                object: "box".to_owned()
            })
        );
    }
}
