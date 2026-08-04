//! The dynamics system: what moves things.
//!
//! Every field system answers one question — what force does my field exert on
//! this body? — and this system answers the other: given the total force, where
//! does the body go? Splitting them that way means a new field becomes
//! dynamically coupled by implementing
//! [`forces`](fieldcad_plugin_api::EquationSystemSolver::forces) and nothing
//! else, and it means motion has one implementation instead of one per plugin.
//!
//! Inertial mass is the only property this system reads. It does not know what
//! charge is, and it must not: the moment it did, a gravity plugin would be
//! reusing an abstraction that was secretly electromagnetic.
//!
//! # What the integrator does
//!
//! Forces are summed, and the body is advanced by a momentum-form leapfrog:
//!
//! ```text
//! p  = γ m v                  γ = 1 / sqrt(1 − v²/c²)
//! p += F Δt
//! v  = p / (m sqrt(1 + (p/mc)²))
//! x += v Δt
//! ```
//!
//! Integrating momentum rather than velocity costs nothing at the interface —
//! a plugin still contributes one force vector — and keeps the model honest at
//! the speeds these scenes reach, where `F = m a` is already wrong by a percent
//! or more. At low speed `γ → 1` and this reduces exactly to `a = F/m`.
//!
//! What it does *not* do is treat a magnetic force as a rotation. A Boris push
//! splits `qv×B` out and applies it as an exact rotation, conserving `|v|` in a
//! static field; a summed force cannot, because by the time the force arrives
//! its velocity-dependence has been evaluated away. That is a deliberate,
//! recorded trade for a coupling interface that no field is privileged in
//! (see `docs/adr/0022-dynamics-is-a-first-party-system.md`).

use fieldcad_core::{
    ObjectId, SPEED_OF_LIGHT, Transform, Velocity, WorldError, WorldSnapshot,
    relativistic_momentum,
};
use fieldcad_mass_sources::{MassSourceError, collect_mass_sources};
use fieldcad_plugin_api::{DynamicBody, ObjectKinematicsUpdate};
use glam::DVec3;

/// Every body the dynamics system is responsible for moving, in stable
/// object-ID order.
///
/// A body qualifies by having inertial mass and not being pinned. Pinned bodies
/// are excluded because their motion is authored: the user, not a force
/// balance, decides where they go.
pub fn collect_dynamic_bodies(world: &WorldSnapshot) -> Result<Vec<DynamicBody>, DynamicsError> {
    Ok(collect_mass_sources(world)?
        .into_iter()
        .filter(|source| !source.pinned)
        .map(|source| DynamicBody {
            object: source.object,
            inertial_mass_kg: source.inertial_mass_kg,
            position: source.position,
            velocity: source.velocity.linear,
        })
        .collect())
}

/// Bodies whose pose the user authored but which still have to be carried along.
///
/// A pinned body with a velocity moves at exactly that velocity, integrating no
/// force at all. Returning these separately keeps "the user owns this motion"
/// from being expressed as a force that happens to cancel.
pub fn collect_carried_bodies(world: &WorldSnapshot) -> Result<Vec<DynamicBody>, DynamicsError> {
    Ok(collect_mass_sources(world)?
        .into_iter()
        .filter(|source| source.pinned && source.velocity.linear != DVec3::ZERO)
        .map(|source| DynamicBody {
            object: source.object,
            inertial_mass_kg: source.inertial_mass_kg,
            position: source.position,
            velocity: source.velocity.linear,
        })
        .collect())
}

/// Sum one force contribution per field system into a total per body.
///
/// Each contribution must have one entry per body; a mismatch means a system
/// answered a different question from the one it was asked, which is rejected
/// here rather than silently mis-attributing a force to the wrong object.
pub fn accumulate_forces(
    bodies: usize,
    contributions: &[Vec<DVec3>],
) -> Result<Vec<DVec3>, DynamicsError> {
    let mut total = vec![DVec3::ZERO; bodies];
    for contribution in contributions {
        if contribution.len() != bodies {
            return Err(DynamicsError::ForceCountMismatch {
                expected: bodies,
                actual: contribution.len(),
            });
        }
        for (sum, force) in total.iter_mut().zip(contribution) {
            if !force.is_finite() {
                return Err(DynamicsError::NonFiniteForce);
            }
            *sum += *force;
        }
    }
    Ok(total)
}

/// Advance every dynamic body by one fixed step under its total force.
pub fn integrate(
    bodies: &[DynamicBody],
    forces: &[DVec3],
    seconds: f64,
) -> Result<Vec<ObjectKinematicsUpdate>, DynamicsError> {
    if forces.len() != bodies.len() {
        return Err(DynamicsError::ForceCountMismatch {
            expected: bodies.len(),
            actual: forces.len(),
        });
    }
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err(DynamicsError::InvalidTimeStep { seconds });
    }

    bodies
        .iter()
        .zip(forces)
        .map(|(body, force)| advance_body(body, *force, seconds))
        .collect()
}

/// Move a body the user is carrying at an authored velocity, integrating no
/// force.
pub fn carry(
    bodies: &[DynamicBody],
    seconds: f64,
) -> Result<Vec<ObjectKinematicsUpdate>, DynamicsError> {
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err(DynamicsError::InvalidTimeStep { seconds });
    }
    bodies
        .iter()
        .map(|body| {
            kinematics(
                body.object,
                body.position + body.velocity * seconds,
                body.velocity,
            )
        })
        .collect()
}

fn advance_body(
    body: &DynamicBody,
    force: DVec3,
    seconds: f64,
) -> Result<ObjectKinematicsUpdate, DynamicsError> {
    let mass = body.inertial_mass_kg;
    if !(mass.is_finite() && mass > 0.0) {
        return Err(DynamicsError::InvalidInertialMass {
            object: body.object,
        });
    }
    if body.velocity.length() >= SPEED_OF_LIGHT {
        return Err(DynamicsError::FasterThanLight {
            object: body.object,
        });
    }

    // Momentum form. `p = γmv` going in, `v = p / (m γ)` coming out, with γ
    // recovered from the momentum itself so the velocity can never be pushed
    // past c no matter how large the force or the step.
    let momentum = relativistic_momentum(body.velocity, mass) + force * seconds;
    let velocity = velocity_from_momentum(momentum, mass);
    kinematics(body.object, body.position + velocity * seconds, velocity)
}

/// The fastest speed this integrator will report, as a fraction of `c`.
///
/// Not physics — a floating-point guard. `v = p/(m√(1+(p/mc)²))` approaches `c`
/// from below analytically, but for a large enough momentum the `1` under the
/// root is lost to rounding and the expression evaluates to exactly `c`. A body
/// returned at exactly `c` would then be rejected by every consumer that checks
/// for subluminal motion, so the result is held just short of it.
const MAX_SPEED_FRACTION: f64 = 1.0 - 1.0e-12;

/// `v = p / (m sqrt(1 + (p/mc)²))`, the inverse that cannot exceed `c`.
fn velocity_from_momentum(momentum: DVec3, mass_kg: f64) -> DVec3 {
    let scaled = momentum.length() / (mass_kg * SPEED_OF_LIGHT);
    let lorentz = (1.0 + scaled * scaled).sqrt();
    let velocity = momentum / (mass_kg * lorentz);
    let speed = velocity.length();
    let ceiling = SPEED_OF_LIGHT * MAX_SPEED_FRACTION;
    if speed > ceiling {
        velocity * (ceiling / speed)
    } else {
        velocity
    }
}

fn kinematics(
    object: ObjectId,
    position: DVec3,
    velocity: DVec3,
) -> Result<ObjectKinematicsUpdate, DynamicsError> {
    Ok(ObjectKinematicsUpdate {
        object,
        transform: Transform::at(position)?,
        velocity: Velocity::new(velocity, DVec3::ZERO)?,
    })
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DynamicsError {
    #[error(transparent)]
    MassSource(#[from] MassSourceError),
    #[error(transparent)]
    World(#[from] WorldError),
    #[error("expected one force per body: {expected} bodies, {actual} forces")]
    ForceCountMismatch { expected: usize, actual: usize },
    #[error("a field system contributed a non-finite force")]
    NonFiniteForce,
    #[error("object {object} must have a finite, positive inertial mass to be advanced")]
    InvalidInertialMass { object: ObjectId },
    #[error("object {object} is already at or above the speed of light")]
    FasterThanLight { object: ObjectId },
    #[error("the dynamics step requires a finite, positive dt, received {seconds}")]
    InvalidTimeStep { seconds: f64 },
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectSpec, Transform as CoreTransform, World, WorldCommand};
    use fieldcad_mass_sources::{
        inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
    };

    use super::*;

    fn body(mass_kg: f64, velocity: DVec3) -> DynamicBody {
        DynamicBody {
            object: ObjectId::new(0),
            inertial_mass_kg: mass_kg,
            position: DVec3::ZERO,
            velocity,
        }
    }

    #[test]
    fn a_constant_force_produces_newtonian_acceleration_at_low_speed() {
        // The classical limit has to be exactly what a user expects, or every
        // intuition they bring to the tool is wrong.
        let body = body(2.0, DVec3::ZERO);
        let force = DVec3::new(4.0, 0.0, 0.0);

        let update = integrate(&[body], &[force], 0.5).unwrap()[0];

        // a = F/m = 2 m/s²; after 0.5 s, v = 1 m/s.
        assert!((update.velocity.linear.x - 1.0).abs() < 1.0e-12);
        assert!((update.transform.translation.x - 0.5).abs() < 1.0e-12);
    }

    #[test]
    fn force_divides_by_inertial_mass_and_nothing_else() {
        let light = integrate(&[body(1.0, DVec3::ZERO)], &[DVec3::X], 1.0).unwrap()[0];
        let heavy = integrate(&[body(4.0, DVec3::ZERO)], &[DVec3::X], 1.0).unwrap()[0];

        assert!((light.velocity.linear.x / heavy.velocity.linear.x - 4.0).abs() < 1.0e-9);
    }

    #[test]
    fn no_force_however_large_can_push_a_body_past_light_speed() {
        // The reason the integrator carries momentum rather than velocity. A
        // naive v += (F/m)·dt would happily return a superluminal body and the
        // field interpolation downstream would then be sampling nonsense.
        let body = body(9.109_383_713_9e-31, DVec3::ZERO);
        let enormous = DVec3::new(1.0e-10, 0.0, 0.0);

        let update = integrate(&[body], &[enormous], 1.0).unwrap()[0];

        assert!(
            update.velocity.linear.length() < SPEED_OF_LIGHT,
            "reached {} m/s",
            update.velocity.linear.length()
        );
    }

    #[test]
    fn relativistic_mass_increase_shows_up_as_reduced_acceleration() {
        // Same force, same rest mass, different starting speed: the faster body
        // must gain less velocity. This is the part `F = ma` gets wrong.
        let slow = body(1.0, DVec3::ZERO);
        let fast = body(1.0, DVec3::new(0.9 * SPEED_OF_LIGHT, 0.0, 0.0));
        let force = DVec3::new(1.0e8, 0.0, 0.0);

        let slow_gain = integrate(&[slow], &[force], 1.0).unwrap()[0]
            .velocity
            .linear
            .x;
        let fast_gain = integrate(&[fast], &[force], 1.0).unwrap()[0]
            .velocity
            .linear
            .x
            - fast.velocity.x;

        assert!(
            fast_gain < slow_gain,
            "a relativistic body gained {fast_gain} m/s where a slow one gained {slow_gain}"
        );
    }

    #[test]
    fn forces_from_several_systems_sum_before_the_body_is_moved() {
        // The whole point of the accumulator: gravity and electromagnetism act
        // on one body as a single resultant, not as two competing pushes.
        let total = accumulate_forces(
            2,
            &[
                vec![DVec3::new(1.0, 0.0, 0.0), DVec3::ZERO],
                vec![DVec3::new(0.0, 2.0, 0.0), DVec3::new(0.0, 0.0, -3.0)],
            ],
        )
        .unwrap();

        assert_eq!(total[0], DVec3::new(1.0, 2.0, 0.0));
        assert_eq!(total[1], DVec3::new(0.0, 0.0, -3.0));
    }

    #[test]
    fn a_system_answering_for_the_wrong_number_of_bodies_is_rejected() {
        assert_eq!(
            accumulate_forces(2, &[vec![DVec3::X]]),
            Err(DynamicsError::ForceCountMismatch {
                expected: 2,
                actual: 1
            })
        );
    }

    #[test]
    fn a_non_finite_force_is_refused_rather_than_moving_a_body_to_nowhere() {
        assert_eq!(
            accumulate_forces(1, &[vec![DVec3::new(f64::NAN, 0.0, 0.0)]]),
            Err(DynamicsError::NonFiniteForce)
        );
    }

    #[test]
    fn pinned_bodies_are_carried_rather_than_integrated() {
        let mut world = World::new();
        let commands = mass_component_schemas()
            .into_iter()
            .map(WorldCommand::RegisterComponentSchema)
            .chain([
                WorldCommand::CreateObject(
                    ObjectSpec::new("free")
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(1.0).unwrap(),
                        )
                        .with_transform(CoreTransform::at(DVec3::ZERO).unwrap()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("carried")
                        .with_pinned(true)
                        .with_velocity(Velocity::new(DVec3::X, DVec3::ZERO).unwrap())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(1.0).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("held").with_pinned(true).with_component(
                        inertial_mass_component_id(),
                        inertial_mass_properties(1.0).unwrap(),
                    ),
                ),
            ]);
        world.commit(commands).unwrap();
        let snapshot = world.snapshot();

        let dynamic = collect_dynamic_bodies(&snapshot).unwrap();
        let carried = collect_carried_bodies(&snapshot).unwrap();

        assert_eq!(dynamic.len(), 1, "only the unpinned body is integrated");
        assert_eq!(dynamic[0].object, ObjectId::new(0));
        assert_eq!(
            carried.len(),
            1,
            "a pinned, stationary body needs no update"
        );
        assert_eq!(carried[0].object, ObjectId::new(1));

        // Carried motion ignores force entirely: exactly the authored velocity.
        let update = carry(&carried, 2.0).unwrap()[0];
        assert_eq!(update.velocity.linear, DVec3::X);
        assert_eq!(update.transform.translation, DVec3::new(2.0, 0.0, 0.0));
    }
}
