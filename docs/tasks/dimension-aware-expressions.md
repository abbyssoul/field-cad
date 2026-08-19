 # Dimension-Aware Expressions, Constants, and Live Bindings

## Summary

Introduce fieldcad-expressions, a solver-independent crate for parsing, dimension-checking, compiling, and evaluating
quantity expressions. Expressions remain authored intent, while the authoritative world continues exposing ordinary finite
SI-valued PropertyValues to plugins.

The incremental product journey is:

1. Mathematical expressions in scalar object-property editors.
2. Document constants and derived constants.
3. Reusable user-library constants with reproducible document embedding.
4. Authoritative live bindings from distance measurements.
5. Later expansion to other quantity fields and observation types.

This enables these initial user stories:

- Enter (6400 / 2) * 1e3 km instead of calculating externally.
- Define doc.body_radius, doc.shell_thickness, or derived constants and update dependent properties atomically.
- Reuse personal constants such as material densities across documents.
- Inspect the formula and provenance behind a resolved value.
- Drive a live-bindable scalar property from a distance probe every simulation tick.
- Save, transfer, and reproduce a document without requiring the originating user library.

## Architecture and Interfaces

- Add crates/fieldcad-expressions with:
    - ExpressionSource, CompiledExpression, ExpressionValue, ExpressionError, and source-span diagnostics.
    - ConstantDefinition, PropertyTarget, PropertyBinding, and a compiled EvaluationPlan.
    - A resolver/provider interface separating compilation from runtime value lookup.
    - Arithmetic +, -, *, /, unary signs, parentheses, scientific notation, unit literals such as 1e3 km, and integer powers
    for unit expressions.

    - Dimensional arithmetic: addition/subtraction require equal dimensions; multiplication/division combine dimensions; the
    final result must exactly match the target schema dimension.

    - Rejection of unknown symbols, ambiguity, cycles, division by zero, non-finite results, excessive nesting, and
    oversized expressions.

    - No general scripting, arbitrary functions, mutation, I/O, or plugin callbacks.

- Use explicit symbol scopes:
    - doc.name for document constants.
    - user.name for embedded user-library constants.
    - Distance references inserted through the UI and persisted using stable DistanceProbeId, not editable display names.
    - Names are unique within each constant scope; renaming updates display source without changing durable references.
    - User constants may reference other user constants. Document constants may reference document or embedded user
    constants. User constants cannot reference document state.

- Keep expressions outside fieldcad-core::PropertyValue:
    - The world and plugins continue receiving resolved Quantity values in SI.
    - The runtime/server owns an ExpressionDocument containing constants, embedded dependencies, and property bindings.
    - Compiled ASTs and topological plans are transient caches rebuilt from persisted source and stable references.
    - Add PropertySchema::live_binding with a backward-compatible default of false. Static expressions/constants work for
    every scalar property; sensor references are accepted only for properties explicitly declaring per-tick support.

- Extend the authoritative command surface with an atomic scene-edit batch covering world commands plus expression/constant
edits:
    - Set or clear a property expression.
    - Add, update, rename, or remove a document constant.
    - Import or explicitly refresh an embedded user constant dependency.
    - Removing referenced constants or distance probes is rejected with dependent targets listed.
    - Removing a target object/component removes its bindings in the same transaction.
    - Replacing an expression with a literal freezes its current value and clears the binding.
    - Expression and constant edits participate in authoritative undo/redo and the running command queue exactly like other
    authored scene edits.

- Extend source state and provenance:
    - Publish definitions, resolved values, dependency status, and expression diagnostics through FieldDataSource, server,
    and MCP surfaces.

    - Add an expression-graph revision/content hash to snapshot and run provenance so two equal numeric worlds with
    different formulas remain distinguishable.

    - Record a new ADR establishing the authored-expression/evaluated-world split and pre-tick live-evaluation semantics.

## Milestones

1. Expression engine and calculator fields
    - Build the parser, dimensional type checker, evaluator, stable diagnostics, and resource limits.
    - Replace the scalar object-property DragValue path with a reusable expression editor.
    - Preserve committed expression source; show its evaluated SI/display-unit value alongside it.
    - Keep local parsing as preview only; authoritative compilation and validation occur on command submission.
    - Literal-only fields retain dragging. Dragging a formula first requires replacing/freezing it as a literal.

2. Persisted object-property expressions
    - Add authoritative property bindings, command serialization, source-state reads, undo/redo capture, and queue behavior.
    - Extend fieldcad.scene/v1 with a default-empty expression section and bump the format version.
    - Load older documents as literal-only documents.
    - Evaluate every edit as a candidate, update all affected component bags atomically, and run existing schema and solver
    validation before adoption.

3. Document constants
    - Add a Variables section to the Simulation inspector with create, rename, edit, delete, evaluated-value, unit,
    dependency, and error displays.

    - Compile the entire affected dependency closure on each edit and reject cycles or incompatible downstream targets.
    - Provide autocomplete/insertion from scalar property editors.
    - Treat one constant edit and all resulting resolved property changes as one undoable scene edit.

4. User constant library
    - Store reusable constants in a dedicated atomically written user-library file beside, but separate from, desktop
    presentation preferences.

    - Manage them from Settings using the same editor and diagnostics.
    - When first referenced, embed the complete referenced dependency closure, stable identity, revision/content hash, and
    source provenance into the document.

    - Documents evaluate solely from their embedded version. If the local library differs, show an available-update state
    and offer an explicit atomic refresh; never update an experiment silently.

    - Headless/server code receives library imports as data and never reads desktop configuration paths itself.

5. Live distance bindings
    - Permit DistanceProbeId references only on schemas marked live-bindable.
    - Before Play, Step, and every tick, evaluate the compiled graph against the current authoritative pre-tick world.
    - Apply resolved property changes before force evaluation and solver advancement, so a distance at state n influences
    tick n → n+1.

    - Validate the complete effective candidate before advancing the clock. On any missing, invalid, wrong-dimension, or
    non-finite value, do not start/advance the simulation, preserve the last valid world and snapshot, pause, and publish
    a precise diagnostic identifying the source and target.

    - Notify solvers only when resolved values they consume changed; avoid reparsing, graph rebuilding, or per-tick
    allocation.

    - Demonstrate the contract through the test solver rather than adding spring mechanics in this feature.

6. Follow-on expansion
    - Reuse the editor for transforms, shapes, domain values, time step, and other authored quantities after object-property
    behavior stabilizes.

    - Add scalar field-probe and aggregate-observation references only after defining their same-tick versus previous-
    snapshot semantics and validity handling.

    - Add vector component, magnitude, and projection expressions as a separate language extension.

## Test and Acceptance Plan

- Parser tests cover precedence, parentheses, scientific notation, prefixed and compound units, the motivating example,
unary operations, and malformed input with accurate spans.

- Dimensional tests prove valid conversion and derived arithmetic while rejecting examples such as assigning another body’s
mass directly to a radius.

- Graph tests cover forward references, derived constants, cycles, ambiguous names, scope rules, deletion guards, stable-ID
behavior across renames, and deterministic topological ordering.

- Runtime tests verify atomic candidate adoption, one-step undo/redo, queued edits, pre-tick distance sampling,
deterministic replay, unchanged-value fast paths, and no clock/world advancement after evaluation failure.

- Persistence tests cover expression round trips, legacy documents, embedded user dependencies, absent/different user
libraries, explicit refresh, and expression provenance in run records.

- Transport tests establish desktop/server/MCP parity for editing and inspecting definitions and diagnostics.
- Add a fieldcad-bench workload measuring evaluation by graph size and live-binding count. Expected runtime is O(nodes +
dependency edges + referenced distances) with reused buffers and no steady-state per-tick allocation.

- Run workspace tests, clippy/checks, desktop smoke, and manual in-app verification of editing, autocomplete, diagnostics,
save/reload, library refresh, and a running distance binding.

## Assumptions and Deferred Work

- Interview decisions adopted here: object scalar properties first; constants form acyclic expression graphs; user constants
are embedded and refreshed explicitly; live references evaluate every tick; invalid graphs pause/refuse simulation;
distance sensors come first; spring mechanics are out of scope.

- Catalog properties and other-object component references are not implemented now. The resolver/reference model must allow
later stable namespaces such as catalog-entry revision plus component/property and object ID plus component/property
without changing the expression language’s evaluation core.

- Live feedback cycles are rejected rather than solved as algebraic systems.
- Scalar expressions are the initial contract; vector-valued expressions and observation projections remain explicit future
milestones.

## Acceptance implementation (2026-08-19)

The remaining acceptance implementation is complete, with final resolution
held behind the manual interaction checklist below.

- `fieldcad-test-field` 0.3.0 declares the test-only `live-gain` object
  component. Its required dimensionless property is live-bindable, at most one
  object may carry it, and `on_world_changed` rereads it before analytic
  sampling. The configured plugin gain and object gain multiply.
- The desktop composes that plugin only when built with
  `--features expression-fixture` and launched with `--expression-fixture`.
  The default production catalog is unchanged; the same opt-in catalog is used
  for new/opened sessions and embedded MCP session replacement.
- Expression draft reconciliation is egui-independent for existing/new
  constants, property formulas, and the user library. Dirty drafts survive
  authoritative changes, clean drafts follow them, accepted submissions are
  acknowledged, and Reset/Cancel/Escape restore the latest accepted baseline.
  User-library rows now stage a complete candidate and mutate the live model
  only after a valid atomic save.
- Candidate previews carry an owner-associated diagnostic/span and deterministic
  transitive dependents. A valid candidate graph supplies affected targets;
  the accepted graph supplies them when candidate compilation fails. Apply and
  Enter require a valid dirty draft. Property formulas retain explicit Freeze
  and add Cancel.
- Document constants and live-bindable property formulas offer distance probes
  by display name while inserting `distance.<stable-id>`. User-library
  constants do not offer observations. Rename coverage proves labels change
  without changing tokens.
- The acceptance fixture found and fixed two contract defects: implicit unit
  suffixes now bind as quantities (`distance.0 / 1 m` is dimensionless), and a
  changed pre-tick live value forces analytic channels to republish rather than
  retaining the prior tick's batch.

Coverage now includes the consuming test solver's sampled pre-tick value and
failure/retry behavior, full local/async `ExpressionState` equality, MCP JSON
parity plus invalid mixed-edit rollback, desktop `ComputeView` parity, staged
draft transitions, v7/v8 persistence compatibility, embedded dependency
provenance/hash/value rebuild without a local library, explicit refresh with
undo/redo, and user-library first-use/version/atomic-save behavior. Existing
runtime tests continue covering queue replay, removal guards, deterministic
recording replay, graph hashes, unrelated-component notification filtering,
and transaction rollback.

## Verification evidence (2026-08-19)

- `cargo fmt --all --check` — passed.
- `cargo test --workspace` — passed. The restricted sandbox initially refused
  Unix-domain socket binds in four MCP transport tests; the same complete suite
  passed outside that restriction.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` —
  passed.
- `cargo test -p fieldcad-expressions --test steady_state_allocations` — passed;
  1,000 warmed evaluations allocate zero times inside the documented boundary.
- `cargo run --release -p fieldcad-bench -- --filter expressions
  --save-baseline docs/perf/2026-08-19-expressions.json` — both full sweeps
  passed their linear declaration (`O(nodes^1.13)` and `O(bindings^1.05)`). See
  `docs/perf/2026-08-19-expression-evaluation.md` and the dated JSON report.
- `cargo run -p fieldcad-desktop -- --smoke 120` — passed on llvmpipe Vulkan.
- `env -u WAYLAND_DISPLAY cargo run -p fieldcad-desktop --features
  expression-fixture -- --expression-fixture --exit-after 3` — initialized the
  Intel Iris Xe Vulkan window and shut down cleanly after the bounded launch.
- A normal build invoked with `--expression-fixture` refuses startup and
  explains that the Cargo feature is required.

## Manual acceptance still required

The bounded window launch verifies feature composition and lifecycle, but is
not a human interaction pass. Before adding `## Status: resolved`, run

```shell
cargo run -p fieldcad-desktop --features expression-fixture -- --expression-fixture
```

and verify all of the following in-app:

- formula, document-variable, new-variable, and user-library drafts;
- byte-span diagnostics and transitive affected-target lists;
- valid/invalid Enter, Apply disabling, Cancel/Reset/Escape, and Freeze;
- invalid user-library save prevention;
- distance labels/tokens across a probe rename;
- running pre-tick binding, sampled field change, zero-distance failure, and
  successful retry;
- save/reload and explicit embedded-library refresh with undo/redo;
- MCP expression inspection matching the desktop after accepted and rejected
  edits.

Quantity/vector/observation expansion and catalog/object-property references
remain deferred to Milestone 6 follow-on work.
