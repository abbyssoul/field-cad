# Milestone 5 review — coupled Maxwell solver

Date: 2026-08-03
Reviewer: code review pass over `plugins/electromagnetism`,
`apps/fieldcad-desktop/src/electromagnetism_gpu.rs`, and the runtime/plugin
seams they depend on.

## Scope and method

The claim under review is that the Maxwell field solver is implemented: a
dynamic time-domain solution that supports waves and that, in the static case,
agrees with the electrostatic equation system.

Static inspection was not treated as sufficient. Every quantitative statement
below comes from running the code or from a numerical model that reproduces the
implementation's exact discrete operators. Baseline before any change:

- `cargo test --workspace` — 185 passed, 15 suites.
- `cargo clippy --workspace --all-targets` — clean.
- `cargo fmt --all --check` — clean.

## Milestone status: claimed vs actual

`PLAN.md` claims Milestone 5 is "complete; performance review gate pending", and
`CONTEXT.md` claims a CPU `f64` reference plus a host-injected `wgpu f32`
backend on a periodic Yee lattice. **Those claims hold.** Specifically verified:

- The solver is genuinely dynamic, not a static evaluator dressed up as one.
  `MaxwellSolver::step` runs a synchronized Yee leapfrog (magnetic half-step,
  electric full step, magnetic half-step) against forward/backward staggered
  curl operators.
- Wave propagation is real and converging. `vacuum_wave_converges_toward_the_
  continuum_wave_speed` advances a prescribed plane wave one full period and
  asserts the 32-cell error is both below the 16-cell error and under 3%.
- The Courant limit is enforced through `validate_time_step` before the clock
  adopts a `dt`, on both backends.
- CPU/GPU parity is exercised over all five channels on a deterministic grid.
- Energy density and both divergence residuals are published as ordinary
  snapshot columns; no solver buffer reaches the renderer.

So the headline claim is not overstated. The defects below are in the
**static-charge initial condition**, which is the path the desktop actually
ships as its default, and in the maintainability of the two backends.

## Findings

### F1 — the static-charge field fabricates a value on the periodic seam (correctness, high)

`static_charge_initial_state` (`plugins/electromagnetism/src/lib.rs:461`)
samples the analytic Coulomb potential on grid nodes and takes a **forward
difference with periodic wrap** to build Yee `E`:

```rust
electric[index] = -DVec3::new(
    (potential[linear_index(counts, wrap_next(x, counts.x), y, z)] - phi) / spacing.x,
    ...
```

The sampled Coulomb potential is not a periodic function. At the last index on
each axis `wrap_next` returns `0`, so the difference is taken between the two
opposite faces of the box. That is not an approximation of the gradient
anywhere — it is a fabricated value on the outermost lattice layer of all three
far faces.

Measured, reproducing the implementation's exact operators (32³ cells, bounds
±5, `dx = 0.3125`, 1 nC point source, `Ez` along the z axis):

| charge position | seam-layer `Ez` | analytic `Ez` | relative error |
| --- | --- | --- | --- |
| `(0, 0, 0.6)` — **the shipped default scene** | `1.900e0` | `4.990e-1` | **281%** |
| `(0, 0, 2.0)` | `6.593e0` | `1.111e0` | 493% |
| `(0, 0, 3.0)` | `1.345e1` | `2.644e0` | 408% |

The interior is unaffected and genuinely good — median error against the
electrostatic oracle over the default XY plane is **0.3%**. The defect is
confined to the seam, but the seam is a whole face of the domain, it is drawn to
the user as an ordinary field value, and it moves with the charge.

It also corrupts the published conservation diagnostic, because
`yee_conservation` integrates energy over every cell including the seam:

| charge position | total energy | from seam cells |
| --- | --- | --- |
| `(0, 0, 0.6)` (default) | 2.364e-8 J | 0.7% |
| `(0, 0, 3.0)` | 2.252e-8 J | 15.6% |
| `(0, 0, 4.0)` | 2.999e-8 J | **29.6%** |

Global Gauss's law is also not satisfied: summing `div E · dV` over the lattice
gives `3.0e-12` where `q/eps0 = 1.13e2`. The construction is curl-free, so the
field is correctly *stationary*, but it is not a solution of Gauss's law — the
enclosed charge is silently cancelled by the seam sheet.

**Why the obvious fix is wrong.** The textbook remedy is to solve the discrete
periodic Poisson equation for a deposited charge density with a neutralising
background, which is self-consistent, smooth, and satisfies `div E = rho/eps0`
to machine precision (measured: `4e-13` against a scale of `5.4e2`). I
prototyped it. On the plane the user actually sees it is **much worse**, because
it correctly reproduces the periodic *lattice* field — charge plus images — not
the isolated charge:

| construction | median error | p90 | max |
| --- | --- | --- | --- |
| sampled potential (current) | **0.003** | 0.014 | 0.146 |
| periodic Poisson solve | 0.226 | 0.473 | 1.364 |

This is not a tuning problem. On a periodic domain the isolated Coulomb field is
not representable, and exact stationarity requires a periodic potential. The
current construction buys its excellent interior accuracy precisely by hiding a
discontinuity in one layer. Given the measurements, the interior accuracy is
worth keeping and the seam should be *declared undefined* rather than published
— which is exactly what this project's `SampleValidity` concept exists for.

### F2 — the regression test cannot detect F1 (test coverage, high)

`default_static_charge_field_matches_electrostatics_and_stays_stationary` places
the charge at the **origin**. For a centred charge the sampled potential is
symmetric across the seam, so the wrap difference is accidentally correct — the
measured seam error at the origin is 0.1%. The test then samples a single
interior point at `(1, 0, 0)` and, after moving the charge, asserts only
`rebuilt[0].x < 0.0`, a sign check.

The test therefore passes on the one charge position where the bug cannot
appear, and never samples near a boundary.

### F3 — the CPU and GPU Maxwell solvers duplicate their scaffolding (duplication, medium)

`MaxwellSolver` and `GpuMaxwellSolver` independently implement the same
non-numerical logic:

- `validate_time_step` — byte-identical Courant check and error string
  (`lib.rs:898`, `electromagnetism_gpu.rs:363`);
- the tick-sequence guard in `step` — identical (`lib.rs:926`,
  `electromagnetism_gpu.rs:403`);
- `validate_world` — identical delegation;
- `on_world_changed` — the same static-rebuild trigger and condition;
- `diagnostics` — ~45 lines differing only in the literal `"CPU f64"` versus
  `"GPU f32"`, including the same two diagnostic codes, the same
  `"(includes charge source)"` conditional, and the same format string.

This is the failure mode ADR 0015 set out to avoid ("CPU/GPU backends expose
identical plugin metadata and channels") but enforced only by convention. The
previous review already found and fixed an electrostatics duplicate that "had
already lost two trait methods"; this is the same shape of risk.

### F4 — the static field is rebuilt on world edits that cannot change it (efficiency, medium)

`SimulationRuntime::commit_world_commands` calls `on_world_changed` on every
enabled solver for every accepted commit (`runtime.rs:670`). Maxwell's
implementation unconditionally rebuilds the entire static field whenever the
initial condition is `StaticCharges`, so dragging a probe or a slice plane — the
most frequent interactions in the app — triggers a full `cells × sources`
potential rebuild plus a gradient pass, and on GPU a full grid upload. Nothing
in those edits can change the charge configuration.

### F5 — ADR 0016 understates the boundary behaviour (documentation, low)

ADR 0016 says the periodic discrete gradient "implies a compensating
boundary/background distribution, so accuracy near the boundary is limited and
must remain visible in the domain metadata". The measurements show this is not a
graceful degradation but a fabricated discontinuous layer with several-hundred
percent error, and it is not in fact visible in any metadata.

## Remediation plan

| # | Finding | Action |
| --- | --- | --- |
| R1 | F1 | Add `UndefinedReason::AcrossPeriodicSeam`. Have the static-charge path mark samples whose interpolation stencil crosses the seam as undefined, and exclude seam cells from the energy/divergence diagnostics. Keep the accurate interior construction. |
| R2 | F2 | Add regression tests with an **off-centre** charge that assert the seam is reported undefined and that interior samples track the electrostatic oracle. |
| R3 | F3 | Extract the shared, backend-neutral solver scaffolding so both backends delegate one implementation of the Courant check, tick guard, world validation, and diagnostics. |
| R4 | F4 | Skip the static rebuild when the collected charge sources are unchanged. |
| R5 | F5 | Update ADR 0016, `CONTEXT.md`, and `PLAN.md` with the measured behaviour. |

R1 is deliberately narrow: the measurements above say the interior construction
is the best available on a periodic domain, so the remediation makes the
solver honest about where it stops being valid rather than replacing physics
that is already accurate where it is defined.
