//! The solver-neutral view of a mass-bearing body, and its collection from
//! the world.
//!
//! What makes a body dynamic is mass, not membership of any particular
//! crate's component: [`collect_particles`] iterates authored masses, and
//! charge is optional (an uncharged body is neutral, not invalid). This
//! stays a private module of the sole consumer (`plugins/gravity` already
//! has its own `fieldcad_sources::collect_gravity_sources` and does not
//! need this) rather than a new shared crate.

use fieldcad_core::quantities::{ChargeCoulombs, MassKg, coulomb};
use fieldcad_core::{ObjectId, WorldSnapshot};
use fieldcad_electromagnetic_sources::{charge_component_id, charge_property_id};
use fieldcad_sources::{SourceError, inertial_mass_component_id, inertial_mass_of};
use glam::DVec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub object: ObjectId,
    pub mass_kg: MassKg,
    /// Zero for an uncharged body. Absence of a charge component means
    /// neutral, which is a physical state rather than a missing input.
    pub charge_coulombs: ChargeCoulombs,
    /// Whether the user, rather than a solver, decides this body's motion.
    ///
    /// A pinned body still moves if it has authored velocity — it simply
    /// moves the way the user said, without integrating any force.
    pub pinned: bool,
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
    /// An unpinned body is integrated from the fields acting on it. A pinned
    /// one only needs claiming when it has authored velocity to be carried
    /// along by; pinned and stationary means no solver has to touch it at
    /// all, which keeps a static configuration from producing a world
    /// revision every tick.
    pub fn needs_kinematic_authority(&self) -> bool {
        !self.pinned || self.velocity != DVec3::ZERO
    }
}

/// Every authored massive body, in the terms a particle pusher needs.
///
/// Driven by the mass component: mass is what makes a body respond to a
/// force, so mass is what makes it a particle. Charge is read if present and
/// defaulted if not.
pub fn collect_particles(world: &WorldSnapshot) -> Result<Vec<Particle>, ParticleError> {
    world
        .objects_with(&inertial_mass_component_id())
        .map(|(object, _properties)| particle_from_object(object))
        .collect()
}

fn particle_from_object(object: &fieldcad_core::WorldObject) -> Result<Particle, ParticleError> {
    let mass_kg = inertial_mass_of(object)?;
    if object.velocity.angular != DVec3::ZERO {
        return Err(ParticleError::AngularVelocity(object.name.clone()));
    }
    let charge_coulombs = object
        .components
        .get(&charge_component_id())
        .and_then(|charge| charge.typed_charge(&charge_property_id()))
        .unwrap_or(ChargeCoulombs::new::<coulomb>(0.0));
    Ok(Particle {
        object: object.id,
        mass_kg,
        charge_coulombs,
        pinned: object.pinned,
        position: object.transform.translation,
        velocity: object.velocity.linear,
    })
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ParticleError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("particle '{0}' cannot have angular velocity in the point-particle model")]
    AngularVelocity(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::quantities::{coulomb, kilogram};
    use fieldcad_core::{ObjectSpec, Velocity, World, WorldCommand};
    use fieldcad_electromagnetic_sources::charge_properties;
    use fieldcad_sources::{inertial_mass_properties, mass_component_schemas};

    fn schema_commands() -> Vec<WorldCommand> {
        mass_component_schemas()
            .into_iter()
            .chain([fieldcad_electromagnetic_sources::charge_component_schema()])
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

    #[test]
    fn mass_alone_makes_a_body_a_dynamic_particle() {
        // The composition story end to end: a bare gizmo plus mass is
        // enough. No charge, no motion mode.
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
        assert!(particles[0].needs_kinematic_authority());
    }

    #[test]
    fn a_charge_without_mass_is_not_a_particle() {
        // It is still a field source; it simply has no inertia to integrate.
        let world = world_with([ObjectSpec::new("static charge")
            .with_shape(fieldcad_core::ObjectShape::default())
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

        // Pinned and stationary needs no solver at all, so a static
        // arrangement does not churn a world revision every tick.
        assert!(!particles[0].needs_kinematic_authority());
        // Pinned with authored velocity still has to be moved by someone.
        assert!(particles[1].needs_kinematic_authority());
    }
}
