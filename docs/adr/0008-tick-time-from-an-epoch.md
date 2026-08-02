# 0008 — Simulation time is reconstructed from a tick count and an epoch

Status: **accepted** (Milestone 2 review gate)

## Context

There are two obvious ways to keep simulation time under a fixed step:

- **Accumulate:** `t += dt` each tick. Simple, but the error compounds. After ten
  ticks of 0.1 s the clock reads 0.9999999999999999, and two runs that tick in
  different-sized groups can diverge.
- **Reconstruct:** `t = tick * dt`. Exact and reproducible regardless of how the
  ticks were grouped — but if `dt` ever changes, every past tick's timestamp
  silently changes with it, invalidating recorded probe history.

Milestone 4 requires an editable `dt`, so neither is sufficient alone.

## Decision

Reconstruct from an epoch:

```
time = epoch_seconds + (tick - epoch_tick) * dt
```

Changing `dt` closes the current epoch at the present time and opens a new one.
Elapsed time is preserved; only future spacing changes. History recorded before
the change keeps the timestamps it was recorded with.

The same reasoning applies to wall-clock pacing, where it bit us for real:
`TickPacer` carries its remainder as a `Duration` — integer nanoseconds — because
an `f64` seconds accumulator dropped roughly one tick in two when `dt` was 0.1 s.
A pacing bug of that shape looks like a physics bug, which is the expensive kind.

## Consequences

- `tick * dt` is not decimally exact. Three ticks of 0.1 s is
  0.30000000000000004, reproducibly. Tests assert against `3.0 * dt`, not `0.3`;
  the promise is determinism, not decimal tidiness.
- The clock carries two extra fields. Cheap.
- A `dt` change is a visible event in the timeline. When project persistence
  lands, epochs should be recorded so a loaded run can be replayed exactly.
- Wall-clock time can only ever request whole ticks. When the runtime cannot keep
  up it drops the backlog and reports `fell_behind`, rather than growing an
  unbounded debt or stretching `dt` to catch up.
