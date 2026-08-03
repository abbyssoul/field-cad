//! Generic authored particles and catalog templates.
//!
//! Electron, proton, positron, and neutron are data templates, not runtime
//! dispatch types. Each template creates the same particle component plus the
//! shared charge component. Equation systems see mass, charge, pose, velocity,
//! and motion mode; a familiar name never activates hidden forces.

use fieldcad_core::{
    ComponentSchema, ComponentTypeId, Dimension, ObjectId, ObjectShape, ObjectSpec, PluginId,
    PropertyBag, PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity, QuantityError,
    Transform, Velocity, WorldError, WorldSnapshot,
};
use fieldcad_electromagnetic_sources::{
    charge_component_id, charge_properties, charge_property_id,
};
use glam::DVec3;

pub const SCHEMA_NAMESPACE: &str = "fieldcad.particles";
pub const PARTICLE_COMPONENT: &str = "particle";
pub const MASS_PROPERTY: &str = "mass";
pub const MOTION_MODE_PROPERTY: &str = "motion-mode";
pub const TEMPLATE_PROPERTY: &str = "catalog-template";

/// Catalog provenance for the numerical values below.
pub const CATALOG_VERSION: &str = "NIST CODATA 2022 / SRD 121";
pub const ELEMENTARY_CHARGE_COULOMBS: f64 = 1.602_176_634e-19;
pub const ELECTRON_MASS_KG: f64 = 9.109_383_713_9e-31;
pub const PROTON_MASS_KG: f64 = 1.672_621_925_95e-27;
pub const NEUTRON_MASS_KG: f64 = 1.674_927_500_56e-27;

pub fn schema_namespace_id() -> PluginId {
    PluginId::new(SCHEMA_NAMESPACE).expect("static schema namespace is valid")
}

pub fn particle_component_id() -> ComponentTypeId {
    ComponentTypeId::new(schema_namespace_id(), PARTICLE_COMPONENT)
        .expect("static component ID is valid")
}

pub fn mass_property_id() -> PropertyId {
    PropertyId::new(MASS_PROPERTY).expect("static property ID is valid")
}

pub fn motion_mode_property_id() -> PropertyId {
    PropertyId::new(MOTION_MODE_PROPERTY).expect("static property ID is valid")
}

pub fn template_property_id() -> PropertyId {
    PropertyId::new(TEMPLATE_PROPERTY).expect("static property ID is valid")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MotionMode {
    #[default]
    Fixed,
    Prescribed,
    Dynamic,
}

impl MotionMode {
    pub const ALL: [Self; 3] = [Self::Fixed, Self::Prescribed, Self::Dynamic];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Fixed => "Fixed",
            Self::Prescribed => "Prescribed",
            Self::Dynamic => "Dynamic",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.label() == label)
    }

    pub const fn has_kinematic_authority(self) -> bool {
        matches!(self, Self::Prescribed | Self::Dynamic)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParticleTemplate {
    #[default]
    Custom,
    Electron,
    Proton,
    Positron,
    Neutron,
}

impl ParticleTemplate {
    pub const ALL: [Self; 5] = [
        Self::Custom,
        Self::Electron,
        Self::Proton,
        Self::Positron,
        Self::Neutron,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Custom => "Custom",
            Self::Electron => "Electron",
            Self::Proton => "Proton",
            Self::Positron => "Positron",
            Self::Neutron => "Neutron",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|template| template.label() == label)
    }

    pub const fn mass_kg(self) -> Option<f64> {
        match self {
            Self::Custom => None,
            Self::Electron | Self::Positron => Some(ELECTRON_MASS_KG),
            Self::Proton => Some(PROTON_MASS_KG),
            Self::Neutron => Some(NEUTRON_MASS_KG),
        }
    }

    pub const fn charge_coulombs(self) -> Option<f64> {
        match self {
            Self::Custom => None,
            Self::Electron => Some(-ELEMENTARY_CHARGE_COULOMBS),
            Self::Proton | Self::Positron => Some(ELEMENTARY_CHARGE_COULOMBS),
            Self::Neutron => Some(0.0),
        }
    }
}

pub fn particle_component_schema() -> ComponentSchema {
    ComponentSchema {
        id: particle_component_id(),
        display_name: "Generic particle".to_owned(),
        properties: vec![
            PropertySchema {
                id: mass_property_id(),
                display_name: "Mass".to_owned(),
                kind: PropertyKind::Scalar(Dimension::MASS),
                required: true,
            },
            PropertySchema {
                id: motion_mode_property_id(),
                display_name: "Motion mode".to_owned(),
                kind: PropertyKind::Choice(
                    MotionMode::ALL
                        .into_iter()
                        .map(|mode| mode.label().to_owned())
                        .collect(),
                ),
                required: true,
            },
            PropertySchema {
                id: template_property_id(),
                display_name: "Catalog template".to_owned(),
                kind: PropertyKind::Choice(
                    ParticleTemplate::ALL
                        .into_iter()
                        .map(|template| template.label().to_owned())
                        .collect(),
                ),
                required: true,
            },
        ],
    }
}

pub fn particle_properties(
    mass_kg: f64,
    motion_mode: MotionMode,
    template: ParticleTemplate,
) -> Result<PropertyBag, QuantityError> {
    Ok([
        (
            mass_property_id(),
            PropertyValue::Scalar(Quantity::new(mass_kg, Dimension::MASS)?),
        ),
        (
            motion_mode_property_id(),
            PropertyValue::Choice(motion_mode.label().to_owned()),
        ),
        (
            template_property_id(),
            PropertyValue::Choice(template.label().to_owned()),
        ),
    ]
    .into_iter()
    .collect())
}

pub fn template_particle_spec(
    template: ParticleTemplate,
    motion_mode: MotionMode,
    position: DVec3,
    velocity: DVec3,
    authoring_radius: f64,
) -> Result<ObjectSpec, ParticleError> {
    let mass_kg = template
        .mass_kg()
        .ok_or(ParticleError::CustomTemplateNeedsValues)?;
    let charge_coulombs = template
        .charge_coulombs()
        .ok_or(ParticleError::CustomTemplateNeedsValues)?;
    Ok(ObjectSpec::new(template.label())
        .with_transform(Transform::at(position)?)
        .with_velocity(Velocity::new(velocity, DVec3::ZERO)?)
        .with_shape(ObjectShape::point(authoring_radius)?)
        .with_component(
            particle_component_id(),
            particle_properties(mass_kg, motion_mode, template)?,
        )
        .with_component(charge_component_id(), charge_properties(charge_coulombs)?))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub object: ObjectId,
    pub mass_kg: f64,
    pub charge_coulombs: f64,
    pub motion_mode: MotionMode,
    pub template: ParticleTemplate,
    pub position: DVec3,
    pub velocity: DVec3,
}

pub fn collect_particles(world: &WorldSnapshot) -> Result<Vec<Particle>, ParticleError> {
    world
        .objects_with(&particle_component_id())
        .map(|(object, properties)| {
            let mass_kg = properties
                .scalar(&mass_property_id())
                .ok_or_else(|| ParticleError::InvalidMass(object.name.clone()))?;
            if mass_kg <= 0.0 {
                return Err(ParticleError::InvalidMass(object.name.clone()));
            }
            let motion_mode = properties
                .get(&motion_mode_property_id())
                .and_then(choice_value)
                .and_then(MotionMode::parse)
                .ok_or_else(|| ParticleError::InvalidMotionMode(object.name.clone()))?;
            let template = properties
                .get(&template_property_id())
                .and_then(choice_value)
                .and_then(ParticleTemplate::parse)
                .ok_or_else(|| ParticleError::InvalidTemplate(object.name.clone()))?;
            let charge_coulombs = object
                .components
                .get(&charge_component_id())
                .and_then(|charge| charge.scalar(&charge_property_id()))
                .ok_or_else(|| ParticleError::MissingCharge(object.name.clone()))?;
            if object.velocity.angular != DVec3::ZERO {
                return Err(ParticleError::AngularVelocity(object.name.clone()));
            }
            Ok(Particle {
                object: object.id,
                mass_kg,
                charge_coulombs,
                motion_mode,
                template,
                position: object.transform.translation,
                velocity: object.velocity.linear,
            })
        })
        .collect()
}

fn choice_value(value: &PropertyValue) -> Option<&str> {
    match value {
        PropertyValue::Choice(choice) => Some(choice),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ParticleError {
    #[error(transparent)]
    World(#[from] WorldError),
    #[error(transparent)]
    Quantity(#[from] QuantityError),
    #[error("the Custom particle template requires explicit mass and charge values")]
    CustomTemplateNeedsValues,
    #[error("particle '{0}' must have a finite positive mass")]
    InvalidMass(String),
    #[error("particle '{0}' has an invalid motion mode")]
    InvalidMotionMode(String),
    #[error("particle '{0}' has an invalid catalog template")]
    InvalidTemplate(String),
    #[error("particle '{0}' must carry the shared charge component (zero is valid)")]
    MissingCharge(String),
    #[error("particle '{0}' cannot have angular velocity in the point-particle model")]
    AngularVelocity(String),
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{World, WorldCommand};

    use super::*;

    #[test]
    fn catalog_entries_create_one_generic_particle_representation() {
        for template in [
            ParticleTemplate::Electron,
            ParticleTemplate::Proton,
            ParticleTemplate::Positron,
            ParticleTemplate::Neutron,
        ] {
            let spec =
                template_particle_spec(template, MotionMode::Dynamic, DVec3::ZERO, DVec3::X, 0.1)
                    .unwrap();
            assert!(spec.components.contains_key(&particle_component_id()));
            assert!(spec.components.contains_key(&charge_component_id()));
        }
    }

    #[test]
    fn catalog_values_and_provenance_survive_world_authoring() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(particle_component_schema()),
                WorldCommand::RegisterComponentSchema(
                    fieldcad_electromagnetic_sources::charge_component_schema(),
                ),
                WorldCommand::CreateObject(
                    template_particle_spec(
                        ParticleTemplate::Electron,
                        MotionMode::Dynamic,
                        DVec3::X,
                        DVec3::Y,
                        0.1,
                    )
                    .unwrap(),
                ),
            ])
            .unwrap();

        let particle = collect_particles(&world.snapshot()).unwrap()[0];
        assert_eq!(particle.template, ParticleTemplate::Electron);
        assert_eq!(particle.mass_kg, ELECTRON_MASS_KG);
        assert_eq!(particle.charge_coulombs, -ELEMENTARY_CHARGE_COULOMBS);
        assert_eq!(particle.position, DVec3::X);
        assert_eq!(particle.velocity, DVec3::Y);
    }
}
