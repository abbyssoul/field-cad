# 0021 — objects are composed from independent components

Status: **accepted** (revisits [0002](0002-no-ecs-for-the-world-model.md),
[0019](0019-generic-particle-catalog-is-data.md))

## Context

The world model already stored plugin data as typed components on an object, but
nothing above it worked that way. The scene panel offered seven buttons, each
fabricating a fully-formed object — a point charge, a charged sphere, an
electron, a proton. `AttachComponent` was only ever used to edit a value on a
component the object was born with, and `DetachComponent` had no caller at all.
There was no way to create an object and *then* decide what it was.

That gap showed up as three concrete defects:

- Mass was a property inside the particle component, next to motion mode and
  catalog provenance. Adding mass meant adopting all three, and
  `collect_particles` rejected any object that did not also carry charge. A
  massive, uncharged body was unrepresentable — which contradicts Milestone 7's
  requirement that mass be an independently declared component.
- The inspector branched on `charge_component_id()` and `particle_component_id()`
  by name, so a component from a new plugin rendered as nothing until the
  desktop crate was edited.
- Attaching charge to a shapeless object was rejected as an unsupported shape,
  so the compose-then-add flow could not reach a valid world even if the UI had
  offered it.

## Decision

An object is a named pose in space. Everything else is a component.

- One authoring action creates an object: a position, an optional extent, and no
  physics. Components are attached and detached afterwards.
- Each shared physical quantity owns its schema in its own crate.
  `fieldcad-mass-sources` joins `fieldcad-electromagnetic-sources` on that
  pattern, so a gravity plugin can consume mass without depending on
  electromagnetism, and one object can carry both.
- Mass is what makes a body dynamic. `collect_particles` iterates authored
  masses; charge is optional and defaults to zero, because an uncharged body is
  neutral rather than invalid.
- Motion is not a component. Every object has a pose, and a pose that changes is
  velocity, so the only real question is *who decides* it.
  `WorldObject::pinned` answers that: unpinned means a solver integrates the
  motion, pinned means the authored transform and velocity are followed exactly.
  This replaces the three-way `MotionMode`; pinned with zero velocity is the old
  `Fixed`, and pinned with authored velocity is the old `Prescribed`.
- The inspector renders components from their registered `ComponentSchema` and
  offers every registered-but-unattached schema. It contains no knowledge of any
  specific component.
- An object with no shape is a point. Both charge and mass treat it that way,
  because it is the intermediate state every composed object passes through.
- Probes and slice planes are grouped separately in the scene panel and labelled
  as not simulated. They are questions asked about the world, not part of it.

## Consequences

Composition is now the only way physics enters a scene, which makes the
inspector's component list the single place to look for what an object will do.
A new plugin's component is authorable and editable the moment its schema is
registered — the Milestone 7 exit criterion, met before gravity exists to test
it.

Catalog templates no longer have scene-panel buttons. A template remains a
shortcut that attaches mass, charge, and provenance together, but the desktop no
longer exposes one; the equivalent arrangement is now three menu selections.
Reintroducing presets is a UI addition, not a model change.

Provenance became a claim that is checked rather than trusted. A generic property
editor cannot know to reset a catalog label when a mass is edited, so
[0019](0019-generic-particle-catalog-is-data.md)'s requirement is enforced in
`collect_particles`: a template whose published values no longer match the
authored ones reports as `Custom`.

[0002](0002-no-ecs-for-the-world-model.md) stands. The composition this record
describes is about component *granularity* and a schema-driven UI, neither of
which an ECS library supplies. `World::commit` remains an atomic validated
transaction over an immutable snapshot, which is what
[0007](0007-validate-before-adopting-a-world-edit.md),
[0011](0011-queue-running-edits-at-fixed-tick-boundaries.md), and
[0018](0018-solvers-return-narrow-kinematic-outcomes.md) rest on, and what keeps
the remote and WebAssembly options in
[0005](0005-defer-runtime-plugins.md) open.
