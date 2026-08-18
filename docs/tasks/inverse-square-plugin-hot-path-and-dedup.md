# Task: Inverse-square pair — hot-path cleanup, force batching, and plugin dedup

## Goal

Cut the per-tick and per-publication cost of the two inverse-square plugins
(gravity, electrostatics) by (a) removing dead work from the shared
`fieldcad-superposition` kernel's hottest loops, (b) making `add_forces`
consume the solver's already-precomputed source buffer instead of
re-deriving it per body per tick, and (c) collapsing the ~150 × 2 lines of
structurally identical solver code into one shared skeleton so
correctness-relevant detail (the batch-length check gravity is missing
today) can never diverge between the two plugins again. Nothing here
changes asymptotics — `evaluate_sources` stays O(positions × sources),
`add_forces` stays O(bodies × sources) — these are constant-factor cuts
(~2× on the force path combined) plus one additive parallelism seam.

Evidence base: `docs/perf/2026-08-17-inverse-square-plugin-audit.md` (rev 2,
revalidated against current code). No benchmarks have been run yet; Phase 0
exists to make every later claim measurable.

## Current limitation

- **Force path (`add_forces`, once per tick per plugin):** the kernel's
  `contribution` always computes a `DMat3` Jacobian that `field_excluding`
  discards (~half the FLOPs); `field_excluding` does not skip zero-strength
  sources at all; and each plugin re-maps `CoupledSource`/`ChargeSource` →
  `InverseSquareSource` per source per body despite a precomputed
  `inverse_square_sources` buffer, paying two `HashMap` lookups per body to
  do it (gravity lib.rs:249-262, electrostatics lib.rs:259-273).
- **Sample path (per publication):** 13 scalar finiteness comparisons per
  (sample × source) inside the loop (kernel lib.rs:152) where one
  end-of-sample check is result-equivalent; byte-identical exterior
  formulas duplicated across the `Point` and exterior-sphere arms;
  per-channel `Vec` allocations on every `sample()` call even on
  `SampleCache` hits.
- **Structure:** the two plugins are near-clones; gravity's `samples_for`
  lacks electrostatics' batch-length verification, so a misbehaving
  injected evaluator surfaces as a generic downstream
  `FieldBatch::new` `LengthMismatch` (sampling.rs:547) instead of a
  solver-contextual error.
- **Measurement:** `fieldcad-bench` has `gravity/forces` but no
  `electrostatics/forces` workload, so kernel force-path changes go
  unmeasured for electrostatics.

## Required behaviour

Phases in order; each lands independently green. Per the AGENTS.md
allocation bar, every phase that claims a performance change cites
`fieldcad-bench` numbers from Phase 0's baselines.

### Phase 0 — Baseline and bench gap

- Save `fieldcad-bench` baselines (`--save-baseline`) covering at least
  `gravity/*` and `electrostatics/*`.
- Add the missing `electrostatics/forces` workload mirroring
  `gravity/forces` (`crates/fieldcad-bench/src/workload.rs:437`), so both
  halves of the shared force path are measured.

### Phase 1 — Kernel hot path, no API change

In `crates/fieldcad-superposition/src/lib.rs`:

- Split `contribution` (lib.rs:81-125) into a field-only variant (used by
  `field_excluding`) and a full field+potential+Jacobian variant (used by
  `evaluate_sources`). The field-only variant builds no `DMat3`.
- Hoist the finiteness check (lib.rs:152) to one end-of-sample check.
  Containment (`InsideSourceRadius`) stays in-loop and early-returns.
  Accepted semantics nuance: for a sample where one source overflows *and*
  a later source contains the position, the recorded `UndefinedReason`
  flips from `NumericalOverflow` to `InsideSourceRadius` — both remain
  `Undefined`; document and test this ordering explicitly.
- Fuse `coupling_constant * strength` once per contribution (likely CSE'd
  today; cleanup, not a claimed win).
- Collapse the duplicated exterior formulas in the `Point` and
  exterior-sphere arms into one shared body behind the radius guard.

### Phase 2 — Plugin force path

- Add `ObjectIndex::index_of(&self, id: ObjectId) -> Option<usize>`
  (`crates/fieldcad-core/src/object_index.rs`), mirroring the order
  guarantees its `iter_excluding` tests already pin (lib.rs:121-128).
- In both plugins' `add_forces`, exclude by index over the precomputed
  `inverse_square_sources` slice (`[..excluded]` chained with
  `[excluded+1..]`) — one lookup, zero per-tick conversions — instead of
  `iter_excluding(object).map(inverse_square_source)` plus a separate
  `get()` for the coupling value.
- Filter zero-strength sources when `inverse_square_sources` is rebuilt on
  world change (both `create_solver` and `on_world_changed`). Filtering is
  observable only through strength-zero sources, whose field/potential
  contributions are exactly zero. Keep the filtered `Vec` and the
  `ObjectIndex` positionally consistent (filter the `ObjectIndex`'s items
  the same way, or resolve the excluded index against the unfiltered set)
  — the index-based exclusion of this phase depends on it.

### Phase 3 — Batch force API

- Add `field_excluding_into` to the kernel: caller-owned `&mut [DVec3]`
  output, per-body excluded index (or `None`), no per-tick allocation —
  the AGENTS.md no-alloc hot-path shape.
- Both plugins' `add_forces` move onto it. This becomes the seam a future
  SIMD/rayon/GPU force path plugs into (one dispatch per tick rather than
  one iterator chain per body).

### Phase 4 — Plugin dedup

- Extract the shared solver skeleton into `fieldcad-superposition` (it
  already owns the evaluator trait; no new crate): generic over coupling
  constant, channel-handle pair, channel schemas, source collector, and
  the coupling-value accessor for `add_forces`. Both plugins shrink to
  ~50-line adapters.
- The skeleton carries, once: the `samples_for` batch-length check
  (closing gravity's gap), the `SampleCache` wiring, the precision gate,
  and the `CountingEvaluator` contract tests (cache-sharing across
  channels, evaluator-agnostic schemas) currently only in
  electrostatics' test module.

### Phase 5 — Parallel CPU evaluator

- `ParallelCpuInverseSquareEvaluator` (`Precision::F64`, rayon) in the
  kernel, injected via the existing `with_evaluator` seam — no plugin
  changes. Bit-identical to the sequential oracle: per-sample source
  summation stays sequential; only samples parallelize.
- `rayon` must be added to the root `[workspace.dependencies]` and pinned
  once (it is currently absent from the workspace).

### Phase 6 — Sample-path allocations

- Share validity/gradient/column derivation across the two channels of one
  geometry (memoized alongside `SampleCache` or derived once per
  `samples_for` result), eliminating the duplicate per-channel collection.
- Only proceed if Phase 0's numbers show per-tick allocation in `sample`
  is worth it per the AGENTS.md allocation bar.

## Tests and acceptance

- **Phase 0:** baselines exist; `electrostatics/forces` runs alongside
  `gravity/forces`.
- **Phase 1:** existing kernel tests stay green, including the
  central-difference Jacobian oracle and the undefined-sample
  gradient-placeholder regression. Add: a parity test that
  `field_excluding` equals `evaluate_sources(...).field` for
  well-defined samples; a test pinning the new overflow-vs-containment
  `UndefinedReason` ordering; a zero-strength-source equivalence test.
  `gravity/forces` and `electrostatics/forces` medians drop at fixed
  scene size.
- **Phase 2:** the PH-2/PH-3 regression tests (grazing exclusion radius,
  interior sphere) in both plugins stay green — they are exactly the
  behaviors index-based exclusion must not disturb. Add: old-vs-new
  `add_forces` parity over a randomized multi-source scene (same forces,
  bit-for-bit for unchanged summation order).
- **Phase 3:** both plugins' force tests green through the new API;
  forces identical to Phase 2 output at the same scene.
- **Phase 4:** gravity's `samples_for` rejects a misbehaving evaluator
  with the solver-contextual error (port electrostatics'
  `CountingEvaluator` length-mismatch test to the skeleton; both plugins
  inherit it). All existing plugin tests green unchanged.
- **Phase 5:** parallel evaluator output bit-identical to
  `CpuInverseSquareEvaluator` across the bench scenes; measurable wall
  -clock win on `sample-plane`/`sample-by-charges` at large sample counts.
- **Phases 1–5:** no change to published values, validity, gradient
  columns, or diagnostics text for any existing test or scene — cost
  changes only, except the one documented Phase 1 reason-ordering nuance.

## Relevant code

- `crates/fieldcad-superposition/src/lib.rs:133` — `contribution` (full
  variant); `lib.rs:96` — `field_contribution` (field-only, force path);
  `lib.rs:78-92` — `exterior_field`/`exterior_potential` shared helpers;
  `lib.rs:184` — `evaluate_sources` (end-of-sample finiteness check);
  `lib.rs:241` — `field_excluding` (zero-strength skip);
  `lib.rs:258` — `field_excluding_at` (slice-based exclusion by index);
  `lib.rs:286-335` — `AddForcesError` and `add_forces_excluding_into`
  (the one-call-per-tick force entry point); `lib.rs:385` —
  `CpuInverseSquareEvaluator` (Phase 5 sibling).
- `plugins/gravity/src/lib.rs:147` — `GravityCoupling`; `lib.rs:133` —
  `create_solver` (one call into the skeleton); `lib.rs:71` —
  `inverse_square_source` / `evaluate_sources` oracle free fns. Since
  Phase 4 the plugin holds no solver code.
- `plugins/electrostatics/src/lib.rs:145` — `ElectrostaticsCoupling`;
  the same adapter shape plus the charge schema re-exports.
- `crates/fieldcad-superposition-solver/src/lib.rs:52` —
  `InverseSquareCoupling` trait; `lib.rs:97` — `InverseSquareSolver`
  (the shared skeleton: filtering invariant, precision gate,
  batch-length check, cache, force delegation); `lib.rs:36` — the
  channel handles both plugins alias.
- `crates/fieldcad-core/src/object_index.rs:54` — `index_of`, beside the
  `iter_excluding` order guarantee it pairs with.
- `crates/fieldcad-plugin-api/src/lib.rs:445` — `SampleCache` (identity
  keying, stale refill-in-place; unchanged by this task, context for
  Phase 6).
- `crates/fieldcad-bench/src/workload.rs:423` — `electrostatics_forces`
  (added in Phase 0); `lib.rs:464` — `gravity_forces`, its template.
- `docs/perf/2026-08-17-inverse-square-plugin-audit.md` — the revalidated
  findings this plan executes.

## Progress log

**2026-08-17 — Phases 0 and 1 landed** (working tree, uncommitted):

- Phase 0: added the `electrostatics/forces` bench workload; baselines
  saved under `target/bench-baselines/` (machine-local, not committed).
- Phase 1: kernel split (`field_contribution` vs `contribution`),
  `exterior_field`/`exterior_potential` helpers, fused `k·strength`,
  end-of-sample finiteness check, zero-strength skip in
  `field_excluding`, six new kernel tests. All expressions keep the
  original left-associative operation order, so well-defined results are
  bit-identical; the two behavior deltas are exactly the documented pair
  (containment-vs-overflow reason ordering; coincident zero-strength
  source no longer NaNs the force sum).
- Measured vs pre-Phase-1 baselines (per-scene medians):
  `gravity/forces` −41…−78%, `electrostatics/forces` −44…−82%,
  `electrostatics/sample-by-charges` −33…−49%, `electrostatics/
  sample-plane` −25…−35% (290–16.6k samples). The 66k-sample plane scene
  is bandwidth-bound with 40–70% run-to-run noise: parity on re-run, no
  regression signal. `gravity/solver-init` unchanged within noise, as
  expected for an untouched path.
- Verification: `cargo test` green for `fieldcad-superposition`,
  `fieldcad-gravity`, `fieldcad-electrostatics`, `fieldcad-bench`
  (57 tests, 9 suites); `cargo clippy` and `cargo fmt --check` clean for
  the same crates. Follow-up: the WIP HEAD commit had commented out
  `Transform::at` (in favor of `Transform::at_finite`,
  `crates/fieldcad-core/src/world.rs:55`) leaving ~30 unmigrated call
  sites in `fieldcad-server` tests, `fieldcad-desktop`, and
  `fieldcad-bench`; this task migrated `fieldcad-bench`'s four sites
  (required to run the baselines), and the remaining sites were migrated
  separately. After that migration, the full workspace passes:
  `cargo test --workspace` 834 passed (48 suites), `cargo clippy
  --workspace --all-targets` and `cargo fmt --all --check` clean.

**2026-08-17 — Phase 2 landed** (working tree, uncommitted):

- `ObjectIndex::index_of` added (with the
  excluding-by-index ≡ `iter_excluding` contract test);
  `fieldcad_superposition::field_excluding_at` added — slice-based
  exclusion by index, parity-tested against `field_excluding` for every
  exclusion position.
- Both plugins: zero-coupling sources filtered at collection (the
  filtered `ObjectIndex` and `inverse_square_sources` share one index
  space by construction — the documented invariant); `add_forces` uses
  one `index_of` lookup per body, takes the coupling value from
  `inverse_square_sources[excluded].strength` (bit-identical to
  `coupling_value.into_si()` by construction, avoiding the second source
  array), and evaluates via `field_excluding_at`. Diagnostics now count
  contributing sources only.
- New tests per plugin: bit-for-bit `add_forces` vs manual superposition
  parity (exterior + sphere interior), and zero-strength objects
  neither exerting nor feeling forces (via
  `independent_gravitational_mass_properties(0)` / `charge_properties(0)`,
  both valid properties). PH-2/PH-3 regressions green; full workspace
  841 passed, clippy/fmt clean.
- **Measured detour worth recording:** the first cut excluded by
  `sources[..i].iter().chain(sources[i+1..].iter()).copied()` — clean,
  but reproducibly **10–18% slower** than HEAD's filter-map at 2–8
  sources (sub-1% noise, both plugins). Disassembly showed the `Chain`
  instantiation pays two loop-exhaustion tests per loop head plus
  per-iteration register shuffles. Replaced with the kernel's
  `field_excluding_at` (one plain enumerate-and-skip loop over the
  slice) — this is effectively the core of Phase 3's
  `field_excluding_into` pulled forward, since a batch wrapper over
  bodies now reduces to calling it per body.
- Measured vs HEAD (paired quiet-window runs, load < 1, per-unit medians,
  both plugins agreeing within 0.5%): `gravity/forces` and
  `electrostatics/forces` −4% to −13% across 1–32 sources. Sampling
  paths are code-identical to Phase 1 and were not re-benched; the
  earlier pre-Phase-2 baseline comparison under load-14 conditions was
  discarded as noise (even untouched paths showed ±30%).

**2026-08-18 — Phase 3 landed, re-scoped to its remainder** (working
tree, uncommitted):

- Phase 2's `field_excluding_at` had already absorbed the original
  Phase 3 motivation (per-body iterator chains, per-source re-mapping,
  per-tick allocation); what landed is the remainder: one public batch
  entry point so the force law (`F = field × own strength`, covering
  both `F = ma` and `F = qE`) lives in the kernel once, with its tests,
  and both plugins' `add_forces` become a single kernel call per tick.
- Kernel: `add_forces_excluding_into(coupling, sources, bodies, out)`
  where `bodies: impl Iterator<Item = (Option<usize>, DVec3)>` — per
  body its own source index (`None` = not a source: neither exerts nor
  feels) and position; accumulates into `out` (multi-system resultant
  contract); `AddForcesError::NonFinite { body }` names the first body
  whose summed field overflowed. The × own-strength product is
  unchecked, matching the per-body path — the dynamics runtime
  validates accumulated forces once per tick. Sources-outer SIMD
  variant explicitly deferred as a measured follow-up; rayon-over-bodies
  (Phase 5) needs no further API change.
- Tests: kernel batch-vs-per-body bit-for-bit parity (exterior, sphere
  interior, `None` bodies, accumulation onto a prior resultant);
  offending-body error index. Both plugins' existing parity, zero-
  strength, and PH-2/PH-3 tests pass unchanged through the new path.
- **Two measured detours worth recording.** (1) The first cut took
  per-body slices and the plugins materialized them through a
  `Mutex<ForceScratch>` (SampleCache precedent): reproducibly
  **+10–96% slower** (sub-2% noise, both plugins) — the uncontended
  lock plus scratch materialization dominates at small body counts.
  Replaced with the iterator shape: zero scratch, zero lock. (2) The
  iterator version still showed a reproducible +6–7% at 1–2 bodies
  (< 1 ns absolute, harness-insignificant): the generic batch fn was
  not inlining across the crate boundary. `#[inline]` on it resolved
  this to exact parity.
- Measured vs pre-Phase-3 HEAD (paired quiet-window runs, sub-2% noise):
  `gravity/forces` and `electrostatics/forces` **−1.0% to +1.1%** across
  1–32 sources, stable on repeat — parity, as an API-shaping phase
  should be. Full workspace 843 passed, clippy/fmt clean.

**2026-08-18 — Phase 4 landed** (working tree, uncommitted):

- New crate `crates/fieldcad-superposition-solver`: the shared
  `InverseSquareSolver<C>` skeleton (`EquationSystemSolver` impl,
  per-geometry `SampleCache`, zero-coupling filtering with the
  index-alignment invariant, precision gate, batch-length verification,
  force delegation to `add_forces_excluding_into`) parameterized by an
  `InverseSquareCoupling` trait (constant, labels, collection, mapping,
  strength). Also owns `FIELD_CHANNEL_HANDLE`/`POTENTIAL_CHANNEL_HANDLE`;
  both plugins' handle constants are re-export aliases of these, so
  advertised schemas and matched handles cannot drift. The crate keeps
  `fieldcad-superposition` dependency-light (its "no plugin" boundary
  statement stands); layering is core ← {plugin-api, superposition} ←
  superposition-solver ← plugins.
- Plugins are adapters: identity/metadata/schemas/constants, the public
  oracle free fns (`inverse_square_source`, `evaluate_sources` — desktop's
  GPU evaluator consumes these), a private coupling impl, and a one-call
  `create_solver`. Gravity 789→624 lines, electrostatics 993→746 (net
  −570 across the pair, new crate 591 including tests). Public plugin
  APIs unchanged — bench, desktop, server, mcp compile untouched.
- Contract tests carried once in the new crate (toy coupling +
  `CountingEvaluator`/`WrongLengthEvaluator` doubles): both-channels-
  share-one-evaluation, world-change cache invalidation, evaluator/domain
  precision rejection, **wrong-length batch rejection with solver
  context** (§3.4 closed for gravity), diagnostics identity. Pruned the
  now-duplicated plugin-side tests per review decision (gravity's
  precision-mismatch; electrostatics' cache-sharing and
  precision-mismatch; both replaced by the crate-level tests). All
  physics/regression tests (PH-2/PH-3, bit-parity, zero-strength,
  evaluator-contract, Jacobian channels) pass unchanged through the
  skeleton. Full workspace 845 passed (50 suites), clippy/fmt clean.
  Benching skipped per review decision — the delegation chain is
  provably identical to Phase 3's measured-parity path.

**2026-08-18 — Phase 4 follow-up: residual dedup audit** (working tree,
uncommitted; Phase 5 parked per review):

- A re-audit of both plugins found two residual implementation mirrors
  and one uncovered skeleton contract. Fixed: (1) the plugin struct +
  constructor trio + `create_solver` (identical except the type name)
  moved into the solver crate as a generic
  `InverseSquarePlugin<C: InverseSquareCoupling>`; the coupling trait
  grew `metadata()`/`channels()`/`component_schemas()` and dropped its
  `PLUGIN_ID` const (metadata is now the single identity source —
  diagnostics report `metadata().id`), so identity cannot drift between
  the two. Plugins are `pub type` aliases over their public couplings —
  `::new()`/`::with_evaluator`/trait-object usage unchanged at every
  call site. (2) The public `inverse_square_source` mapping (the one the
  desktop GPU evaluator consumes) is now one generic
  `coupled_inverse_square_source<T: SiScalar>` in the solver crate,
  re-exported under each plugin's existing name. (3) The four mirrored
  plugin gradient-channel tests (skeleton logic since Phase 4, which the
  skeleton's own `CountingEvaluator` tests — `gradient: None` — did not
  cover) were replaced by one skeleton test driving the real
  `CpuInverseSquareEvaluator`, asserting the field channel's per-sample
  Jacobian and the potential channel's exact `−field` gradient.
- Kept as accepted mirroring: per-plugin force-parity and zero-strength
  tests (world authoring, collection, and coupling mapping are the
  plugin-specific content), and `evaluate_sources`/channel constants
  (identity by design).
- End state: gravity 530 lines (145 non-test), electrostatics 648,
  solver crate 734 (incl. tests); net −473 lines vs pre-Phase-4 while
  adding coverage. Full workspace 842 passed (50 suites), clippy/fmt
  clean. Phase 5 (rayon evaluator) parked; Phase 6 remains
  measurement-gated on the sample path.

**2026-08-18 — Phase 6 landed** (working tree, uncommitted):

- Gate opened per Phase 6a's own criterion: the `sample_path` example
  (`crates/fieldcad-bench/examples/sample_path.rs`, added this phase)
  isolates the cache-hit steady-state read cost from evaluation. Pre-6b,
  each `sample()` call re-derived validity/gradient/values buffers by
  iterating the cached raw samples — an O(n) allocation pass per channel
  read, present even on a cache hit. That share was decisively over the
  ~10% bar (it *was* the entire steady-state cost), so 6b proceeded.
- `InverseSquareSolver`'s cache changed from `SampleCache<InverseSquareSample>`
  to `SampleCache<GeometrySamples>`: `GeometrySamples` bundles one
  evaluation's raw samples plus every derived column (validity, jacobians,
  field/potential values, negated-field gradient) as `Arc`s, computed once.
  `sample()` shrank to a lock, one cache fetch, and per-channel `Arc::clone`s
  via `SampledColumn::from_shared_parts` (new small constructor added to
  `fieldcad-plugin-api`, since `SampledColumn`'s fields are private).
- **Documented caveat**: `SampleCache<T>`'s in-place `refresh` path is
  gated on `entry.len() == geometry.len()`, a check written for its
  original per-sample-point `T`. With `T = GeometrySamples` (one bundle
  per geometry, so `entry.len()` is always 1), that guard only re-admits
  `refresh` when `geometry.len() == 1` (a single probe); every
  multi-sample geometry (planes, grids, boxes, spheres) takes the
  `compute` fallback on a stale hit and pays one fresh evaluation — the
  same cost the pre-Phase-6 cache always paid there. No correctness loss
  (the fallback is always valid), only a foregone secondary optimization
  the plan's design didn't fully account for. Recorded here rather than
  silently accepted, per the no-SampleCache-API-change decision in the
  locked plan.
- New test: `repeated_reads_of_one_geometry_share_the_same_validity_and_
  value_buffers`, pinning the memoization itself via `Arc::ptr_eq` — both
  channels of one geometry, and two reads of an unchanged world, hand out
  the identical buffer, not merely an equal one.
- Measured (release, `sample_path`, paired quiet-window runs): steady-state
  per-read cost is now **flat at ~58–62 ns regardless of geometry size**
  across 4,225/16,641/66,049 samples — down from an O(n) per-read
  derivation to the lock + `Arc::clone` floor the design targeted. Drag
  publication (evaluate + both channels after a world change) is
  unaffected, as expected: it was never on the memoized path, and the
  Phase-6 caveat above means multi-sample geometries still re-evaluate
  fully on every drag tick, same as before. `profile_scene`'s
  `CountingAlloc` cross-check was skipped — no `.fcscene` sample file
  exists in the repo to drive it; the direct steady-state measurement
  already demonstrates the O(1) read the phase set out to prove.
- Also fixed in passing: an unrelated pre-existing regression in the
  working tree — `fieldcad-sources::inertial_mass_of` had dropped its
  `mass > 0` validation when refactored to share `inertial_mass_property`
  with `fieldcad-dynamics` (which correctly wants the permissive
  finite-only filter, since it only sees already-validated masses).
  Restored the `> 0` check at `inertial_mass_of`'s call site, where the
  guarantee is actually enforced. Caught by the workspace test suite,
  unrelated to Phase 6 itself.
- Verification: `cargo test --workspace` 843 passed (50 suites, +1 for
  the new memoization test), `cargo clippy --workspace --all-targets` and
  `cargo fmt --all --check` clean. Phase 5 (rayon evaluator) remains
  parked.

**2026-08-18 — `perf`-driven follow-up on the Phase 6 sample path**
(post-closure, found profiling `profile_scene` against a real
hand-authored scene, `~/Documents/field-cad/earth-moon-titan.fcscene`,
under `perf record --call-graph=dwarf`, `kernel.perf_event_paranoid=1`):

- `SampleGeometry::positions()` (`crates/fieldcad-core/src/sampling.rs`)
  was `(0..len).filter_map(|i| self.position(i))`. `position(i)` is
  `None` only for `i >= len()`, which the range never produces — every
  variant's `position` impl confirms this, and every call site (verified
  by grep) zips the iterator 1:1 against a same-length buffer — so the
  `None` arm was unreachable by construction. `filter_map` can't be
  `ExactSizeIterator`, so every `.collect()` over it (the per-tick sample
  buffer in `CpuInverseSquareEvaluator::evaluate`) grew by repeated
  reallocation instead of one upfront allocation. Changed to
  `.map(...).expect(...)`; `Map` over `Range<usize>` is
  `ExactSizeIterator`, so `.collect()` now sizes the buffer once.
  Confirmed via `perf`: `__rust_realloc`/`CountingAlloc::realloc` no
  longer appear above the 0.5% sample threshold (previously 5.61% of
  cycles). Also fixed `profile_scene`'s `CountingAlloc`, which only
  overrode `alloc`/`dealloc` — without a `realloc` override,
  `GlobalAlloc`'s default falls back to alloc-copy-dealloc instead of
  letting the system allocator extend in place, inflating the profiler's
  own realloc cost above what an instrumented build would otherwise pay.
- `GeometrySamples::from_samples` (`fieldcad-superposition-solver`) did
  five separate `.iter().map(...).collect()` passes over one sample
  slice. Fused into one loop over pre-sized `Vec`s. The measured win
  wasn't the five-passes-over-cache-lines angle expected going in — it
  was that the old `jacobians` column collected through
  `Option<Vec<_>>>`'s `FromIterator` impl, which routes through a
  `GenericShunt` adapter (for early-exit on the first `None`) that can't
  report a size hint, so it grew by reallocation even though the other
  four columns' plain collects were already exact-sized. The fused loop's
  plain `Vec::with_capacity` + push has no such gap.
- Measured (paired runs, same scene, same machine/load window):
  allocations per tick **183 → 173** (`positions()` fix) **→ 167**
  (`from_samples` fusion), reproducible to 3 decimal places across
  repeated runs. Wall-clock min/mean/median were within the session's
  machine noise (~load 4 on a 20-core box) either side of both fixes —
  a real, `perf`-confirmed allocation-count win, not a wall-clock claim.
- Verification: `cargo build/test/clippy/fmt --workspace` clean, 844
  tests passing (both fixes are behavior-preserving: no channel, gradient,
  or validity semantics changed, only how the buffers backing them are
  built).

This closes the task. Phase 5 (rayon evaluator) remains the only
deliberately parked item — additive, measurement-gated, no correctness or
dedup debt attached to leaving it parked.
