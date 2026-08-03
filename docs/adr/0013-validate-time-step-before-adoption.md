# 0013 — Equation systems validate a time step before adoption

Status: **accepted** (Milestone 5)

## Context

Explicit numerical solvers have stability limits that depend on their domain.
The plugin contract previously exposed `dt` only to `step`, after the runtime and
UI had already accepted the value. A Yee solver could therefore discover an
unstable Courant number only when the next tick failed.

## Decision

Every solver may validate a candidate `TimeStep`. The runtime asks every loaded
solver before accepting both the initial step and a later `SetTimeStep` command.
Validation is read-only; one rejection leaves the authoritative clock unchanged.
Analytic solvers use the default implementation and accept every positive,
finite step already validated by the core.

## Consequences

- Stability errors are reported at the edit that caused them, before numerical
  state advances.
- One shared `dt` must satisfy every active time-stepped equation system.
- Adaptive or per-solver stepping remains a future runtime design and cannot be
  smuggled into a plugin behind the authoritative simulation clock.
