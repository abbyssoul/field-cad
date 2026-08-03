# 0012 — Local compute does not run on the window thread

Status: **accepted** (Milestone 5 preparation)

## Context

The first electrostatic GPU evaluator waits for readback before it can publish a
complete snapshot. That was tolerable for proving the static solver, but a
world edit or dense subscription could stall pointer input and window redraws.
A time-stepped Maxwell solver makes such stalls continuous. The remote data
source already implies that command submission and authoritative completion are
different events.

## Decision

The desktop wraps its local data source in one ordered compute worker. Submitting
a command returns a provisional `Submitted` receipt with no snapshot sequence;
the worker later emits an authoritative applied, queued, or failed event. The
desktop continues presenting the last complete snapshot as `Solving` while work
is in flight.

Only one wall-clock poll may be in flight. Additional elapsed time is coalesced
instead of creating an unbounded frame-rate queue. Commands retain submission
order with polls on the worker channel. The worker publishes only ordinary
snapshots through the same completeness-checking mailbox used elsewhere.

## Consequences

- GPU dispatch, readback, CPU sampling, and snapshot construction no longer
  block camera or authoring input.
- Local command semantics now model the pending/final acknowledgement split a
  remote compute client needs.
- The current evaluator may still wait synchronously *inside* the worker. A
  Maxwell GPU backend should use native asynchronous buffer completion and
  cancellation/supersession rather than accumulate expensive obsolete edits.
- Closing the application may wait for the current worker operation to finish;
  compute kernels need bounded workloads and later cancellation support.
