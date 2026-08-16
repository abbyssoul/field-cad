# Task: wire session recording and replay into server/MCP

## Status: open (P1-7 of `docs/tasks/product-capability-gaps-audit.md`)

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
