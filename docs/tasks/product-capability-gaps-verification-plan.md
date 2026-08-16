# Task: verification plan — CAD authoring and reproducibility completion review

## Status: run 2026-08-16 — task NOT complete

Executed against the working tree at `0d1ed36` (plus the in-flight
observation-history changes). All automated gates pass (210 tests, clippy,
desktop check, smoke 120), and the P1 items verified, but every **[new]**
item failed: A5 (configuration undo), A6/A7/B5 (queued-path and MCP-level
tests), C2–C4 (manual desktop verification), and E3 (audit doc freshness)
were never addressed by a follow-up. A8 (non-default-configuration
round-trip) and E1 (US-55/62/63 statuses) also failed. All outstanding work
is aggregated in `docs/tasks/product-capability-gaps-completion.md`; re-run
this plan once that document is resolved.

Original description follows.

Re-review of the `docs/tasks/product-capability-gaps.md` implementation once
the agent finishes the follow-up points found in the first audit
(`docs/tasks/product-capability-gaps-audit.md`, 2026-08-15). Run this plan
against the final working tree (before or right after commit), not mid-edit.

## How to use this plan

Each item names the concrete evidence to check (symbol, file, test name) and
what "pass" means. Items tagged **[new]** are gaps that existed at first
audit time and must be closed by the agent's follow-up; items without a tag
are re-verification of already-confirmed work. Anything in **Known gaps**
that still fails is a blocking finding; anything else is a note.

## Verification commands

```sh
cargo test -p fieldcad-simulation -p fieldcad-server -p fieldcad-mcp
cargo clippy -p fieldcad-simulation -p fieldcad-server -p fieldcad-mcp --all-targets
cargo check -p fieldcad-desktop
cargo run -p fieldcad-desktop -- --smoke 120
```

Desktop UI/rendering has no driven GUI harness — automated checks top out at
build/test/clippy/smoke; interactive items (C) are manual, per
`apps/fieldcad-desktop/AGENTS.md`.

## A. Solver-configuration editing (P0-4)

- **A1. Command surface.** `CommandPayload::SetFieldSystemConfiguration` in
  `crates/fieldcad-simulation/src/source.rs` with a `CommandKind` label;
  queued at a tick boundary while running (same pattern as
  `ReconfigureDomain`), persisted in `QueueDocument` (`PendingPayload::Configuration`),
  and flushed through `flush_one_pending_mutation` to the same runtime method.
  Pass: queued path and applied path both exist and are reachable.
- **A2. Reset-class semantics.** `SimulationRuntime::set_field_system_configuration`
  (`crates/fieldcad-simulation/src/runtime.rs`): schema-validates first,
  short-circuits identical values, refuses during an interactive edit,
  rebuilds every active solver and validates world + time step before
  adoption, resets the clock to paused `t = 0`, increments `run_generation`,
  clears run-derived history, publishes. Pass: all of the above, verified by
  tests `set_field_system_configuration_*` in `runtime.rs`.
- **A3. MCP tool.** `set_field_system_configuration`
  (`crates/fieldcad-mcp/src/lib.rs`) with typed property conversion via
  `typed_world::convert_configuration` (kind/dimension/choice checked against
  the plugin's declared schema); structured errors for unknown plugin and
  invalid properties; receipts through the normal submit path.
- **A4. Desktop editor.** Staged per-plugin settings editor in
  `apps/fieldcad-desktop/src/ui/panels/world_inspector.rs`
  (`configuration_editor`): drafts tracked in `UiModel::field_system_configuration_drafts`
  with staleness detection, Apply button enabled only when dirty + valid +
  no edit in progress, submission via `CommandPayload::SetFieldSystemConfiguration`
  on click only.
- **A5. [new] Undo/history behavior.** This was **missing at first audit**:
  `set_field_system_configuration` records no edit-history entry, and
  `NumericalCheckpoint` (runtime.rs) holds only `domain`/`time_step`, so a
  configuration change cannot be undone (while `reconfigure_domain` is
  undoable via `history.record_domain`). Pass: either the checkpoint carries
  plugin configuration (or equivalent) and undo/redo restores it with the
  same reset semantics, or the omission is a documented, deliberate
  exclusion. Confirm both directions and that undo/redo of a config change
  bumps the run generation as a reset.
- **A6. [new] Queued-path test.** No test exercised queued
  `SetFieldSystemConfiguration` (submit while running/paused-queue → `Queued`
  receipt → flush at boundary → config adopted, one generation bump, no
  second apply). Pass: such a test exists in `fieldcad-simulation`.
- **A7. [new] MCP integration test.** No test drove the tool through a real
  `HeadlessServer`/`McpServer`. Pass: a test sets a configuration via the
  tool, asserts `list_field_systems`/`field_systems` reflects it and
  `run_generation` advanced, and that an invalid property is rejected with a
  structured error and leaves state untouched.
- **A8. Persistence round trip.** A changed configuration survives
  save/load through `SceneDocument.field_systems[].configuration` and is
  re-validated at `resolve_plugins` on load (an invalid stored configuration
  is a structured load error, never silently dropped). Pass: existing
  document round-trip tests cover a non-default configuration.

## B. Preflight validation (P0-5)

- **B1. Non-mutation at runtime.** `SimulationRuntime::validate_world_commands`
  builds a candidate world off to the side, validates it against every
  enabled solver, and discards it — no revision change, no history entry, no
  snapshot publish. `validate_field_system_configuration` builds and discards
  a candidate solver; no `run_generation`/clock change. Pass: test
  `validate_field_system_configuration_never_mutates` exists and passes.
- **B2. Parity with commit rules.** The preflight checks are the same ones
  `commit_world_commands`/`set_field_system_configuration` run before
  mutating (world commit + solver `validate_world`; schema + solver
  construction + `validate_world` + `validate_time_step`). Pass: no check
  divergence between the two paths.
- **B3. Transport path.** `AsyncLocalDataSource::validate_world_commands` /
  `validate_field_system_configuration` are blocking worker round-trips with
  dedicated `WorkerRequest`/`WorkerEvent` variants, and
  `HeadlessServer` exposes both.
- **B4. MCP tool.** `validate_world_transaction`
  (`crates/fieldcad-mcp/src/lib.rs`) accepts both shapes — an
  `edit_world`-shaped typed command batch and a field-system configuration —
  and returns the `CommitReport` / `{ valid: true }`, with structured errors
  for unknown plugins and invalid properties.
- **B5. [new] MCP integration test.** No test exercised
  `validate_world_transaction` through `McpServer`. Pass: a test validates a
  rejected transaction (e.g. wrong-dimension property or unresolvable
  command) and asserts a structured error and unchanged world revision;
  validates an accepted one and asserts the world revision still did not
  change.
- **B6. Preflight/commit divergence.** A same-shaped rejection at commit
  after a successful preflight must remain possible and safe (state changed
  between). Pass: documented in tool/runtime docs and not optimised away by
  caching preflight results.

## C. Desktop manual verification (no GUI harness)

- **C1.** `cargo run -p fieldcad-desktop -- --smoke 120` passes.
- **C2.** Manual: World panel → field system → "Fields and settings" →
  Settings. Edit a property → Apply enabled only when dirty/valid; click
  Apply → simulation resets to paused `t = 0`, run generation advances
  (visible in status), settings read back updated; an invalid value disables
  Apply and shows "Invalid configuration."
- **C3.** Manual: undo/redo of a configuration change behaves per A5; domain
  reconfigure undo still restores the pre-change domain.
- **C4.** Manual regression spot check: rename in object/probe/shape
  inspectors and particle-template instantiation from the catalog panel
  still work (untouched paths).

## D. P1 items (when their task docs land as resolved)

For each, verify the task spec's own acceptance criteria
(`docs/tasks/run-records-and-comparison.md`, `session-recording-and-replay.md`,
`observation-export.md`) and:

- **D1. Run records/comparison.** Named run capture (generation,
  configurations, domain, observation references), list + compare via MCP
  tools, configuration-difference surfacing, persistence in the scene
  document with a save/load round-trip test (US-55).
- **D2. Session recording/replay.** Server-level start/stop/replay through
  the worker round-trip pattern; MCP `record_session`/`replay_session`;
  replay reproduces the same `RecordedEvent` sequence deterministically;
  recordings persist to files; desktop start/stop affordance verified
  manually.
- **D3. Observation export.** Scoped export (probes/channels/time range) as a
  versioned file with validity/provenance; import with structured
  version/compat rejection; bit-for-bit round trip; no scene bleed into the
  export; MCP `export_experiment`-style tool.

## E. Documentation consistency

- **E1.** `docs/user-stories/README.md` US-17/18/24/26 remain accurate;
  US-55/62/63 statuses updated when D1–D3 land.
- **E2.** `crates/fieldcad-mcp/src/lib.rs` module doc's "left for later" list
  matches reality (currently: probe history as server-side series,
  diagnostics as a dedicated read, run comparison, record/replay, export).
- **E3.** `docs/tasks/product-capability-gaps-audit.md` is updated or
  superseded once A/B are complete (its P0-4/P0-5 rows still say "Missing").
- **E4.** `goal.md` checkbox for this task reflects completion.

## Known gaps at first audit (2026-08-16)

Blocking until closed, then re-verified here:

1. **A5 — configuration changes are not undoable** (no history record;
   `NumericalCheckpoint` has no configuration slot). Largest open question.
2. **A6/A7/B5 — no queued-path or MCP-level tests** for
   `SetFieldSystemConfiguration` / `validate_world_transaction`; coverage
   stops at runtime unit level.
3. **C — desktop config editor never manually verified** in the running app.
4. **E3 — the audit document's status table is stale.**
