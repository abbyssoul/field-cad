# Task: named run records and comparison

## Status: resolved (P1-6 of `docs/tasks/product-capability-gaps-audit.md`)

**Resolution (2026-08-16):** `fieldcad_scene_document::RunRecord` (new
`crates/fieldcad-scene-document/src/run_record.rs`) captures `run_generation`,
`Domain`, `TimeStep`, `Vec<FieldSystemComposition>`, and a copy of
`ProbeHistoryState`/`DistanceHistoryState`/`MassAggregateHistoryState`.
`compare_run_records` is a pure function producing a `RunComparison`: per-
plugin configuration differences (only plugins that actually differ, or are
present in only one run, are reported) plus every probe/distance/mass-
aggregate series from either run paired by key.

The prerequisite this task's "current limitation" undersold: probe/distance/
mass-aggregate history was previously a **desktop-only, client-local**
concern (`fieldcad-mcp`'s own module doc said so explicitly) — an MCP-only
session had nothing to copy into a run record. `HeadlessServer` now retains
its own copy (`probe_history`/`distance_history`/`mass_aggregate_history`
fields, fed from every published snapshot in `publish()`, pruned of deleted
probes the same way the desktop's client-local copy already was), so
`save_run` has real observation data to snapshot regardless of transport.

`SceneDocument.run_records: Vec<RunRecord>` persists retained runs with the
scene (`FORMAT_VERSION` 6 → 7); `HeadlessServer::{run_records, run_record,
save_run, delete_run, compare_runs, restore_run_records}` own the in-session
list; MCP exposes `save_run`/`list_runs`/`get_run`/`delete_run`/
`compare_runs`; desktop's `save_scene`/load paths thread `run_records`
through the same way they already do `document_entries`. No desktop UI panel
was built — the task's own "Required behavior" never asked for one, unlike
P1-7/P1-8's explicit desktop-affordance requirement.

Tests: `crates/fieldcad-scene-document/src/run_record.rs` (config-diff
detection, plugin-present-in-only-one-run, JSON round-trip),
`crates/fieldcad-scene-document/tests/roundtrip.rs` (empty `run_records`
still round-trips), `crates/fieldcad-server/src/lib.rs` (save/list/get/
delete/compare, restore-after-`replace_source`), `crates/fieldcad-server/
tests/headless_session.rs` (`save_run_retains_a_copy_of_recorded_probe_history`,
proving the server-side retention actually receives real readings from a
stepped, charge-and-probe scene through the async worker).

---

## Goal

Let a modeller name and retain a numerical run — the configuration that
produced it plus the observations it yielded — and compare two retained runs
against each other, so changing one parameter and re-running is a comparison
rather than a one-off overwrite of whatever was on screen before.

## Current limitation

Probe/distance/mass-aggregate histories and body trajectories are already
retained and persisted (`crates/fieldcad-simulation/src/history.rs`,
`body_history.rs`), and `SimulationRuntime::run_generation` (bumped by
`reconfigure_domain` and, as of this session, `set_field_system_configuration`
— see `crates/fieldcad-simulation/src/runtime.rs`) already distinguishes one
numerical run from the next within a session. But nothing gives a run its own
retained identity: there is no "save this run under a name," no list of past
runs, and no way to compare two runs' configuration or observations side by
side. Re-running today simply keeps advancing the same generation counter or
overwrites history on the next `commit_world`/`reconfigure_domain`/
`set_field_system_configuration`.

## Required behavior

- A way to snapshot "this run" — its `run_generation`, the field-system
  configurations and domain that produced it, and a reference to (or copy of)
  its retained observation histories — under a user-chosen name.
- A list of retained run records for the current scene, discoverable locally
  and through MCP.
- A comparison view/tool: given two run records, report what configuration
  differs between them and place their observation series alongside each
  other (same probe/channel, two series).
- Run records persist with the scene document (`fieldcad-scene-document`) so
  they survive a save/load cycle, matching the "immutable, versioned
  observations with validity and provenance" boundary in `AGENTS.md`.

## Tests and acceptance

- Naming a run captures enough state that reopening the saved scene and
  requesting the named run's configuration reproduces the same
  domain/field-system configuration that was active when it was named.
- Comparing two run records with a known, deliberately different
  configuration (e.g. two different `gain`-style plugin settings) surfaces
  that difference in the comparison result.
- Run records round-trip through save/load without loss (US-55 in
  `docs/user-stories/README.md`).

## Relevant code

- `crates/fieldcad-simulation/src/history.rs`, `body_history.rs` — existing
  retained observation histories to reuse or reference from a run record.
- `crates/fieldcad-simulation/src/runtime.rs` — `run_generation`,
  `FieldSystemStatus` (configuration + schema), `Domain`.
- `crates/fieldcad-scene-document/src/lib.rs` — where a run record's
  persisted shape would live alongside `FieldSystemComposition`.
- `crates/fieldcad-mcp/src/lib.rs` — where list/get/compare run-record tools
  would be exposed.
- `docs/user-stories/README.md` US-55 and the "Reproducibility" row of the
  Suggested MCP surface table.
