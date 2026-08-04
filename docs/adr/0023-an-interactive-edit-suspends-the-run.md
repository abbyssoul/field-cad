# 0023 — an interactive edit suspends the run and may defer a system

Status: **accepted** (revisits
[0011](0011-queue-running-edits-at-fixed-tick-boundaries.md))

## Context

[0011](0011-queue-running-edits-at-fixed-tick-boundaries.md) decided *when* an
edit enters a running world: at the next fixed-tick boundary, atomically. That
is the right rule for an edit that arrives complete — a new object, a typed
value, a deleted probe.

It is the wrong rule for an edit that is still being made. Dragging a body
across the viewport is one edit spread over a hundred frames, and each frame
submits a pose. Under 0011 the simulation interleaves solver ticks with those
poses: the body teleports, the solver integrates from wherever it was dropped,
and the trajectory on screen belongs to neither the equations nor the
arrangement the user is building. The intermediate poses are authored, not
physical — nothing computed them and nothing will reproduce them.

The same gesture is also the project's worst performance case. Every
intermediate pose is a world revision, and every world revision republishes
every active system. For an analytic evaluator that means a full solve between
one mouse position and the next, and a scene becomes undraggable long before it
becomes unsolvable.

## Decision

An **interactive edit** is a scene edit that spans frames: a viewport drag, or
an inspector control held down or being typed into. It has a beginning and a
commit, and both are visible to the authoritative side as ordinary correlated
commands, `SetInteractiveEdit(bool)`.

- **The run is suspended for its duration.** A gesture that begins while the
  simulation is advancing pauses it, and resumes it when the gesture commits. A
  run the user had already paused is handed back paused: the gesture restores
  the transport, it does not command it.
- **Only held controls count.** A checkbox or a menu choice is already atomic —
  one command, and it is over — so there is no duration to suspend across. The
  desktop reports the held part and nothing else.
- **A field system may opt out of following the gesture.** `realtime` is
  per-system scene state, default on. While a gesture is open, a system with it
  off is not shown the intermediate worlds and does not resample; it keeps its
  last complete result and is brought current once, at the commit.

Validation is not deferred. A candidate world is still offered to *every* active
solver before it is adopted ([0007](0007-validate-before-adopting-a-world-edit.md)),
so a gesture cannot smuggle in a world some solver cannot represent. What a
non-realtime system skips is the work, not the veto.

The gesture is recognised in the desktop, because it is made of pointer events,
but it has no local effect: pausing and deferral both happen where the solving
does, and reach it through the same command boundary a remote client would use.

## Consequences

Deferral is a cost choice, not a physical one. The same committed world produces
the same field either way — the test asserts the exact value continuous update
would have arrived at — so a user trading responsiveness for liveness cannot
trade away a result. A deferred system says so: it publishes a
`deferred-during-edit` diagnostic for as long as it is holding, and the
transport bar names the gesture holding the clock, because a run that stops on
its own is otherwise indistinguishable from one that broke.

A gesture that commits nothing costs nothing: the runtime records the revision
the edit opened at and skips the catch-up when it is unchanged.

**What this trade costs.** A non-realtime system is not shown intermediate
worlds at all, so `on_world_changed` is called once per gesture rather than once
per frame. For an analytic solver that is pure gain. For a time-stepped one it
also means intermediate poses are not counted as separate interventions, which
is arguably the truer reading of one drag — but it is a behaviour change, and a
solver that accumulated state per world change rather than recomputing from the
world would notice. None currently does.

**Not addressed.** Turning realtime off does not make a *committed* edit cheap;
it makes a gesture cost one edit instead of a hundred. A scene whose single
solve is too slow to wait for needs progressive or partial publication, which
the snapshot completeness field already anticipates and nothing yet uses.
