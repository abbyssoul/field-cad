# Task: authoritative session events and controllable mutation queue

## Goal

Replace the current count-only, destructively observed mutation queue with one
authoritative queue model and an event hub. The desktop and every MCP client
must independently observe state changes without consuming events belonging to
another consumer. This is the foundation for the later desktop job-queue panel
and MCP queue controls.

## Current limitation

`SessionCore` stores only payloads for tick-boundary mutations, so queued work
loses its command ID. `AsyncLocalDataSource` reports a queued receipt before
the boundary application, and `HeadlessServer::drain_events()` is one
destructive stream. The system can report a pending count but cannot inspect,
pause, cancel, or broadcast authoritative command lifecycle.

## Required behavior

### Queue model

- Replace payload-only pending mutations with command records containing command
  ID, label/payload kind, submission order/time, lifecycle state, and optional
  terminal receipt/error.
- Lifecycle states are `submitted`, `queued`, `applied`, `rejected`, and
  `cancelled`.
- Keep the command identity through worker submission and tick-boundary
  application. A queued acknowledgement is not terminal completion.
- Retain the latest 256 terminal command records per session by default.
- Expose authoritative reads and mutations:
  - `get_queue()` returns enabled/paused state, ordered pending records, and
    terminal history;
  - `pause_queue()` / `resume_queue()`;
  - `cancel_queued_command(command_id)`.

### Queue policy (decided)

- Queue pause holds queued scene/domain mutations while simulation ticks
  continue.
- New eligible mutations are accepted and appended while the queue is paused.
- Only a mutation still waiting for a tick boundary is individually
  cancellable. Do not add per-command cancellation for in-flight solver work
  in this task; `SolverCancellation` remains session-level cancellation.
- `Pause`, `Step`, Undo, and Redo must not flush a paused queue. Return a
  structured `queue-paused` conflict until the queue resumes or relevant work
  is cancelled.
- On resume, eligible mutations apply at the next normal tick boundary, in
  submission order.

## Event hub

- Add a bounded broadcast hub owned by `HeadlessServer`; all publication flows
  through its canonical poll/drain path.
- Publish typed events for complete-snapshot changes, source/simulation/queue
  status changes, diagnostics changes, command lifecycle transitions, and
  watcher lag/resync.
- Deduplicate state events by snapshot identity/status version. Snapshot
  updates may be superseded under backpressure; command terminal records must
  remain recoverable through queue history.
- A lagging watcher receives a resync marker and re-reads authoritative
  resources; do not retain unbounded per-watcher event queues or disconnect it
  by default.
- Preserve one-shot command waiters, but fulfill them from the same hub rather
  than competing with desktop or MCP consumers for a destructive drain.

## MCP surface

- Advertise resource and resource-subscription capability.
- Provide stable resources:
  - `fieldcad://session/status`
  - `fieldcad://session/snapshot`
  - `fieldcad://session/diagnostics`
  - `fieldcad://session/queue`
- Emit `notifications/resources/updated` for affected resources. Event
  notifications are invalidation/summary signals; clients read the resource
  for the full authoritative payload.
- Add tools: `get_queue`, `pause_queue`, `resume_queue`, and
  `cancel_queued_command`.
- Existing synchronous mutation tools still await their own terminal result
  where appropriate, while resource subscribers observe the same transition.

## Desktop follow-on

Use the same server queue read/control surface for a future queue panel:
pending command list, labels/IDs, paused state, pause/resume/cancel controls,
and recent terminal results. Do not expose or inspect an async-source private
queue from the UI.

## Tests and acceptance

- Two concurrent consumers both receive command completion and snapshot/status
  updates without drain races.
- Pausing the queue holds a mutation across simulation ticks; resuming applies
  it at the next eligible boundary; cancelling prevents application.
- Validate paused-queue conflicts for Pause, Step, Undo, and Redo.
- Verify terminal-history eviction at 256 records and lag → resync → resource
  re-read recovery.
- MCP tests cover resource listing, subscription notification, and updated
  resource reads; no polling-only watch API is introduced.

## Relevant code

- `crates/fieldcad-simulation/src/source.rs` — tick-boundary mutation queue.
- `crates/fieldcad-simulation/src/async_source.rs` — async command completion.
- `crates/fieldcad-server/src/lib.rs` — authoritative shared-session boundary.
- `crates/fieldcad-mcp/src/lib.rs` — MCP tools/resources/notifications.
