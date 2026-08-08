//! The shared inertial- and gravitational-mass object components.
//!
//! Mass does two unrelated jobs that happen to take the same number in every
//! experiment performed so far, and this crate keeps them apart:
//!
//! - **Inertial mass** is the constant relating force to acceleration. Every
//!   body that can be pushed has one, whatever is pushing it, so it belongs to
//!   the dynamics system rather than to any field.
//! - **Gravitational mass** is a coupling charge: it is to the gravitational
//!   field what electric charge is to the electromagnetic one. Only bodies that
//!   gravitate carry it.
//!
//! Their numerical equality is the weak equivalence principle — an experimental
//! result, not a definition. Modelling them as one number would build that
//! result into the tool and make "what if they differed?" unaskable. So they are
//! separate components, linked by default because that is what the universe
//! appears to do, and separable because a simulator whose premises cannot be
//! varied is not an instrument.
//!
//! This mirrors [`fieldcad_electromagnetic_sources`]: one crate per shared
//! physical quantity, so attaching mass to an object never drags in a motion
//! model, a catalog identity, or a charge.

use fieldcad_core::quantities::{MassKg, SiScalar};
use fieldcad_core::{
    ChargeDistribution, ComponentSchema, ComponentTypeId, Dimension, PluginId, PointOrSphereError,
    PropertyBag, PropertyCondition, PropertyId, PropertyKind, PropertySchema, PropertyValue,
    Quantity, QuantityError, WorldObject, WorldSnapshot,
};

pub const SCHEMA_NAMESPACE: &str = "fieldcad.mass-sources";
pub const INERTIAL_MASS_COMPONENT: &str = "inertial-mass";
pub const GRAVITATIONAL_MASS_COMPONENT: &str = "gravitational-mass";
pub const MASS_PROPERTY: &str = "mass";
pub const FOLLOWS_INERTIAL_PROPERTY: &str = "follows-inertial";

pub fn schema_namespace_id() -> PluginId {
    PluginId::new(SCHEMA_NAMESPACE).expect("static schema namespace is valid")
}

pub fn inertial_mass_component_id() -> ComponentTypeId {
    ComponentTypeId::new(schema_namespace_id(), INERTIAL_MASS_COMPONENT)
        .expect("static component ID is valid")
}

pub fn gravitational_mass_component_id() -> ComponentTypeId {
    ComponentTypeId::new(schema_namespace_id(), GRAVITATIONAL_MASS_COMPONENT)
        .expect("static component ID is valid")
}

pub fn mass_property_id() -> PropertyId {
    PropertyId::new(MASS_PROPERTY).expect("static property ID is valid")
}

pub fn follows_inertial_property_id() -> PropertyId {
    PropertyId::new(FOLLOWS_INERTIAL_PROPERTY).expect("static property ID is valid")
}

/// The mass given to an object when either component is first attached.
///
/// Mass must be strictly positive — inertia divides — so this component cannot
/// take the zero that a dimension-only schema would otherwise default to. One
/// kilogram is an obvious placeholder: large enough to be visibly inert under
/// the nanocoulomb charges these scenes use, so an object that has just been
/// made movable does not leap out of the domain before the user has set a real
/// value.
const DEFAULT_MASS_KILOGRAMS: f64 = 1.0;

fn mass_property_schema() -> PropertySchema {
    PropertySchema {
        id: mass_property_id(),
        display_name: "Mass".to_owned(),
        kind: PropertyKind::Scalar(Dimension::MASS),
        required: true,
        relevant_when: None,
        default_value: Some(PropertyValue::Scalar(
            Quantity::new(DEFAULT_MASS_KILOGRAMS, Dimension::MASS)
                .expect("static default mass is finite"),
        )),
    }
}

/// The inertia that turns an accumulated force into an acceleration.
///
/// Attaching this is what makes a body dynamic: it says the object has somewhere
/// for a force to act. It says nothing about *which* field acts on it.
pub fn inertial_mass_component_schema() -> ComponentSchema {
    ComponentSchema {
        id: inertial_mass_component_id(),
        display_name: "Inertial mass".to_owned(),
        properties: vec![mass_property_schema()],
    }
}

/// The coupling charge for the gravitational field.
///
/// `follows-inertial` defaults to true, so the ordinary case needs no second
/// number and cannot drift out of step with the inertial mass by accident.
/// Clearing it is how a user asks what a world with a violated equivalence
/// principle would look like.
pub fn gravitational_mass_component_schema() -> ComponentSchema {
    ComponentSchema {
        id: gravitational_mass_component_id(),
        display_name: "Gravitational mass".to_owned(),
        properties: vec![
            PropertySchema {
                id: follows_inertial_property_id(),
                display_name: "Equal to inertial mass".to_owned(),
                kind: PropertyKind::Boolean,
                required: true,
                default_value: Some(PropertyValue::Boolean(true)),
                relevant_when: None,
            },
            // Declared second so a generic editor renders the switch above the
            // value it governs, and conditional on that switch so the value
            // cannot be edited while it is being ignored. A number a user can
            // type but the model will not read is worse than no field at all.
            PropertySchema {
                relevant_when: Some(PropertyCondition {
                    property: follows_inertial_property_id(),
                    equals: PropertyValue::Boolean(false),
                    because: "This body's gravitational mass is its inertial mass. \
                              Clear “Equal to inertial mass” to set it independently."
                        .to_owned(),
                }),
                ..mass_property_schema()
            },
        ],
    }
}

/// Both schemas, for a plugin declaring what it consumes.
pub fn mass_component_schemas() -> Vec<ComponentSchema> {
    vec![
        inertial_mass_component_schema(),
        gravitational_mass_component_schema(),
    ]
}

pub fn inertial_mass_properties(kilograms: MassKg) -> Result<PropertyBag, QuantityError> {
    Ok([(
        mass_property_id(),
        PropertyValue::Scalar(Quantity::new(kilograms.into_si(), Dimension::MASS)?),
    )]
    .into_iter()
    .collect())
}

/// A gravitational mass that tracks whatever inertial mass the body has.
pub fn linked_gravitational_mass_properties() -> PropertyBag {
    [
        (follows_inertial_property_id(), PropertyValue::Boolean(true)),
        (
            mass_property_id(),
            PropertyValue::Scalar(
                Quantity::new(DEFAULT_MASS_KILOGRAMS, Dimension::MASS)
                    .expect("static mass is finite"),
            ),
        ),
    ]
    .into_iter()
    .collect()
}

/// A gravitational mass authored independently of the body's inertia.
pub fn independent_gravitational_mass_properties(
    kilograms: MassKg,
) -> Result<PropertyBag, QuantityError> {
    Ok([
        (
            follows_inertial_property_id(),
            PropertyValue::Boolean(false),
        ),
        (
            mass_property_id(),
            PropertyValue::Scalar(Quantity::new(kilograms.into_si(), Dimension::MASS)?),
        ),
    ]
    .into_iter()
    .collect())
}

pub use fieldcad_core::CoupledSource;

pub type MassDistribution = ChargeDistribution;

/// The exclusion radius given to a massive body with no authored shape.
pub const DEFAULT_POINT_RADIUS: f64 = fieldcad_core::DEFAULT_PROXY_RADIUS;

/// Extract every body that gravitates as a `CoupledSource<MassKg>`, including those
/// whose inertia was never authored.
pub fn collect_gravity_sources(
    world: &WorldSnapshot,
) -> Result<Vec<CoupledSource<MassKg>>, SourceError> {
    world
        .objects()
        .values()
        .filter_map(|object| match gravitational_mass_of(object) {
            Ok(Some(mass)) => Some(source_from_object_for_coupling(object, mass)),
            Ok(None) => None,
            Err(err) => Some(Err(err)),
        })
        .collect()
}

/// The gravitational coupling charge of one object, resolving the link.
pub fn gravitational_mass_of(object: &WorldObject) -> Result<Option<MassKg>, SourceError> {
    let Some(properties) = object.components.get(&gravitational_mass_component_id()) else {
        return Ok(None);
    };
    let follows = matches!(
        properties.get(&follows_inertial_property_id()),
        Some(PropertyValue::Boolean(true))
    );
    if follows {
        return inertial_mass_of(object).map(Some);
    }
    let mass = properties.scalar(&mass_property_id()).ok_or_else(|| {
        SourceError::InvalidGravitationalMass {
            object: object.name.clone(),
        }
    })?;
    if !mass.is_finite() || mass < 0.0 {
        return Err(SourceError::InvalidGravitationalMass {
            object: object.name.clone(),
        });
    }
    Ok(Some(MassKg::from_si(mass)))
}

pub fn inertial_mass_of(object: &WorldObject) -> Result<MassKg, SourceError> {
    let mass = object
        .components
        .get(&inertial_mass_component_id())
        .and_then(|properties| properties.scalar(&mass_property_id()))
        .ok_or_else(|| SourceError::InvalidMass {
            object: object.name.clone(),
        })?;
    if !mass.is_finite() || mass <= 0.0 {
        return Err(SourceError::InvalidMass {
            object: object.name.clone(),
        });
    }
    Ok(MassKg::from_si(mass))
}

fn source_from_object_for_coupling(
    object: &WorldObject,
    coupling_value: MassKg,
) -> Result<CoupledSource<MassKg>, SourceError> {
    let distribution =
        ChargeDistribution::from_shape(object.shape, DEFAULT_POINT_RADIUS).map_err(|error| {
            match error {
                PointOrSphereError::NonPositiveSphere => SourceError::NonPositiveSphere {
                    object: object.name.clone(),
                },
                PointOrSphereError::UnsupportedShape => SourceError::UnsupportedShape {
                    object: object.name.clone(),
                },
            }
        })?;
    Ok(CoupledSource::new(
        object.id,
        object.transform.translation,
        object.velocity,
        coupling_value,
        distribution,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("object '{object}' must have a finite, positive inertial mass")]
    InvalidMass { object: String },
    #[error("object '{object}' must have a finite, non-negative gravitational mass")]
    InvalidGravitationalMass { object: String },
    #[error("sphere source '{object}' must have a positive radius")]
    NonPositiveSphere { object: String },
    #[error("source object '{object}' must use a point or sphere shape")]
    UnsupportedShape { object: String },
}

pub type MassSourceError = SourceError;

#[cfg(test)]
mod tests {
    use fieldcad_core::quantities::kilogram;
    use fieldcad_core::{ObjectId, ObjectSpec, Transform, World, WorldCommand};
    use glam::DVec3;

    use super::*;

    fn world_with(specs: impl IntoIterator<Item = ObjectSpec>) -> World {
        let mut world = World::new();
        let commands = mass_component_schemas()
            .into_iter()
            .map(WorldCommand::RegisterComponentSchema)
            .chain(specs.into_iter().map(WorldCommand::CreateObject));
        world.commit(commands).unwrap();
        world
    }

    fn inertial(kilograms: f64) -> (ComponentTypeId, PropertyBag) {
        (
            inertial_mass_component_id(),
            inertial_mass_properties(MassKg::new::<kilogram>(kilograms)).unwrap(),
        )
    }

    #[test]
    fn a_shapeless_gizmo_given_mass_is_a_point_body() {
        let (component, properties) = inertial(2.0);
        let world = world_with([ObjectSpec::new("gizmo")
            .with_transform(Transform::at(DVec3::Y).unwrap())
            .with_component(component, properties)
            .with_component(
                gravitational_mass_component_id(),
                linked_gravitational_mass_properties(),
            )]);

        let sources = collect_gravity_sources(&world.snapshot()).unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].coupling_value, MassKg::new::<kilogram>(2.0));
        assert_eq!(sources[0].position, DVec3::Y);
        assert_eq!(
            sources[0].distribution,
            ChargeDistribution::Point {
                exclusion_radius: DEFAULT_POINT_RADIUS
            }
        );
    }

    #[test]
    fn inertia_alone_does_not_make_a_body_gravitate() {
        let (component, properties) = inertial(5.0);
        let world = world_with([ObjectSpec::new("inert").with_component(component, properties)]);

        let sources = collect_gravity_sources(&world.snapshot()).unwrap();

        assert!(sources.is_empty());
        assert_eq!(
            inertial_mass_of(world.snapshot().objects().values().next().unwrap()).unwrap(),
            MassKg::new::<kilogram>(5.0)
        );
        assert_eq!(
            gravitational_mass_of(world.snapshot().objects().values().next().unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn a_linked_gravitational_mass_tracks_inertia_rather_than_storing_a_copy() {
        let (component, properties) = inertial(3.0);
        let mut world = world_with([ObjectSpec::new("body")
            .with_component(component, properties)
            .with_component(
                gravitational_mass_component_id(),
                linked_gravitational_mass_properties(),
            )]);

        let sources = collect_gravity_sources(&world.snapshot()).unwrap();
        assert_eq!(sources[0].coupling_value, MassKg::new::<kilogram>(3.0));

        world
            .commit([WorldCommand::AttachComponent {
                object: ObjectId::new(0),
                component: inertial_mass_component_id(),
                properties: inertial_mass_properties(MassKg::new::<kilogram>(11.0)).unwrap(),
            }])
            .unwrap();

        let sources = collect_gravity_sources(&world.snapshot()).unwrap();
        assert_eq!(sources[0].coupling_value, MassKg::new::<kilogram>(11.0));
    }

    #[test]
    fn the_gravitational_mass_value_is_inert_while_it_follows_inertia() {
        let schema = gravitational_mass_component_schema();
        let mass = schema
            .properties
            .iter()
            .find(|property| property.id == mass_property_id())
            .expect("the component declares a mass value");

        assert!(!mass.is_relevant(&linked_gravitational_mass_properties()));
        assert!(
            mass.is_relevant(
                &independent_gravitational_mass_properties(MassKg::new::<kilogram>(2.0)).unwrap(),
            ),
            "unlinking must make the value editable again"
        );

        let switch = schema
            .properties
            .iter()
            .find(|property| property.id == follows_inertial_property_id())
            .expect("the component declares the link switch");
        assert!(switch.relevant_when.is_none());

        assert_eq!(schema.properties[0].id, follows_inertial_property_id());
    }

    #[test]
    fn the_inertial_mass_value_is_always_editable() {
        let schema = inertial_mass_component_schema();

        assert!(schema.properties[0].is_relevant(&PropertyBag::default()));
        assert!(schema.properties[0].relevant_when.is_none());
    }

    #[test]
    fn unlinking_allows_the_equivalence_principle_to_be_violated_on_purpose() {
        let (component, properties) = inertial(2.0);
        let world = world_with([ObjectSpec::new("odd")
            .with_component(component, properties)
            .with_component(
                gravitational_mass_component_id(),
                independent_gravitational_mass_properties(MassKg::new::<kilogram>(7.0)).unwrap(),
            )]);

        let sources = collect_gravity_sources(&world.snapshot()).unwrap();

        assert_eq!(sources[0].coupling_value, MassKg::new::<kilogram>(7.0));
        assert_eq!(
            inertial_mass_of(world.snapshot().objects().values().next().unwrap()).unwrap(),
            MassKg::new::<kilogram>(2.0)
        );
    }

    #[test]
    fn a_body_may_gravitate_with_zero_gravitational_mass() {
        let (component, properties) = inertial(1.0);
        let world = world_with([ObjectSpec::new("neutral")
            .with_component(component, properties)
            .with_component(
                gravitational_mass_component_id(),
                independent_gravitational_mass_properties(MassKg::new::<kilogram>(0.0)).unwrap(),
            )]);

        let sources = collect_gravity_sources(&world.snapshot()).unwrap();
        assert_eq!(sources[0].coupling_value, MassKg::new::<kilogram>(0.0));
    }

    #[test]
    fn a_non_positive_inertial_mass_is_rejected_before_a_pusher_can_divide_by_it() {
        let world = world_with([ObjectSpec::new("massless").with_component(
            inertial_mass_component_id(),
            inertial_mass_properties(MassKg::new::<kilogram>(0.0)).unwrap(),
        )]);

        assert_eq!(
            inertial_mass_of(world.snapshot().objects().values().next().unwrap()),
            Err(SourceError::InvalidMass {
                object: "massless".to_owned()
            })
        );
    }

    #[test]
    fn gravitational_mass_alone_sources_gravity() {
        let world = world_with([ObjectSpec::new("moon")
            .with_transform(Transform::at(DVec3::X * 10.0).unwrap())
            .with_component(
                gravitational_mass_component_id(),
                independent_gravitational_mass_properties(MassKg::new::<kilogram>(7.0)).unwrap(),
            )]);

        let via_gravity = collect_gravity_sources(&world.snapshot()).unwrap();
        assert_eq!(via_gravity.len(), 1);
        assert_eq!(via_gravity[0].coupling_value, MassKg::new::<kilogram>(7.0));
        assert_eq!(via_gravity[0].position, DVec3::X * 10.0);
    }
}
