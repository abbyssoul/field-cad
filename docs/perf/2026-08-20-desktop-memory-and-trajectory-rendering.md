# Desktop trajectory rendering: CPU allocation pass (done) + an open GPU-side memory question

Date: 2026-08-20
Scope: `apps/fieldcad-desktop/src/scene/{flow_lines,trajectory,field}.rs`,
`apps/fieldcad-desktop/src/app.rs` (`compute_field_layer_geometry`,
`WindowState::redraw`'s trajectory loop), `apps/fieldcad-desktop/src/renderer.rs`
(`DynamicFlowLineBuffer`).
Method: `dhat` heap profiling of `target/profiling/fieldcad` (new
`dhat`-feature-gated desktop build, see below) against
`~/Documents/field-cad/earth-moon-titan.fcscene`, cross-checked against the
Diagnostics panel's `Mem` plot (which reads whole-process RSS, not the Rust
heap — see §3) and, for the open question, direct `/proc/<pid>/status` +
`smaps_rollup` polling of a live run. Per `AGENTS.md`, treat every number
below as a snapshot to re-verify, not a permanent truth — re-run the
profile before trusting an old figure.

## 0. New harness this session

The desktop app now builds with a `dhat` cargo feature (`apps/fieldcad-desktop/Cargo.toml`),
gating a `#[global_allocator] static ALLOC: dhat::Alloc` and a
`dhat::Profiler` held for the lifetime of `main()` in `main.rs`. A new
`[profile.profiling]` in the root `Cargo.toml` (`inherits = "release"`,
`debug = true`) keeps symbols for call-site attribution in an otherwise
optimized build.

```sh
cargo build --profile profiling -p fieldcad-desktop --features dhat
./target/profiling/fieldcad ~/Documents/field-cad/earth-moon-titan.fcscene
# play the scene, let it run, then quit via the window's normal close —
# only a clean `main()` return flushes dhat-heap-desktop.json (Ctrl-C/kill
# skips it)
```

Writes `dhat-heap-desktop.json` in the CWD. `crates/fieldcad-bench/examples/profile_scene.rs`
remains the equivalent *headless* harness (no GPU/renderer, CPU solver only)
from the prior session — use that when the question is solver/simulation
allocation, use the desktop build when the question involves rendering,
UI, or (per §3) anything GPU-adjacent.

## 1. CPU-side fixes landed this session (committed: `3edfe80 trajectory calc perf pass`)

Starting point: a live play session's dhat profile showed ~77% of all
bytes ever allocated (61–83 GB across several runs) at one call site:
`FlowRibbonVertex` buffers. Two distinct bugs stacked here, found in order:

1. **`compute_field_layer_geometry`'s per-frame merge** (`app.rs`) built
   `FieldGeometry::default()` fresh every frame and `.extend()`'d each
   visible region's geometry into it one at a time — repeated
   reallocate-and-copy instead of one `Vec::with_capacity(sum of region
   sizes)`. Fixed.
2. **The real dominant cost**: `apps/fieldcad-desktop/src/scene/trajectory.rs`'s
   `append_trajectory_geometry` (the orbit-trail renderer) had *no cache at
   all* — every redraw frame, for every visible trajectory, it fully
   re-derived the Hermite-fit polyline, recency-fade colors, and ribbon
   from the object's entire trimmed `BodySample` history, even on frames
   where history hadn't advanced. History length grows one sample per tick
   toward a capacity `TrajectoryDisplay::required_body_history_capacity`
   computes from `trail_seconds / dt` (hard ceiling 200,000 samples,
   `scene/mod.rs`), so for a session's whole "fill" period this was a
   real, unbounded-feeling, `O(history.len())`-and-growing cost every
   single frame.

   Fixed with `TrajectoryGeometryCache`/`TrajectoryGeometryInputs`
   (`app.rs`), mirroring the already-existing `RegionGeometryCache`
   exactly: keyed on `ObjectId`, invalidated only on `(history_len,
   newest_tick, display, scene_scale)`. A redraw where history hasn't
   advanced now costs an `Arc::clone`. `append_trajectory_geometry` and
   `build_flow_ribbon` (`flow_lines.rs`) were both changed to write into a
   caller-supplied `&mut Vec<FlowRibbonVertex>` instead of returning a
   fresh one, so that buffer's capacity survives across frames
   (`Arc::try_unwrap` reclaim, same idiom `RegionGeometryCache` already
   used).
3. **Reserve the known ceiling up front.** `TrajectoryDisplay::required_body_history_capacity`
   already gives an exact upper bound on ribbon size, fixed until a user
   edits `trail_seconds`/`dt`. Added `scene::trajectory::max_ribbon_vertices`
   (same polyline/ribbon-vertex math run in reverse) and reserve it on the
   cache buffer immediately, instead of letting the `Vec` regrow tick by
   tick through the doubling sequence while history fills toward capacity.

**Verified effect:** dhat's total-bytes-allocated dropped from ~91 GB to
~71 GB over comparable runs, and the `FlowRibbonVertex` share of that total
fell from ~78% to the same ballpark but at a much smaller absolute size.
All of this is real and worth keeping regardless of §3 below — it fixed a
genuine `O(history.len())`-per-frame CPU cost, confirmed by dhat, and the
`RegionGeometryCache`-style caching is the right shape for this kind of
problem generically (any per-object/per-region geometry that's expensive
to rebuild and doesn't change every frame should get one).

## 2. What did NOT explain the Diagnostics `Mem` plot's climb-then-plateau

After the fixes in §1, a live play session's Diagnostics panel still showed
`Mem` climbing steadily for the first `trail_seconds` of wall-clock, then
flattening — e.g. 123.8 MiB → 195.6 MiB over a run whose trajectory
capacity (`trail_seconds=172800s`, `dt=60s` → 2880 ticks) filled at almost
exactly the same tick the climb stopped. This *looked* like it should be
explained by §1's `Vec` filling from empty even with the reservation in
§1.3 — worth checking why it wasn't:

- `Diagnostics`'s `Mem` field is **whole-process RSS**, read from
  `/proc/self/status` (`apps/fieldcad-desktop/src/app.rs`, `frame_stats`),
  **not the Rust heap**. dhat only instruments the Rust global allocator.
- A dhat pass over the same climbing-RSS run showed `gmax` (peak
  simultaneous Rust-heap bytes) flat at ~84 MB the whole time — nowhere
  near the ~189 MB RSS the same run's Diagnostics panel reported. The gap
  is real and dhat cannot see into it by construction.

## 3. The open question: where does the ~40–60 MB of non-Rust-heap RSS growth live?

**Ruled out by direct experiment, not just reasoning:** `renderer.rs`'s
`DynamicFlowLineBuffer` (backing the flow-ribbon GPU vertex buffer, shared
by streamlines and trajectories) grows its `wgpu::Buffer` reactively via
`next_power_of_two` — the same amortized-growth shape as any `Vec`. Added
`SceneRenderer::reserve_flow_line_capacity`/`DynamicFlowLineBuffer::ensure_capacity`
so `WindowState::redraw`'s trajectory loop could reserve the same known
ceiling from §1.3 on the GPU buffer up front (summed across every
currently-watched object — a small, bounded set, unlike field-layer
streamlines, which is why only this buffer got the treatment).

**This had zero measurable effect**, confirmed by a controlled A/B: the
same `dhat`+`profiling` build, launched twice via a temporary (reverted,
not committed) `FIELDCAD_DEBUG_AUTOPLAY` env-var hook that submits
`CommandPayload::Play` at startup so the run could be driven and measured
without GUI input:

| | paused (no ticks) | running (autoplay), buffer pre-sized |
|---|---|---|
| RSS over ~100 s wall-clock | flat, 125 MB | 144 MB → 180 MB, plateaus |
| dhat `gmax` | — | flat, 84 MB |

Identical climb-then-plateau shape to before the GPU buffer fix. The
destination `wgpu::Buffer`'s capacity was never the bottleneck.

**Better evidence, still short of a fix:** `/proc/<pid>/smaps_rollup`
during the climb shows the growth is `Private_Dirty`/`Anonymous` pages
(regular heap-shaped, not file-backed, not `Shared_*`) — growing in
lock-step with RSS (e.g. 62.6 MB → 99.2 MB `Anonymous` while RSS grew
36.6 MB). Combined with dhat staying flat, this points at **allocations
made by a dynamically-loaded C library outside Rust's global allocator
entirely** — the Vulkan loader / Mesa Intel driver (`libvulkan_intel.so`,
loaded via `dlopen`), whose own internal `malloc` calls `#[global_allocator]`
cannot intercept. The leading candidate: `queue.write_buffer()`'s
byte-length is still `base.len() + overlay.len()` vertices — a number that
still ramps up every tick while trajectory history fills, *independent* of
whether the destination buffer was pre-sized — and wgpu-hal/the Vulkan ICD
may retain internal staging/bounce-buffer chunks sized to the largest
write seen so far, never released, without exposing that as Rust-heap
activity.

**Not confirmed.** The next step (always writing a constant, full-capacity
amount of data every frame instead of the actual, growing content) is a
real structural change and still a guess about wgpu/Mesa internals, not
verified against any GPU-side profiler. Decided to stop guessing after two
misses (region-geometry merge fix, then GPU buffer pre-sizing) rather than
try a third blind patch — see §4 for how to get real evidence next time.

**Status of code:** the GPU buffer pre-sizing (`renderer.rs`,
`SceneRenderer::reserve_flow_line_capacity` and the `app.rs` call site) is
still in the working tree, uncommitted, as of this note. It's a real,
correct, low-risk improvement in its own right (a session with many
watched trajectories now avoids a handful of reactive GPU buffer
recreations) even though it didn't touch the RSS mystery — worth keeping
and committing regardless of whether §4's investigation ever resumes.

## 4. Getting real GPU memory evidence next time

Nothing below was run this session — these are setup instructions for
whoever (agent or human) picks this back up, so the next round starts with
real driver-level numbers instead of another guess.

### Fastest, lowest-setup: `/proc/<pid>/smaps_rollup` while driving the app headlessly

This alone got real signal this session (it's how §3's `Anonymous`/`Private_Dirty`
finding was made) and needs no new tooling — but it doesn't attribute
memory to a *specific* GPU allocation, only confirms non-Rust-heap growth.
To reproduce and extend:

```sh
cargo build --profile profiling -p fieldcad-desktop --features dhat
DHAT_HEAP_FILE=dhat-heap-desktop.json ./target/profiling/fieldcad \
    ~/Documents/field-cad/earth-moon-titan.fcscene --exit-after 100 &
PID=$!
for i in $(seq 1 12); do
  sleep 8
  awk '/VmRSS/{print $2}' /proc/$PID/status
  awk '/^Rss:|^Pss:|^Private_Dirty:|^Anonymous:/{printf "%s=%s ", $1, $2}' \
      /proc/$PID/smaps_rollup; echo
done
```

The app has no CLI flag to autoplay a loaded scene (by design — it's an
interactive app). To reach a "running" state without GUI input, either:
drive it over its own MCP server (`--mcp 127.0.0.1:PORT`, prints a bearer
token; POST JSON-RPC `initialize` then `tools/call` `play` to `/mcp` —
**note:** hit an unrelated pre-existing panic in `crates/fieldcad-mcp/src/lib.rs:1518`
(`ValidateWorldTransactionParams`'s generated schema is missing a `type`
field) that broke tool listing/calls this session, not investigated,
worth fixing before relying on MCP for this again; or temporarily
reintroduce a `FIELDCAD_DEBUG_AUTOPLAY` env-var hook submitting
`CommandPayload::Play` right after `WindowState::new` builds `data_source`
(this session's version, not committed — small enough to redo in a
minute, see `git log`/this doc for the shape) and remember to revert it.

### `/proc/<pid>/smaps` (full, not rollup) mapped to `/dev/dri/renderD*`

One level more specific than `smaps_rollup`: individual mappings backed by
the GPU device node show up as distinct entries, so growth specifically in
DRM/GEM-backed (GPU buffer object) mappings vs. generic anonymous driver
heap allocations becomes visible:

```sh
grep -A20 "renderD128\|/dev/dri" /proc/$PID/smaps | less
# or, for a running total over time:
watch -n2 'grep -B1 -A18 renderD /proc/'"$PID"'/smaps | awk "/^Rss:/{s+=\$2} END{print s\" kB\"}"'
```

### Real GPU-memory profilers (not yet installed/tried this session)

For the Intel iGPU this machine uses (`Intel(R) Iris(R) Xe Graphics
(RPL-P)`, Mesa driver, Vulkan backend):

- **`intel_gpu_top`** (package `intel-gpu-tools` — `sudo apt install
  intel-gpu-tools` on Debian/Ubuntu) — live engine/memory utilization,
  per-process. Run alongside the app (`sudo intel_gpu_top`) while it plays
  the same scene; watch for a client entry matching the `fieldcad` PID and
  whether its memory column tracks the RSS climb.
- **Vulkan `VK_LAYER_KHRONOS_validation` with `VK_EXT_device_memory_report`
  / `VK_EXT_memory_budget`** — the Khronos validation layer (package
  `vulkan-validationlayers`, or bundled with the LunarG Vulkan SDK) can
  report every `vkAllocateMemory`/`vkFreeMemory` call with size and type,
  which would show *exactly* whether the driver is allocating new device
  memory blocks during the climb, and never freeing them. Enable via
  `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation` and
  `VK_LAYER_ENABLES=VK_VALIDATION_FEATURE_ENABLE_DEBUG_PRINTF_EXT` (or the
  dedicated GPU-assisted/memory report extension flags — check the layer's
  own docs for the current flag names) before launching
  `target/profiling/fieldcad`. This is the most direct way to settle §3
  for good: either device memory allocations track the RSS climb (driver
  buffer growth, actionable) or they don't (something else entirely, e.g.
  shader/pipeline cache growth, and the flow-ribbon buffer theory is a dead
  end).
- **`RenderDoc`** (`sudo apt install renderdoc`, or from renderdoc.org) —
  attach and capture a frame at the start and near the plateau of a
  climbing-RSS run; its resource browser lists every live GPU
  buffer/texture with size, which would directly show whether any
  `fieldcad`-created resource is larger than expected, or whether the
  driver holds internal resources RenderDoc can still see (it captures at
  the Vulkan API level, so driver-internal-only allocations — e.g. a
  staging pool never exposed as a `VkBuffer` — would still be invisible to
  it; the validation-layer memory report above is the more exhaustive
  option for exactly that case).
- **Mesa-specific env vars** worth trying first since they need no
  install: `MESA_VK_TRACE=1`/`MESA_DEBUG=allocs` style env vars vary by
  Mesa version — check `vulkaninfo` output and `man vulkaninfo`/Mesa's own
  docs for what this Mesa build supports before assuming a flag name;
  don't guess one into the launch command without checking it's real.

### Suggested next-session order

1. Reproduce §3's controlled A/B (paused vs. running RSS) once more to
   confirm the finding still holds against current code — `docs/perf/`
   findings go stale fast (`AGENTS.md`).
2. Run with `VK_LAYER_KHRONOS_validation`'s memory report enabled across
   the same paused/running comparison. If device memory allocations track
   the RSS climb, that confirms driver buffer growth and narrows down
   *which* Vulkan object triggers it (by size/type signature); if they
   don't, drop the flow-ribbon-upload theory entirely and look elsewhere
   (shader cache, descriptor pool churn, something in egui-wgpu's own
   resource management).
3. Only then write the next fix — the constant-size-upload experiment
   from §3, or whatever the validation layer actually points at.
