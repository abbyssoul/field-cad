# Troubleshooting the graphics stack

If the window freezes, the desktop freezes, or the app crashes on exit, start
here. The commands are ordered so that the safest ones come first.

## 1. Check the GPU path without opening a window

```shell
cargo run --release -p fieldcad-desktop -- --smoke 120
```

This creates a device, compiles the shaders, builds the pipelines, and renders
120 frames to an offscreen texture. It never creates a surface, so it cannot
involve or wedge a compositor.

- **It passes** → the adapter, driver, shaders, and pipelines are fine. Any
  remaining problem is in presentation or the event loop. Continue to step 2.
- **It fails** → the problem is below us. Try another backend (step 3) and
  report the error text.

## 2. Try a windowed run that ends by itself

```shell
cargo run --release -p fieldcad-desktop -- --exit-after 20
```

The process quits on its own after twenty seconds. If a windowed run has
previously locked up your session, use this rather than an open-ended run: the
app leaves without needing to be killed from another machine or a TTY.

## 3. Choose a different backend or present mode

Every one of these is a plain environment variable; no rebuild is needed.

```shell
# OpenGL instead of Vulkan
WGPU_BACKEND=gl cargo run --release -p fieldcad-desktop

# Do not block waiting for vertical blank
FIELDCAD_PRESENT_MODE=no-vsync cargo run --release -p fieldcad-desktop

# Software rendering — slow, but independent of the GPU driver
FIELDCAD_FORCE_FALLBACK=1 cargo run --release -p fieldcad-desktop
```

| Variable | Values | Notes |
| --- | --- | --- |
| `WGPU_BACKEND` | `vulkan`, `gl`, `metal`, `dx12`, or a comma-separated list | Read by `wgpu` itself |
| `FIELDCAD_PRESENT_MODE` | `vsync`, `no-vsync`, `fifo`, `fifo-relaxed`, `mailbox`, `immediate` | An unsupported mode is reported and ignored rather than submitted |
| `FIELDCAD_FORCE_FALLBACK` | `1` | Demands a software adapter |

The selected backend, present mode, and surface format are logged at startup:

```shell
RUST_LOG=fieldcad_desktop=debug cargo run --release -p fieldcad-desktop
```

## 4. Tell the two failure shapes apart

Whether the GPU actually hung is worth knowing, because it points at completely
different causes:

```shell
journalctl -k -b 0 | grep -iE "GPU HANG|i915|xe |reset|drm"
```

- **A GPU hang or engine reset is logged** → the driver received work it could
  not complete. Capture the log and the output of `--smoke`.
- **Nothing is logged** → the GPU was healthy and something above it blocked.
  That is a client or compositor problem, not a driver fault.

---

## Known-fixed causes

Three defects with exactly these symptoms were found and fixed on 2026-08-02.
They are recorded here because the symptoms are easy to misattribute to the
driver.

### Teardown in the wrong order — segfault on exit

Rust drops struct fields in declaration order. `ViewportRenderer` declared
`instance, surface, adapter, device, queue, …, depth, scene, gui`, which
destroys the instance first and the GPU resources last — the exact inverse of
the required order. `WindowState` likewise declared `window` before `renderer`.

Fixed by making declaration order match the required teardown order (GUI and
scene resources, depth, queue, device, surface, adapter, instance) and by adding
a `Drop` that drains in-flight submissions first. `WindowState` now drops the
renderer before the window, and `ApplicationHandler::exiting` releases the
graphics stack while the event loop is still alive.

**A segfault on exit is a lifetime bug until proven otherwise.** It is rarely
the driver.

### An unconditional redraw spin — compositor starvation

The event loop used `ControlFlow::Poll` and called `request_redraw()` from
`about_to_wait` on every single iteration. The loop therefore never idled and
asked for a new frame as fast as the machine allowed, while blocking on vsync
inside the event callback.

Fixed by making redraws demand-driven: `ControlFlow::WaitUntil` a deadline taken
from egui's own `repaint_delay`, overridden to a short interval only while the
simulation is actually advancing. A paused, idle application now sleeps instead
of spinning.

### Rendering into a surface that cannot present

`CurrentSurfaceTexture::Occluded` was treated the same as a timeout and retried
immediately, so a minimized or fully covered window produced a tight retry loop
against a surface that would never yield a frame.

Fixed by tracking `WindowEvent::Occluded`, skipping GPU work entirely while
occluded, and backing off to a 200 ms retry.

### Surface recreation

`recreate_surface` assigned the new surface over the old one, so both briefly
existed for the same window — not valid on every backend. It now drains the
queue and drops the old surface before creating the replacement.

## If it still misbehaves

Please capture:

1. `cargo run --release -p fieldcad-desktop -- --smoke 120` for each of
   `WGPU_BACKEND` unset, `=vulkan`, and `=gl`;
2. `RUST_LOG=fieldcad_desktop=debug,wgpu_core=info` output from an
   `--exit-after 20` run;
3. `journalctl -k -b 0 | grep -iE "GPU HANG|i915|drm"`;
4. `vulkaninfo --summary | head -40` and `glxinfo -B`.

A note on multi-monitor systems: if a virtual or USB display driver such as
`evdi`/DisplayLink is loaded, check whether the problem follows the window to a
different screen. `evdi` reporting `flip_done timed out` in the kernel log is a
compositor-level fault that no client-side change can address.
