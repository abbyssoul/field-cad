# Task: trajectory ribbons are fully rebuilt from scratch every redraw

## Status: mostly resolved (2026-08-19)

Added `TrajectoryGeometryCache`/`TrajectoryGeometryInputs`
(`apps/fieldcad-desktop/src/app.rs`), a per-object cache mirroring
`RegionGeometryCache` exactly: keyed on `ObjectId`, invalidated on
`(history_len, newest_tick, display, scene_scale)` — sufficient because a
`BodyHistory` ring buffer's *content* is fully determined by its length and
newest tick (older samples age out from the front in strict tick order).
`append_trajectory_geometry` now writes into a plain `Vec<FlowRibbonVertex>`
(not a whole `FieldGeometry`) so that buffer can be cached and its capacity
reclaimed via `Arc::try_unwrap` across frames, same idiom
`compute_field_layer_geometry` already used for `RegionGeometryCache`. A
redraw where a trajectory's history hasn't advanced since last frame now
costs an `Arc::clone` instead of the full Hermite refit + recency-fade +
ribbon build.

**Update (2026-08-19, later):** a dhat pass over the same scene showed the
cache above working (no more `O(history.len())` recompute every frame,
confirmed via `--features dhat`), but a live run's Diagnostics memory plot
still climbed steadily for the first `trail_seconds` of wall-clock — because
even with the cache, each tick's rebuild grew the ribbon `Vec` one
`build_flow_ribbon` call's worth at a time via ordinary amortized growth,
same as any other `Vec` filling up from empty. Confirmed by observation:
the climb stopped exactly at the tick where `history.len()` reached
`TrajectoryDisplay::required_body_history_capacity` (2880 ticks at this
scene's `trail_seconds = 172800s`, `dt = 60s`) — i.e. it was legitimate
fill-up, not a leak, but still wasteful, since that capacity is known in
advance and does not change tick to tick. Fixed by adding
`scene::trajectory::max_ribbon_vertices(capacity_samples)` (the same
polyline/ribbon math `append_trajectory_geometry` uses, run in reverse) and
reserving it on the cache buffer up front, in `app.rs`'s rebuild branch —
the ribbon `Vec` now reaches its lifetime capacity in one reservation
instead of the usual doubling sequence, so filling from an empty history to
a full one costs one allocation per object instead of ~`log₂(capacity)`.

**Update (2026-08-19, later still):** the CPU-side fix above didn't stop
the Diagnostics panel's `Mem` plot from still climbing-then-plateauing —
because that field reads whole-process RSS from `/proc/self/status`
(`frame_stats` in `app.rs`), not the Rust heap, and the actual remaining
growth was on the GPU side, invisible to dhat entirely. Confirmed by
comparing a dhat pass's `gmax` (peak simultaneous Rust-heap bytes, ~84 MB)
against the RSS the same run's Diagnostics panel reported (~189 MB): dhat
only instruments Rust's global allocator, and `renderer.rs`'s
`DynamicFlowLineBuffer` (backing the flow-ribbon draw call, shared by
streamlines and trajectories) grows its `wgpu::Buffer` reactively via
`next_power_of_two` — a real GPU allocation on every regrow, invisible to
dhat, that ratchets up in step with the same trajectory-history-filling
timeline and then holds at that high-water mark. Fixed the same way as the
CPU buffer: `SceneRenderer::reserve_flow_line_capacity`/
`DynamicFlowLineBuffer::ensure_capacity` let a caller reserve a tight known
upper bound up front. `WindowState::redraw`'s trajectory loop now sums
`scene::max_ribbon_vertices` across every currently-watched object (a
small, bounded set) and reserves that on the GPU buffer before `update`
would otherwise discover it needs to grow reactively — one GPU allocation
at (or near) the buffer's lifetime size instead of the usual doubling
sequence. Deliberately *not* applied to `field_surface`/`field_lines`
(the other two `Dynamic*Buffer`s, backing streamline/glyph geometry): those
depend on open-ended per-layer density/streamline settings with no single
worthwhile bound to precompute, unlike a session's small, explicit set of
watched trajectory objects.

**What's still open:** `overlay.flow_ribbons.extend(ribbon.iter().copied())`
still copies the (now-cached) ribbon into `overlay` every single redraw,
because `overlay` itself is deliberately rebuilt fresh every frame (see the
comment where it's constructed in `WindowState::redraw` — it also carries
drag/selection-dependent geometry that genuinely can't be cached the same
way). That copy is a memcpy-shaped cost proportional to ribbon size, not the
`O(history.len())` recompute this task was originally about, and is a much
smaller, bounded, steady-state cost — but for a very long trail it is not
free. If a future profile shows this copy itself is significant, the fix is
probably to stop routing trajectory ribbons through `overlay` at all and
merge them the way `base` (the field-layer geometry) is merged instead.

## Goal

A watched object's trajectory trail (`apps/fieldcad-desktop/src/scene/trajectory.rs`)
should cost roughly the same per frame regardless of how long the session has
been running, the way the field-layer flow-line region cache
(`apps/fieldcad-desktop/src/app.rs`'s `RegionGeometryCache`) already does for
streamlines.

## Current limitation

`WindowState::redraw` calls `scene::append_trajectory_geometry` for every
visible trajectory on *every* redraw frame, unconditionally. That function
always: re-derives `trimmed` from the object's full recorded `BodySample`
history, rebuilds the entire Hermite-interpolated polyline
(`hermite_polyline`), rebuilds every vertex's recency-fade colour
(`recency_fade`), and rebuilds the whole ribbon (`build_flow_ribbon`) — an
`O(history.len())` cost paid in full every frame, even on a frame where
nothing about that object's history changed (`request_body_history`'s own
doc comment already notes the fetch is async and can be a frame stale).

`history.len()` itself is bounded (`BodyHistory`'s per-object capacity,
`apps/fieldcad-desktop/src/scene/mod.rs`'s `MAX_BODY_HISTORY_CAPACITY =
200_000`), but a long `trail_seconds` at a coarse `dt` can require a capacity
far larger than the number of ticks a session actually runs in practice —
`TrajectoryDisplay::required_body_history_capacity` computes
`trail_seconds / time_step_seconds`, uncapped short of the 200k ceiling. In
that configuration `history.len()` grows by one every tick for the entire
session (never reaching steady state within a normal play session), so the
per-frame rebuild cost — and every allocation it drives — grows linearly for
as long as the session runs. This was misread once already as a memory leak
during a dhat profiling pass (`dhat-heap-desktop.json`, 2026-08-19): total
bytes-allocated matched a real, bounded-but-growing cost concentrated at
`trajectory.rs`'s `build_flow_ribbon` call site, not an actual unfreed leak
(dhat's own end-of-run live-byte count was ~40 KB the whole time).

Two narrower fixes already landed as part of that pass (2026-08-19):
`build_flow_ribbon` now writes directly into the caller's output buffer
instead of allocating its own `Vec` and having the caller `.extend()` it in
(cut the per-ribbon allocation count in half), and `WindowState` now carries
`overlay_flow_ribbons_capacity_hint` so `overlay`'s `flow_ribbons` buffer is
pre-sized from last frame's length instead of growing from empty every
frame. Neither addresses the underlying `O(history.len())`-every-frame
recompute this task is about.

## Required behavior

- A frame in which a watched object's trajectory history has not advanced
  (no new tick recorded since last redraw) should not rebuild that object's
  ribbon at all — reuse last frame's result, mirroring
  `RegionGeometryCache`'s hit path and `WindowState::cached_field_layer_geometry`.
- A frame in which history *has* advanced by a small number of new samples
  should not repay the full `O(history.len())` cost. Prefer an incremental
  update: append ribbon geometry for the newly recorded tail, and trim from
  the front only the vertices whose source samples aged out past
  `trail_seconds`this frame — both proportional to the number of *new*
  samples, not the trail's total length.
- `recency_fade`'s colours depend on a vertex's position in the whole
  trimmed window, not just newness, so an incremental scheme needs either a
  cheap way to re-fade only the affected span or a fade formula that doesn't
  require revisiting the whole buffer when the window's front trims.
- Preserve today's behavior exactly: Hermite fit through recorded
  position/velocity, `SUBSTEPS_PER_INTERVAL` smoothing, the `trail_seconds`
  cutoff with its floor at 2 samples (see `CLAUDE.md`'s "simulated time vs.
  coarse timestep" note — do not regress that), and animated-trail scrolling
  independent of the simulation clock.

## Tests and acceptance

- A regression test that asserts the ribbon-building cost for a fixed
  history *tail* growth (e.g. +1 sample) does not scale with total history
  length — e.g. compare allocation count or a cheap proxy (calls into
  `build_flow_ribbon`/its replacement) at a short vs. a long history for the
  same one-sample advance.
- Existing `trajectory.rs` tests (`trail_seconds_trims_samples_older_than_the_cutoff`,
  `a_time_step_coarser_than_trail_seconds_still_draws_the_latest_leg`, the
  Hermite/recency-fade unit tests) continue to pass unmodified in behavior.
- A dhat pass over `earth-moon-titan.fcscene` (or similar) with a long
  `trail_seconds` shows the trajectory-ribbon allocation total no longer
  growing with session length.

## Relevant code

- `apps/fieldcad-desktop/src/scene/trajectory.rs` — `append_trajectory_geometry`,
  `hermite_polyline`, `recency_fade`.
- `apps/fieldcad-desktop/src/scene/flow_lines.rs` — `build_flow_ribbon`
  (shared with field streamlines; already takes an `&mut Vec<FlowRibbonVertex>`
  output param as of 2026-08-19).
- `apps/fieldcad-desktop/src/app.rs` — `WindowState::redraw`'s trajectory
  loop (`object_trajectories`), `RegionGeometryCache`
  (`field_layer_geometry`) for the existing per-region cache pattern this
  should mirror.
- `apps/fieldcad-desktop/src/scene/mod.rs` — `TrajectoryDisplay`,
  `required_body_history_capacity`, `MAX_BODY_HISTORY_CAPACITY`.
