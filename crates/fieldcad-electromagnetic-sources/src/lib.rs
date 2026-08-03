//! Shared authored sources consumed by electrostatic and time-domain
//! electromagnetic equation systems.
//!
//! Charge is a property of the world object, not of one numerical model. This
//! module owns its stable schema and translates supported object geometry into
//! a solver-neutral source description. Equation-system plugins may contribute
//! the same schema and consume these sources without depending on one another.

use fieldcad_core::{
    ComponentSchema, ComponentTypeId, Dimension, ObjectId, ObjectShape, PluginId, PropertyBag,
    PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity, QuantityError, Velocity,
    WorldObject, WorldSnapshot,
};
use glam::DVec3;

pub const SCHEMA_NAMESPACE: &str = "fieldcad.electromagnetic-sources";
pub const CHARGE_COMPONENT: &str = "charge-source";
pub const CHARGE_PROPERTY: &str = "charge";

pub fn schema_namespace_id() -> PluginId {
    PluginId::new(SCHEMA_NAMESPACE).expect("static schema namespace is valid")
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChargeDistribution {
    Point { exclusion_radius: f64 },
    UniformSphere { radius: f64 },
}

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
    let distribution = match object.shape {
        Some(ObjectShape::Point { radius }) => ChargeDistribution::Point {
            exclusion_radius: radius,
        },
        Some(ObjectShape::Sphere { radius }) if radius > 0.0 => {
            ChargeDistribution::UniformSphere { radius }
        }
        Some(ObjectShape::Sphere { .. }) => {
            return Err(ChargeSourceError::NonPositiveSphere {
                object: object.name.clone(),
            });
        }
        _ => {
            return Err(ChargeSourceError::UnsupportedShape {
                object: object.name.clone(),
            });
        }
    };
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
    use fieldcad_core::{ObjectSpec, Transform, World, WorldCommand};

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
