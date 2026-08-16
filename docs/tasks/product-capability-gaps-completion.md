# Task: CAD authoring and reproducibility completion — aggregate follow-up

## Status: open (verification run 2026-08-16 found the task NOT complete)

This is the single aggregate follow-up for closing
`docs/tasks/product-capability-gaps.md`. It collects every outstanding item
found when `docs/tasks/product-capability-gaps-verification-plan.md` was run
against the working tree on 2026-08-16 (post-commit `0d1ed36`, with the
in-flight observation-history changes for
`docs/tasks/authoritative-observation-history.md` still uncommitted). Every
reviewed document points here; do not open parallel follow-ups for items
already listed below.

**What the verification run confirmed done** (no further action): A1 command
surface, A3 MCP tool, A4 desktop editor, B1–B4 + B6 preflight, C1 smoke, and
all three P1 items — D1 run records/comparison, D2 session recording/replay,
D3 observation export. All automated gates pass: 210 tests across
`fieldcad-simulation`/`fieldcad-server`/`fieldcad-mcp`, clippy clean,
`cargo check -p fieldcad-desktop`, smoke 120 frames OK.

**What remains:** the first audit's four follow-up points (A5 undo, A6/A7/B5
tests, C manual verification, E3 doc freshness) were never addressed, plus
two coverage gaps the verification plan had assumed were already covered
(A2 test depth, A8 non-default-configuration persistence) and stale status
markers (E1, E4).

## Item register

Blocking items are the verification plan's "Known gaps at first audit" still
failing; notes are everything else the run surfaced.

| # | Plan item | Finding | Class |
| --- | --- | --- | --- |
| 1 | A5 | Configuration changes are not undoable; no history record, no checkpoint slot, no documented exclusion | Blocking |
| 2 | A6 | No queued-path test for `SetFieldSystemConfiguration` anywhere | Blocking |
| 3 | A7 | No MCP-level test for `set_field_system_configuration` | Blocking |
| 4 | B5 | No MCP-level test for `validate_world_transaction` | Blocking |
| 5 | C2–C4 | Desktop config editor never manually verified in the running app | Blocking |
| 6 | E3 | Audit doc P0-4/P0-5 section headers still say "missing"; new gaps unrecorded | Blocking |
| 7 | A2 | Runtime tests miss short-circuit, refuse-while-editing, clock-reset, history-clear assertions | Note |
| 8 | A8 | No document round-trip test covers a non-default plugin configuration | Note |
| 9 | E1 | US-55/62/63 statuses stale in `docs/user-stories/README.md` | Note |
| 10 | E4 | `goal.md` checkbox marked complete prematurely | Note |
| 11 | D1 | Scene-document round-trip only exercises *empty* `run_records` | Note |
| 12 | D3 | Export scope has no time-range selection (task doc offered it as one scope shape) | Note |

## Goal

Close every remaining item so a re-run of
`docs/tasks/product-capability-gaps-verification-plan.md` passes outright,
then mark the task complete in one consistent documentation sweep. Keep the
document model authoritative: undo, tests, and persistence all go through the
existing typed-command and validation paths, never around them.

## Required behavior

### 1. A5 — make configuration changes undoable (blocking)

`SimulationRuntime::set_field_system_configuration`
(`crates/fieldcad-simulation/src/runtime.rs:1334`) records no edit-history
entry, and `NumericalCheckpoint` (`runtime.rs:241`) holds only
`domain`/`time_step`, while `reconfigure_domain` is undoable via
`history.record_domain` (`runtime.rs:1603`). Either:

- give the checkpoint a configuration slot (or equivalent) so undo/redo
  restores the prior plugin configuration with the same reset semantics as
  the forward path — rebuild active solvers, validate world + time step,
  reset to paused `t = 0`, bump `run_generation`, clear run-derived history,
  publish — and confirm both directions bump the run generation as a reset;
  or
- record the omission as a documented, deliberate exclusion (doc comment on
  the runtime method plus a note in `docs/tasks/product-capability-gaps-audit.md`),
  if undo of reset-class numerical settings is judged out of scope for the
  history model.

The implementation choice must be tested, not just made.

### 2. A6 — queued-path test (blocking)

In `fieldcad-simulation`: submit `SetFieldSystemConfiguration` while running
(or into a paused queue) → `Queued` receipt → flush at the tick boundary →
configuration adopted, exactly one `run_generation` bump, no second apply.
Mirror the existing queued-edit tests (`tests::local_running_edits_are_queued_for_a_fixed_tick_boundary`,
`tests::a_queued_command_reports_completion_only_after_its_tick_boundary` in
`crates/fieldcad-simulation/src/lib.rs`) and the `ReconfigureDomain` queue
pattern in `source.rs:878-895`.

### 3. A7 — MCP integration test for `set_field_system_configuration` (blocking)

Drive the tool through a real `HeadlessServer`/`McpServer`
(`crates/fieldcad-mcp/src/lib.rs:1483`): set a configuration, assert
`list_field_systems`/the `field_systems` read reflects it and `run_generation`
advanced; assert an invalid property is rejected with a structured error and
leaves state untouched. Follow the shape of
`field_system_realtime_mode_is_set_through_the_tool` and
`reconfiguring_the_domain_reports_the_reset_authoritative_state`.

### 4. B5 — MCP integration test for `validate_world_transaction` (blocking)

Through `McpServer` (`crates/fieldcad-mcp/src/lib.rs:1262`): validate a
rejected transaction (wrong-dimension property or unresolvable command) and
assert a structured error and unchanged world revision; validate an accepted
one and assert the world revision *still* did not change (preflight never
mutates, B1).

### 5. C2–C4 — manual desktop verification (blocking)

Run the three manual checks from the verification plan's section C in the
real app: config-editor Apply flow and reset readback (C2), undo/redo of a
configuration change per item 1 plus domain-reconfigure undo regression (C3),
and rename/catalog-instantiation spot checks (C4). Record the outcome in this
document's status and in `docs/tasks/product-capability-gaps-audit.md`. C3
depends on item 1 landing first.

### 6. E3 — audit document consistency (blocking)

In `docs/tasks/product-capability-gaps-audit.md`: correct the stale
"P0-4 … missing"/"P0-5 … missing" section headers (lines 139/158) to match
the summary table, and record the 2026-08-16 verification outcome — which
follow-up points were found unaddressed and that they are tracked here.

### 7. A2 — deepen the runtime reset-semantics tests (note)

Extend the `set_field_system_configuration_*` tests
(`crates/fieldcad-simulation/src/runtime.rs:3892-3954`) to also assert:
identical-value short-circuit (no generation bump), refusal during an
interactive edit, clock reset to paused `t = 0`, and run-derived history
(`last_forces`/`body_history`) cleared.

### 8. A8 — non-default configuration persistence (note)

The round-trip test plugins declare no configuration schema
(`crates/fieldcad-scene-document/tests/roundtrip.rs:120-125`), so no test
proves a changed configuration survives save/load through
`SceneDocument.field_systems[].configuration` and is re-validated at
`resolve_plugins`. Give one test plugin a real configuration property, set a
non-default value before save, and assert it round-trips; assert an invalid
stored configuration is a structured load error
(`resolve_plugins`, `crates/fieldcad-scene-document/src/lib.rs:370`).

### 9. E1 — user-story statuses (note)

In `docs/user-stories/README.md`: US-55 (line 331, still "Required for
API/MCP parity"), US-62 (line 353, still "Required for API/MCP exposure"),
and US-63 (line 359, still "Partially implemented" with recordings/export
called "out of scope") all predate P1-6/7/8 landing. Update the three
statuses and US-63's body text.

### 10. E4 — goal.md checkbox (note)

`goal.md` line 243 marks the task `[X]`. Leave the checkbox as-is only once
every blocking item above is closed; until then the 2026-08-16 verification
note there should point at this document.

### 11. D1 — non-empty run-records round trip (note)

`crates/fieldcad-scene-document/tests/roundtrip.rs:156` only round-trips an
empty `run_records`. Add (or extend) a case that saves at least one
`RunRecord` through the full scene document and loads it back.

### 12. D3 — time-range export scope (note, optional)

`ObservationExportScope` (`crates/fieldcad-server/src/lib.rs:1334`) selects
probes/channels/distance/mass-aggregate probes and the latest snapshot, but
not a time range, which the original task doc offered as one scope shape.
Either implement a tick/time-range filter on the captured series or record a
deliberate exclusion in `docs/tasks/observation-export.md`.

## Suggested order

1. Items 2–4 and 7 (test-only, no behavior change; one pass in
   `fieldcad-simulation` + `fieldcad-mcp`).
2. Items 8 and 11 (`fieldcad-scene-document` coverage; independent of 1).
3. Item 1 (the only behavior change), then item 5's C3 part becomes possible.
4. Items 6, 9, 10, 12 — the documentation sweep, once the code items above
   have landed and statuses describe reality.
5. Item 5 (C2/C4 parts can run any time; C3 after item 1).
6. Re-run `docs/tasks/product-capability-gaps-verification-plan.md` end to
   end; if green, mark this document resolved and flip the audit/verification
   plan/goal.md statuses in the same change.

## Tests and acceptance

- Every blocking item cites its own test shape above; acceptance is a clean
  re-run of the verification plan's command block plus its A/B/C checklists.
- No item may introduce a parallel validation or persistence path: undo goes
  through `EditHistory`, queued commands through `flush_one_pending_mutation`,
  persistence through `SceneDocument`/`resolve_plugins`.
- Determinism preserved: configuration undo/redo is a reset (new
  `run_generation`), never a restore of evolved solver state.

## Related follow-ups (separate tasks, not aggregated here)

- `docs/tasks/run-records-desktop-ui.md` (open) — desktop UI for run records;
  spun out of D1 deliberately.
- `docs/tasks/observation-stream.md` (open) — live SSE observation stream.
- `docs/tasks/authoritative-observation-history.md` (partially done) —
  per-probe `history_capacity`, MCP history reads, desktop deduplication.
- `docs/tasks/server-authoritative-catalog.md` — catalog ownership closed;
  its two deliberately-deferred inspector-wiring items remain tracked there.

## Relevant code

- `crates/fieldcad-simulation/src/runtime.rs` — `set_field_system_configuration`
  (~1334), `reconfigure_domain` (~1597), `NumericalCheckpoint` (~241),
  `EditHistory` (~246), configuration tests (~3892-3988).
- `crates/fieldcad-simulation/src/source.rs` — `SetFieldSystemConfiguration`
  payload (~126), queue path (~917), `flush_one_pending_mutation` (~1201),
  `QueueDocument` (~381).
- `crates/fieldcad-mcp/src/lib.rs` — `set_field_system_configuration` (~1483),
  `validate_world_transaction` (~1262), test module (~2325 onward).
- `crates/fieldcad-scene-document/src/lib.rs` — `FieldSystemComposition`
  (~173), `resolve_plugins` configuration validation (~370);
  `tests/roundtrip.rs` for item 8.
- `crates/fieldcad-scene-document/src/run_record.rs` — `RunRecord` for item 11.
- `crates/fieldcad-server/src/lib.rs` — `ObservationExportScope` (~1334) for
  item 12.
- `apps/fieldcad-desktop/src/ui/panels/world_inspector.rs` —
  `configuration_editor` (~435) that C2 exercises.
- `docs/user-stories/README.md` — US-55/62/63 for item 9.
