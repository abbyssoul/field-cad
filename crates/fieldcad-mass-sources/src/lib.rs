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

use fieldcad_core::{
    ComponentSchema, ComponentTypeId, Dimension, ObjectId, ObjectShape, PluginId, PropertyBag,
    PropertyCondition, PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity,
    QuantityError, Velocity, WorldObject, WorldSnapshot,
};
use glam::DVec3;

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
pub const DEFAULT_MASS_KG: f64 = 1.0;

fn mass_property_schema() -> PropertySchema {
    PropertySchema {
        id: mass_property_id(),
        display_name: "Mass".to_owned(),
        kind: PropertyKind::Scalar(Dimension::MASS),
        required: true,
        relevant_when: None,
        default_value: Some(PropertyValue::Scalar(
            Quantity::new(DEFAULT_MASS_KG, Dimension::MASS).expect("static default mass is finite"),
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

pub fn inertial_mass_properties(kilograms: f64) -> Result<PropertyBag, QuantityError> {
    Ok([(
        mass_property_id(),
        PropertyValue::Scalar(Quantity::new(kilograms, Dimension::MASS)?),
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
                Quantity::new(DEFAULT_MASS_KG, Dimension::MASS).expect("static mass is finite"),
            ),
        ),
    ]
    .into_iter()
    .collect()
}

/// A gravitational mass authored independently of the body's inertia.
pub fn independent_gravitational_mass_properties(
    kilograms: f64,
) -> Result<PropertyBag, QuantityError> {
    Ok([
        (
            follows_inertial_property_id(),
            PropertyValue::Boolean(false),
        ),
        (
            mass_property_id(),
            PropertyValue::Scalar(Quantity::new(kilograms, Dimension::MASS)?),
        ),
    ]
    .into_iter()
    .collect())
}

/// How a massive body's volume is distributed, for solvers that need more than
/// a point.
///
/// The variants mirror [`fieldcad_electromagnetic_sources::ChargeDistribution`]
/// because the geometry question is the same one; the quantity being spread over
/// that geometry is what differs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MassDistribution {
    Point { exclusion_radius: f64 },
    UniformSphere { radius: f64 },
}

/// One authored massive body, in solver-neutral terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassSource {
    pub object: ObjectId,
    pub position: DVec3,
    pub velocity: Velocity,
    /// The inertia the dynamics system divides an accumulated force by.
    pub inertial_mass_kg: f64,
    /// The charge a gravitational field couples to, if this body gravitates.
    ///
    /// `None` means the body has inertia but does not source or feel gravity —
    /// the gravitational equivalent of an uncharged body.
    pub gravitational_mass_kg: Option<f64>,
    /// Whether the user, rather than a solver, decides this body's motion.
    pub pinned: bool,
    pub distribution: MassDistribution,
}

impl MassSource {
    /// Whether this body's gravitational and inertial masses differ.
    ///
    /// Worth surfacing: a scene where these are unequal is not modelling the
    /// universe as measured, and a reader of its results needs to know that.
    pub fn violates_equivalence(&self) -> bool {
        self.gravitational_mass_kg
            .is_some_and(|gravitational| gravitational != self.inertial_mass_kg)
    }
}

/// The exclusion radius given to a massive body with no authored shape.
///
/// A bare gizmo that has just been given mass is a legitimate point body. It
/// gets the same treatment as a charge attached to a shapeless object rather
/// than being rejected, so that composing an object one component at a time
/// never passes through an invalid intermediate state.
pub const DEFAULT_POINT_RADIUS: f64 = fieldcad_core::DEFAULT_PROXY_RADIUS;

/// Extract every authored massive body in deterministic object-ID order.
pub fn collect_mass_sources(world: &WorldSnapshot) -> Result<Vec<MassSource>, MassSourceError> {
    world
        .objects_with(&inertial_mass_component_id())
        .map(|(object, properties)| source_from_object(object, properties))
        .collect()
}

/// The gravitational coupling charge of one object, resolving the link.
///
/// Separated from [`collect_mass_sources`] so a gravity plugin can ask about a
/// body it already holds without rebuilding the whole list.
pub fn gravitational_mass_of(object: &WorldObject) -> Result<Option<f64>, MassSourceError> {
    let Some(properties) = object.components.get(&gravitational_mass_component_id()) else {
        return Ok(None);
    };
    let follows = matches!(
        properties.get(&follows_inertial_property_id()),
        Some(PropertyValue::Boolean(true))
    );
    if follows {
        // Linked: the authored gravitational value is ignored entirely rather
        // than kept in sync, so the two can never disagree while the link is on.
        return inertial_mass_of(object).map(Some);
    }
    let mass = properties.scalar(&mass_property_id()).ok_or_else(|| {
        MassSourceError::InvalidGravitationalMass {
            object: object.name.clone(),
        }
    })?;
    // Zero is meaningful here — a body with inertia that does not gravitate —
    // but a negative gravitational mass is not something this model represents.
    if !mass.is_finite() || mass < 0.0 {
        return Err(MassSourceError::InvalidGravitationalMass {
            object: object.name.clone(),
        });
    }
    Ok(Some(mass))
}

fn inertial_mass_of(object: &WorldObject) -> Result<f64, MassSourceError> {
    let mass = object
        .components
        .get(&inertial_mass_component_id())
        .and_then(|properties| properties.scalar(&mass_property_id()))
        .ok_or_else(|| MassSourceError::InvalidMass {
            object: object.name.clone(),
        })?;
    // Inertia divides. A zero or negative mass is not a body a pusher can
    // integrate, so it is rejected at the boundary rather than producing an
    // infinity several layers deeper.
    if !mass.is_finite() || mass <= 0.0 {
        return Err(MassSourceError::InvalidMass {
            object: object.name.clone(),
        });
    }
    Ok(mass)
}

fn source_from_object(
    object: &WorldObject,
    _properties: &PropertyBag,
) -> Result<MassSource, MassSourceError> {
    let distribution = match object.shape {
        Some(ObjectShape::Point { radius }) => MassDistribution::Point {
            exclusion_radius: radius,
        },
        Some(ObjectShape::Sphere { radius }) if radius > 0.0 => {
            MassDistribution::UniformSphere { radius }
        }
        Some(ObjectShape::Sphere { .. }) => {
            return Err(MassSourceError::NonPositiveSphere {
                object: object.name.clone(),
            });
        }
        None => MassDistribution::Point {
            exclusion_radius: DEFAULT_POINT_RADIUS,
        },
        Some(ObjectShape::Box { .. }) => {
            return Err(MassSourceError::UnsupportedShape {
                object: object.name.clone(),
            });
        }
    };
    Ok(MassSource {
        object: object.id,
        position: object.transform.translation,
        velocity: object.velocity,
        inertial_mass_kg: inertial_mass_of(object)?,
        gravitational_mass_kg: gravitational_mass_of(object)?,
        pinned: object.pinned,
        distribution,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MassSourceError {
    #[error("object '{object}' must have a finite, positive inertial mass")]
    InvalidMass { object: String },
    #[error("object '{object}' must have a finite, non-negative gravitational mass")]
    InvalidGravitationalMass { object: String },
    #[error("massive sphere '{object}' must have a positive radius")]
    NonPositiveSphere { object: String },
    #[error("massive object '{object}' must use a point or sphere shape")]
    UnsupportedShape { object: String },
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectSpec, Transform, World, WorldCommand};

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
            inertial_mass_properties(kilograms).unwrap(),
        )
    }

    #[test]
    fn a_shapeless_gizmo_given_mass_is_a_point_body() {
        // The composition flow adds a bare object first and mass second. That
        // intermediate object has no shape, and must not be rejected for it.
        let (component, properties) = inertial(2.0);
        let world = world_with([ObjectSpec::new("gizmo")
            .with_transform(Transform::at(DVec3::Y).unwrap())
            .with_component(component, properties)]);

        let sources = collect_mass_sources(&world.snapshot()).unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].inertial_mass_kg, 2.0);
        assert_eq!(sources[0].position, DVec3::Y);
        assert_eq!(
            sources[0].distribution,
            MassDistribution::Point {
                exclusion_radius: DEFAULT_POINT_RADIUS
            }
        );
    }

    #[test]
    fn inertia_alone_does_not_make_a_body_gravitate() {
        // The point of the split: having somewhere for a force to act is a
        // different claim from coupling to the gravitational field.
        let (component, properties) = inertial(5.0);
        let world = world_with([ObjectSpec::new("inert").with_component(component, properties)]);

        let sources = collect_mass_sources(&world.snapshot()).unwrap();

        assert_eq!(sources[0].inertial_mass_kg, 5.0);
        assert_eq!(sources[0].gravitational_mass_kg, None);
        assert!(!sources[0].violates_equivalence());
    }

    #[test]
    fn a_linked_gravitational_mass_tracks_inertia_rather_than_storing_a_copy() {
        let (component, properties) = inertial(3.0);
        let mut world = world_with([ObjectSpec::new("body")
            .with_component(component, properties)
            .with_component(
                gravitational_mass_component_id(),
                // The stored value is deliberately wrong; while linked it must
                // never be consulted, or the two can silently disagree.
                linked_gravitational_mass_properties(),
            )]);

        let sources = collect_mass_sources(&world.snapshot()).unwrap();
        assert_eq!(sources[0].gravitational_mass_kg, Some(3.0));
        assert!(!sources[0].violates_equivalence());

        // Changing inertia carries the gravitational mass with it, with no
        // second edit and no opportunity to drift.
        world
            .commit([WorldCommand::AttachComponent {
                object: ObjectId::new(0),
                component: inertial_mass_component_id(),
                properties: inertial_mass_properties(11.0).unwrap(),
            }])
            .unwrap();

        let sources = collect_mass_sources(&world.snapshot()).unwrap();
        assert_eq!(sources[0].inertial_mass_kg, 11.0);
        assert_eq!(sources[0].gravitational_mass_kg, Some(11.0));
    }

    /// While the link is on, the gravitational mass value must not be offered
    /// for editing: the model does not read it, so a number typed there would
    /// silently do nothing.
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
            mass.is_relevant(&independent_gravitational_mass_properties(2.0).unwrap()),
            "unlinking must make the value editable again"
        );

        // The switch itself is never conditional, or there would be no way back.
        let switch = schema
            .properties
            .iter()
            .find(|property| property.id == follows_inertial_property_id())
            .expect("the component declares the link switch");
        assert!(switch.relevant_when.is_none());

        // Declaration order matters: a generic editor renders in schema order,
        // and the switch has to appear above the value it governs.
        assert_eq!(schema.properties[0].id, follows_inertial_property_id());
    }

    /// The inertial mass shares its property schema with the gravitational one,
    /// so a copy-paste of the condition would make it uneditable too.
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
                independent_gravitational_mass_properties(7.0).unwrap(),
            )]);

        let sources = collect_mass_sources(&world.snapshot()).unwrap();

        assert_eq!(sources[0].inertial_mass_kg, 2.0);
        assert_eq!(sources[0].gravitational_mass_kg, Some(7.0));
        assert!(
            sources[0].violates_equivalence(),
            "an unequal pair must be reportable, not silent"
        );
    }

    #[test]
    fn a_body_may_gravitate_with_zero_gravitational_mass() {
        // Zero is a physical state — inert under gravity — unlike zero inertia,
        // which is a body a pusher cannot integrate.
        let (component, properties) = inertial(1.0);
        let world = world_with([ObjectSpec::new("neutral")
            .with_component(component, properties)
            .with_component(
                gravitational_mass_component_id(),
                independent_gravitational_mass_properties(0.0).unwrap(),
            )]);

        let sources = collect_mass_sources(&world.snapshot()).unwrap();
        assert_eq!(sources[0].gravitational_mass_kg, Some(0.0));
    }

    #[test]
    fn a_non_positive_inertial_mass_is_rejected_before_a_pusher_can_divide_by_it() {
        let world = world_with([ObjectSpec::new("massless").with_component(
            inertial_mass_component_id(),
            inertial_mass_properties(0.0).unwrap(),
        )]);

        assert_eq!(
            collect_mass_sources(&world.snapshot()),
            Err(MassSourceError::InvalidMass {
                object: "massless".to_owned()
            })
        );
    }

    #[test]
    fn pinning_is_read_from_the_object_not_the_component() {
        let (component, properties) = inertial(1.0);
        let (other, other_properties) = inertial(1.0);
        let world = world_with([
            ObjectSpec::new("held")
                .with_pinned(true)
                .with_component(component, properties),
            ObjectSpec::new("free").with_component(other, other_properties),
        ]);

        let sources = collect_mass_sources(&world.snapshot()).unwrap();

        assert!(sources[0].pinned);
        assert!(!sources[1].pinned);
    }
}
