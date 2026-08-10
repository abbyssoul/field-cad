# Task: `SampleCache` buffer reuse can't help a sample plane attached to a moving object

## Goal

Extend (or replace) `SampleCache`'s geometry-value keying so a plane/box/
sphere attached to a moving or per-tick-recomputed object still gets the
buffer-reuse benefit `sample-cache-still-allocates-every-tick` added for
geometry with a stable world-space frame.

## Current limitation

`sample-cache-still-allocates-every-tick` (deleted as part of landing —
see the commit that removed it) made `SampleCache<T>::clear()` mark
entries stale instead of dropping them, so a repeated request for the
*same* `SampleGeometry` value refills the existing `Arc<[T]>` in place via
`Arc::get_mut` instead of reallocating (`crates/fieldcad-plugin-api/src/lib.rs`,
`SampleCache::get_or_try_insert_with`). That depends entirely on the
`SampleGeometry` value itself staying `==`-equal tick to tick — true for a
plane/box/sphere anchored to the domain, but **not** true for one attached
to an object, because `resolve_plane_frame`/the equivalent box/sphere
resolvers (`crates/fieldcad-simulation/src/runtime.rs::geometries`) bake
the object's *current resolved* origin/normal/axes into the `SampleGeometry`
value itself. A `Plane { lattice, .. }` attached to a moving object
produces a structurally different value every tick, which is a genuine
cache miss under `SampleCache`'s equality-keyed design — no amount of
in-place-refill machinery can help an entry whose key never recurs.

Measured directly against `earth-moon-2.fcscene` (2 gravitating bodies,
`VelocityVerlet`) while verifying the buffer-reuse fix: this scene's two
visible planes are both attached — one directly to the orbiting Moon, one
to a `"derived": true` "Center of mass" marker object that is recomputed
from Earth/Moon's live positions every tick even though it carries no
mass/dynamics components of its own (`pinned: true` only stops *dynamics*
from moving it, not the per-tick "derived" recomputation). Instrumenting
`SampleCache::get_or_try_insert_with` confirmed the actual per-tick
outcome for gravity's three sampled geometries in this scene: the
domain-anchored `Grid` (from `Subscription::with_domain_stride`) hits the
in-place refill path every tick as designed; **both** planes hit the
allocating cold path (`compute()`) every tick, because their resolved
`SampleGeometry` value is never equal to the previous tick's.

Net effect on this specific, real, saved scene: allocations/tick dropped
only from 182.009 to 174.135 (a real, reproducible ~4.3% reduction,
confirmed deterministic across repeated runs) — most of the 182 baseline
survives, because 2 of the 3 geometries gravity samples here can't be
helped by geometry-value keying at all. `valgrind --tool=callgrind`
confirms the same modest shift: `memmove` drops from 16.95% to 15.89% of
instruction count, `evaluate_sources`/`contribution`'s own share rises
correspondingly (57.70% → 59.10%). `fieldcad-bench`'s `gravity/sample-plane`,
`gravity/sample-by-charges`, `electrostatics/sample-plane`,
`electrostatics/sample-by-charges` show no regression (all deltas within
noise via `--baseline`/`--fail-on-regression`), consistent with those
benchmarks never clearing the cache mid-rep in the first place.

This isn't a bug in the buffer-reuse fix — it does exactly what it was
designed to do, and the unit tests added for it
(`a_stale_hit_refreshes_the_same_buffer_instead_of_reallocating`) prove
the mechanism. It's a scope limit worth its own task: **attaching a
visualization plane to a moving body is an ordinary, expected use case**
(orbital mechanics scenes plausibly want a field slice that tracks a body),
and today it silently defeats every geometry-keyed cache in the sampling
path, gravity's included.

## Required behavior

- A plane/box/sphere attached to a moving object should still get some
  form of buffer reuse across ticks, without giving up correctness (a
  moved attachment must still produce a correctly-updated sample).
- Whatever new keying scheme is used must not regress the
  already-working case (domain-anchored geometry, unattached probes) that
  `sample-cache-still-allocates-every-tick` fixed.
- Preserve the existing channel-sharing property within one publish (two
  channels sampling the same geometry share one evaluation).

## Possible directions (not decided — investigate before implementing)

- Key `SampleCache` by *identity* (plane/box/sphere/probe-set ID) instead
  of by-value equality on the fully-resolved `SampleGeometry`, and detect
  "did the resolved frame actually change" as a separate, explicit check
  inside the refill path rather than via the entry lookup itself. This
  would let an attached-but-momentarily-unmoving frame still hit the
  cheap path, and would always take the in-place-refill path (never the
  allocating one) for a *previously seen* ID, at the cost of an equality
  check on the frame moving from "is this the same cache entry" to "does
  this cache entry's stored value need recomputing" — which is close to
  free once the entry is already found by ID.
- Investigate whether `resolve_plane_frame`/box/sphere equivalents can
  report "this object is dynamics-driven and moved since last resolve" as
  a cheap boolean (probably already implied by whatever revision/dirty
  tracking `WorldSnapshot`/`WorldRevision` already does) instead of
  requiring a full `SampleGeometry` value comparison to discover it.

## Tests and acceptance

- A synthetic scene with a plane attached to a body under active
  integration, run through `profile_scene.rs`, should show allocations/tick
  drop materially below whatever this task's baseline is measured at
  (record a fresh baseline against `earth-moon-2.fcscene` first, since
  `sample-cache-still-allocates-every-tick` already moved it from 182.009
  to 174.135).
- No change in sampled values at a given world state (bit-identical to
  today).

## Relevant code

- `crates/fieldcad-plugin-api/src/lib.rs` — `SampleCache`, in particular
  the `entries.iter_mut().find(|entry| &entry.geometry == geometry)`
  lookup this task would change the keying of.
- `crates/fieldcad-simulation/src/runtime.rs::geometries` — where an
  attached plane/box/sphere's frame is resolved into a `SampleGeometry`
  value fresh each publish; the `resolve_plane_frame`/equivalent calls are
  the place a "did this move" signal would come from.
- `plugins/gravity/src/lib.rs`, `plugins/electrostatics/src/lib.rs` —
  `samples_for`, unaffected in shape by this task but the callers of
  whatever `SampleCache` API changes.

Found while measuring `sample-cache-still-allocates-every-tick` against a
real saved scene rather than a synthetic one — the synthetic
`fieldcad-bench` sweeps don't have attached-geometry scenes and so never
surfaced this. Not urgent (the base fix still improves things, and doesn't
regress anything), but the gap is real and specific enough to be worth
recording rather than losing.
