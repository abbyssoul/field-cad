# Architecture & Algorithm Performance Review

Date: 2026-08-10
Scope: Full workspace, focused on algorithmic complexity and architectural
allocation patterns rather than line-level micro-allocations (those are
already catalogued in the two 2026-08-08 reports).
Method: Static review, cross-checked against `git log` since the last audit
(five commits: dynamics integrator selection, the `n*m` lookup fix,
attachable sensors, and scene saving/loading) to separate what has already
been fixed from what is newly introduced or still open.

This report does not repeat findings from
`2026-08-08-performance-memory-audit.md` or
`2026-08-08-allocation-bottleneck-review.md` except to record their current
status. Its job is (1) verify those findings against current code, since two
days of substantial refactoring have passed, and (2) look at the codebase
the way those reports didn't: for algorithmic complexity that no amount of
buffer reuse fixes, and for architecture newly added since the last pass.

---

## 1. What the last audit flagged that is now fixed

Worth recording so the priority matrix isn't acted on against stale line
numbers.

| Prior finding | Status | Evidence |
|---|---|---|
| P0: Yee E/B grid `.clone()` per particle-coupled tick (`electromagnetism/lib.rs:1394`) | Open — see §3.1 below, same root cause, no change found in this area's diff | still a `.clone()` in `advance_particles` call site |
| P0: `YeeFieldView` centred-field full-grid allocation per sample (`lib.rs:1054`) | **Fixed** | `sample_yee_fields` now allocates `Vec::with_capacity(geometry.len())` and interpolates per requested sample position — the "on-demand, sized to sample geometry" fix the report recommended, not a full-grid precompute |
| P0: electrostatic GPU buffers re-created per `evaluate_batch` dispatch | **Fixed** | `electrostatics_gpu.rs:60` doc comment: "Buffers reused across `evaluate` calls instead of being created fresh"; has its own regression test `evaluate_reuses_buffers_across_growing_and_shrinking_calls` |
| P0/P1 (disputed between the two reports): `WorldState`/`WorldSnapshot` deep clone on every edit | **Fixed, architecturally** | `WorldState`'s seven collections (`objects`, `planes`, `boxes`, `spheres`, `probes`, `distance_probes`, `component_schemas`) are now each `Arc<BTreeMap<..>>` individually, and edits go through `Arc::make_mut` per touched map (`world.rs:1611-1641`). A commit that only touches `probes` now clones one `BTreeMap` node path, not the whole world. This is the single largest architectural improvement since the last audit — worth confirming it was intentional and not incidental, because it's exactly the "structural sharing" fix the audit only gestured at (`im`-crate-style persistent maps) |
| P1: O(N²) particle↔source lookup in EM source update (`lib.rs:813`) | **Fixed** | `fieldcad_core::index_by_object(coupling.particles())` builds a `HashMap` once per call site |
| P1/P3: O(n·m) source lookup in `electrostatics`/`gravity` `forces()` | **Partially fixed — see §2, this is the main finding of this report** | New `fieldcad_core::ObjectIndex` (in a new file, `object_index.rs`) gives O(1) lookup of *a body's own* source. The pairwise force summation itself is unchanged and still O(N·M) — see below |

The commit titled "solvers optimization to avoid n*m lookup" (9425a97) did
real, well-tested work — `ObjectIndex`/`index_by_object` in
`crates/fieldcad-core/src/object_index.rs` is a clean, shared abstraction
used consistently across `electrostatics`, `gravity`, and the EM particle
coupling — but the name overstates what it fixed. That's the subject of §2.

---

## 2. The `n*m` fix removed a lookup, not the O(N·M) force sum (Architectural)

**Files**: `plugins/electrostatics/src/lib.rs:310-334`,
`plugins/gravity/src/lib.rs:141-165`

Before the fix, `forces()` did two O(M) scans per body: `.find()` for the
body's own source (to get its charge/mass) and a `.filter()` to exclude that
source from the field/acceleration sum. `ObjectIndex::get` turned the first
scan into a hash lookup. But the second part — summing the field or
acceleration contribution from every *other* source — is still

```rust
let acceleration = evaluate_acceleration_excluding(
    self.sources.iter_excluding(body.object),   // O(M) per body
    body.position,
)
```

called once per body, so `forces()` for N dynamic bodies against M charge/mass
sources is still exactly O(N·M): `ObjectIndex::iter_excluding` (in the new
`object_index.rs`) walks and filters the full backing `Vec` on every call —
by construction, since excluding one item from a linear scan is itself O(M).

This is not a bug. Direct pairwise summation is the correct, simplest
algorithm for small-to-moderate source counts, and rewriting it changes
numerical behavior (ordering, rounding) as well as code shape, so it should
not be done reflexively. But it means:

- The commit's stated goal ("avoid n*m lookup") was achieved for the *self*
  lookup only. The dominant cost — the force sum — is unchanged in big-O
  terms. Framing this as solved would be a mistake if body/source counts grow.
- **This is the one place in the codebase where the ceiling is algorithmic,
  not allocation-shaped.** Every other finding across all three reports is
  "the same O(N) or O(N·M) work, done with fewer allocations." This one is
  "the same physics, done with fewer operations" — Barnes–Hut or a fast
  multipole method turns O(N·M) into O(N log M) or O(N+M), but only pays off
  once M is large (tens of thousands of sources); below that, the constant
  factors of a tree/multipole approach lose to direct summation.
- **Recommendation**: not a change to make now. Record the O(N·M) ceiling
  explicitly (e.g., a doc comment on `forces()` and/or a benchmark in
  `fieldcad-bench` sweeping source count) so that if a future scene wants
  hundreds of charges/masses, this is the first place profiled rather than
  rediscovered. `fieldcad-bench`'s existing `solver-init-by-charges` sweep
  (per the 2026-08-03 report) already measures per-source cost at init; an
  equivalent `forces`-per-tick sweep would show the same O(N·M) slope
  directly and turn this into a tracked number instead of a static claim.

---

## 3. New algorithmic/architectural findings since 2026-08-08

### 3.1 Scene save blocks the UI thread with synchronous, fsync'd disk I/O

**Files**: `apps/fieldcad-desktop/src/app.rs:1889` (`AppAction::SaveScene =>
self.save_scene(...)`), `app.rs:2010-2042` (`save_scene`),
`crates/fieldcad-scene-document/src/lib.rs:378-391` (`save_to_path`)

**Severity: P1 (High) — new in this review; scene save/load didn't exist at
the last audit.**

`save_scene` runs inline inside the egui action-dispatch path
(`apply_app_action`), not behind a `tokio::spawn` the way every mutating
world edit elsewhere in `app.rs` is (compare `AppAction::SaveScene` at
`app.rs:1889` to the four `tokio::spawn(async move { ... commit ... })` call
sites for ordinary edits). `save_to_path` then does, synchronously, on that
same thread:

```rust
let bytes = serde_json::to_vec_pretty(document)?;   // full-document JSON encode
let mut file = fs::File::create(&tmp)?;
io::Write::write_all(&mut file, &bytes)?;
file.sync_all()?;                                    // fsync — can block tens of ms
if try_load(path).is_some() {
    fs::copy(path, backup_path(path))?;              // read+decode+re-encode the *old* file, then a full copy
}
fs::rename(&tmp, path)?;
```

Every step here is a blocking syscall or a full-document JSON pass, all on
the thread that also has to keep producing egui frames. `sync_all()` in
particular forces the OS to flush to physical storage before returning —
tens of milliseconds is normal, hundreds is possible under load or on
spinning/network storage. Between `SaveScene` firing and `save_to_path`
returning, the render loop cannot advance a frame, so a save on a large
scene (or a slow disk) is a visible freeze, not just a slow status message.

Compounding this: `try_load(path)` — called specifically to decide whether a
`.bak` is warranted — fully re-reads and re-deserializes the *previous*
on-disk document, on the same blocking call, immediately before the
`fs::copy`. That's a second full JSON decode of a document the caller is
about to overwrite anyway, purely to answer "was it valid," which could be
answered far more cheaply (e.g., a stored flag from the last successful
save) without changing the durable-write protocol's guarantees.

**Recommendation**: Move `save_scene`'s document capture (needs the model
lock, must stay on the calling task) apart from `save_to_path`'s I/O (does
not need the model lock, and the model is not touched again until the save
completes). Run `save_to_path` on a blocking-safe context (`tokio::task::
spawn_blocking`, matching the async pattern already used for edits) so the
render loop keeps producing frames during the fsync. This is a small,
mechanical change since `SceneDocument` is already fully owned/`Clone` by
the time `save_to_path` is called — no lifetime threading needed.

### 3.2 `last_forces` still rebuilt as a fresh `BTreeMap` every tick

**File**: `crates/fieldcad-simulation/src/runtime.rs:1683-1687`

Already flagged in the 2026-08-08 allocation review (Tier 5.1) at different
line numbers; still present verbatim after the Velocity Verlet work landed
(`self.last_forces = bodies.iter().zip(&new_forces).map(...).collect()`).
Noted here only because Velocity Verlet now *reads* `last_forces` every tick
too (`runtime.rs:1671-1676`, to seed the half-kick), which raises this from
"an unread-per-tick rebuild" to "a rebuild immediately followed by a
per-body read of the map that's about to be discarded" — the same
`.clear()` + `.extend()` fix already recommended applies, and is now
marginally higher-value than when it was first flagged. Not re-priced as a
new finding; still P2.

### 3.3 New per-frame UI code follows the existing bounded-history pattern — no new concern

**Files**: `apps/fieldcad-desktop/src/ui/plot.rs`,
`apps/fieldcad-desktop/src/ui/panels/distance_probe_inspector.rs`

The new distance-probe plotting collects `history.readings(...).copied()
.collect()` into a fresh `Vec` every frame. This is the same shape as the
Tier 3 findings in the 2026-08-08 allocation review (per-frame `Vec`
allocation from bounded state) — `BodyHistory`/`ProbeHistory` are capacity-
bounded (`DEFAULT_BODY_HISTORY`), so this is a small, fixed-size allocation
at UI frame rate, not a growth risk. Consistent with the existing pattern,
not called out as a new separate finding; would be swept up by the same
"persistent scratch buffer in the UI model" fix already on the list if that
work is ever done.

### 3.4 `ObjectIndex` construction cost is correctly amortized

Checked because it's new shared infrastructure now on the hot path of three
plugins: `ObjectIndex::new` (O(M) hash-map build) runs only inside each
solver's `on_world_changed`, not inside `forces()`/`sample()`. Confirmed for
`electrostatics` (`lib.rs:239`) and `gravity` (equivalent `on_world_changed`
override) — the index is rebuilt once per world edit, not once per tick.
This is the right place for it; flagging only to close the loop, since a
misplaced rebuild here would have silently reintroduced O(N·M) *per tick*
on top of the O(N·M) that's already inherent to the force sum (§2).

---

## Priority Summary

| Priority | Finding | File:Line | Status |
|---|---|---|---|
| P1 | Scene save does blocking, fsync'd I/O on the UI thread | `app.rs:2010`, `scene-document/lib.rs:378` | **New** |
| — | O(N·M) direct force summation is the algorithmic ceiling for electrostatics/gravity, not fixed by the `n*m` commit | `electrostatics/lib.rs:362`, `gravity/lib.rs:154` | **Clarified** — correct as-is at current scale, record the ceiling rather than chase it now |
| P2 | `last_forces` `BTreeMap` rebuilt every tick, now also read every tick under Velocity Verlet | `runtime.rs:1683` | Carried forward from 2026-08-08, slightly higher value now |
| P0 (open) | Yee E/B grid `.clone()` per particle-coupled tick | `electromagnetism/lib.rs:1394` | Carried forward, unchanged — see the 2026-08-08 reports for the `Cow<[DVec3]>` fix already proposed |

## What's confirmed fixed (no action needed)

- Full-grid `YeeFieldView` centred-field allocation per sample → now sized to sample geometry
- Electrostatic GPU buffer re-creation per dispatch → buffers persist and are reused, with a regression test
- `WorldState` deep clone per edit → per-collection `Arc<BTreeMap>` + `Arc::make_mut`, genuinely O(edited collection) now
- O(N²) EM particle↔source lookup → `HashMap`-backed index built once per tick
