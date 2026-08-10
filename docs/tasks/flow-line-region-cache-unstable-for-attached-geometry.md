# Task: `RegionGeometryCache` retraces flow lines every tick for a region attached to a moving object

## Goal

Stop a plane/box/sphere attached to a moving (or per-tick-recomputed)
object from forcing a full RK4 streamline retrace and a fresh
multi-megabyte `Vec<FlowRibbonVertex>` allocation every physics tick, the
same way `sample-cache-key-unstable-for-attached-geometry` (already
landed — see the commit that removed that doc) fixed the analogous
problem one layer down, in `fieldcad-plugin-api`'s `SampleCache`.

## Current limitation

Found live, profiling `apps/fieldcad-desktop` with a user driving the
session: turning a plane's flow-line **animation** off (leaving the lines
themselves visible) shrank the observed memory sawtooth noticeably but
did not eliminate it — a residual, coarser step pattern remained. That
residual matches this cache, not the one animation actually fixes (see
`docs/tasks/` history — the *frame-merge* issue, unconditionally
rebuilding `FieldGeometry` every redraw regardless of whether any region
changed, was fixed separately and explains the larger, animation-coupled
part of the sawtooth).

`WindowState`'s `region_geometry_cache: BTreeMap<(ChannelId, RegionId),
RegionGeometryCache>` (`apps/fieldcad-desktop/src/app.rs:450` area) memoizes
each visible region's traced/triangulated geometry, keyed on
`RegionGeometryInputs` (`app.rs:258`) and compared by full
`#[derive(PartialEq)]` (`app.rs:387`: `entry.inputs.batch == *batch`).
`RegionGeometryInputs::batch` is a whole `fieldcad_core::FieldBatch`,
which embeds the resolved `SampleGeometry` — exactly the value that, for
a plane/box/sphere `attached_to` a moving object, changes every tick
(`crates/fieldcad-simulation/src/runtime.rs::geometries`, same resolution
path the `SampleCache` task already documented in detail). So this cache
misses every tick for such a region too, and a miss here is much more
expensive than a miss was in `SampleCache`: it re-runs
`scene::region_geometry` → `flow_lines::trace_*_streamlines`, full RK4
adaptive-step integration over every seed, and allocates a fresh
`Vec<FlowRibbonVertex>` (up to `MAX_RIBBON_VERTICES = 300_000`,
~56 bytes/vertex, ~16.8 MB at the cap — `apps/fieldcad-desktop/src/scene/flow_lines.rs:62`)
that the old one is immediately dropped in favor of.

Confirmed live against `earth-moon-2.fcscene`: both its visible planes are
attached (one to the Moon, one to a `derived: true` "Center of mass"
marker), and flow lines on either produced this pattern.

## Why this needs more design than the `SampleCache` fix did

The `SampleCache` fix was safe to do mechanically (key by identity, refill
the existing buffer in place) because recomputation was *already*
unavoidable every tick regardless of whether the geometry's resolved
value changed — sources move every tick, so the sampled values need
refreshing regardless. The fix only removed a reallocation, never skipped
real work.

Here, "attached geometry resolves to a different value" and "the region's
flow lines need to be retraced" are not the same fact, and conflating
them is the actual risk:

- A plane attached to the **Moon** genuinely needs retracing most ticks —
  the field in the plane's own co-moving frame is not static just because
  the plane is "attached" (Earth's position relative to the Moon keeps
  changing as they orbit).
- A plane attached to something like the **derived "Center of mass"**
  marker resolves to a *different* value every tick too, but that looks
  like floating-point-level drift around a point that should be exactly
  fixed for an isolated two-body system, not real motion — retracing here
  is closer to pure waste.

A fix that decides "skip retracing" based on some tolerance on how much
the resolved geometry changed would need to pick that tolerance
carefully: too loose and a genuinely-moving attached plane shows visibly
stale flow lines; too tight and it does nothing for the FP-noise case.
That is a real design decision, not a mechanical swap — do not port the
`SampleCache` identity-keying approach here verbatim without addressing
this.

## Recommended direction

Mirror `SampleCache`'s actual insight rather than its specific mechanism:
**don't try to decide whether retracing is necessary — assume it is (same
as `SampleCache` did), and remove only the allocation.** Concretely:

- Key `RegionGeometryCache` entries by the region's stable identity
  (`PlaneId`/`BoxId`/`SphereId`, already available via `RegionId` —
  `apps/fieldcad-desktop/src/scene/mod.rs`) rather than relying solely on
  `RegionGeometryInputs` equality to decide identity vs. staleness, the
  same identity/staleness split `SampleCache` now uses.
- On a stale-but-same-identity hit, still call `scene::region_geometry`
  (retracing is not skipped — this preserves current behavior exactly for
  a genuinely moving attachment), but write the result into the existing
  `Arc<FieldGeometry>`'s buffers in place (`Arc::get_mut`, refilling
  `surface_triangles`/`vector_lines`/`flow_ribbons` `Vec`s via `clear()` +
  `extend()` rather than allocating fresh ones) when the cache is the sole
  owner and the shapes match, falling back to a fresh allocation
  otherwise — exactly `SampleCache::get_or_try_insert_with`'s shape.
- This sidesteps the tolerance question entirely: it costs the same CPU
  time as today (full retrace, every tick, for an attached region) but
  removes the allocate-then-drop churn, which is what actually shows up as
  the memory sawtooth and the `memmove`/allocator overhead. A genuinely
  cheaper fix — skipping the retrace itself for the FP-noise case — is a
  separate, harder problem worth its own task if this doesn't fully
  resolve the residual pattern.

## Tests and acceptance

- A `RegionGeometryCache` test (mirroring `SampleCache`'s
  `a_stale_hit_with_a_different_resolved_value_still_reuses_the_buffer`,
  `apps/fieldcad-desktop/src/app.rs`'s `mod field_layer_geometry_cache`):
  two calls for the same `RegionId` with different resolved `batch`
  geometry (simulating a moved attachment) must retrace (content differs,
  correctness preserved) but reuse the same underlying allocation
  (`Arc::as_ptr` unchanged) rather than reallocating.
- Live re-check against `earth-moon-2.fcscene` with flow lines visible,
  animation off (isolating this cache from the already-fixed frame-merge
  issue): the residual step-pattern in the Diagnostics memory plot should
  shrink or disappear.
- No visual/behavioral change: flow lines for a moving attachment must
  still look correct (no staleness), since retracing itself is unchanged.

## Relevant code

- `apps/fieldcad-desktop/src/app.rs:238` — `RegionGeometryCache`.
- `apps/fieldcad-desktop/src/app.rs:258` — `RegionGeometryInputs`, its
  `PartialEq` derive, and the `batch` field that carries the
  attachment-defeated `SampleGeometry`.
- `apps/fieldcad-desktop/src/app.rs:345` — `compute_field_layer_geometry`,
  the per-region hit/miss decision at line 387.
- `apps/fieldcad-desktop/src/scene/field.rs` — `region_geometry`, called
  on every miss.
- `apps/fieldcad-desktop/src/scene/flow_lines.rs` — the actual RK4
  tracing and `Vec<FlowRibbonVertex>` allocation this task wants reused in
  place rather than reallocated.
- `crates/fieldcad-plugin-api/src/lib.rs` — `SampleCache`, the mechanism
  to mirror (identity-keyed entries, stale-refill-in-place via
  `Arc::get_mut`, fallback to reallocation when shapes differ or the
  buffer is still shared).

Found live, driving `apps/fieldcad-desktop` with a user to profile a
session after landing the `SampleCache` fix and the frame-merge fix
(`compute_field_layer_geometry` no longer rebuilding `FieldGeometry`
every redraw) — both already addressed a larger share of the observed
sawtooth; this is the smaller, still-real remainder. Not urgent.
