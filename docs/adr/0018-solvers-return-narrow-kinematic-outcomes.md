# 0018 — solvers return narrow kinematic outcomes

## Context

The world owns every object's canonical transform and velocity, but the plugin
Interface previously allowed a time-stepped solver to mutate only private field
state. Milestone 6 needs a coupled electromagnetic Implementation to deposit
sources, push dynamic charged objects, and publish their new poses without
giving plugins mutable world access or routing physical motion through the UI.

Allowing a solver to emit arbitrary world commands would make ownership and
validation shallow. Multiple active equation systems could also attempt to
integrate the same object in one tick.

## Decision

- A solver declares the object IDs for which it has kinematic authority.
- Before any solver advances, the runtime verifies those objects exist and that
  exactly one active solver claims each of them.
- `step` returns a `SolverStepOutcome`. Its only world-facing result is a list of
  complete transform-and-velocity updates for declared objects.
- The runtime remains the sole world writer. It adopts all returned kinematics
  through the same validate-before-adopt Interface as authored commands,
  notifies every active solver of the new revision, and republishes all fields
  because motion may invalidate analytic systems too.
- A solver cannot emit arbitrary create/delete/component commands as a side
  effect of stepping.

## Consequences

The Interface is deep enough for Lorentz-force particle motion while preserving
one authoritative world and one revision path. Motion conflicts and undeclared,
missing, or duplicate updates fail with stable runtime error codes. Solvers that
only evolve fields return an empty outcome and retain their existing behaviour.

The numerical ordering of deposition, field advance, interpolation, and particle
pushing remains inside the coupled equation system; the runtime does not impose
one integrator on future gravity or other plugins. Authored drag/edit commands
remain external interventions and are not confused with solver-produced motion.

Status: accepted, revisited by
[0022](0022-dynamics-is-a-first-party-system.md).

## Revisited (0022)

The runtime is still the sole world writer and still validates every returned
kinematic result — that part is unchanged. What changed is who produces them:
a first-party dynamics system now moves any body with inertial mass, and a
solver contributes a force instead of a trajectory. `kinematic_objects` survives
as the way a solver reserves a body it must integrate itself, which is currently
only the coupled Maxwell particle path.
