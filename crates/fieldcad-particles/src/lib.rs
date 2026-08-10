//! Catalog provenance for authored particles, and the solver-neutral view of a
//! massive body.
//!
//! Electron, proton, positron, and neutron are data templates, not runtime
//! dispatch types. A template is a convenience that attaches the *shared* mass
//! and charge components with catalog values; the only thing this crate owns is
//! the record of where those numbers came from.
//!
//! What makes a body dynamic is mass, not membership of this crate's component.
//! [`collect_particles`] therefore iterates authored masses: charge is optional
//! (an uncharged body is neutral, not invalid) and catalog provenance is
//! optional (a hand-built body is `Custom`). Equation systems see mass, charge,
//! pose, velocity, and whether the user pinned the object; a familiar name never
//! activates hidden forces.

use fieldcad_core::quantities::{ChargeCoulombs, MassKg, coulomb, kilogram};
use fieldcad_core::{
    ComponentSchema, ComponentTypeId, ObjectId, ObjectShape, ObjectSpec, PluginId, PropertyBag,
    PropertyId, PropertyKind, PropertySchema, PropertyValue, QuantityError, Transform, Velocity,
    WorldError, WorldSnapshot,
};
use fieldcad_electromagnetic_sources::{
    charge_component_id, charge_properties, charge_property_id,
};
use fieldcad_sources::{
    SourceError, inertial_mass_component_id, inertial_mass_of, inertial_mass_properties,
};
use glam::DVec3;

pub const SCHEMA_NAMESPACE: &str = "fieldcad.particles";
pub const PARTICLE_COMPONENT: &str = "particle";
pub const TEMPLATE_PROPERTY: &str = "catalog-template";

/// Catalog provenance for the numerical values below.
pub const CATALOG_VERSION: &str = "NIST CODATA 2022 / SRD 121";
pub const ELEMENTARY_CHARGE_COULOMBS: f64 = 1.602_176_634e-19;
pub const ELECTRON_MASS_KG: f64 = 9.109_383_713_9e-31;
pub const PROTON_MASS_KG: f64 = 1.672_621_925_95e-27;
pub const NEUTRON_MASS_KG: f64 = 1.674_927_500_56e-27;

/// Typed public wrappers for particle constants.
pub fn elementary_charge_coulombs() -> ChargeCoulombs {
    ChargeCoulombs::new::<coulomb>(ELEMENTARY_CHARGE_COULOMBS)
}
pub fn electron_mass_kg() -> MassKg {
    MassKg::new::<kilogram>(ELECTRON_MASS_KG)
}
pub fn proton_mass_kg() -> MassKg {
    MassKg::new::<kilogram>(PROTON_MASS_KG)
}
pub fn neutron_mass_kg() -> MassKg {
    MassKg::new::<kilogram>(NEUTRON_MASS_KG)
}

pub fn schema_namespace_id() -> PluginId {
    PluginId::new(SCHEMA_NAMESPACE).expect("static schema namespace is valid")
}

pub fn particle_component_id() -> ComponentTypeId {
    ComponentTypeId::new(schema_namespace_id(), PARTICLE_COMPONENT)
        .expect("static component ID is valid")
}

pub fn template_property_id() -> PropertyId {
    PropertyId::new(TEMPLATE_PROPERTY).expect("static property ID is valid")
}

/// One entry in the particle preset catalog.
///
/// This is the single place a new preset is added: a name and its published
/// mass and charge. Nothing else in this crate, or in a consumer, names a
/// preset directly — [`ParticleTemplate::Catalog`] carries the name, and every
/// other question (mass, charge, whether an edit dropped the claim) is
/// answered by looking the name up here.
pub struct ParticleTemplateData {
    pub name: &'static str,
    pub mass_kg: f64,
    pub charge_coulombs: f64,
}

/// The particle preset catalog. Add a preset by adding a line.
pub const CATALOG: &[ParticleTemplateData] = &[
    ParticleTemplateData {
        name: "Electron",
        mass_kg: ELECTRON_MASS_KG,
        charge_coulombs: -ELEMENTARY_CHARGE_COULOMBS,
    },
    ParticleTemplateData {
        name: "Positron",
        mass_kg: ELECTRON_MASS_KG,
        charge_coulombs: ELEMENTARY_CHARGE_COULOMBS,
    },
    ParticleTemplateData {
        name: "Proton",
        mass_kg: PROTON_MASS_KG,
        charge_coulombs: ELEMENTARY_CHARGE_COULOMBS,
    },
    ParticleTemplateData {
        name: "Anti-proton",
        mass_kg: PROTON_MASS_KG,
        charge_coulombs: -ELEMENTARY_CHARGE_COULOMBS,
    },
    ParticleTemplateData {
        name: "Neutron",
        mass_kg: NEUTRON_MASS_KG,
        charge_coulombs: 0.0,
    },
];

fn catalog_entry(name: &str) -> Option<&'static ParticleTemplateData> {
    CATALOG.iter().find(|entry| entry.name == name)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParticleTemplate {
    #[default]
    Custom,
    /// A named entry in [`CATALOG`]. Not guaranteed to resolve — a name that
    /// no longer has a catalog entry behaves like [`Self::Custom`], the same
    /// way an edited value drops the claim in [`Self::matches`].
    Catalog(&'static str),
}

impl ParticleTemplate {
    /// Every offerable choice, `Custom` first: what a picker (a dropdown, a
    /// schema's list of choices) should show.
    pub fn all() -> impl Iterator<Item = Self> {
        std::iter::once(Self::Custom).chain(CATALOG.iter().map(|entry| Self::Catalog(entry.name)))
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Custom => "Custom",
            Self::Catalog(name) => name,
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        if label == "Custom" {
            return Some(Self::Custom);
        }
        catalog_entry(label).map(|entry| Self::Catalog(entry.name))
    }

    pub fn mass_kg(self) -> Option<MassKg> {
        match self {
            Self::Custom => None,
            Self::Catalog(name) => {
                catalog_entry(name).map(|entry| MassKg::new::<kilogram>(entry.mass_kg))
            }
        }
    }

    pub fn charge_coulombs(self) -> Option<ChargeCoulombs> {
        match self {
            Self::Custom => None,
            Self::Catalog(name) => catalog_entry(name)
                .map(|entry| ChargeCoulombs::new::<coulomb>(entry.charge_coulombs)),
        }
    }

    /// Whether authored values still match what this template published.
    ///
    /// Exact equality is the right test: these are the literal constants this
    /// crate wrote into the object, so any difference at all means a user
    /// changed them and the catalog claim no longer holds.
    pub fn matches(self, mass_kg: MassKg, charge_coulombs: ChargeCoulombs) -> bool {
        self.mass_kg() == Some(mass_kg) && self.charge_coulombs() == Some(charge_coulombs)
    }
}

/// Catalog provenance only.
///
/// Mass moved to the shared [`fieldcad_sources`] component so that a body
/// can be massive without being a catalog particle, and motion mode became
/// [`fieldcad_core::WorldObject::pinned`] so that it applies to any object
/// rather than only to particles. What is left is the question this crate is
/// uniquely able to answer: which published values these numbers came from.
pub fn particle_component_schema() -> ComponentSchema {
    ComponentSchema {
        id: particle_component_id(),
        display_name: "Catalog provenance".to_owned(),
        properties: vec![PropertySchema {
            id: template_property_id(),
            display_name: "Catalog template".to_owned(),
            description: None,
            kind: PropertyKind::Choice(
                ParticleTemplate::all()
                    .map(|template| template.label().to_owned())
                    .collect(),
            ),
            required: true,
            default_value: None,
            relevant_when: None,
        }],
    }
}

pub fn particle_properties(template: ParticleTemplate) -> Result<PropertyBag, QuantityError> {
    Ok([(
        template_property_id(),
        PropertyValue::Choice(template.label().to_owned()),
    )]
    .into_iter()
    .collect())
}

/// Build a catalog particle as three independent components.
///
/// The result is indistinguishable from the same object composed by hand: mass,
/// charge, and provenance are separately attachable, and `pinned` is ordinary
/// object state. A template is a shortcut through the composition flow, not a
/// different kind of thing.
pub fn template_particle_spec(
    template: ParticleTemplate,
    pinned: bool,
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
        .with_pinned(pinned)
        .with_component(
            inertial_mass_component_id(),
            inertial_mass_properties(mass_kg)?,
        )
        .with_component(charge_component_id(), charge_properties(charge_coulombs)?)
        .with_component(particle_component_id(), particle_properties(template)?))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub object: ObjectId,
    pub mass_kg: MassKg,
    /// Zero for an uncharged body. Absence of a charge component means neutral,
    /// which is a physical state rather than a missing input.
    pub charge_coulombs: ChargeCoulombs,
    /// Whether the user, rather than a solver, decides this body's motion.
    ///
    /// A pinned body still moves if it has authored velocity — it simply moves
    /// the way the user said, without integrating any force.
    pub pinned: bool,
    pub template: ParticleTemplate,
    pub position: DVec3,
    pub velocity: DVec3,
}

impl fieldcad_core::IdentifiedByObject for Particle {
    fn object_id(&self) -> ObjectId {
        self.object
    }
}

impl Particle {
    /// Whether a solver must claim and advance this body's pose.
    ///
    /// An unpinned body is integrated from the fields acting on it. A pinned one
    /// only needs claiming when it has authored velocity to be carried along by;
    /// pinned and stationary means no solver has to touch it at all, which keeps
    /// a static configuration from producing a world revision every tick.
    pub fn needs_kinematic_authority(&self) -> bool {
        !self.pinned || self.velocity != DVec3::ZERO
    }
}

/// Every authored massive body, in the terms a particle pusher needs.
///
/// Driven by the mass component: mass is what makes a body respond to a force,
/// so mass is what makes it a particle. Charge and catalog provenance are read
/// if present and defaulted if not.
pub fn collect_particles(world: &WorldSnapshot) -> Result<Vec<Particle>, ParticleError> {
    world
        .objects_with(&inertial_mass_component_id())
        .map(|(object, _properties)| particle_from_object(world, object))
        .collect()
}

fn particle_from_object(
    _world: &WorldSnapshot,
    object: &fieldcad_core::WorldObject,
) -> Result<Particle, ParticleError> {
    let mass_kg = inertial_mass_of(object)?;
    if object.velocity.angular != DVec3::ZERO {
        return Err(ParticleError::AngularVelocity(object.name.clone()));
    }
    let charge_coulombs = object
        .components
        .get(&charge_component_id())
        .and_then(|charge| charge.typed_charge(&charge_property_id()))
        .unwrap_or(ChargeCoulombs::new::<coulomb>(0.0));
    let template = object
        .components
        .get(&particle_component_id())
        .and_then(|provenance| provenance.get(&template_property_id()))
        .and_then(choice_value)
        .and_then(ParticleTemplate::parse)
        .filter(|template| template.matches(mass_kg, charge_coulombs))
        .unwrap_or(ParticleTemplate::Custom);
    Ok(Particle {
        object: object.id,
        mass_kg,
        charge_coulombs,
        pinned: object.pinned,
        template,
        position: object.transform.translation,
        velocity: object.velocity.linear,
    })
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
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("the Custom particle template requires explicit mass and charge values")]
    CustomTemplateNeedsValues,
    #[error("particle '{0}' must have a finite positive mass")]
    InvalidMass(String),
    #[error("particle '{0}' cannot have angular velocity in the point-particle model")]
    AngularVelocity(String),
}

#[cfg(test)]
mod tests {
    use fieldcad_core::quantities::{coulomb, kilogram};
    use fieldcad_core::{ObjectSpec, World, WorldCommand};
    use fieldcad_sources::mass_component_schemas;

    use super::*;

    fn schema_commands() -> Vec<WorldCommand> {
        mass_component_schemas()
            .into_iter()
            .chain([
                fieldcad_electromagnetic_sources::charge_component_schema(),
                particle_component_schema(),
            ])
            .map(WorldCommand::RegisterComponentSchema)
            .collect()
    }

    fn world_with(specs: impl IntoIterator<Item = ObjectSpec>) -> World {
        let mut world = World::new();
        world
            .commit(
                schema_commands()
                    .into_iter()
                    .chain(specs.into_iter().map(WorldCommand::CreateObject)),
            )
            .unwrap();
        world
    }

    /// Every catalog entry, not a hand-picked few: adding a preset to
    /// `CATALOG` must not require also remembering to list it in a test.
    #[test]
    fn catalog_entries_compose_from_independently_attachable_components() {
        for template in
            ParticleTemplate::all().filter(|template| *template != ParticleTemplate::Custom)
        {
            let spec = template_particle_spec(template, false, DVec3::ZERO, DVec3::X, 0.1).unwrap();
            assert!(spec.components.contains_key(&inertial_mass_component_id()));
            assert!(spec.components.contains_key(&charge_component_id()));
            assert!(spec.components.contains_key(&particle_component_id()));
        }
    }

    #[test]
    fn anti_proton_has_proton_mass_and_opposite_charge() {
        let anti_proton = ParticleTemplate::Catalog("Anti-proton");
        assert_eq!(
            anti_proton.mass_kg(),
            Some(MassKg::new::<kilogram>(PROTON_MASS_KG))
        );
        assert_eq!(
            anti_proton.charge_coulombs(),
            Some(ChargeCoulombs::new::<coulomb>(-ELEMENTARY_CHARGE_COULOMBS))
        );
    }

    #[test]
    fn a_catalog_name_with_no_entry_behaves_like_custom() {
        let unknown = ParticleTemplate::Catalog("Muon");
        assert_eq!(unknown.mass_kg(), None);
        assert_eq!(unknown.charge_coulombs(), None);
        assert!(!unknown.matches(
            MassKg::new::<kilogram>(0.0),
            ChargeCoulombs::new::<coulomb>(0.0)
        ));
    }

    #[test]
    fn catalog_values_and_provenance_survive_world_authoring() {
        let world = world_with([template_particle_spec(
            ParticleTemplate::Catalog("Electron"),
            false,
            DVec3::X,
            DVec3::Y,
            0.1,
        )
        .unwrap()]);

        let particle = collect_particles(&world.snapshot()).unwrap()[0];
        assert_eq!(particle.template, ParticleTemplate::Catalog("Electron"));
        assert_eq!(particle.mass_kg, MassKg::new::<kilogram>(ELECTRON_MASS_KG));
        assert_eq!(
            particle.charge_coulombs,
            ChargeCoulombs::new::<coulomb>(-ELEMENTARY_CHARGE_COULOMBS)
        );
        assert_eq!(particle.position, DVec3::X);
        assert_eq!(particle.velocity, DVec3::Y);
        assert!(!particle.pinned);
    }

    #[test]
    fn editing_a_catalog_mass_drops_the_catalog_claim() {
        // ADR 0019: no edit may keep claiming a published value it no longer
        // holds. The generic property editor cannot know to reset the label, so
        // the claim has to fail its own check.
        let mut world = world_with([template_particle_spec(
            ParticleTemplate::Catalog("Electron"),
            false,
            DVec3::ZERO,
            DVec3::ZERO,
            0.1,
        )
        .unwrap()]);
        let object = fieldcad_core::ObjectId::new(0);
        assert_eq!(
            collect_particles(&world.snapshot()).unwrap()[0].template,
            ParticleTemplate::Catalog("Electron")
        );

        world
            .commit([WorldCommand::AttachComponent {
                object,
                component: inertial_mass_component_id(),
                properties: inertial_mass_properties(MassKg::new::<kilogram>(
                    ELECTRON_MASS_KG * 2.0,
                ))
                .unwrap(),
            }])
            .unwrap();

        let particle = collect_particles(&world.snapshot()).unwrap()[0];
        assert_eq!(
            particle.mass_kg,
            MassKg::new::<kilogram>(ELECTRON_MASS_KG * 2.0)
        );
        assert_eq!(particle.template, ParticleTemplate::Custom);
    }

    #[test]
    fn mass_alone_makes_a_body_a_dynamic_particle() {
        // The composition story end to end: a bare gizmo plus mass is enough.
        // No charge, no catalog identity, no motion mode.
        let world = world_with([ObjectSpec::new("gizmo").with_component(
            inertial_mass_component_id(),
            inertial_mass_properties(MassKg::new::<kilogram>(3.0)).unwrap(),
        )]);

        let particles = collect_particles(&world.snapshot()).unwrap();

        assert_eq!(particles.len(), 1);
        assert_eq!(particles[0].mass_kg, MassKg::new::<kilogram>(3.0));
        assert_eq!(
            particles[0].charge_coulombs,
            ChargeCoulombs::new::<coulomb>(0.0)
        );
        assert_eq!(particles[0].template, ParticleTemplate::Custom);
        assert!(particles[0].needs_kinematic_authority());
    }

    #[test]
    fn a_charge_without_mass_is_not_a_particle() {
        // It is still a field source; it simply has no inertia to integrate.
        let world = world_with([ObjectSpec::new("static charge")
            .with_shape(ObjectShape::point(0.1).unwrap())
            .with_component(
                charge_component_id(),
                charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
            )]);

        assert!(collect_particles(&world.snapshot()).unwrap().is_empty());
    }

    #[test]
    fn pinning_decides_who_owns_the_motion() {
        let world = world_with([
            ObjectSpec::new("held still")
                .with_pinned(true)
                .with_component(
                    inertial_mass_component_id(),
                    inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
                ),
            ObjectSpec::new("carried along")
                .with_pinned(true)
                .with_velocity(Velocity::new(DVec3::X, DVec3::ZERO).unwrap())
                .with_component(
                    inertial_mass_component_id(),
                    inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
                ),
        ]);

        let particles = collect_particles(&world.snapshot()).unwrap();

        // Pinned and stationary needs no solver at all, so a static arrangement
        // does not churn a world revision every tick.
        assert!(!particles[0].needs_kinematic_authority());
        // Pinned with authored velocity still has to be moved by someone.
        assert!(particles[1].needs_kinematic_authority());
    }
}
