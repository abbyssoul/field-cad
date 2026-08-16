# Task: desktop UI for named run records and comparison

## Status: open

## Goal

Give the desktop app a panel to name the current run, browse retained run
records for the open scene, and compare two of them — the UI counterpart to
the MCP surface `save_run`/`list_runs`/`get_run`/`delete_run`/`compare_runs`,
so a modeller working in the desktop app (not just an MCP agent) can retain
and compare runs without hand-writing tool calls.

## Current limitation

Run records are a fully working, persisted capability
(`docs/tasks/run-records-and-comparison.md`, resolved 2026-08-16):
`fieldcad_scene_document::RunRecord`/`RunComparison`,
`HeadlessServer::{save_run, run_records, run_record, delete_run,
compare_runs, restore_run_records}`, and scene-document round-trip
(`SceneDocument.run_records`, `FORMAT_VERSION` 7) all exist and are tested.
But nothing in `apps/fieldcad-desktop` calls any of it — no menu action, no
panel, no keybinding. A desktop user has no way to name a run, see what's
retained, or compare two runs; the feature is reachable only through an MCP
client today, which breaks the UI/MCP feature-parity expectation the rest of
this app follows (compare `apps/fieldcad-desktop/src/ui/panels/
world_inspector.rs`, which *does* expose `set_field_system_configuration`/
`validate_world_transaction` from the same P1 audit alongside their MCP
tools).

This was deliberately left out of the original run-records task: its own
"Required behavior" section never asked for a desktop affordance, unlike the
recording/replay and export tasks, which explicitly do.

## Required behavior

- A way to name the current run from the desktop UI — a button/action
  (toolbar, menu, or a new section of an existing panel such as the scene
  inspector) that calls `HeadlessServer::save_run` and shows the result.
- A list of this scene's retained run records (`HeadlessServer::run_records`),
  each showing at minimum name, `created_at`, and `run_generation` — mirrors
  `list_runs`' summary shape (`fieldcad_scene_document::RunRecordSummary`).
- A way to delete a retained record (`HeadlessServer::delete_run`).
- A comparison view: pick two retained records, call
  `HeadlessServer::compare_runs`, and render the result — which field-system
  configuration differs (domain/time-step/per-plugin enabled/realtime/
  configuration) and, for observation series both runs recorded, some way to
  see them alongside each other (a table of readings is an acceptable first
  cut; a dual-series plot, reusing whatever charting the existing probe
  History view already uses, is the natural follow-up if a table proves too
  hard to read for dense series).
- Follows this app's existing state-cache pattern for server-owned state
  that multiple transports (embedded MCP, desktop UI) can mutate
  concurrently — see `WindowState.catalog`/`document_entries` in `app.rs` and
  its accompanying doc comment: read-only local caches refreshed from the
  server, never locally mutated and pushed back, so a concurrent MCP
  `save_run`/`delete_run` is never silently clobbered by a stale desktop
  copy.
- State plainly that this app has no driven GUI test harness (see
  `apps/fieldcad-desktop/AGENTS.md`); automated verification tops out at
  `cargo build`/`test`/`clippy`, and manual in-app verification of naming,
  listing, deleting, and comparing runs is required before calling this
  done.

## Tests and acceptance

- Naming a run from the desktop UI produces a record retrievable through
  `HeadlessServer::run_records`/`run_record` (an automated test can drive
  this at the `HeadlessServer` level, the same way
  `crates/fieldcad-server/tests/headless_session.rs` already does — a UI
  click handler calling the same method needs no separate proof of the
  method's own correctness).
- The desktop's run-record cache visibly updates after an MCP client (in the
  same embedded-server session) calls `save_run`/`delete_run` — proves the
  read-only-cache-refreshed-from-server pattern actually closes the loop
  documented as required above, not just a local list that only reacts to
  the desktop's own actions.
- Manual: name two runs with a deliberately different field-system
  configuration in between (e.g. toggle a plugin setting via the existing
  `world_inspector.rs` configuration UI), compare them, and confirm the
  difference is visible in the comparison view.

## Relevant code

- `crates/fieldcad-server/src/lib.rs` — `HeadlessServer::{save_run,
  run_records, run_record, delete_run, compare_runs, restore_run_records}`.
- `crates/fieldcad-scene-document/src/run_record.rs` — `RunRecord`,
  `RunRecordSummary`, `RunComparison`, `compare_run_records`.
- `apps/fieldcad-desktop/src/app.rs` — `WindowState`'s existing
  server-state-cache pattern (`catalog`, `document_entries`,
  `quick_add_hidden`) to mirror for a new `run_records` cache; where
  `save_scene`/load/`replace_session` already thread `run_records` through
  scene persistence (added alongside the original run-records task, so no
  further scene-document wiring should be needed here).
- `apps/fieldcad-desktop/src/ui/panels/world_inspector.rs` — existing
  pattern for a panel that both reads and mutates server-owned
  configuration state, to follow for the new run-records panel.
- `apps/fieldcad-desktop/src/ui/mod.rs`,
  `apps/fieldcad-desktop/src/ui/panels/mod.rs` — where a new panel gets
  registered alongside the app's existing collapsible panels (diagnostics,
  history, etc).
