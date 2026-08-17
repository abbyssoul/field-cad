# Inverse-Square Plugin Pair: Duplication & Hot-Path Audit

Date: 2026-08-17 (rev 2 — revalidated and expanded the same day; every
line reference below was re-checked against current code. See the action
plan in `docs/tasks/inverse-square-plugin-hot-path-and-dedup.md`.)
Scope: `plugins/gravity/src/lib.rs`, `plugins/electrostatics/src/lib.rs`,
and their shared kernel `crates/fieldcad-superposition/src/lib.rs`, plus
the `SampleCache` in `crates/fieldcad-plugin-api/src/lib.rs:425-523` and
`ObjectIndex` in `crates/fieldcad-core/src/object_index.rs`.
Method: Static comparative review of the two plugins against each other and
against the shared `fieldcad-superposition` kernel. No benchmarks were run;
every claim below should be re-measured with `fieldcad-bench` before acting
on it (see `AGENTS.md` on `docs/perf/` being a backlog, not truth).

This report extends §2 of `2026-08-10-architecture-algorithm-review.md`,
which covered the O(N·M) force-sum shape. It looks one level deeper: at what
the shared kernel does per (sample × source) and per (body × source)
iteration, and at the structural duplication between the two plugin files.

Stated complexity, unchanged by anything proposed here: `evaluate_sources`
is O(positions × sources) per publication; `add_forces` via
`field_excluding` is O(bodies × sources) per tick. The findings below are
constant-factor cuts (roughly 2× on the per-tick force path combined) plus
one additive parallelism opportunity — no asymptotic change.

---

## 1. Compare and contrast

The two plugins are deliberately near-identical thin adapters over
`fieldcad-superposition` (gravity's module doc says so explicitly). Both
hold an `Arc<dyn InverseSquareBatchEvaluator>`, gate `create_solver` on
evaluator/domain precision agreement, build an `ObjectIndex<S>` plus a
pre-mapped `Vec<InverseSquareSource>` on world change, memoize per-geometry
batches in a `SampleCache` (capacity 16), and implement `sample` (field
channel + Jacobian; potential channel derives `∇φ = −E` gratis from the
already-computed field), `add_forces` (coupling-value × excluding-self
field), and one info diagnostic. Real differences:

| | gravity | electrostatics |
|---|---|---|
| Coupling constant | `−GRAVITATIONAL_CONSTANT` (attractive) | `+COULOMB_CONSTANT` |
| Source type / collection | `CoupledSource<MassKg>` via `collect_gravity_sources` | `ChargeSource` via `collect_charge_sources` |
| Force law | `F = m·a_excluding_self` | `F = q·E_excluding_self` |
| Channel schemas | local `channels()` constructors | re-exported from `fieldcad-electromagnetic-sources` (the electric field is *the* field, not this plugin's — ADR-0025) |
| `samples_for` length check | **missing** — gravity lib.rs:305-333 never verifies `evaluated.len() == geometry.len()` | present (lib.rs:327-333) |
| Test coverage | 5 tests | 12+ tests, incl. `CountingEvaluator` cache-sharing and evaluator-contract tests |

The mirroring is documented intent, but it is ~150 lines × 2 of structurally
identical solver code where only the constant, channel handle pair, and force
multiplier differ — and the gravity length-check gap shows the duplication
already leaking correctness-relevant detail to one side only. Gravity does
not go silently wrong on a misbehaving injected evaluator: the mismatch
surfaces later as a generic `FieldBatch::new` `LengthMismatch` error
(`crates/fieldcad-core/src/sampling.rs:547`) instead of electrostatics'
precise solver-contextual error — a worse diagnostic, not corruption.

## 2. Kernel hot-path findings (`fieldcad-superposition`)

### 2.1 The Jacobian is computed and discarded on the force path

`contribution` (lib.rs:81-125) always builds a `DMat3` Jacobian: an outer
product, 9 muls, and a scalar matrix multiply via `point_jacobian`
(lib.rs:72-76), plus `powi(3)` and a matrix subtraction. `field_excluding`
(lib.rs:186) calls `contribution` and throws the matrix — and the potential
— away; so does the interior-sphere path. `add_forces` runs once per tick
per plugin, so roughly half the FLOPs of every per-tick force evaluation
are dead work. Fix shape: split `contribution` into field-only and full
variants (the field-only variant needs no `DMat3` at all, which also lets
the interior-sphere arm drop its diagonal-matrix construction at
lib.rs:110-112).

### 2.2 Zero-strength filtering is inconsistent *and* misplaced

Two distinct problems:

- `evaluate_sources` re-checks `strength == 0.0` per sample × source
  (lib.rs:141) — one branch in the hottest loop that could move to build
  time.
- `field_excluding` has **no** zero-strength skip at all: a zero-strength
  source costs the full displacement/sqrt/division/matrix work per
  (body × source) per tick on the force path, for a provably zero
  contribution. (rev 1 of this report described only the first half of
  this finding.)

Both plugins already rebuild `inverse_square_sources` on every world change
(gravity lib.rs:189-196, electrostatics lib.rs:191-196); filtering
zero-strength sources there removes the branch from the sample loop and the
dead source from the force loop entirely. Note a knock-on requirement:
index-aligned exclusion (finding 2.7) needs the filtered `Vec` and the
`ObjectIndex` to stay positionally consistent — filter both together, or
exclude by lookup into the unfiltered set. (Filtering is observable only
through strength-zero sources, whose contribution is exactly zero in both
field and potential — safe.)

### 2.3 Finiteness checks run per source contribution — 13 scalar compares

Each iteration of `evaluate_sources` pays `!field.is_finite() ||
!potential.is_finite() || !matrix_is_finite(gradient)` (lib.rs:152), i.e.
3 + 1 + 9 scalar comparisons when everything is finite. A single
end-of-loop check is result-equivalent: any non-finite contribution
propagates through `+=` and survives to the end (including the
`+inf + (−inf) → NaN` case — NaN is still non-finite).

One deliberate semantics nuance to accept and test for: today the *reason*
recorded for an undefined sample is whichever condition the loop hit first;
with a hoisted check, a batch where one source overflows and a later source
contains the position flips from `NumericalOverflow` to
`InsideSourceRadius` (the containment check must stay in-loop to return
early). Both mark the sample `Undefined`; only the reason can differ.

### 2.4 `coupling_constant * strength` is re-multiplied per contribution

Computed in every match arm of `contribution` and again inside
`point_jacobian` (lib.rs:96-98, 103-112, 119-121) — three to four multiplies
where one suffices. Likely already CSE'd by LLVM; treat as cleanup that
shrinks codegen, and verify with the bench rather than claiming a win.

### 2.5 Duplicated exterior-field math

The `Point` arm (lib.rs:89-100) and the exterior `UniformSphere` arm
(lib.rs:115-123) are byte-identical field/potential/Jacobian formulas; only
the radius guard differs. Collapsing them (guard → shared exterior body)
reduces hot-loop code size.

### 2.6 CPU `evaluate` is single-threaded

`CpuInverseSquareEvaluator` (lib.rs:258-296) maps `geometry.positions()`
sequentially. Samples are independent, so a rayon-backed
`ParallelCpuInverseSquareEvaluator` is a drop-in `InverseSquareBatchEvaluator`
for both plugins via the existing `with_evaluator` seam — no plugin changes.
Determinism is preserved bit-for-bit because each sample's source summation
stays sequential; only samples parallelize. Note: `rayon` is currently **not**
a workspace dependency (absent from the root `[workspace.dependencies]`);
adding it must follow the AGENTS.md pin-once convention.

## 3. Plugin-path findings

### 3.1 `add_forces` re-derives its inputs every tick

Per body, both plugins (gravity lib.rs:249-262, electrostatics
lib.rs:259-273):

1. `self.sources.get(body.object)` — one `HashMap` lookup for the coupling
   value;
2. `self.sources.iter_excluding(body.object)` — a *second* `HashMap`
   lookup inside `ObjectIndex::iter_excluding`
   (`crates/fieldcad-core/src/object_index.rs:64-70`);
3. `.map(inverse_square_source)` — a struct conversion per source per body,
   re-doing work that already sits in the solver's precomputed
   `inverse_square_sources` buffer.

The fix falls out of a property `ObjectIndex` already guarantees and tests
(object_index.rs:121-128): `iter_excluding` yields every item except the
match, **in original order**. So excluding by *index* over the precomputed
`inverse_square_sources` slice — `sources[..excluded].iter().chain(sources[excluded+1..].iter())`
— is exactly equivalent to the current iterator chain, with one lookup, zero
conversions, and no per-body iterator plumbing. That needs a small new
`ObjectIndex::index_of(&self, id) -> Option<usize>` accessor.

### 3.2 `add_forces` cannot batch

Each body builds its own iterator chain and error plumbing, so there is no
place to amortize setup, and no seam for a future SIMD-over-bodies, rayon,
or GPU force evaluation (one dispatch per tick instead of one per body).
A kernel-level batch API — `field_excluding_into(coupling, sources,
&[excluded_index], &[DVec3], &mut [DVec3])`, caller-owned output per the
AGENTS.md no-alloc hot-path convention — provides it. This is also the
natural shape for the GPU evaluator to grow a force path against later.

### 3.3 Per-channel allocations in `sample` even on cache hits

`validity`, `gradients`, and both `FieldColumn`/`GradientColumn` `Vec`s are
freshly allocated on every channel read (gravity lib.rs:207-244,
electrostatics lib.rs:207-246) — the `SampleCache` hit saves the
*evaluation*, not the column assembly. The two channels over one geometry
duplicate the validity/gradient collection work. Sharing scratch buffers
across the channels of one publication would cut per-tick allocations;
measure with `fieldcad-bench` to justify (AGENTS.md allocation bar).

### 3.4 Correctness gap carried by the duplication

Gravity's `samples_for` lacks electrostatics' batch-length verification
(§1). Porting electrostatics' check — and its `CountingEvaluator` contract
tests — to a shared solver skeleton fixes the gap and prevents recurrence;
that is finding 2.2's structural payoff, not a one-line patch to gravity.

### 3.5 Bench coverage gap

`fieldcad-bench` has `gravity/forces` (workload.rs:437) but **no**
`electrostatics/forces` workload, even though both plugins share this exact
per-tick hot path. Any force-path work must add the missing workload first
or improvements to the shared kernel go unmeasured for electrostatics.

## 4. Recommended plan

Recorded as a phased action plan in
`docs/tasks/inverse-square-plugin-hot-path-and-dedup.md` — see there for
per-phase verification requirements. Summary:

- **Phase 0 — baseline:** save `fieldcad-bench` baselines; add the missing
  `electrostatics/forces` workload.
- **Phase 1 — kernel hot path, no API change:** split field-only vs full
  `contribution`; hoist finiteness to end-of-sample; fuse `k·strength`;
  collapse the duplicated exterior arms.
- **Phase 2 — plugin force path:** `ObjectIndex::index_of` + index-based
  exclusion over the precomputed buffer; filter zero-strength sources at
  world-change build time (with the §2.2 index-consistency requirement).
- **Phase 3 — batch force API:** `field_excluding_into` in the kernel,
  used by both plugins' `add_forces`; zero per-tick allocation.
- **Phase 4 — plugin dedup:** shared solver skeleton in
  `fieldcad-superposition` (generic over coupling constant, channel
  handles, source mapper); both plugins become ~50-line adapters; the
  skeleton carries the length check and contract tests once.
- **Phase 5 — parallel CPU evaluator:** rayon `Precision::F64` evaluator
  behind the existing injection seam; parity-tested; benched.
- **Phase 6 — sample-path allocations:** share validity/gradient/column
  derivation across the two channels of one geometry, gated on Phase 0
  numbers.

Phases are ordered by risk-adjusted payoff: 1–2 are localized
high-certainty wins; 3 unblocks the accelerated force path; 4 is the
structural cleanup that makes everything after single-site and closes the
§3.4 gap; 5–6 are additive and measurement-gated.
