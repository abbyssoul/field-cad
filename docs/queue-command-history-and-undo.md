# The mutation queue, command lifecycle, and undo/redo

A map of how a command travels from a UI click or MCP call to the
authoritative world, why some commands wait, how that wait is now made
visible, and how undo interacts with it. Written after fixing three related
UX bugs (pause latency, queue window sizing, undo-vs-pending-edits) so the
next pass through this code doesn't have to re-derive it from scratch.

Companion reading: [ADR 0011](adr/0011-queue-running-edits-at-fixed-tick-boundaries.md)
(why edits queue at all), [ADR 0023](adr/0023-an-interactive-edit-suspends-the-run.md)
(interactive edits and gestures), [ADR 0024](adr/0024-undo-restores-a-captured-scene.md)
(why undo is snapshot-based, not inverse commands), and
[docs/tasks/session-events-and-queue-control.md](tasks/session-events-and-queue-control.md)
(the original queue/event-hub design task).

## Key files

| File | Role |
| --- | --- |
| `crates/fieldcad-simulation/src/source.rs` | `SessionCore` — the authoritative queue (`pending_mutations`), `queue_paused` flag, command lifecycle, undo/redo dispatch. `LocalDataSource` wraps it for in-process use. |
| `crates/fieldcad-simulation/src/async_source.rs` | `AsyncLocalDataSource` — runs a `LocalDataSource` on a dedicated worker thread. Owns the two mpsc channels (normal + priority) and `worker_loop`. |
| `crates/fieldcad-simulation/src/runtime.rs` | `SimulationRuntime` — owns `EditHistory` (the undo/redo stack), separate from the queue. |
| `crates/fieldcad-server/src/lib.rs` | `HeadlessServer` — the shared authoritative session multiple transports (desktop, MCP) submit commands against. |
| `crates/fieldcad-mcp/src/lib.rs` | MCP tools (`pause_queue`, `resume_queue`, `cancel_queued_command`, `undo`, `redo`, …) and `submit_and_wait`. |
| `apps/fieldcad-desktop/src/ui/panels/queue.rs` | The Queue inspector window (pending list, history, pause/resume, cancel). |
| `apps/fieldcad-desktop/src/app.rs` | `synchronize_edit_gesture` — how a viewport drag defers its `CommitWorld` while the queue is paused. |

## Two independent "wait" mechanisms — don't conflate them

1. **The pending-mutation queue** (`SessionCore::pending_mutations`, a
   `VecDeque<CommandRecord>`) — edits that are accepted but not yet applied
   to the world. This is what "the queue" means everywhere in the UI.
2. **The undo/redo stack** (`SimulationRuntime`'s `EditHistory`) — scenes
   already applied, kept so Ctrl-Z can restore an earlier one. Whole-scene
   snapshots (`WorldCheckpoint`), not inverse commands — see ADR 0024 for why.

An edit only enters `EditHistory` once it actually applies. While it's
sitting in `pending_mutations` it doesn't exist there yet — this is the
whole reason undo needed special handling (see "Undo and the pending
queue" below).

## Why edits get held: `should_queue_mutation`

`SessionCore::should_queue_mutation()` (`source.rs`) is the single gate:

```rust
fn should_queue_mutation(&self) -> bool {
    self.runtime.status().mode() == SimulationMode::Running || self.queue_paused
}
```

A `CommitWorld`/`ReconfigureDomain` is held in `pending_mutations` when
either is true:

- the simulation is `Running` (ADR 0011 — an edit must land atomically with
  a fixed tick, not at arbitrary render-frame cadence), or
- the queue has been explicitly paused (`queue_paused`), independent of
  simulation mode.

Otherwise it applies immediately, synchronously, right there in `execute()`.

`queue_paused` **only** gates mutation queueing (via `should_queue_mutation`)
and blocks `Pause`/`Step`/`Redo` when something is actually held
(`reject_if_queue_paused`, fires only if `queue_paused && !pending_mutations.is_empty()`).
It does **not** pause the simulation clock — ticks keep advancing while the
queue is paused; only scene/domain mutations are held back.

## Flushing: atomic vs. incremental

There are two ways `pending_mutations` gets drained, and the difference is
the whole reason the "resume jumps to the final position" bug existed.

- **`flush_pending_mutations`** — drains everything, in one synchronous
  call, stopping early only if a mutation is rejected. Used where the
  backlog must land atomically with one boundary: `advance()`'s `Running`
  tick boundary, and `flush_and_check` ahead of `Pause`/`Step`/`Redo`.
- **`flush_one_pending_mutation`** — drains exactly the oldest record, or
  does nothing and returns `None`. Driven one call per `LocalDataSource::poll`
  whenever the queue isn't paused, the sim isn't `Running` (so there's no
  tick boundary to be atomic with), and something is still pending.

`ResumeQueue` itself does nothing but flip `queue_paused = false` — it does
**not** flush. The backlog it was holding (which can be a whole drag
gesture's worth of held edits, each its own real solve) drains on
subsequent polls, one edit at a time, each producing its own fresh
snapshot. Since the desktop app already calls `poll()` roughly once per
frame, this makes a resumed backlog animate through its held states the
same way a live (unpaused) edit would, instead of freezing and then
jumping straight to the end.

If you need the *old* atomic-resume behavior for some future call site, use
`flush_pending_mutations` directly rather than relying on `ResumeQueue`.

## The worker thread and the priority channel

`AsyncLocalDataSource` (`async_source.rs`) runs a `LocalDataSource` on a
dedicated thread so a slow solve never blocks the window/event-loop thread
(ADR 0012). Two `mpsc` channels feed `worker_loop`:

- `requests: Sender<WorkerRequest>` — `Execute(Command)` for ordinary
  commands (including heavy `CommitWorld`s) and `Poll(Duration)`/`Stop`.
- `priority_requests: Sender<Command>` — **only** `PauseQueue`,
  `ResumeQueue`, and `CancelQueuedCommand`.

`worker_loop` drains `priority_requests` (non-blocking `try_recv`)
unconditionally at the top of every iteration — i.e. right after whatever
`Execute` it just finished, before looking at the next backlog item. A
control command therefore waits behind **at most one already-in-flight
command**, never the whole backlog. Before this existed, `PauseQueue` sat
in the same FIFO as a backlog of heavy solves and could take minutes to
take effect on a large grid.

`requests.recv_timeout(20ms)` (not a blocking `recv`) means the worker
still notices a priority arrival promptly even when otherwise idle.

`AsyncLocalDataSource::execute()` decides which channel a command goes on;
`submitted_commands` bookkeeping (used to synthesize `Submitted` display
rows in `get_queue()` before the worker has even looked at a command) is
identical either way.

`run_command()` is the shared "execute one command, build a `WorkerEvent`"
helper both dispatch paths call.

## Undo and the pending queue

`SessionCore::execute`'s `Undo` arm, in order:

1. If `pending_mutations` is non-empty, pop the **most recently queued**
   record (`pop_back` — LIFO, "undo the last thing I did") and cancel it —
   the same mechanism `CancelQueuedCommand` uses by explicit id, factored
   into `cancel_pending_record`. Nothing touches `EditHistory`, because
   nothing was ever recorded there for an edit that hadn't applied yet.
2. Otherwise, fall through to the original path: `reject_if_queue_paused`
   (now effectively a no-op here, since it only fires on non-empty
   pending) → `flush_and_check` → `runtime.undo()` against `EditHistory`.

This composes: the first Ctrl-Z cancels a still-pending edit if there is
one; a second Ctrl-Z, once the queue is idle, undoes the last *applied*
edit as before. Applies regardless of *why* something is pending (queue
explicitly paused, or just waiting for a `Running` tick boundary) —
matching `CancelQueuedCommand`'s existing pause-agnostic behavior.

`Redo` has no counterpart — there's nothing to "redo" a still-pending
mutation into, so it's unchanged (still guarded by `reject_if_queue_paused`).
`Pause`/`Step` are likewise unchanged.

## Command lifecycle at a glance

```
Submitted (async-source only, before the worker looks at it)
   -> Queued   (should_queue_mutation() was true; sitting in pending_mutations)
   -> Applied  (landed in the world)  |  Rejected (validation failed)  |  Cancelled (CancelQueuedCommand or Undo)
```

`CommandRecord` carries this state plus `command`/`kind`/`sequence`; a
terminal record (`Applied`/`Rejected`/`Cancelled`) drops its payload and
moves into `terminal_history` (capped at `MAX_TERMINAL_HISTORY`, oldest
evicted first — see `record_terminal`). `QueueStatus { paused, pending,
history }` is what `get_queue()`/the Queue panel/MCP's `get_queue` tool all
read; `QueueSummary` is the cheap "did anything change shape" version used
by `ui/compute.rs`'s per-frame `queue_matches_summary` cache check.

## Desktop UI

`apps/fieldcad-desktop/src/ui/panels/queue.rs`'s `queue_window`: pause/resume
header row, then a resizable, scrollable (`egui::ScrollArea::vertical`)
body listing `queue.pending` and a collapsible `queue.history`. Each row
(`queue_record_row`) shows id/kind/state and a Cancel button for `Queued`
records.

`apps/fieldcad-desktop/src/app.rs`'s `synchronize_edit_gesture`: while
`queue_paused && scene_is_being_edited()`, a viewport drag or held
inspector control defers — it stashes its edit locally
(`pending_deferred_edit`) instead of resubmitting a `CommitWorld` every
frame, and submits exactly one `CommitWorld` when the gesture closes. This
is what keeps a paused queue from being flooded with per-frame commits
during a drag.

## Tests worth reading first

- `crates/fieldcad-simulation/src/lib.rs` — `resuming_the_queue_drains_a_multi_edit_backlog_one_poll_at_a_time`
  is the executable spec for incremental resume-draining.
  `undo_cancels_the_most_recently_queued_command_when_the_queue_is_paused` /
  `undo_cancels_only_the_most_recently_queued_command_leaving_earlier_ones_pending`
  for undo-vs-pending. `pausing_the_queue_holds_a_running_edit_across_tick_boundaries`
  and neighbors for the basic pause/queue contract.
- `crates/fieldcad-simulation/src/async_source.rs` (`mod tests`) —
  `priority_channel_commands_are_observed_before_the_backlog_drains` for the
  worker-thread ordering guarantee.
- `crates/fieldcad-mcp/src/lib.rs` — `undo_cancels_a_queued_command_instead_of_being_refused`,
  `cancel_queued_command_prevents_its_application` for the same behavior
  through the MCP transport.
