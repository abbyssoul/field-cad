# 0002 — A plain object model, not an ECS

Status: **accepted** (Milestone 0)

## Context

Field CAD has objects with transforms, plugin-contributed properties, and a
handful of systems that read them. That shape invites an entity-component-system
library, and Rust has good ones.

But the access patterns are not known yet. We do not know whether solvers will
iterate objects, index them spatially, or mostly ignore them in favour of grid
state. Choosing a storage architecture before knowing its query pattern picks the
answer before the question.

There is a second, larger cost: the plugin contract is the thing this project
must get right. An ECS makes the natural plugin boundary "a system with a query",
which drags the host's storage layout into every plugin. That is exactly the
coupling that would make an out-of-process or WebAssembly plugin impossible
later ([0005](0005-defer-runtime-plugins.md)).

## Decision

Use a plain `World` of `BTreeMap<ObjectId, WorldObject>` behind an immutable
`WorldSnapshot`. Plugin data attaches as typed `PropertyBag` components keyed by
`ComponentTypeId`. Solvers receive a read-only snapshot, not a query handle.

Object counts in the first milestones are in the tens. `BTreeMap` is not the
bottleneck; a field solve is.

## Consequences

- Iterating "every object with a charge" is a filter over all objects rather than
  an indexed query. Fine at current scale; `WorldSnapshot::objects_with` exists
  so the call sites do not have to change if the storage does.
- Revisit when a solver demonstrates a query pattern that hurts — with a
  measurement, not a projection.
- The plugin contract stays expressible over a serializable snapshot, which is
  what keeps the remote and WebAssembly options open.
