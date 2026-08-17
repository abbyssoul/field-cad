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

- `crates/fieldcad-superposition/src/lib.rs:81` — `contribution`, the
  split target; `lib.rs:132` — `evaluate_sources` (finiteness hoist,
  zero-strength skip); `lib.rs:179` — `field_excluding` (field-only
  variant, future `field_excluding_into`); `lib.rs:258` —
  `CpuInverseSquareEvaluator` (Phase 5 sibling).
- `plugins/gravity/src/lib.rs:249` — `add_forces`; `lib.rs:305` —
  `samples_for` without the length check; `lib.rs:286` —
  `acceleration_excluding`.
- `plugins/electrostatics/src/lib.rs:259` — `add_forces`; `lib.rs:311` —
  `samples_for` with the check to generalize; `lib.rs:602` —
  `CountingEvaluator` test double.
- `crates/fieldcad-core/src/object_index.rs:64` — `iter_excluding` and
  the order guarantee `index_of`-based exclusion relies on.
- `crates/fieldcad-plugin-api/src/lib.rs:445` — `SampleCache` (identity
  keying, stale refill-in-place; unchanged by this task, context for
  Phase 6).
- `crates/fieldcad-bench/src/workload.rs:437` — `gravity/forces`, the
  template for the missing `electrostatics/forces`.
- `docs/perf/2026-08-17-inverse-square-plugin-audit.md` — the revalidated
  findings this plan executes.
