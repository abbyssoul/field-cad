# Task: wire session recording and replay into server/MCP

## Status: resolved (P1-7 of `docs/tasks/product-capability-gaps-audit.md`)

**Verification (2026-08-16):** confirmed done by
`docs/tasks/product-capability-gaps-verification-plan.md` item D2. The
desktop start/stop/replay affordance was verified only via build/test/smoke,
never interactively — that manual check is aggregated in
`docs/tasks/product-capability-gaps-completion.md` (item 5).

**Resolution (2026-08-16):**

- `SessionRecording`/`RecordedEvent` (`crates/fieldcad-simulation/src/
  recording.rs`) gained `Serialize`/`Deserialize` so a recording can be
  written to and read back from a file.
- `AsyncLocalDataSource::execute_blocking`/`poll_blocking`
  (`crates/fieldcad-simulation/src/async_source.rs`) are new blocking
  primitives, following `capture_document`/`validate_world_commands`'s
  existing pattern exactly as this task's "Required behavior" specified.
  They exist because `SessionRecording::replay`'s generic driver assumes a
  synchronous `FieldDataSource` (as `recording.rs`'s own tests use via
  `LocalDataSource`/`LoopbackDataSource`); `HeadlessServer` sits on the
  fully async `AsyncLocalDataSource`, where `execute`/`poll` return
  immediately with `Submitted` and the real outcome arrives later on a
  worker thread. Replaying through the non-blocking calls would capture an
  observation before a recorded event actually settled, so two replays of
  the same recording could disagree depending on worker-thread timing —
  `execute_blocking`/`poll_blocking` remove that race by fully settling
  each event (including any concurrent worker events encountered along the
  way, same discipline as `capture_document`) before returning.
- `HeadlessServer::{start_recording, stop_recording, is_recording,
  replay_recording}` (`crates/fieldcad-server/src/lib.rs`). Recording
  capture is hooked into the existing `execute`/`advance` methods
  (not new entry points), so both desktop and MCP get it automatically —
  they already funnel through these. One deliberate departure from a literal
  transcript: a zero-duration poll is never recorded. Every transport's own
  wait-for-completion loop (`submit_and_wait` in this crate's tests and in
  `fieldcad-mcp`) calls `advance(Duration::ZERO)` many times per command
  while waiting; recording those would flood a recording with hundreds of
  no-op entries for a handful of real commands. `replay_recording` returns
  `Vec<ReplayStep>` — deliberately not `recording.rs`'s own
  `ReplayObservation`: it reports a `CommandEvent` (a command's actual
  terminal outcome) rather than that type's `Option<CommandReceipt>` (an
  immediate, possibly-`Submitted` receipt), because the async transport has
  no synchronous "applied" outcome to report at submission time.
- Recordings persist as their own small versioned file,
  `fieldcad.recording/v1` (`crates/fieldcad-scene-document/src/
  recording_file.rs`, `save_recording_to_path`/`load_recording_from_path`),
  not a field on `SceneDocument`: a recording describes what was *done* to a
  session, independent of which scene it started from, matching this task's
  "persist as files (or embed in the scene document)" requirement's first
  option. Simpler than `SceneDocument`'s atomic-write-with-backup discipline
  on purpose — a recording is written once (on `stop_recording`) and read
  once (before a replay), not repeatedly resaved over the same path.
- MCP: `start_recording`, `stop_recording(path)`, `recording_status`,
  `replay_session(path)` (`crates/fieldcad-mcp/src/lib.rs`).
- Desktop: File menu gained "Start Recording" / "Stop Recording…" (native
  save dialog, mirrors `Save Scene As…`) / "Replay Recording…" (native open
  dialog), plus a "● Recording" indicator in the menu bar
  (`apps/fieldcad-desktop/src/ui/panels/menu_bar.rs`). `FrameContext` gained
  `is_recording: bool`, read live each frame from `HeadlessServer::
  is_recording` the same way `catalog_revision` is polled elsewhere — no
  driven GUI harness exists for this app (`apps/fieldcad-desktop/AGENTS.md`);
  verified via `cargo build`/`test`/`clippy` plus `cargo run -p
  fieldcad-desktop -- --smoke 60`, not interactive manual use.

Tests: `crates/fieldcad-simulation/src/async_source.rs` (blocking-call unit
tests, plus an async-transport replay-equivalence test mirroring
`a_recorded_command_sequence_replays_bit_identically`),
`crates/fieldcad-scene-document/src/recording_file.rs` (round-trip, wrong
format, unsupported version), `crates/fieldcad-server/tests/
headless_session.rs` (`a_recorded_session_replays_through_the_server_level_api`
— a real charge-and-probe scene, recorded live and replayed into two
independent fresh sessions, requiring the same final `Vec<ReplayStep>`;
`starting`/`stopping_a_recording`'s structured-error cases).

---

## Goal

Expose the existing semantic recording/replay capability to real callers
(desktop UI, MCP clients), so a session can be recorded and replayed
end-to-end rather than only inside `fieldcad-simulation`'s own test suite.

## Current limitation

`SessionRecording`, `RecordedEvent`, and `replay` already exist and work in
`crates/fieldcad-simulation/src/recording.rs`, exercised by tests in
`crates/fieldcad-simulation/src/lib.rs`. Re-verify these line references
before acting — refactors shift them. Nothing outside the test suite reaches
this capability: no desktop menu/action starts or stops a recording, no
`fieldcad-server::HeadlessServer` method exposes it, and there is no MCP
`record_session`/`replay_session` tool (both are named in the Suggested MCP
surface table in `docs/user-stories/README.md` under "Reproducibility" but
unimplemented).

## Required behavior

- `HeadlessServer` (or its `AsyncLocalDataSource`) exposes start/stop
  recording and replay-from-recording operations, following the same
  request/response pattern used for `capture_document`/`validate_world_commands`
  (a blocking worker round trip — see
  `crates/fieldcad-simulation/src/async_source.rs`).
- MCP tools `record_session` and `replay_session` (or equivalent), each a
  thin wrapper over the above, matching this crate's existing "one tool per
  `CommandPayload` variant or exposed read" convention
  (`crates/fieldcad-mcp/src/lib.rs`'s module doc).
- Desktop UI affordance (menu action or panel control) to start/stop
  recording and to load and replay a recording — state plainly that no
  driven GUI harness exists for this app and that manual in-app verification
  is required (`apps/fieldcad-desktop/AGENTS.md`).
- Recordings persist as files (or embed in the scene document) so a
  recording started in one session can be replayed in a later one.

## Tests and acceptance

- A recorded session, replayed through the new server-level API, reproduces
  the same sequence of `RecordedEvent`s the original session produced —
  reusing `recording.rs`'s existing replay-equivalence tests as the model.
- `record_session`/`replay_session` MCP tools round-trip through a real
  `HeadlessServer` instance in an integration test
  (`crates/fieldcad-server/tests/`).
- Desktop recording start/stop is verified manually in the running app;
  automated coverage tops out at `cargo build`/`test`/`clippy`.

## Relevant code

- `crates/fieldcad-simulation/src/recording.rs` — `SessionRecording`,
  `RecordedEvent`, `replay`.
- `crates/fieldcad-simulation/src/lib.rs` — existing recording/replay tests
  to use as the equivalence model.
- `crates/fieldcad-simulation/src/async_source.rs` — worker request/response
  pattern (`WorkerRequest`/`WorkerEvent`, `capture_document`,
  `validate_world_commands`) to follow for a new blocking recording API.
- `crates/fieldcad-server/src/lib.rs` — where server-level
  record/replay methods would live, alongside `field_systems`,
  `validate_world_commands`.
- `crates/fieldcad-mcp/src/lib.rs` — where the new tools would be registered.
- `docs/user-stories/README.md` — "Reproducibility" row of the Suggested MCP
  surface table.
