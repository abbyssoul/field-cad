# 0007 — Solvers validate a candidate world before it is adopted

Status: **accepted** (Milestone 2 review gate)

## Context

The runtime originally committed an edit and then told the solvers:

```rust
let report = self.world.commit(commands)?;   // world has already advanced
for slot in &mut self.plugins {
    slot.solver.on_world_changed(&snapshot)?; // a solver may refuse here
}
```

If a solver refused, the error propagated but the world stayed committed at the
new revision. Some solvers had adopted the edit and some had not, and no snapshot
was published — so the newest snapshot was pinned at the previous revision and
the UI reported `Stale` permanently. The world had a revision that nothing had
computed.

This violates the invariant that a plugin failure must be reported with context
without corrupting the world.

## Decision

Validate on a candidate, adopt only on success:

1. Clone the world (`Arc` plus three counters — cheap) and commit onto the clone.
2. Ask every solver `validate_world(&candidate)`. This takes `&self` and must not
   mutate, so a refusal costs nothing.
3. Only if all accept, adopt the clone and call `on_world_changed` on each.

A failure at step 1 or 2 leaves the committed world, the identifier counters, and
every solver exactly as they were.

`validate_world` sits on the *solver*, not the plugin, because whether a world is
representable depends on the solver's configuration — a grid resolution, a
stability limit — not on the plugin type.

## Consequences

- Solvers must be able to judge a world without adopting it. That is a real
  constraint and the right one: it forces stability and representability checks
  to be expressible as predicates rather than discovered mid-update.
- A solver that passes `validate_world` and then fails `on_world_changed` is a
  bug in that plugin. The runtime does not attempt to roll back solver state;
  making solvers transactional would be a large cost for a case that indicates a
  defect.
- World edits are already atomic within a batch (`World::commit` builds a
  candidate and discards it on any error). This extends the same discipline
  across the runtime boundary.
