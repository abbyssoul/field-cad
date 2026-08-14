# Unified dynamics integrator — design context (2026-08-14)

This note captures the context of a working session on the particle/field
coupling architecture: what the current split between the electromagnetism
plugin's internal Boris integrator and the first-party dynamics system is,
why it exists, and what it will take to reach the stated product goal of
arbitrary field combinations sharing one dynamics integrator.

## 1. The goal — and how far along we are

`goal.md` ("Solver re-design") and ADR 0022 express the same target: dynamics
is a first-party system. Bodies are moved by *one* integrator; every field
system contributes only forces to it; a body feels the sum of all fields.

That target is **not yet complete**. ADR 0022 itself says so under
"Not yet migrated":

> The electromagnetism plugin still integrates its own coupled particles.
> Its charge-conserving current deposition ([0020]) is numerically tied to
> the same old-to-new displacement the pusher produces, and its intervention
> diagnostics distinguish solver motion from authored edits by comparing
> against its own prediction.

Today the runtime enforces exactly-one-mover per body
(`crates/fieldcad-simulation/src/runtime.rs:1665-1685`): a solver that
declares an object through `kinematic_objects()` owns its trajectory, and the
dynamics force loop excludes that body (`runtime.rs:1694-1697`). The Maxwell
solver claims every charged body needing motion (`needs_kinematic_authority`,
`plugins/electromagnetism/src/particle.rs:47-49`), so a body carrying both
charge and mass feels **only** the Lorentz force — gravity's `add_forces`
contribution is never applied to it. This is an accepted interim state, not
the design endpoint.

## 2. Why the environmental solver uses Boris — and why that's a good reason

The Maxwell solver integrates its coupled particles with a relativistic
Boris pusher (`relativistic_boris_velocity`,
`plugins/electromagnetism/src/coupling.rs:634-671`). The reason matters:

- The Lorentz force's magnetic term, `q v × B`, is velocity-dependent and
  **rotational**. Boris splits the update into electric half-kick, exact
  rotation, electric half-kick, which conserves `|v|` in a static B-field to
  roundoff and keeps energy drift bounded.
- The generic `add_forces` interface returns a single summed `DVec3` force
  per body, and the dynamics integrator applies it as a plain momentum kick.
  ADR 0022 accepts this knowingly:

> Collapsing a system's action into one force vector means a magnetic force
> arrives with its velocity-dependence already evaluated, so it cannot be
> applied as a rotation... this integrator does not [conserve `|v|`], and
> will show energy drift where Boris did not.

So Boris is not an electromagnetism specialty — it is the correct tool for
**any** field whose coupling force splits into a direct part and a
velocity-dependent rotational part. Gravitoelectromagnetism is the same
shape of theory.

## 3. Lift Boris into the dynamics solver

The Boris structure belongs in the first-party dynamics integrator, not in
the electromagnetism plugin:

- For current Newtonian gravity and electrostatics (static, direct forces)
  the rotation part is absent and a plain kick suffices — this is why today's
  dynamics solver "does not need" Boris. That is an indication of the
  currently *limited* force repertoire, not an argument that the current
  integrator is complete.
- For the next gravity iteration — **GEM** (gravitoelectromagnetism, the
  linearised relativistic gravity field: a static Newtonian part plus a
  velocity-dependent "gravitomagnetic" part, e.g. frame-dragging) — the
  coupling force is again of the form direct + `v × field`. Advancing bodies
  under GEM without a Boris-style rotation would drift in energy exactly the
  way ADR 0022 predicts for magnetism.
- The requirement is therefore that the GEM solver must be able to push
  bodies with a Boris-equivalent scheme **without any dependency on the
  electromagnetism plugin**. One dynamics integrator, one execution scheme,
  selected by the session (`IntegrationScheme`), with rotational force
  handling available to every field.

ADR 0022 already anticipates the interface change this needs: "An interface
carrying `{ direct, rotational }` would recover it and remains open if the
drift proves to matter."

## 4. The target: arbitrary field combinations, configurable solver, not currently possible

The product direction (`goal.md`, "we should avoid double solving", and the
GEM/Newtonian gravity parallel drawn there for gravity) is that a single
physical field — one gravitational field, one electric field — is computed
by a chosen model, where the model is user-configurable:

- electrostatic ↔ Maxwell for the electric field;
- Newtonian ↔ GEM (later) for the gravitational field;
- and the systems may be freely combined (e.g. Maxwell + GEM acting on the
  same massive charged body).

None of that is possible today for moving bodies: a body claimed by the
Maxwell solver cannot simultaneously receive forces from gravity (point 1),
and there is no GEM solver at all. The simulation surface must support
modeling an arbitrary combination of fields, each with a configurable
solver, all contributing to one dynamics integrator.

## 5. What the changes are

1. **Extend the force contract** (`fieldcad-plugin-api::EquationSystemSolver::add_forces`)
   so a contribution can be declared as a rotation as well as a direct
   force — ADR 0022's `{ direct, rotational }` shape — and extend
   `fieldcad-dynamics`' integrators (`IntegrationScheme`) to consume it with
   a Boris-style split.
2. **Move the relativistic Boris core** out of
   `plugins/electromagnetism/src/coupling.rs` into
   `crates/fieldcad-dynamics`, parameterised by coupling charge and mass —
   the electromagnetism plugin then calls it, and any future GEM solver
   calls it too.
3. **Decouple current deposition from the pusher.** The deposition kernel is
   already a pure function of the displacement:
   `deposit_charge_conserving_current(domain, charge, old_position,
   new_unwrapped_position, seconds, ...)` (`coupling.rs:436-491`). It does
   not care where `new_position` came from. Feeding it the world-revision
   diff of a dynamics-integrated body is exactly the "reconstructing the
   displacement from an observed world revision" step ADR 0022 lists as the
   remaining work, and it converts Maxwell from a kinematic owner into a
   force contributor + current acceptor.
4. **Retire `kinematic_objects` ownership for Maxwell** once 1-3 land, so the
   runtime reaches a single integrator for every body while the Maxwell
   field advance remains in its own plugin (charge deposition can run as a
   solver-side step observing the committed motion).
5. **Keep the current exclusive-ownership semantics until then.** The energy
   diagnostics, intervention detection, and Gauss-consistency guarantees of
   ADR 0020 depend on the pusher knowing the exact displacement; a partial
   migration that leaves two integrators for one body is the one state this
   architecture must not fall into.

## Relevant code and documents

- `crates/fieldcad-simulation/src/runtime.rs` — `apply_tick_inner`:
  `kinematic_owners` exclusivity and dynamics exclusion.
- `crates/fieldcad-plugin-api/src/lib.rs` — `DynamicBody` (line 173),
  `ObjectKinematicsUpdate` (line 188), `kinematic_objects()` (line 318),
  `add_forces` (line 367).
- `plugins/electromagnetism/src/coupling.rs` — `ParticleCoupling` (line 49),
  `advance` (line 165), `relativistic_boris_velocity` (line 634),
  `deposit_charge_conserving_current` (line 436).
- `plugins/electromagnetism/src/particle.rs` — `needs_kinematic_authority`
  (line 47).
- `docs/adr/0022-dynamics-is-a-first-party-system.md` — the force-coupled
  contract, the rotational-force trade-off, "Not yet migrated".
- `docs/adr/0020-charge-conserving-periodic-particle-coupling.md` — why the
  deposition is displacement-tied.
- `docs/adr/0018-solvers-return-narrow-kinematic-outcomes.md` — how solver
  motion reaches the runtime.
- `goal.md` — "Solver re-design" and the single-field/configurable-model
  direction.
