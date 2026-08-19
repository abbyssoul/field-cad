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

### 0.1 `--autorun` (2026-08-20, later): permanent CLI flag, no more hand-rolled hooks

This session repeatedly needed the simulation actually *running* without
any GUI input (to drive a controlled A/B against `/proc/<pid>` and GPU
tooling), and had no way to do that — the app has no headless input
driver by design (`apps/fieldcad-desktop/AGENTS.md`). Worked around it
each time with a throwaway env-var hook in `WindowState::new`, reverted
after every use. That's now a permanent flag instead:

```sh
fieldcad path/to/scene.fcscene --autorun --exit-after 100
```

`--autorun` submits `CommandPayload::Play` once, right after the session
loads, the same way a user's first press of the Play button would —
combine with `--exit-after SECONDS` (pre-existing) for a scripted, bounded
run. This is the correct replacement for every `FIELDCAD_DEBUG_AUTOPLAY`
reference elsewhere in this document (written before the flag existed);
skip straight to `--autorun` rather than reintroducing that hook.

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
same `dhat`+`profiling` build, launched twice via what was at the time a
temporary env-var hook (now the permanent `--autorun` flag, §0.1) so the
run could be driven and measured without GUI input:

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

### 3.1 Update (2026-08-20, later): `intel_gpu_top` rules out GPU buffer growth entirely

Run live (`sudo intel_gpu_top`) alongside a playing session of the same
scene, watching the `fieldcad` row's `MEM`/`RSS` columns (per-process
GEM/GTT-backed GPU memory, tracked by the i915/Xe kernel driver — not
guessed at, read directly off `/sys`/`debugfs` the driver exposes) across
the same climb-then-plateau window the Diagnostics panel's `Mem` (process
RSS) shows: **`fieldcad`'s GPU-attributed memory was pinned at exactly
320972K the entire time**, in both an early and a later screenshot, while
process RSS visibly climbed from ~140 MiB toward ~187 MiB in the same
window.

This is stronger and more direct than the `smaps_rollup` inference in §3 —
it rules out *any* growing GPU buffer object for this process, not just
`DynamicFlowLineBuffer` specifically. The growth is not a `wgpu::Buffer`,
not a Vulkan device-memory allocation, not a GEM object of any kind. Drop
the "wgpu/Mesa staging-buffer" theory from §3 entirely; the write-size
ramp on `queue.write_buffer()` was a plausible-sounding guess that turned
out to be looking in the wrong memory pool altogether.

**Revised leading theory:** given RSS growth is `Anonymous`/`Private_Dirty`
(§3) but *not* GPU-attributed (this section) and *not* Rust-heap (dhat's
flat `gmax`), the remaining candidate is **host-side (non-GPU-memory,
non-Rust-heap) `malloc` activity inside a dynamically-loaded C library** —
most plausibly `libvulkan_intel.so` (the Mesa Intel Vulkan ICD) or the
Vulkan loader itself doing routine bookkeeping (command buffer pools,
descriptor allocations, shader variant/pipeline caches, per-submission
tracking structures) that happens to scale with *something* correlated to
trajectory history filling (draw call count stays the same; vertex *data
size* per draw call still ramps — the byte-count theory from the original
§3 may still be right, just landing in host RAM the driver manages for
itself rather than in a `VkDeviceMemory` allocation `intel_gpu_top` would
count). Still not confirmed — see the revised plan below.

## 4. Getting real GPU/host memory evidence next time

Confirmed available on this machine as of 2026-08-20 (checked, not
assumed): `vulkaninfo` reports only `VK_LAYER_KHRONOS_validation` (plus
`VK_LAYER_INTEL_nullhw` and two Mesa layers) installed —
**`VK_LAYER_LUNARG_api_dump` (the per-call Vulkan trace layer) is not
available** via apt here (`vulkan-tools`/`vulkan-validationlayers` don't
ship it; it normally comes from the full LunarG Vulkan SDK, not packaged
for this distro). `valgrind` (`massif`) is installed. `intel_gpu_top`
(`intel-gpu-tools`) is installed and already gave the decisive result
above.

### Fastest, lowest-setup: `/proc/<pid>/smaps_rollup` while driving the app headlessly

This alone got real signal this session (it's how §3's `Anonymous`/`Private_Dirty`
finding was made) and needs no new tooling — but it doesn't attribute
memory to a *specific* GPU allocation, only confirms non-Rust-heap growth.
To reproduce and extend:

```sh
cargo build --profile profiling -p fieldcad-desktop --features dhat
DHAT_HEAP_FILE=dhat-heap-desktop.json ./target/profiling/fieldcad \
    ~/Documents/field-cad/earth-moon-titan.fcscene --autorun --exit-after 100 &
PID=$!
for i in $(seq 1 12); do
  sleep 8
  awk '/VmRSS/{print $2}' /proc/$PID/status
  awk '/^Rss:|^Pss:|^Private_Dirty:|^Anonymous:/{printf "%s=%s ", $1, $2}' \
      /proc/$PID/smaps_rollup; echo
done
```

Use `--autorun` (§0.1) to reach a "running" state without GUI input —
this used to need a throwaway env-var hook or driving the app's MCP
server (which, as of this session, has an unrelated pre-existing panic at
`crates/fieldcad-mcp/src/lib.rs:1518` — `ValidateWorldTransactionParams`'s
generated schema is missing a `type` field, breaking tool listing/calls;
worth fixing before relying on MCP for this kind of thing again, but no
longer necessary now that `--autorun` exists).

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

### `VK_LAYER_KHRONOS_validation` best-practices — already run (2026-08-20), negative result

Run against the `dhat`+`profiling` build, driven headlessly via what was
at the time a temporary env-var hook — now the permanent `--autorun` flag
(§0.1):

```sh
VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation \
VK_LAYER_ENABLES=VK_VALIDATION_FEATURE_ENABLE_BEST_PRACTICES_EXT \
./target/profiling/fieldcad ~/Documents/field-cad/earth-moon-titan.fcscene \
    --autorun --exit-after 100 > /tmp/vk_validation.log 2>&1
```

Full run, `grep -iE "alloc|memory leak|memory usage|budget"
/tmp/vk_validation.log` → **nothing.** The only warnings the whole 100 s
run produced were three `BestPractices-deprecated-extension` (harmless,
fires once at instance/device creation) and ten
`BestPractices-ClearValueWithoutLoadOpClear` (a real but unrelated
cosmetic finding — `SceneRenderer`'s render pass sets clear values without
`LOAD_OP_CLEAR`, so they're silently ignored; worth a one-line fix
sometime, not memory-related, not investigated further). No allocation-
count or memory-growth warning fired at all, across the entire climb.
Matches §3.1's `intel_gpu_top` result: consistent, converging negative
evidence that this is not a Vulkan-API-visible device-memory problem.

Also surfaced, unrelated to this investigation: repeated
`VUID-StandaloneSpirv-None-10684` **validation errors** ("Invalid explicit
layout decorations") on several shader modules at startup — `spirv-val`
flagging naga/wgpu's generated SPIR-V as spec-non-compliant even though
Mesa's runtime accepts and runs it fine. Not chased down (out of scope for
a memory investigation), but worth a note for whoever next touches the
WGSL shaders or bumps `wgpu`/`naga`: this may be a real spec-compliance
gap in generated SPIR-V, or a validator-vs-runtime strictness mismatch —
worth a `spirv-val` pass at least once to characterize before dismissing.

### `intel_gpu_top` — already run, already decisive (§3.1)

`sudo intel_gpu_top` while the scene plays, watching the `fieldcad` row's
`MEM`/`RSS` columns. **Already ruled out GPU buffer growth entirely** —
don't re-spend time confirming this again, it's settled. Still useful as a
quick sanity check that a *future* fix attempt didn't regress this back
into an actual GPU-memory-growth problem.

### `VK_LAYER_KHRONOS_validation` (installed; this is what's realistically
available here — no `api_dump` layer, see above)

This layer does **not** log every routine `vkAllocateMemory`/`vkFreeMemory`
call the way `VK_LAYER_LUNARG_api_dump` would (that layer isn't installed
and isn't in the apt repos for this distro — would need the full LunarG
Vulkan SDK, a heavier install than attempted this session). What it *can*
do without extra installs: **best-practices checks**, which include
warnings about excessive allocation counts and suballocation patterns.
Given §3.1 already shows GPU memory is flat, this is a lower-value next
step now (it's checking for a category of problem already ruled out) —
try it anyway only if you want a second confirmation:

```sh
VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation \
VK_LAYER_ENABLES=VK_VALIDATION_FEATURE_ENABLE_BEST_PRACTICES_EXT \
VK_LOADER_LAYERS_ENABLE='*validation' \
./target/profiling/fieldcad ~/Documents/field-cad/earth-moon-titan.fcscene \
    2>&1 | tee /tmp/vk_validation.log
# then, while it plays: grep -i "alloc\|memory" /tmp/vk_validation.log
```

### `valgrind --tool=massif` — the strongest next candidate given §3.1

Installed (`valgrind 3.25.1`). Unlike dhat (Rust global allocator only)
and `intel_gpu_top` (GPU/GEM memory only), massif intercepts `malloc` at
the **glibc level, process-wide** — every dynamically loaded library,
including `libvulkan_intel.so` and the Vulkan loader, whatever they do
internally. This is the one tool that can actually attribute the
§3.1-revised "host-side, non-GPU, non-Rust-heap" growth to a specific
call stack. Caveat: Valgrind's instrumentation overhead is large (often
20–50×) and GPU-heavy apps sometimes don't run cleanly under it — budget
extra wall-clock time and be ready for it to simply not tolerate the
Vulkan driver's threading/JIT behavior:

```sh
valgrind --tool=massif --pages-as-heap=yes --massif-out-file=/tmp/massif.out \
    ./target/profiling/fieldcad ~/Documents/field-cad/earth-moon-titan.fcscene \
    --autorun --exit-after 100
ms_print /tmp/massif.out | less
```

`--pages-as-heap=yes` matters here — plain massif only tracks
heap-allocator (`malloc`) activity by default, and some driver-internal
allocations may go through raw `mmap` instead; `--pages-as-heap` catches
those too, at the cost of more noise to filter through. If the app won't
run under Valgrind at all (a real possibility for a GPU/Vulkan app), fall
back to `heaptrack` (`sudo apt install heaptrack` — not confirmed
installed or available in this distro's repos, check first) — lower
overhead, same "catches every `malloc`, any library" property, purpose-
built for exactly this kind of "which library is allocating this" question.

## 5. `massif` run (2026-08-20, later still): found and fixed a real allocation-churn bug

Ran `valgrind --tool=massif --pages-as-heap=yes` against the same scene
via `--autorun --exit-after 60`. First attempt used the `--features dhat`
binary and was contaminated by an artifact worth remembering: `dhat`'s own
`Profiler::drop_inner` resolves every collected call-site backtrace via
`mmap`-based ELF symbol lookup at process exit, and massif's automatic
"detailed snapshot" picker happened to land near that moment, misattributing
~695 MB (37% of that snapshot) to `dhat::Globals::finish`/`fieldcad::main`
— a one-time shutdown cost of running dhat itself, not a bug. **Always run
massif against a build *without* `--features dhat`** (`cargo build
--profile profiling -p fieldcad-desktop`, no `--features dhat`) to avoid
this.

Re-run clean (no `dhat`), and the total memory trend across 55 massif
snapshots climbed continuously the entire 60s run, from 1.2 GB to 1.87 GB
with no sign of plateauing — worse than what `/proc`-based RSS polling
alone had shown, and Valgrind's overhead means this doesn't correspond 1:1
to a 60s native run, but the *shape* (unbounded climb) and the *dominant
call stack* were the real finding. The detailed peak snapshot's largest
contributor, uncontaminated by the dhat artifact: **30.82% (546 MB)**
attributed to:

```
<fieldcad_desktop::app::WindowState>::redraw (app.rs:1381)
  Vec::<FlowRibbonVertex>::with_capacity
```

That's `overlay`'s `flow_ribbons` field — at the time, built as
`Vec::with_capacity(self.overlay_flow_ribbons_capacity_hint)` **fresh every
single redraw frame**, sized correctly (no regrowth, §1.3's fix worked as
intended) but never *reused* — a brand-new allocation, immediately dropped
next frame, over and over, tens of times a second. Verified the hint's
actual value was correct (`history_capacity=2881`, `max_ribbon_vertices=138240`
per object — exactly the designed ceiling from §1.3, confirmed via a
temporary `tracing::warn!` in the trajectory loop, since removed) — this
was never a sizing bug, purely a *reuse* bug: right pattern for the
trajectory ribbon cache (§1), wrong pattern for the buffer that copies
those cached ribbons into `overlay` every frame.

**Fixed:** `overlay_flow_ribbons_capacity_hint: usize` replaced with a
persistent `overlay_flow_ribbons: Vec<scene::FlowRibbonVertex>` field on
`WindowState`. `redraw` now `std::mem::take`s it, `.clear()`s it (keeping
capacity), builds `overlay` around it, and moves it back out of `overlay`
after `self.renderer.render(...)` (`overlay`'s last reader) returns — same
take-clear-refill-give-back idiom already used for the trajectory ribbon
cache and `RegionGeometryCache`, just applied one level up, to the buffer
that merges *into*.

**Verified with a controlled before/after massif A/B, same methodology:**

| | before | after |
|---|---|---|
| Total memory over 60s (massif, `--pages-as-heap`) | 1.2 GB → 1.87 GB, still climbing | 1.2 GB → ~1.25 GB, flat |

A native (non-Valgrind) `--autorun --exit-after 100` run's `/proc/<pid>/status`
`VmRSS` still climbs during the trajectory history fill period (142 MB →
174 MB) — expected and correct, since the ribbon's actual *content* grows
while history fills toward its capacity (§2/§3 already established this
part is legitimate) — but now plateaus around the same time history fills
(~54s in this run), not well past it, and settles at roughly the same or a
slightly lower steady-state RSS than before this fix, instead of
continuing to climb indefinitely.

This is the first fix in this whole investigation with hard, reproducible,
tool-verified evidence behind it (not a guess later disproven) — the
`intel_gpu_top`/validation-layer results in §3.1/§4 earned their keep by
ruling out two wrong hypotheses fast, but this is the one that found and
fixed an actual bug.

### Suggested next-session order

`intel_gpu_top` (§3.1) and `VK_LAYER_KHRONOS_validation` best practices
(§4) are closed questions (GPU memory, Vulkan-API-visible allocation) —
don't redo unless a reproduction disagrees. §5's fix landed and was
verified with the same massif methodology; if RSS growth is still
reported, re-run massif first (uncontaminated, no `dhat` feature — see the
warning above) rather than guessing again. Whatever call stack it points
at next is real signal; this session's massif runs proved the tool works
for this problem.
