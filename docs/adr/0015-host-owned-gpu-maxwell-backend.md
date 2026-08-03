# 0015 — Maxwell uses a host-owned GPU backend and publishes ordinary snapshots

## Context

The CPU `f64` Yee solver is the reference implementation, but an interactive
three-dimensional time-domain simulation needs GPU field updates. The renderer
must not read solver buffers directly: the desktop will later visualize fields
streamed from a dedicated compute machine, and plugin code must not create a
second graphics device.

A time-stepped backend also differs from the analytic electrostatics evaluator:
field state must remain resident between ticks and shutdown must be able to
cancel an outstanding asynchronous readback.

## Decision

- The electromagnetism plugin retains one identity, configuration, channel
  schema, and validation path. A `MaxwellSolverBackend` factory supplies either
  the CPU reference or a host-injected implementation with declared precision.
- The desktop injects a backend using clones of its existing `wgpu::Device` and
  `wgpu::Queue`. No graphics handle crosses into the renderer or a snapshot.
- Electric and magnetic Yee components use `vec4<f32>` storage buffers. Each
  fixed tick submits magnetic half-step, electric full-step, and magnetic
  half-step compute passes. Electric state ping-pongs; magnetic state uses one
  half-step scratch buffer.
- GPU submission is non-blocking. Snapshot publication copies the current E/B
  grids to a staging buffer, completes through `map_async`, and cooperatively
  polls a session cancellation token. Desktop shutdown cancels the wait before
  joining the compute worker.
- One readback is cached across diagnostics, channels, probes, planes, and grid
  batches in a publication. Shared Yee reconstruction code converts that state
  into the same typed columns as the CPU reference.
- The GPU domain declares `f32`; the CPU oracle remains `f64`. A deterministic
  small-grid parity test advances both backends and compares all five channels.
- The source-owned field-system catalog reports configuration schemas and
  authoritative values, so the desktop can show the prescribed-wave amplitude
  and mode without reaching into a local solver.

## Consequences

Maxwell E, B, energy, and divergence data travel through the existing immutable
snapshot/data-source boundary, so local and future remote visualization remain
the same code path. Enabling or disabling the field system creates or releases
its GPU buffers consistently with scene-level composition.

The first backend reads the complete small reference grid once per published
tick. This is appropriate for the current 32³ desktop scenario but is not the
remote-transfer strategy: representative profiling must set budgets before
introducing chunked subscriptions or GPU-side sparse sampling. Periodic
boundaries and the prescribed vacuum plane wave remain the deliberate numerical
validation scenario. ADR 0016 changes the desktop default to a stationary-charge
constraint while leaving moving-charge current deposition and absorbing
boundaries for later work.

Status: accepted.
