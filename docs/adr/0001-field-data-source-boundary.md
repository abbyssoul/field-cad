# 0001 — The visualizer consumes a field data source, never a solver

Status: **accepted** (Milestone 0)

## Context

Field CAD must eventually move compute to a dedicated service so that a laptop
can drive a simulation it could not run locally. The tempting order of work is to
build the desktop application against an in-process solver and add a network
layer later.

That order does not work. A renderer that reads solver memory directly — a GPU
buffer handle, a `&Grid`, an interior-mutable cache — encodes an assumption that
the solver is in this process. Removing that assumption later is not a
refactoring; it is a rewrite of every consumer.

## Decision

The application talks to a `FieldDataSource`: a transport-neutral contract of
commands in, versioned immutable snapshots out. Two implementations exist from
the start — `LocalDataSource` wrapping an in-process runtime, and
`LoopbackDataSource` standing in for a remote session — and they are required to
be *interchangeable*, not merely similar.

Concretely:

- Commands carry a client-issued `CommandId` and are answered by a correlated
  receipt. Play, pause, and step are never inferred from frame timing.
- Both sources publish through a `SnapshotMailbox`, so completeness, session
  identity, and supersession rules are enforced on one code path.
- Snapshots are values. No renderer holds a reference into solver state.
- `SourceError` names no in-process type. A solver failure crosses as a stable
  code plus a message, because a remote source could not produce a `RuntimeError`.

A test drives both sources through one script and asserts the observations are
identical. That test is the decision's teeth; without it "same contract" is a
claim rather than a property.

## Consequences

- Every field value is copied at least once, even locally. Accepted: correctness
  of the boundary is worth more than the copy, and the columnar layout
  ([0006](0006-columnar-batched-field-sampling.md)) keeps the copy cheap.
- The local path carries machinery it does not need — sequence numbers, a
  mailbox, receipts. That machinery is what makes the remote path a
  configuration change instead of a project.
- An acknowledgement does not mean the pixels changed. The loopback source makes
  that visible immediately, which is the intent.
