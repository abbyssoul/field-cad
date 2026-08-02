# 0010 — GPU evaluation publishes ordinary field snapshots

Status: **accepted** (Milestone 3)

## Context

Interactive plane and sparse 3D sampling needs a batched GPU evaluator, while
ADR 0001 requires the visualizer to consume transportable field snapshots and
never solver-owned GPU memory. A direct renderer-to-solver buffer path would be
fast locally but could not exist when compute moves to a dedicated machine.

The application host also owns the `wgpu` device, queue, and resource budget;
an equation-system plugin must not create a second graphics stack or own a
window/presentation surface.

## Decision

The electrostatics plugin declares a narrow `ElectrostaticBatchEvaluator` seam.
In local desktop mode the composition root injects a host-owned `wgpu` evaluator
that shares the application's device and queue. It evaluates both electric
field and potential for one probe, plane, or grid geometry in a single dispatch.

GPU results are read back and published through the same immutable
`FieldSnapshot` columns as CPU and remote results. The snapshot domain declares
`f32`; the independent analytic oracle remains `f64`. Automated parity uses an
absolute tolerance of `2e-3` plus a relative tolerance of `5e-4` across point
sources, positive/negative superposition, a uniform sphere, a plane, a 3D grid,
and undefined source-radius samples.

Readback is synchronous in this first static solver, but it occurs only when a
world or subscription edit invalidates the analytic result—not once per rendered
frame. A time-stepped GPU solver must use asynchronous publication rather than
copy this scheduling choice.

## Consequences

- The renderer consumes only snapshot values and validity, so switching to a
  remote source still does not change visualization code.
- Local mode pays one GPU-to-CPU copy per invalidated geometry. That copy is
  accepted for the static milestone and keeps probe, network, and renderer
  consumers identical.
- Large edits currently block the command path until readback completes. Before
  Maxwell/FDTD or full-resolution grids, compute publication must become
  asynchronous and expose solving/stale state while work is in flight.
- A dedicated compute service can reuse the kernel and publish the same columns;
  no server GPU handle crosses the protocol boundary.
