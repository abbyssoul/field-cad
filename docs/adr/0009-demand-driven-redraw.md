# 0009 — Redraws are demand-driven, not continuous

Status: **accepted** (2026-08-02)

## Context

A 3D application looks like it wants to render continuously, and the obvious
event loop says so:

```rust
event_loop.set_control_flow(ControlFlow::Poll);

fn about_to_wait(&mut self, _: &ActiveEventLoop) {
    self.window.request_redraw();   // every iteration, unconditionally
}
```

This is what Field CAD did, and it is wrong for this application in two ways.

**It never idles.** Field CAD is paused most of the time. A user placing a
charge, reading a probe, or thinking is not asking for 60 frames a second, but
this loop delivers them anyway — burning CPU, draining battery, and heating a
laptop to render an unchanged image.

**It applies sustained pressure to the compositor.** The loop requests a frame
as fast as the machine allows while blocking on vertical blank inside the event
callback. On a Wayland compositor that is a client which never yields. Combined
with the surface paths that resize and monitor changes exercise, this was part
of a failure that froze GNOME Shell hard enough to need a session restart.

The naive fix — a fixed frame timer — is also wrong. It either idles too slowly
to feel responsive or too quickly to be idle, and it has no way to know that a
tooltip is fading in or that the simulation is running.

## Decision

Redraws are requested only when a frame is actually due. The loop uses
`ControlFlow::WaitUntil(deadline)`, and the deadline comes from whoever knows
best:

- **egui** reports how long it is content to wait, via
  `viewport_output[ViewportId::ROOT].repaint_delay`. It knows about animations,
  hover transitions, and text cursors; we do not.
- **A running simulation** overrides that with a short interval
  (`RUNNING_FRAME_INTERVAL`, 4 ms), because it must keep advancing whether or not
  the UI has anything new to say.
- **Occlusion** overrides both with a long one (200 ms). A minimized or covered
  window cannot present, so no GPU work is done at all.

Input events and window changes call `schedule_redraw`, which only ever moves the
deadline *earlier*. `set_next_redraw` replaces it outright and is used at the end
of a frame.

Both are clamped to `MAX_IDLE_INTERVAL` (1 s). Clamping is not decoration:
egui returns `Duration::MAX` when nothing needs repainting, and
`Instant::now() + Duration::MAX` panics.

## Consequences

- A paused, idle Field CAD sleeps. That is the correct behaviour for a
  scientific tool that spends most of its life waiting for a person.
- The frame-time readout stops updating when nothing is being drawn. This is
  honest — there are no frames — but it looks like a hang if you do not know why.
- Anything that must animate has to *say so*. Code that mutates state and assumes
  a frame will follow will appear to do nothing until the next event. This is the
  real cost of the decision, and the most likely source of future confusion.
- `RUNNING_FRAME_INTERVAL` is deliberately a short wait rather than zero. Zero
  reintroduces the spin; the wait lets the loop sleep while vertical blank does
  the actual pacing.
- Simulation time is unaffected either way. The clock advances from elapsed
  wall-clock time in whole fixed ticks ([0008](0008-tick-time-from-an-epoch.md)),
  so a slower redraw cadence changes how often results are *seen*, never the
  numerics.

Do not "simplify" this back to `ControlFlow::Poll`. It looks like an
optimisation and is a regression; see
[troubleshooting](../troubleshooting-graphics.md) for the failure it caused.
