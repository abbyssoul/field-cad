# 0022 — dynamics is a first-party system, coupled by force

Status: **accepted** (revisits
[0018](0018-solvers-return-narrow-kinematic-outcomes.md),
[0019](0019-generic-particle-catalog-is-data.md))

## Context

Motion lived inside the electromagnetism plugin. Its relativistic Boris pusher
was the only thing in the project that could move a body, so "make this respond
to gravity" would have meant either a second integrator in a second plugin or a
gravity plugin that depended on an electromagnetic one. Neither is a model of
electro-gravi-dynamics; both are a model of electromagnetism with an appendix.

Two questions were tangled together: *what force does my field exert on this
body?*, which only a field system can answer, and *given the force, where does
the body go?*, which has one answer for every field.

Mass was tangled too. One `mass` component served as both the inertia in
`F = m a` and the coupling charge a gravitational field would act on. Their
numerical equality is the weak equivalence principle — a measured result — and
encoding it as a single number would have made the most interesting question
about it impossible to ask.

## Decision

- A solver answers `forces(&[DynamicBody]) -> Vec<DVec3>`: the total force in
  newtons its field exerts on each body. It reads its own coupling charge from
  the world it already adopted, so the contract never has to enumerate the
  quantities a future field might couple to.
- `fieldcad-dynamics` sums the contributions and advances every body. It reads
  inertial mass and nothing else; it does not know what charge is.
- Inertial mass and gravitational mass are separate components in
  `fieldcad-mass-sources`. Inertial mass is what makes a body dynamic.
  Gravitational mass is opt-in and carries `follows-inertial`, default true;
  while set, the authored gravitational value is not consulted at all, so the
  two cannot silently disagree. A `PropertySchema` may declare `relevant_when`,
  naming a sibling property and the value it must hold, so a generic editor
  disables a value the model will not read. Relevance is presentation, not
  validity: the inert value is still stored, so re-enabling the condition
  returns what the user last chose.
- The runtime runs the dynamics system inside the tick it already owns, and
  adopts the result through the same validated path as authored edits. It
  remains the sole world writer ([0018](0018-solvers-return-narrow-kinematic-outcomes.md)).
- A body a solver claims through `kinematic_objects` is excluded from the
  dynamics system, so exactly one integrator ever moves a body.

### Coupling is force, not potential

Force is the coupling charge times the *field*, which is minus the gradient of
the potential: `F = qE`, `F = m_g·g`. `q·φ` is a potential energy in joules, not
a force, and an accumulator built on it would be dimensionally wrong. Systems
therefore contribute forces, and any potential they also publish is an
observable rather than an input to motion.

## Consequences

A new field becomes dynamically coupled by implementing one method. Gravity will
need no integrator, no kinematic-authority declaration, and no dependency on
electromagnetism — Milestone 7's real test.

The integrator advances momentum, `p = γmv`, rather than velocity. This costs
nothing at the interface and keeps the model honest at the speeds these scenes
reach, where `F = m a` is already wrong by a percent; it also makes it
impossible to push a body past `c`.

*How* momentum turns into motion is, since the Velocity Verlet upgrade, a
closed set a session selects from (`IntegrationScheme`) rather than one
hard-coded scheme — this does not reopen the decision above. The set is
compiled in, not a plugin extension point: a new scheme is added to
`fieldcad-dynamics` itself, the same first-party module this ADR describes,
not registered by a third party the way a field system is.

**What this trade costs.** Collapsing a system's action into one force vector
means a magnetic force arrives with its velocity-dependence already evaluated,
so it cannot be applied as a rotation. A Boris push splits `qv×B` out and
rotates exactly, conserving `|v|` in a static field; this integrator does not,
and will show energy drift where Boris did not. That was accepted knowingly in
exchange for a coupling interface in which no field is privileged. An interface
carrying `{ direct, rotational }` would recover it and remains open if the drift
proves to matter.

**Not yet migrated.** The electromagnetism plugin still integrates its own
coupled particles. Its charge-conserving current deposition
([0020](0020-charge-conserving-periodic-particle-coupling.md)) is numerically
tied to the same old-to-new displacement the pusher produces, and its
intervention diagnostics distinguish solver motion from authored edits by
comparing against its own prediction. Moving that path onto the dynamics system
means reconstructing the displacement from an observed world revision and
re-deriving intervention detection; until then it declares those objects through
`kinematic_objects` and the dynamics system leaves them alone. Electrostatics is
migrated and is the worked example.
