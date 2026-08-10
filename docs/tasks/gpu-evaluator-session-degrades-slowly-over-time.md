# Task: simulation "Step" time still creeps up slowly over a long GPU-evaluator session

## Goal

Find and fix whatever remaining cause makes a running session's per-tick
compute time ("Step" in the Diagnostics panel) grow slowly over thousands
of ticks, after the dominant cause (GPU bind-group churn) was already
fixed and confirmed to cut the growth rate by ~30×.

## Status: partially fixed, residual not yet found

This is the third finding from one live-profiling pass with a user driving
`apps/fieldcad-desktop` (session date 2026-08-10). The first two —
`SampleCache` keyed by resolved geometry value instead of stable identity,
and `compute_field_layer_geometry` unconditionally rebuilding
`FieldGeometry` every frame — are already fixed and their own task docs
deleted per this repo's convention. This doc is for the third, only
partially resolved.

## The actual symptom (as reported, not assumed)

Two related but distinct live-session observations, both against a GPU
evaluator scene ("f32 batched evaluator" in Solver diagnostics —
`GpuInverseSquareEvaluator`, injected into both `plugins/gravity` and
`plugins/electrostatics` as `Arc<dyn InverseSquareBatchEvaluator>` since
`unify-inverse-square-sample-and-evaluator` landed; not the CPU reference
evaluator):

1. The Diagnostics **Mem** plot shows a sawtooth — it goes up and down,
   not just up. Partly explained already: the frame-merge fix (above)
   removed a large source of per-*frame* allocate-then-free churn tied to
   animated flow lines' forced ~250 Hz redraw. Still jagged after that fix
   per the user's own re-test, though — not fully explained.
2. The Diagnostics **Step** value (compute-thread time for one simulation
   tick — force collection, solver advance, dynamics integration, and
   publishing the snapshot) climbs steadily over a session, and the
   reported "real-time factor" (simulated dt ÷ step time) drops
   correspondingly. This is on the compute thread, not the render thread —
   distinct from the Mem sawtooth's likely rendering-side causes.

## What was ruled out (do not re-investigate these)

A dedicated investigation (this session) checked every stateful
container in the simulation tick path for unbounded/unpruned growth and
found none:

- **Relativistic Velocity integration** (`crates/fieldcad-dynamics/src/lib.rs`,
  `velocity_from_momentum`): closed-form, one `sqrt`, no iteration, cost
  fixed per body per tick. Position magnitude does not enter the formula
  at all, so domain scale (±300 Mm bounds) is irrelevant to its
  conditioning.
- **Compute-worker queue backlog**: `terminal_history` is capped at
  `MAX_TERMINAL_HISTORY` (`crates/fieldcad-simulation/src/source.rs`),
  and the user's own diagnostics showed `Que... 0` throughout — no
  backlog.
- **`World::commit`** (`crates/fieldcad-core/src/world.rs`): cost scales
  with live object count (~3 for the test scene), not with revision
  count, even at revision ~15,000.
- **`ProbeHistory`/`DistanceHistory`** (`crates/fieldcad-simulation/src/history.rs`),
  **`BodyHistory`**, **`EditHistory`**: all explicitly capacity-bounded
  with active pruning (checked directly, including their unit tests).
  `EditHistory` in particular is `.clear()`d every kinematic tick, not
  appended to.
- **The desktop app's own diagnostic plot buffers** (`History<const N:
  usize>`, `apps/fieldcad-desktop/src/app.rs`, `HISTORY_SIZE = 120`):
  fixed-size ring buffers, not a source of unbounded growth.
- **Electromagnetism's GPU backend** (`apps/fieldcad-desktop/src/electromagnetism_gpu.rs`):
  checked for the same anti-pattern as the fix below — it does *not* have
  it. `GpuMaxwellSolver::new` builds its `StepBindings` (bind groups) once
  at construction (lines ~155-199) and `step()` reuses them via
  `&self.bindings[self.current_electric]` (line ~512), a ping-pong index.
  No per-tick bind-group recreation here; nothing to fix in this file.

## What was fixed, and its measured effect (the receipts)

`apps/fieldcad-desktop/src/gpu_inverse_square.rs`'s `GpuInverseSquareEvaluator::evaluate_raw`
(the low-level dispatch method — renamed from `evaluate` when
`unify-inverse-square-sample-and-evaluator` landed `GpuInverseSquareEvaluator`'s
own `InverseSquareBatchEvaluator::evaluate` as a thin wrapper over it;
shared by both the gravity and electrostatics GPU paths either way) built a
**fresh `wgpu::BindGroup` on every single call** — every sampled geometry,
every tick — even though only the underlying buffers' *contents* changed,
not their identity (the buffers themselves were already correctly reused
via `ensure_capacity`, just not the bind group over them). Fixed by
caching the bind group in `GpuScratchBuffers`, invalidating it only when
`ensure_capacity` actually recreates one of the three buffers it
references (`source`/`position`/`output` — `params` never resizes,
`staging` isn't bound to the shader so its resizes don't matter).

Measured live, same scene, same user, before/after:

| | ticks measured over | Step time | doubling rate |
|---|---|---|---|
| Before this fix (fresh scene, reproduced from scratch via File > New) | revision 42 → 115 (~73 ticks) | 3.39ms → 6.05ms | ~73 ticks |
| After this fix (`earth-moon-2.fcscene`, resumed session) | revision 10278 → 12431 (~2153 ticks) | 1.55ms → 3.02ms | ~2153 ticks |

**~30× slower growth rate.** Memory also *decreased* slightly between the
two post-fix samples (161.0 → 156.3 MiB) rather than climbing, though the
user still describes the Mem plot as jagged.

Also established during this investigation, worth keeping — it's why the
"before" numbers above come from a *freshly built* scene rather than the
original `earth-moon-2.fcscene`: the growth is not specific to that file
or to anything about attached-geometry planes (a live comparison showed
Step time growing measurably within ~100 ticks of a **brand-new 2-object
scene**, built from `File > New`, with no flow lines and no attached
geometry at all) — and it is not tied to a memory floor rising, since one
before/after pair showed Step time nearly doubling while Mem stayed
*exactly flat* (174.6 MiB → 174.6 MiB). That decoupling from memory is
what pointed away from the CPU-side "small unscratched per-tick
containers" theory and toward the GPU dispatch path in the first place.

## What's left — candidates, not yet confirmed

In rough order of suspicion:

1. **`device.poll(wgpu::PollType::Wait { .. })`** in
   `GpuInverseSquareEvaluator::evaluate_raw` (`apps/fieldcad-desktop/src/gpu_inverse_square.rs`,
   near the end of the function) blocks once per dispatch — once per
   sampled geometry per tick. If `wgpu`'s or the Vulkan driver's internal
   submission/fence tracking has its own cost that grows with cumulative
   submission count (independent of bind-group identity, which is now
   fixed), this would still show up as exactly this pattern: slow,
   monotonic, not tied to RSS. The only real mitigation if this is it:
   batch every sampled geometry's dispatch into **one** command encoder /
   one submit / one poll per tick instead of one pair per geometry — a
   bigger, more invasive change than today's fix, needs its own design
   (the current code processes one `SampleGeometry` per
   `InverseSquareBatchEvaluator::evaluate`/`evaluate_into` call, called
   independently per geometry from `plugins/gravity`'s/`plugins/electrostatics`'s
   `samples_for`/`SampleCache`).
2. **`wgpu::CommandEncoder`** is still created fresh every call
   (`self.device.create_command_encoder(...)`, same function). Lower
   suspicion than (1) — encoders are meant to be short-lived/one-shot in
   wgpu's model, unlike bind groups — but not ruled out.
3. **CPU-side per-tick allocations** flagged (not confirmed) by the
   original investigation, now proportionally more significant since the
   GPU-side cost dropped: `crates/fieldcad-simulation/src/runtime.rs`'s
   `apply_tick_inner`/`adopt_world_commands` freshly allocate
   `kinematic_owners: BTreeMap`, `kinematics: BTreeMap`, a `cached: Vec`
   (VelocityVerlet only), a `commands: Vec`, and fully rebuild
   `self.last_forces: BTreeMap` every tick, instead of reusing persistent
   `SimulationRuntime`-owned scratch fields the way `force_scratch`
   already does (see that field's own doc comment — this exact bug class
   was already fixed once for forces specifically, just not for these
   siblings). All are `O(live object count)` (~3 here), so this is very
   unlikely to explain a multi-thousand-tick doubling on its own, but it's
   cheap and low-risk to convert regardless, and doing so would help rule
   it out cleanly.
4. Not yet checked at all: whether the *electrostatics* GPU evaluator
   path (which is now literally the same `GpuInverseSquareEvaluator`
   instance type as gravity's, not just sharing an internal core, since
   `unify-inverse-square-sample-and-evaluator` removed the separate
   `GpuElectrostaticEvaluator`/`GpuNewtonianGravityEvaluator` wrappers —
   so it already benefits from today's fix identically) or any CPU-side
   sampling path shows the same residual creep, or whether it's specific
   to gravity/Newtonian scenes.

## Suggested next steps

- Re-run the same live test this task's numbers come from (`File > New`,
  build a minimal 2-object gravity scene, watch Step time over a few
  thousand ticks) as the baseline for any further change — today's
  post-fix number (1.55ms → 3.02ms over ~2153 ticks) is the number to beat.
- If pursuing (1) above: this needs a real design pass (how the
  gravity/electrostatics `samples_for` → cache → evaluator call chain
  would batch multiple geometries per tick into one GPU submission),
  not a mechanical fix — don't start coding without one.
- If pursuing (3): convert `kinematic_owners`, `kinematics`, `cached`,
  `commands`, and `last_forces`'s rebuild in `runtime.rs` to persistent
  scratch fields, mirroring `force_scratch` exactly. Cheap enough to just
  try and re-measure, even without strong evidence it's the (or a) cause.
- Consider whether the Mem sawtooth (still jagged per the user, separate
  from the Step-time growth this doc is about) deserves its own
  investigation, or whether it's simply normal allocator page-return
  behavior around the per-tick GPU readback/staging buffers and not worth
  chasing further — no evidence either way yet.

## Relevant code

- `apps/fieldcad-desktop/src/gpu_inverse_square.rs` — `GpuInverseSquareEvaluator::evaluate_raw`
  (renamed from `evaluate` by `unify-inverse-square-sample-and-evaluator`;
  the type now also implements `fieldcad_superposition::InverseSquareBatchEvaluator`
  directly, with `evaluate` a thin wrapper over `evaluate_raw`), already
  fixed for bind-group caching this session; `device.poll`/
  `create_command_encoder` are the remaining per-call GPU resource
  creation in this function.
- `crates/fieldcad-simulation/src/runtime.rs` — `apply_tick_inner`,
  `adopt_world_commands`, `force_scratch` (the existing good pattern to
  mirror for the other per-tick containers).
- `apps/fieldcad-desktop/src/electromagnetism_gpu.rs` — checked, already
  correct (bind groups built once at construction); reference for what
  "already fine" looks like if extending the batching idea from (1) here
  too.

Found live, driving `apps/fieldcad-desktop` with a user across several
rounds of profile → hypothesize → fix → re-test. Explicitly not urgent —
the fix already landed cut the problem's growth rate by ~30×, which the
user judged good enough to pause on for now. Keep this doc's numbers as
the baseline when resuming.
