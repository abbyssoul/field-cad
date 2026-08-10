# Task: `EquationSystemSolver::forces` writes into a caller-owned buffer

## Goal

Change `EquationSystemSolver::forces` from returning a freshly allocated
`Vec<DVec3>` per plugin per force evaluation to accumulating directly into a
caller-provided buffer, eliminating the per-tick allocation cascade in
`SimulationRuntime`'s force evaluation. Same pattern already applied locally
to `plugins/electromagnetism/src/coupling.rs`'s particle-coupling scratch
(`current_density`/`delta_scratch`/`flux_scratch`, all resized-and-reused
struct fields instead of `vec![]` per call), generalized here across every
coupled field system instead of one plugin's internals.

## Current limitation

`crates/fieldcad-simulation/src/runtime.rs`'s `eval_forces` closure (around
line 1648) runs once per tick under `SymplecticEuler`, or twice per tick
(pre-tick and post-half-drift) under `VelocityVerlet`. Each call:

- Allocates a fresh `contributions: Vec<Vec<DVec3>>` (`Vec::new()`, grown via
  `.push()`).
- Calls `EquationSystemSolver::forces(&self, bodies: &[DynamicBody]) ->
  Result<Vec<DVec3>, PluginError>` on every enabled plugin
  (`crates/fieldcad-plugin-api/src/lib.rs:357`), each of which allocates and
  returns its own body-count-sized `Vec<DVec3>` — implemented today by
  `plugins/electrostatics/src/lib.rs:315` and `plugins/gravity/src/lib.rs:200`
  (every other plugin falls through to the trait's default, a freshly
  allocated zero-filled `Vec`). Both `forces()` implementations call their
  crate's `field_excluding`/`evaluate_acceleration_excluding` directly,
  bypassing each plugin's swappable `ElectrostaticBatchEvaluator`/
  `GravityBatchEvaluator` (added after this doc was first written, for the
  GPU electrostatics/gravity backend unification — see `git log
  plugins/gravity/src/lib.rs`) — that evaluator only covers batched
  `sample()`, never per-body force queries, so it's irrelevant to this task
  and nothing here depends on it.
- Passes the whole `&[Vec<DVec3>]` to `dynamics::accumulate_forces`
  (`crates/fieldcad-dynamics/src/lib.rs:218`), which allocates yet another
  `total: Vec<DVec3>` and sums into it.

One tick with `N` enabled coupled systems and `B` dynamic bodies costs `N + 2`
allocations (`N` per-plugin buffers, the outer `contributions` `Vec`, and
`accumulate_forces`'s `total`), all sized `O(B)`, purely to move numbers from
where a plugin computed them to where the integrator sums them — none of it
retained across ticks. Velocity Verlet's `cached`/`half_bodies` locals add a
couple more `O(B)` allocations of the same shape.

This is much smaller in absolute terms than the electromagnetism grid-sized
clone already fixed (`O(B)` vs `O(grid cells)`), but it is unconditional
per-tick allocation pressure that scales with body count × active system
count. As with the EM fix, the buffers are the same size call to call within
a session (body count and the enabled-plugin set only change on an authored
edit, not every tick), so reuse is straightforward — this just hasn't been
done here yet.

## Required behavior

- Change the trait method to something like `fn add_forces(&self, bodies:
  &[DynamicBody], out: &mut [DVec3]) -> Result<(), PluginError>`, documented
  as *adding into* `out` (already zeroed by the caller), never overwriting
  it — matching `accumulate_forces`'s current summation semantics exactly. A
  system exerting no force on any body needs no special case (an empty body
  is a valid implementation, same as today's default trait body).
- `SimulationRuntime` (or wherever `eval_forces` ends up) owns one
  `Vec<DVec3>` scratch buffer, resized (not reallocated) and zeroed once per
  call, passed by `&mut` to every enabled plugin in turn — replacing both the
  `contributions` outer `Vec` and `accumulate_forces`'s `total`.
- Preserve `DynamicsError::ForceCountMismatch`/`NonFiniteForce` validation: a
  length mismatch can no longer happen through the return path (the caller
  now owns the length), so assert `out.len() == bodies.len()` once before the
  loop instead; keep the finiteness check, run after each plugin writes.
- Update every implementor: `plugins/electrostatics/src/lib.rs`,
  `plugins/gravity/src/lib.rs`, and the default trait body in
  `crates/fieldcad-plugin-api/src/lib.rs`.
- `dynamics::accumulate_forces` either goes away entirely (folded into the
  new in-place accumulation at the call site) or becomes a thin helper over
  `&mut [DVec3]` — decide during implementation, whichever reads more clearly.

## Tests and acceptance

- `crates/fieldcad-dynamics`'s existing
  `forces_from_several_systems_sum_before_the_body_is_moved` test (and the
  rest of that module) continues to pass, adapted to the new signature —
  summation semantics across multiple systems must be unchanged.
- `plugins/electrostatics` and `plugins/gravity`'s own force tests continue
  to pass under the new signature.
- No allocation appears in `eval_forces`'s steady-state path once the scratch
  buffer is sized — a manual check is enough (Rust has no cheap per-call
  allocation counter to assert on in a unit test); a `fieldcad-bench` entry
  is a nice-to-have, not required.
- Purely an internal allocation-shape change: no observable behavior, physics,
  or external API differs.

## Relevant code

- `crates/fieldcad-plugin-api/src/lib.rs:357` — trait method + default
  implementation.
- `plugins/electrostatics/src/lib.rs:315` — `forces` override.
- `plugins/gravity/src/lib.rs:200` — `forces` override.
- `crates/fieldcad-dynamics/src/lib.rs:218` — `accumulate_forces`.
- `crates/fieldcad-simulation/src/runtime.rs:1648` — `eval_forces`, the
  per-tick call site (and its Velocity Verlet half-step locals).

(Line numbers current as of the GPU electrostatics/gravity backend
unification landing — re-check with `grep -n` before editing, since both
plugin files shifted once already after this doc was first written.)

Not urgent — recorded here so it isn't lost. The GPU electrostatics/gravity
backend unification this was deliberately deferred behind has now landed, so
this is unblocked and ready to pick up.
