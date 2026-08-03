# 0014 — Field-system activation is scene state, separate from object schemas

## Context

An equation-system plugin may publish several coupled channels, such as Maxwell
`E` and `B`, and may also contribute object properties such as charge or mass.
Users need to compose a scene from the available equation systems without losing
authored properties whenever one solver is inactive. Published snapshot
provenance cannot serve as the catalog because inactive systems publish nothing.

Time-stepped solver memory also cannot simply be paused while the shared scene
clock advances: resuming stale state under a later snapshot time would make the
field's timestamp false.

## Decision

- The activation unit is an equation system, not an individual field channel.
  Coupled channels are enabled and disabled together.
- The field data source exposes every available system, its plugin metadata,
  declared channels, and authoritative enabled state independently of snapshots.
- Activation changes cross the same command boundary used by local and future
  remote compute.
- Plugin component schemas remain registered on the world while inactive, so
  objects retain and can edit those properties.
- An inactive system performs no world validation, time-step validation, world
  adoption, stepping, diagnostics, sampling, or snapshot publication.
- Disabling releases its solver instance. Enabling creates a new solver from the
  current world, domain, configuration, scene tick, time, and time step, then
  validates the current sampling budget before activation is adopted.
- Only active plugins appear in snapshot provenance. A rejected enable command
  leaves the system inactive and preserves the last complete snapshot.

## Consequences

The scene inspector can always show available fields, even when no values are
currently published. Charge, mass, and future plugin properties are durable
scene data rather than side effects of solver lifetime. Local, asynchronous, and
remote-style sources expose the same composition semantics.

Plugins must be able to initialize at a non-zero scene time. Restarting a
time-stepped system intentionally creates fresh state at that time; it does not
silently catch up every missed tick. Dependencies between separate equation
systems will require an explicit compatibility/dependency contract before such
plugins are introduced.

Status: accepted.
