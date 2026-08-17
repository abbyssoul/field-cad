//! The dynamics system: what moves things.
//!
//! Every field system answers one question — what force does my field exert on
//! this body? — and this system answers the other: given the total force, where
//! does the body go? Splitting them that way means a new field becomes
//! dynamically coupled by implementing
//! [`add_forces`](fieldcad_plugin_api::EquationSystemSolver::add_forces) and
//! nothing else, and it means motion has one implementation instead of one
//! per plugin.
//!
//! Inertial mass is the only property this system reads. It does not know what
//! charge is, and it must not: the moment it did, a gravity plugin would be
//! reusing an abstraction that was secretly electromagnetic.
//!
//! # Integration schemes
//!
//! How a summed force turns into motion is a choice, [`IntegrationScheme`],
//! made once per running simulation (see `SimulationRuntime::set_integration_scheme`
//! in `fieldcad-simulation`) rather than hard-coded. Both schemes share the
//! same relativistic momentum machinery — [`relativistic_momentum`] going in,
//! `velocity_from_momentum` coming out — so a body can never be pushed past
//! `c` no matter which scheme is driving it.
//!
//! ## Symplectic Euler
//!
//! One force evaluation per tick, applied via [`integrate`]:
//!
//! ```text
//! p  = γ m v                  γ = 1 / sqrt(1 − v²/c²)
//! p += F Δt
//! v  = p / (m sqrt(1 + (p/mc)²))
//! x += v Δt
//! ```
//!
//! First-order accurate: correct in the classical limit, but its `O(Δt)`
//! truncation error shows up as phase lag and orbital precession over a long
//! run unless the step is kept small.
//!
//! ## Relativistic Velocity Verlet (default)
//!
//! Second-order accurate, via a half-kick/drift/half-kick split across
//! [`verlet_half_step`] and [`verlet_finish_step`]:
//!
//! ```text
//! p_(n+1/2) = p(v_n) + F_n Δt/2      // half-kick with the *previous* tick's force
//! v_(n+1/2) = p_(n+1/2) → velocity
//! x_(n+1)   = x_n + v_(n+1/2) Δt     // drift
//! F_(n+1)   = forces(x_(n+1), v_(n+1/2))   // the one new evaluation this tick
//! p_(n+1)   = p_(n+1/2) + F_(n+1) Δt/2
//! v_(n+1)   = p_(n+1) → velocity
//! ```
//!
//! `F_n` is the force this scheme itself produced last tick, so a caller that
//! retains it (as `SimulationRuntime::last_forces` already does) pays for only
//! one new force evaluation per tick, the same as Symplectic Euler, while
//! gaining a full order of accuracy.
//!
//! Integrating momentum rather than velocity costs nothing at the interface —
//! a plugin still contributes one force vector — and keeps the model honest at
//! the speeds these scenes reach, where `F = m a` is already wrong by a percent
//! or more. At low speed `γ → 1` and Symplectic Euler's update reduces exactly
//! to `a = F/m`.
//!
//! What neither scheme does is treat a magnetic force as a rotation. A Boris
//! push splits `qv×B` out and applies it as an exact rotation, conserving `|v|`
//! in a static field; a summed force cannot, because by the time the force
//! arrives its velocity-dependence has been evaluated away. That is a
//! deliberate, recorded trade for a coupling interface that no field is
//! privileged in (see `docs/adr/0022-dynamics-is-a-first-party-system.md`).

use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    MassAggregateSample, MassSelection, ObjectId, SPEED_OF_LIGHT, Transform, Velocity, WorldError,
    WorldSnapshot, relativistic_momentum,
};
use fieldcad_plugin_api::{DynamicBody, ObjectKinematicsUpdate};
use fieldcad_sources::{SourceError, inertial_mass_component_id, inertial_mass_of};
use glam::DVec3;

/// Which numerical scheme advances a dynamic body from its summed force.
///
/// A closed, compile-time set — dynamics is a first-party system, not a
/// plugin extension point (`docs/adr/0022-dynamics-is-a-first-party-system.md`)
/// — but the set is small and named so a session can select and report which
/// one is running, the same way it selects a domain precision or boundary
/// condition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IntegrationScheme {
    /// First-order. One force evaluation per tick, at the pre-tick state.
    /// Cheapest, and the longtime baseline; kept for comparison and for scenes
    /// that don't need long-run orbital accuracy.
    SymplecticEuler,
    /// Second-order. One new force evaluation per tick (amortized — see the
    /// module docs), at the half-step state. The default: better trajectory
    /// accuracy at the same per-tick cost as Symplectic Euler.
    #[default]
    VelocityVerlet,
}

impl IntegrationScheme {
    pub const ALL: [Self; 2] = [Self::SymplecticEuler, Self::VelocityVerlet];

    pub const fn label(self) -> &'static str {
        match self {
            Self::SymplecticEuler => "Symplectic Euler",
            Self::VelocityVerlet => "Relativistic Velocity Verlet",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::SymplecticEuler => {
                "First-order. One force evaluation per tick. Simple and cheap, but phase lag \
                 and orbital precession accumulate over a long run."
            }
            Self::VelocityVerlet => {
                "Second-order. Same per-tick cost as Symplectic Euler, with a full order more \
                 trajectory accuracy — the default for long-running scenes."
            }
        }
    }
}

/// Every body the dynamics system is responsible for, partitioned into unpinned
/// (force-integrated) and pinned-moving (carried at authored velocity) bodies.
pub fn collect_bodies(
    world: &WorldSnapshot,
) -> Result<(Vec<DynamicBody>, Vec<DynamicBody>), DynamicsError> {
    let mut dynamic = Vec::new();
    let mut carried = Vec::new();
    for (object, _properties) in world.objects_with(&inertial_mass_component_id()) {
        let inertial_mass_kg = inertial_mass_of(object)?;
        let body = DynamicBody {
            object: object.id,
            inertial_mass_kg,
            position: object.transform.translation,
            velocity: object.velocity.linear,
        };
        if !object.pinned {
            dynamic.push(body);
        } else if object.velocity.linear != DVec3::ZERO {
            carried.push(body);
        }
    }
    Ok((dynamic, carried))
}

/// Every object carrying inertial mass, regardless of pinned state.
///
/// Unlike [`collect_bodies`], this keeps pinned-and-stationary bodies too:
/// that partition is about which bodies need a per-tick kinematics update,
/// but a pinned, stationary mass still has a position and (zero) velocity
/// that physical totals like center of mass and kinetic energy must include.
pub fn collect_mass_bearing_bodies(
    world: &WorldSnapshot,
) -> Result<Vec<DynamicBody>, DynamicsError> {
    world
        .objects_with(&inertial_mass_component_id())
        .map(|(object, _properties)| {
            Ok(DynamicBody {
                object: object.id,
                inertial_mass_kg: inertial_mass_of(object)?,
                position: object.transform.translation,
                velocity: object.velocity.linear,
            })
        })
        .collect()
}

/// Live totals over the mass-bearing bodies a [`MassSelection`] names —
/// center of mass, its own velocity, total momentum, angular momentum, and
/// total kinetic energy. `None` when no member currently carries mass (a
/// zero or negative total is otherwise impossible: mass is validated
/// positive wherever it is attached).
///
/// Takes an already-collected body slice rather than a [`WorldSnapshot`] so a
/// caller computing this for several probes in the same tick — as
/// `SimulationRuntime` does — pays for [`collect_mass_bearing_bodies`]'s
/// world walk once, not once per probe.
///
/// Momentum and kinetic energy use the same relativistic formulas as a
/// single object's own derived-values display (`relativistic_momentum`/
/// `relativistic_kinetic_energy`), so this total never quietly disagrees
/// with the per-object numbers it's summed from. `velocity` is deliberately
/// *not* derived from `total_momentum`: it uses the same rest-mass weighting
/// as `center_of_mass`, so it is that point's own time-derivative rather
/// than a relativistic quantity with a different physical meaning.
/// `angular_momentum` is taken about the centroid itself (each body's `r` is
/// its position relative to `center_of_mass`), giving the system's intrinsic
/// angular momentum rather than a value that shifts with an arbitrary choice
/// of origin.
pub fn mass_aggregate<'a>(
    bodies: impl Iterator<Item = &'a DynamicBody>,
    selection: &MassSelection,
) -> Option<MassAggregateSample> {
    let members: Vec<&DynamicBody> = bodies
        .filter(|body| selection.includes(body.object))
        .collect();
    let total_mass_kg: f64 = members
        .iter()
        .map(|body| body.inertial_mass_kg.into_si())
        .sum();
    if total_mass_kg <= 0.0 {
        return None;
    }
    let center_of_mass = members
        .iter()
        .map(|body| body.position * body.inertial_mass_kg.into_si())
        .sum::<DVec3>()
        / total_mass_kg;
    let velocity = members
        .iter()
        .map(|body| body.velocity * body.inertial_mass_kg.into_si())
        .sum::<DVec3>()
        / total_mass_kg;
    let momenta: Vec<DVec3> = members
        .iter()
        .map(|body| {
            fieldcad_core::relativistic_momentum(body.velocity, body.inertial_mass_kg.into_si())
        })
        .collect();
    let total_momentum: DVec3 = momenta.iter().copied().sum();
    let angular_momentum: DVec3 = members
        .iter()
        .zip(&momenta)
        .map(|(body, &momentum)| (body.position - center_of_mass).cross(momentum))
        .sum();
    let total_kinetic_energy_j = members
        .iter()
        .map(|body| {
            fieldcad_core::relativistic_kinetic_energy(
                body.velocity,
                body.inertial_mass_kg.into_si(),
            )
        })
        .sum();
    Some(MassAggregateSample {
        center_of_mass,
        velocity,
        total_momentum,
        angular_momentum,
        total_kinetic_energy_j,
        total_mass_kg,
        member_count: members.len(),
    })
}

/// Confirm that every entry a plugin's `add_forces` call just accumulated
/// into `out` is still finite.
///
/// Called once per enabled plugin, immediately after its contribution has
/// been added in place — see
/// [`add_forces`](fieldcad_plugin_api::EquationSystemSolver::add_forces) —
/// so a system whose result overflowed or divided by zero is caught at the
/// plugin that produced it, rather than laundered into a body's position
/// several steps later.
///
/// There is no length to check here the way summing separate per-plugin
/// contributions once had to: `out` is a slice sized once by the caller
/// before any plugin runs, and a plugin has no way to grow or shrink it, so
/// a wrong body count is a caller bug the type system already prevents, not
/// a condition this needs to guard against at runtime.
pub fn validate_forces_finite(out: &[DVec3]) -> Result<(), DynamicsError> {
    if out.iter().any(|force| !force.is_finite()) {
        return Err(DynamicsError::NonFiniteForce);
    }
    Ok(())
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
    let mass = validated_mass(body)?;
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

/// The Velocity Verlet half-kick and drift: advance every body's position to
/// `x_(n+1)` using its *previous* tick's cached force, and its velocity to the
/// half-step `v_(n+1/2)` a caller feeds back into
/// [`add_forces`](fieldcad_plugin_api::EquationSystemSolver::add_forces) for
/// the one new evaluation this tick.
///
/// The returned bodies are a legitimate `DynamicBody` list in their own
/// right — `position` is `x_(n+1)`, `velocity` is `v_(n+1/2)` — so they can be
/// passed straight to a plugin's `add_forces` and then on to
/// [`verlet_finish_step`] with no extra bookkeeping in between.
pub fn verlet_half_step(
    bodies: &[DynamicBody],
    cached_forces: &[DVec3],
    seconds: f64,
) -> Result<Vec<DynamicBody>, DynamicsError> {
    if cached_forces.len() != bodies.len() {
        return Err(DynamicsError::ForceCountMismatch {
            expected: bodies.len(),
            actual: cached_forces.len(),
        });
    }
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err(DynamicsError::InvalidTimeStep { seconds });
    }
    bodies
        .iter()
        .zip(cached_forces)
        .map(|(body, force)| half_kick_and_drift(body, *force, seconds))
        .collect()
}

fn half_kick_and_drift(
    body: &DynamicBody,
    cached_force: DVec3,
    seconds: f64,
) -> Result<DynamicBody, DynamicsError> {
    let mass = validated_mass(body)?;
    if body.velocity.length() >= SPEED_OF_LIGHT {
        return Err(DynamicsError::FasterThanLight {
            object: body.object,
        });
    }
    let half_momentum = relativistic_momentum(body.velocity, mass) + cached_force * (seconds * 0.5);
    let half_velocity = velocity_from_momentum(half_momentum, mass);
    Ok(DynamicBody {
        object: body.object,
        inertial_mass_kg: body.inertial_mass_kg,
        position: body.position + half_velocity * seconds,
        velocity: half_velocity,
    })
}

/// The Velocity Verlet final half-kick: finish each body's velocity using the
/// force evaluated at the half-step state [`verlet_half_step`] produced. The
/// position is already final — `half_bodies[i].position` is `x_(n+1)` — so
/// this only recovers `p_(n+1/2)` from `half_bodies[i].velocity` and adds the
/// second half-kick.
pub fn verlet_finish_step(
    half_bodies: &[DynamicBody],
    new_forces: &[DVec3],
    seconds: f64,
) -> Result<Vec<ObjectKinematicsUpdate>, DynamicsError> {
    if new_forces.len() != half_bodies.len() {
        return Err(DynamicsError::ForceCountMismatch {
            expected: half_bodies.len(),
            actual: new_forces.len(),
        });
    }
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err(DynamicsError::InvalidTimeStep { seconds });
    }
    half_bodies
        .iter()
        .zip(new_forces)
        .map(|(half_body, force)| finish_kick(half_body, *force, seconds))
        .collect()
}

fn finish_kick(
    half_body: &DynamicBody,
    new_force: DVec3,
    seconds: f64,
) -> Result<ObjectKinematicsUpdate, DynamicsError> {
    let mass = validated_mass(half_body)?;
    // `half_body.velocity` is v_(n+1/2); recovering p_(n+1/2) from it, rather
    // than threading the half-step momentum through as extra state, is what
    // keeps both Verlet stages plain `DynamicBody -> DynamicBody` functions
    // with no hidden coupling between them.
    let half_momentum = relativistic_momentum(half_body.velocity, mass);
    let momentum = half_momentum + new_force * (seconds * 0.5);
    let velocity = velocity_from_momentum(momentum, mass);
    kinematics(half_body.object, half_body.position, velocity)
}

fn validated_mass(body: &DynamicBody) -> Result<f64, DynamicsError> {
    let mass = body.inertial_mass_kg.into_si();
    if !(mass.is_finite() && mass > 0.0) {
        return Err(DynamicsError::InvalidInertialMass {
            object: body.object,
        });
    }
    Ok(mass)
}

/// The fastest speed this integrator will report, as a fraction of `c`.
///
/// Not physics — a floating-point guard. `v = p/(m√(1+(p/mc)²))` approaches `c`
/// from below analytically, but for a large enough momentum the `1` under the
/// root is lost to rounding and the expression evaluates to exactly `c`. A body
/// returned at exactly `c` would then be rejected by every consumer that checks
/// for subluminal motion, so the result is held just short of it.
const MAX_SPEED_FRACTION: f64 = 1.0 - 1.0e-12;

const SPEED_CEILING: f64 = SPEED_OF_LIGHT * MAX_SPEED_FRACTION;

/// `v = p / (m sqrt(1 + (p/mc)²))`, the inverse that cannot exceed `c`.
fn velocity_from_momentum(momentum: DVec3, mass_kg: f64) -> DVec3 {
    let scaled = momentum.length() / (mass_kg * SPEED_OF_LIGHT);
    let lorentz = (1.0 + scaled * scaled).sqrt();
    let velocity = momentum / (mass_kg * lorentz);
    let speed = velocity.length();

    if speed > SPEED_CEILING {
        velocity * (SPEED_CEILING / speed)
    } else {
        velocity
    }
}

// TODO:
// 1. Ensure inputs are finite
// 2. Change signature to not return a result, but direct value
fn kinematics(
    object: ObjectId,
    position: DVec3,
    velocity: DVec3,
) -> Result<ObjectKinematicsUpdate, DynamicsError> {
    Ok(ObjectKinematicsUpdate {
        object,
        transform: Transform::at_finite(position),
        velocity: Velocity::new_linear_finite(velocity),
    })
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DynamicsError {
    #[error(transparent)]
    Source(#[from] SourceError),
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
    use std::collections::BTreeSet;

    use fieldcad_core::{
        ObjectSpec, Transform as CoreTransform, World, WorldCommand,
        quantities::{MassKg, kilogram},
    };
    use fieldcad_sources::{
        inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
    };

    use super::*;

    fn body(mass_kg: f64, velocity: DVec3) -> DynamicBody {
        DynamicBody {
            object: ObjectId::new(0),
            inertial_mass_kg: MassKg::new::<kilogram>(mass_kg),
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
        // The whole point of in-place accumulation: gravity and
        // electromagnetism act on one body as a single resultant, not as two
        // competing pushes. `add_forces` implementations add into `out`
        // rather than returning their own vector (see
        // `fieldcad_plugin_api::EquationSystemSolver::add_forces`); this
        // reproduces that contract against a shared buffer, the way
        // `SimulationRuntime::eval_forces` drives real plugins.
        let mut total = vec![DVec3::ZERO; 2];

        for (out_force, force) in total
            .iter_mut()
            .zip([DVec3::new(1.0, 0.0, 0.0), DVec3::ZERO])
        {
            *out_force += force;
        }
        validate_forces_finite(&total).unwrap();

        // Second system's contribution, added on top rather than overwriting.
        for (out_force, force) in total
            .iter_mut()
            .zip([DVec3::new(0.0, 2.0, 0.0), DVec3::new(0.0, 0.0, -3.0)])
        {
            *out_force += force;
        }
        validate_forces_finite(&total).unwrap();

        assert_eq!(total[0], DVec3::new(1.0, 2.0, 0.0));
        assert_eq!(total[1], DVec3::new(0.0, 0.0, -3.0));
    }

    #[test]
    fn a_non_finite_force_is_refused_rather_than_moving_a_body_to_nowhere() {
        assert_eq!(
            validate_forces_finite(&[DVec3::new(f64::NAN, 0.0, 0.0)]),
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
                            inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
                        )
                        .with_transform(CoreTransform::default()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("carried")
                        .with_pinned(true)
                        .with_velocity(Velocity::new(DVec3::X, DVec3::ZERO).unwrap())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("held").with_pinned(true).with_component(
                        inertial_mass_component_id(),
                        inertial_mass_properties(MassKg::new::<kilogram>(1.0)).unwrap(),
                    ),
                ),
            ]);
        world.commit(commands).unwrap();
        let snapshot = world.snapshot();

        let (dynamic, carried) = collect_bodies(&snapshot).unwrap();

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

        // `collect_mass_bearing_bodies` keeps all three, including "held" —
        // it's about physical bookkeeping, not who needs a tick update.
        let all = collect_mass_bearing_bodies(&snapshot).unwrap();
        assert_eq!(all.len(), 3);
    }

    /// Three equal-mass bodies: `a` at x=0 (stationary), `b` at x=4 moving at
    /// (1,0,0), `c` at x=100 (stationary) — `c` is placed far away so a test
    /// that excludes/omits it can tell at a glance whether it was actually
    /// dropped from the sum. Returns `(world, [a, b, c] object ids)`.
    fn three_body_world() -> (World, [ObjectId; 3]) {
        let mut world = World::new();
        let commands = mass_component_schemas()
            .into_iter()
            .map(WorldCommand::RegisterComponentSchema)
            .chain([
                WorldCommand::CreateObject(
                    ObjectSpec::new("a")
                        .with_transform(CoreTransform::default())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(2.0)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("b")
                        .with_transform(CoreTransform::at_finite(DVec3::new(4.0, 0.0, 0.0)))
                        .with_velocity(Velocity::new(DVec3::X, DVec3::ZERO).unwrap())
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(2.0)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("c")
                        .with_transform(CoreTransform::at_finite(DVec3::new(100.0, 0.0, 0.0)))
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(2.0)).unwrap(),
                        ),
                ),
            ]);
        let report = world.commit(commands).unwrap();
        let ids: [ObjectId; 3] = report.created_objects.try_into().unwrap();
        (world, ids)
    }

    #[test]
    fn mass_aggregate_is_none_without_any_mass() {
        let world = World::new();
        let bodies = collect_mass_bearing_bodies(&world.snapshot()).unwrap();
        let selection = MassSelection::Universe {
            excluded: BTreeSet::new(),
        };
        assert_eq!(mass_aggregate(bodies.iter(), &selection), None);
    }

    #[test]
    fn mass_aggregate_computes_center_of_mass_velocity_momentum_and_energy() {
        let (world, [a, b, _c]) = three_body_world();
        let bodies = collect_mass_bearing_bodies(&world.snapshot()).unwrap();
        let selection = MassSelection::Selection {
            included: BTreeSet::from([a, b]),
        };

        let summary = mass_aggregate(bodies.iter(), &selection).unwrap();

        // Equal masses at x=0 and x=4: centroid at x=2.
        assert!((summary.center_of_mass - DVec3::new(2.0, 0.0, 0.0)).length() < 1.0e-12);
        // v_com = (m_a*0 + m_b*(1,0,0)) / (m_a+m_b) = (0.5, 0, 0).
        assert!((summary.velocity - DVec3::new(0.5, 0.0, 0.0)).length() < 1.0e-12);
        // p = m*v, only "b" moves: 2 kg * (1, 0, 0) m/s.
        assert!((summary.total_momentum - DVec3::new(2.0, 0.0, 0.0)).length() < 1.0e-12);
        // KE = 1/2 * 2 * 1^2 = 1 J.
        assert!((summary.total_kinetic_energy_j - 1.0).abs() < 1.0e-12);
        assert!((summary.total_mass_kg - 4.0).abs() < 1.0e-12);
        assert_eq!(summary.member_count, 2);
    }

    #[test]
    fn mass_aggregate_computes_angular_momentum_about_the_centroid() {
        // Two equal masses placed symmetrically about the origin, each with
        // a tangential velocity: the origin is their centroid, so this is a
        // hand-computable case. L = Σ r×p:
        // (1,0,0)×(0,1,0) + (-1,0,0)×(0,-1,0) = (0,0,1) + (0,0,1) = (0,0,2).
        let a = DynamicBody {
            object: ObjectId::new(0),
            inertial_mass_kg: MassKg::new::<kilogram>(1.0),
            position: DVec3::new(1.0, 0.0, 0.0),
            velocity: DVec3::new(0.0, 1.0, 0.0),
        };
        let b = DynamicBody {
            object: ObjectId::new(1),
            inertial_mass_kg: MassKg::new::<kilogram>(1.0),
            position: DVec3::new(-1.0, 0.0, 0.0),
            velocity: DVec3::new(0.0, -1.0, 0.0),
        };
        let selection = MassSelection::Universe {
            excluded: BTreeSet::new(),
        };

        let summary = mass_aggregate([a, b].iter(), &selection).unwrap();

        assert!(summary.center_of_mass.length() < 1.0e-12);
        assert!((summary.angular_momentum - DVec3::new(0.0, 0.0, 2.0)).length() < 1.0e-9);
    }

    #[test]
    fn mass_aggregate_universe_mode_drops_an_excluded_object() {
        let (world, [_a, _b, c]) = three_body_world();
        let bodies = collect_mass_bearing_bodies(&world.snapshot()).unwrap();
        let selection = MassSelection::Universe {
            excluded: BTreeSet::from([c]),
        };

        let summary = mass_aggregate(bodies.iter(), &selection).unwrap();

        // Same result as the two-body (a, b) case above: excluding "c" at
        // x=100 must pull the centroid back from wherever including it would
        // put it.
        assert!((summary.center_of_mass - DVec3::new(2.0, 0.0, 0.0)).length() < 1.0e-9);
        assert_eq!(summary.member_count, 2);
    }

    #[test]
    fn mass_aggregate_selection_mode_ignores_unlisted_objects() {
        let (world, [a, b, _c]) = three_body_world();
        let bodies = collect_mass_bearing_bodies(&world.snapshot()).unwrap();
        let selection = MassSelection::Selection {
            included: BTreeSet::from([a, b]),
        };

        let summary = mass_aggregate(bodies.iter(), &selection).unwrap();

        assert!((summary.center_of_mass - DVec3::new(2.0, 0.0, 0.0)).length() < 1.0e-9);
        assert_eq!(summary.member_count, 2);
    }

    #[test]
    fn mass_aggregate_selection_mode_with_no_members_is_none() {
        let (world, _ids) = three_body_world();
        let bodies = collect_mass_bearing_bodies(&world.snapshot()).unwrap();
        let selection = MassSelection::Selection {
            included: BTreeSet::new(),
        };

        assert_eq!(mass_aggregate(bodies.iter(), &selection), None);
    }

    #[test]
    fn verlet_matches_newtonian_kinematics_for_a_constant_force_at_low_speed() {
        let mass = 2.0;
        let force = DVec3::new(4.0, 0.0, 0.0);
        let dt = 0.5;

        let mut current = body(mass, DVec3::ZERO);
        // Spatially uniform, so the force at the starting position is already
        // known — this test is about the two-stage math, not cold-start
        // behaviour (a fresh object with no prior tick is a runtime concern,
        // not a `fieldcad-dynamics` one).
        let mut cached_force = force;
        let mut time = 0.0;
        for _ in 0..4 {
            let half = verlet_half_step(&[current], &[cached_force], dt).unwrap();
            let update = verlet_finish_step(&half, &[force], dt).unwrap()[0];
            current = DynamicBody {
                position: update.transform.translation,
                velocity: update.velocity.linear,
                ..current
            };
            cached_force = force;
            time += dt;
        }

        // a = F/m = 2 m/s², velocity-independent, so Velocity Verlet is exact
        // for this case: v = a t, x = ½ a t².
        let acceleration = force.x / mass;
        assert!((current.velocity.x - acceleration * time).abs() < 1.0e-9);
        assert!((current.position.x - 0.5 * acceleration * time * time).abs() < 1.0e-9);
    }

    #[test]
    fn verlet_cannot_push_a_body_past_light_speed() {
        // Same guard as Symplectic Euler's, exercised across both Verlet
        // stages: the half-kick must not exceed c, and neither must the final
        // kick built from it.
        let body = body(9.109_383_713_9e-31, DVec3::ZERO);
        let enormous = DVec3::new(1.0e-10, 0.0, 0.0);

        let half = verlet_half_step(&[body], &[enormous], 1.0).unwrap();
        assert!(
            half[0].velocity.length() < SPEED_OF_LIGHT,
            "half-step reached {} m/s",
            half[0].velocity.length()
        );

        let update = verlet_finish_step(&half, &[enormous], 1.0).unwrap();
        assert!(
            update[0].velocity.linear.length() < SPEED_OF_LIGHT,
            "final step reached {} m/s",
            update[0].velocity.linear.length()
        );
    }

    #[test]
    fn verlet_conserves_energy_better_than_symplectic_euler_for_a_harmonic_oscillator() {
        // A spring force F = -kx is velocity-independent, so both schemes can
        // be driven from nothing but each tick's position. Velocity Verlet's
        // extra order of accuracy should show up directly as a smaller peak
        // energy drift over the same run — this is the design doc's §5.2
        // "Harmonic Oscillator Test".
        let mass = 1.0;
        let k = 1.0;
        let dt = 0.1;
        let steps = 2_000;
        let start_position = DVec3::new(1.0, 0.0, 0.0);

        let energy = |position: DVec3, velocity: DVec3| {
            0.5 * mass * velocity.length_squared() + 0.5 * k * position.length_squared()
        };
        let initial_energy = energy(start_position, DVec3::ZERO);

        let base = body(mass, DVec3::ZERO);

        // Symplectic Euler: one evaluation per tick, at the current position.
        let mut euler_position = start_position;
        let mut euler_velocity = DVec3::ZERO;
        let mut euler_max_drift = 0.0_f64;
        for _ in 0..steps {
            let current = DynamicBody {
                position: euler_position,
                velocity: euler_velocity,
                ..base
            };
            let force = euler_position * -k;
            let update = integrate(&[current], &[force], dt).unwrap()[0];
            euler_position = update.transform.translation;
            euler_velocity = update.velocity.linear;
            let drift =
                (energy(euler_position, euler_velocity) - initial_energy).abs() / initial_energy;
            euler_max_drift = euler_max_drift.max(drift);
        }

        // Velocity Verlet: half-kick with the previous tick's force, drift,
        // evaluate at the half-step state, finish. Seeded with the exact
        // force at the start, since it's known analytically here.
        let mut verlet_position = start_position;
        let mut verlet_velocity = DVec3::ZERO;
        let mut cached_force = start_position * -k;
        let mut verlet_max_drift = 0.0_f64;
        for _ in 0..steps {
            let current = DynamicBody {
                position: verlet_position,
                velocity: verlet_velocity,
                ..base
            };
            let half = verlet_half_step(&[current], &[cached_force], dt).unwrap();
            let new_force = half[0].position * -k;
            let update = verlet_finish_step(&half, &[new_force], dt).unwrap()[0];
            verlet_position = update.transform.translation;
            verlet_velocity = update.velocity.linear;
            cached_force = new_force;
            let drift =
                (energy(verlet_position, verlet_velocity) - initial_energy).abs() / initial_energy;
            verlet_max_drift = verlet_max_drift.max(drift);
        }

        assert!(
            verlet_max_drift < euler_max_drift * 0.5,
            "expected Verlet's peak relative energy drift ({verlet_max_drift}) to be well below \
             Symplectic Euler's ({euler_max_drift}) over the same {steps}-step run"
        );
    }
}
