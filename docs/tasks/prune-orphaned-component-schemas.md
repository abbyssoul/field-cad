# Task: user-confirmed pruning of orphaned component schemas

## Goal

Let a user permanently remove component schema data from a scene document
that no currently-active plugin declares — explicitly, per document, never
silently — so a scene saved by an older or differently-composed build stops
carrying forward dead schema data forever.

## Current limitation

`WorldState.component_schemas` (`crates/fieldcad-core/src/world.rs`) is
additive and permanent: `RegisterComponentSchema` inserts if absent and
never removes (`crates/fieldcad-core/src/world.rs:1916-1919`). On session
build, `SimulationRuntime::new` (`crates/fieldcad-simulation/src/runtime.rs:679-712`)
only *adds* schemas the currently-composed plugins declare that are missing
from a loaded world; a schema already present but undeclared by any current
plugin is left untouched, by design (portability/recovery for a genuinely
missing third-party plugin — see Milestone 8 in `PLAN.md`, "project files
retain unknown plugin data for recovery").

The unintended consequence: once a schema from a *removed* plugin (not
merely a disabled one) is baked into a document, it survives every
subsequent load/save cycle indefinitely. This was found via a real scene
(`earth-moon-2.fcscene`) still carrying `fieldcad.particles:particle`
("Catalog provenance") years after `fieldcad-particles` was deleted (see
`docs/tasks/user-configurable-object-catalog.md`), attached to zero objects
in the document — pure dead weight, re-saved forward forever.

As of this task's writing, the immediate UX symptom (an orphaned schema
showing up as a real, addable "+ Add component" option) is fixed: the menu
disables schemas with no active plugin, and an attached component whose
schema has no active plugin shows a `(!)` marker with an explanatory label —
see `apps/fieldcad-desktop/src/ui/panels/object_inspector.rs`
(`active_component_schemas`, `add_component_menu`, `object_components`) and
`FieldSystemStatus::component_schemas`
(`crates/fieldcad-simulation/src/runtime.rs:446-461,884-913`). That stops
the leak from being *offered* again and makes existing orphaned data
visible, but does not remove it from the document.

## Required behavior

- Detect, at scene load, which persisted `component_schemas` entries are
  orphaned: present in the loaded `WorldState` but declared by no plugin in
  the current session's composition (`FieldSystemStatus::component_schemas`
  unioned across every entry, active or not — matching the distinction this
  task's prerequisite work established between "inactive plugin" and "no
  plugin at all").
- Distinguish, if practical, an orphaned schema still *attached to at least
  one object* from one attached to none — the former is user data worth a
  clearer warning before removal; the latter (like the `earth-moon-2.fcscene`
  case) is unambiguous dead weight.
- Surface this as an explicit, dismissible notice — most likely alongside
  the existing catalog/document diagnostics surface, or a dedicated "Scene
  diagnostics" section — naming which schemas are orphaned, their
  display-name, and (if attached) which/how many objects reference them.
- Offer an explicit "Prune on next save" or "Prune now" action. Pruning:
  removes the orphaned `ComponentSchema` from `WorldState.component_schemas`,
  and for any object still carrying a component under that schema, either
  (a) blocks pruning until those components are removed/reassigned, or (b)
  detaches the orphaned components as part of the same confirmed action —
  decide which before implementing; (b) is more convenient but destroys
  object data the user may not have reviewed per-object, so the
  confirmation dialog must show exactly what will be lost.
- Never prune automatically on load or on an ordinary save. This mirrors the
  existing catalog propagation rule (`docs/tasks/server-authoritative-catalog.md`):
  a structural change that can't be undone by reopening the file requires an
  explicit, informed user action, not an implicit side effect of routine
  save/load.
- Pruning should go through the normal authoritative command/undo path if
  it detaches components from objects (reuse `WorldCommand::DetachComponent`
  and a new schema-removal command), so it participates in ordinary
  undo/history rather than being a special-cased document rewrite.

## Limitations

- This does not change the underlying design decision that a *disabled but
  composed* plugin's schemas remain fully authorable (that's intentional,
  documented behavior, not the bug this task addresses).
- This does not attempt to recover or migrate an orphaned component's data
  into some replacement schema — pruning is destructive by nature; migration
  is a separate, much larger problem this task does not take on.

## Tests and acceptance

- Loading a document with a schema no current plugin declares surfaces it as
  orphaned, without blocking load or silently dropping it.
- The "+ Add component" menu and attached-component list correctly
  distinguish orphaned-but-still-declared-elsewhere from
  genuinely-unavailable (schema entirely missing from
  `world.component_schemas()`, the pre-existing "(schema unavailable)" case)
  — these are different states with different remedies and must not be
  conflated in the UI.
- Explicit prune removes the schema and reports what changed; an
  unconfirmed load/save cycle leaves the orphaned schema exactly as it was.
- Pruning that detaches components is undoable through the normal edit
  history.
- A regression test loads a fixture scene document carrying a schema ID no
  registered plugin declares (synthesizing one, not depending on a real
  deleted crate) and asserts the orphan is detected and prune removes it.

## Relevant code

- `crates/fieldcad-core/src/world.rs` — `WorldState.component_schemas`,
  `RegisterComponentSchema` handling (~line 1896-1919), the world command
  boundary any new schema-removal command would extend.
- `crates/fieldcad-simulation/src/runtime.rs:679-712` — where a loaded
  world's existing schemas versus active plugins' declared schemas are
  reconciled; `FieldSystemStatus::component_schemas` (446-461, populated at
  884-913) is the existing "what's currently backed" signal to reuse.
- `apps/fieldcad-desktop/src/ui/panels/object_inspector.rs` —
  `active_component_schemas`, `add_component_menu`, `object_components`:
  the prerequisite UI work (disable/mark orphaned entries) this task builds
  a removal action on top of.
- `crates/fieldcad-scene-document/src/lib.rs` — scene load/save path; where
  a load-time orphan scan and a save-time prune confirmation would hook in.
- `docs/tasks/server-authoritative-catalog.md` — the precedent for
  "detect and offer, never silently mutate" that this task should follow for
  consistency (propagation confirmation dialog).
