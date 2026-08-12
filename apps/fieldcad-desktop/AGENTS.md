# Desktop client notes

Read the repository-level `AGENTS.md`, then these rules before changing the
native window, renderer, GPU compute, or UI event loop.

## Verification limit

This crate has no driven GUI test harness (no Xvfb, input replay, or screenshot
test path). For UI or rendering work, run the relevant build/tests/clippy checks
and `cargo run -p fieldcad-desktop -- --smoke 120`; state plainly when manual
in-app verification is still required.

## Window and renderer lifetime

- Struct declaration order defines GPU resource drop order. Keep renderer
  resources ordered from pipelines/buffers/textures through queue, device,
  surface, adapter, then instance; keep the renderer before the window in its
  owner so the window outlives every object that references it.
- Tear down graphics in `ApplicationHandler::exiting` while the event loop is
  alive. Drain in-flight work with `device.poll(wgpu::PollType::Wait { .. })`
  before dropping resources.
- Recreate a surface by draining work, taking and dropping the old `Option`
  surface, then creating its replacement. Do not keep two surfaces for one
  window alive together.

## Event loop and presentation

- Keep redraw demand-driven: use `ControlFlow::WaitUntil` from egui's repaint
  deadline rather than continuous `Poll` plus unconditional redraw requests.
- When occluded, stop rendering and back off; a covered or minimised window
  cannot present and retrying at frame rate wastes CPU/GPU time.
- When a graphics failure is unclear, first separate window/presentation from
  shader/GPU work with the smoke or offscreen path; inspect kernel GPU-reset
  logs before attributing it to the driver.
