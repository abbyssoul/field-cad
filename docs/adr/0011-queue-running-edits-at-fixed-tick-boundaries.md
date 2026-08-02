# 0011 — Queue running edits at fixed-tick boundaries

Status: accepted

## Context

Interactive edits can arrive while a simulation is advancing. Applying them at
an arbitrary render frame would make results depend on GUI cadence, while
holding them without an explicit acknowledgement would make a future remote
visualizer guess whether the compute service accepted the command. Playback
speed also needs to change how quickly ticks are requested without changing the
solver's numerical time step.

## Decision

A world edit submitted while `Running` is acknowledged with disposition
`Queued`. The authoritative data source preserves submission order and applies
queued edit transactions immediately before the next fixed simulation tick.
The tick and the snapshot it publishes therefore observe the new world
revision atomically. If `Pause` arrives first, the source flushes the queue at
the current tick boundary and then enters `Paused`, so no accepted edit is
stranded.

Edits submitted while paused remain immediate. A command receipt distinguishes
`Applied` from `Queued`; source status exposes the pending count, and polling
reports how many queued transactions crossed a boundary.

Playback speed is a data-source/session property. It scales wall-clock elapsed
time before fixed-tick demand is calculated. It never changes `dt`; exceeding
the per-poll tick budget is reported as falling behind and excess backlog is
discarded.

## Consequences

- Deterministic command recordings can replay semantic commands and elapsed
  intervals independently of rendered frame rate.
- Local and remote sources expose the same edit timing and acknowledgement
  semantics.
- The UI must not present a queued edit as authoritative before the source's
  world revision advances.
- A deferred edit can fail only when its boundary is processed; a production
  remote protocol will need an asynchronous final applied/rejected event in
  addition to the initial queued acknowledgement.
