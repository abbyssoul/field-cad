# Review — Milestones 3 and 4

Date: 2026-08-03
Scope: the whole implementation as it stands — 13,444 lines of Rust across five
crates and two plugins — read against `CONTEXT.md`, `PLAN.md`, and ADRs
0001–0011.

Baseline at review time:

| Check | Result |
| --- | --- |
| `cargo test --workspace` | 152 passing, 0 failing |
| `cargo clippy --workspace --all-targets` | clean |
| `cargo build --workspace --all-targets` | clean |

Companion document: [remediation report](2026-08-03-remediation-report.md).

Every finding carries a disposition. `Fixed` means the defect is gone and a test
covers it. `Deferred` means it was accepted and left, with a reason.

---

## Claimed status versus actual status

`PLAN.md` claims Milestones 0–4 implemented, with Milestone 3's visual-legibility
gate and Milestone 4's UX acceptance outstanding as manual steps. Reading the
code against each milestone's exit criteria:

| Milestone | Claim | Verdict |
| --- | --- | --- |
| 0 — foundation | accepted | **Accurate.** Eleven ADRs exist and are referenced from the code they govern. |
| 1 — viewport spike | complete on Linux; cross-platform interactive checks pending | **Accurate.** CI builds and tests all three platforms; `gpu::smoke_test` covers the GPU path windowlessly; the fallback-adapter path exists. |
| 2 — core and plugin seam | complete, gate held | **Accurate.** Every exit criterion has a test, including the local/loopback interchangeability script. |
| 3 — electrostatic slice | complete; manual UX gate pending | **Substantially accurate**, with one qualification below. |
| 4 — workbench | complete; manual UX gate pending | **Accurate.** Playback pacing, running-edit boundaries, bounded probe history with attachment, and deterministic record/replay all have tests. |

The qualification on Milestone 3 is finding **F3**: the visualization engine is
not channel-generic. It renders exactly one hardcoded channel — electrostatics'
electric field — so `fieldcad-desktop` has a compile-time dependency on a
specific equation system in its *rendering* path, not merely in its authoring
UI. Milestone 3's own text calls for "generic renderers operat[ing] on declared
channel layouts", and Milestone 7's exit criterion is that gravity reuses
"generic planes, glyphs, colour maps". Nothing renders today unless the
electrostatics plugin is loaded.

That does not invalidate the Milestone 3 claim — the milestone's stated exit
criteria are all met — but it is a debt that Milestone 5 (magnetic `B`
alongside `E`) collides with immediately, because a second vector channel has
nowhere to be drawn.

One other planned item is genuinely outstanding and correctly recorded: ADR
0010's requirement to replace synchronous GPU readback with asynchronous
publication before a time-stepped GPU solver. `electrostatics_gpu.rs` still
blocks on `device.poll(Wait)`. That is deliberate and in-scope for the work
preceding Milestone 5, not a defect now.

---

## Overall assessment

This is disciplined code. Validated newtypes, atomic world commits, `Arc`-shared
immutable snapshots, per-sample validity that is never silently clamped, a clock
that reconstructs time from a tick count rather than accumulating it, and tests
that assert what they claim rather than what is easy. Comments explain *why*
where the reason is not obvious — drop order in `ViewportRenderer`, the
`Duration` remainder in `TickPacer`, the epoch in `SimulationClock` — and are
absent where the code speaks for itself. No clippy warnings, no dead code, no UI
types leaking into the core.

The findings below are therefore mostly about **structural duplication that has
begun to diverge**, and about **abstractions that stopped one step short of the
boundary the plan needs next**. There is one latent correctness hole (F5) and
one performance defect that scales quadratically with a user-facing setting
(F4).

Three themes dominate:

1. **Two implementations of one behaviour, already drifting.** The local and
   loopback data sources duplicate command dispatch, pacing, and edit queueing
   (F1). The electrostatics plugin exists twice, and the copy has already lost
   two trait methods (F2).
2. **The visualization layer is bound to one equation system** (F3).
3. **Capabilities exist and are tested but are unreachable from the product**
   (F8): `SimulationRuntime::set_subscription` is never called by the desktop,
   so transport sampling density is frozen at construction.

---

## Findings

### F1 — `LocalDataSource` and `LoopbackDataSource` duplicate the command and pacing semantics they are supposed to share

`crates/fieldcad-simulation/src/source.rs`

The two sources exist precisely so that "swapping a local source for a remote
source does not change probe or visualization consumers", and the module comment
says both "offer the *same* guarantees, not merely the same method names… so the
rules about completeness, session identity, and supersession are enforced on one
code path rather than only on the path that happens to be remote."

That is true of the mailbox. It is **not** true of anything else. Both types
independently reimplement:

- the six-arm `CommandPayload` match, including which payloads reset the pacer;
- the running/paused branch that decides `Applied` versus `Queued`;
- `apply_pending_world_edits`;
- the `poll` body: scale elapsed, ask the pacer, flush queued edits at the
  boundary, advance up to the demanded ticks, compute `fell_behind`.

Roughly 130 lines are duplicated, and they have already diverged in ways that
are invisible today but are exactly the class of divergence the design was
meant to prevent:

- `LocalDataSource::execute` publishes for every disposition; `LoopbackDataSource::execute`
  returns early for `Queued` without transmitting, then repeats the entire
  `CommandReceipt` construction verbatim in the following branch — duplication
  *within a single function*.
- Local computes `fell_behind` from `self.runtime`, loopback from `self.server`,
  with identical logic written twice.

The consequence is that ADR 0011's edit-timing rules are enforced by two copies.
A change to queueing must be made twice and correctly both times, and the
interchangeability test would only catch it if the script happens to exercise
the divergent path.

**Disposition: Fixed.** A `SessionCore` now owns the runtime, pacer, playback
speed, and pending-edit queue, and implements command dispatch and tick pacing
once. `LocalDataSource` and `LoopbackDataSource` compose it and differ only in
delivery — immediate publication versus a link plus a believed client replica,
which is the only thing that should actually differ between them.

### F2 — The electrostatics plugin exists twice, and the copy has silently lost two trait methods

`plugins/electrostatics/src/lib.rs`

`ElectrostaticsPlugin` and `AcceleratedElectrostaticsPlugin` are two
`EquationSystemPlugin` implementations; `ElectrostaticsSolver` and
`AcceleratedElectrostaticsSolver` are two `EquationSystemSolver` implementations
whose `kind`, `validate_world`, `on_world_changed`, and `diagnostics` are
near-identical.

The accelerated plugin delegates `metadata`, `channels`, and
`component_schemas` to the plain one. It does **not** delegate
`configuration_schema` or `default_configuration`. Those silently fall through
to the trait's empty defaults. The two agree today only because plain
electrostatics also has no configuration.

The moment electrostatics declares a configuration property — a permittivity, a
softening length, a superposition cutoff — the GPU-backed plugin will accept a
world it should have rejected and run with an empty configuration bag, and no
test will notice, because the type that is tested for configuration validation
is the one that still works.

The split is also unnecessary. The `ElectrostaticBatchEvaluator` seam is already
the right abstraction; the CPU oracle is simply an evaluator that has not been
written as one.

**Disposition: Fixed.** There is now one `ElectrostaticsPlugin` holding an
`Arc<dyn ElectrostaticBatchEvaluator>`, one solver, and a `CpuBatchEvaluator`
implementing the seam at `f64`. `evaluate_sources` remains a free function, so
the analytic oracle is still directly callable by tests and by future numerical
solvers as a regression oracle, exactly as the invariants require.

### F3 — The visualization engine renders one hardcoded channel from one specific plugin

`apps/fieldcad-desktop/src/scene.rs`

```rust
use fieldcad_electrostatics::electric_field_channel_id;
...
let Some(channel) = snapshot.channel(&electric_field_channel_id()) else {
    return FieldGeometry::default();
};
```

`CONTEXT.md`: "Generic renderers operate on declared channel layouts, while a
plugin may later contribute an optional specialized visualization." A snapshot
already carries `ChannelSchema` with `FieldValueKind`, which is all the
information a generic vector-glyph layer needs. The renderer nonetheless asks
for one channel by name.

Concretely this blocks two planned milestones. Milestone 5 introduces `E` and
`B` together and requires that "electric and magnetic channels can be inspected
together on independent visualization layers" — `B` would be published and never
drawn. Milestone 7's exit criterion is that gravity reuses the generic layers
with no dependency added; today gravity would need `scene.rs` edited to name its
channel.

The desktop's dependency on `fieldcad-electrostatics` for *authoring* — the +Q
button, the charge editor — is defensible and stays. Depending on it to decide
what to draw is not.

**Disposition: Fixed.** `field_geometry` takes the channel to render.
`FieldSnapshot::vector_channels` enumerates what is available from declared
schemas, the desktop defaults to the first vector channel, and the View panel
offers a selector when more than one exists. `scene.rs` no longer references
`fieldcad-electrostatics`.

### F4 — Glyph scaling recomputes the batch maximum once per glyph

`apps/fieldcad-desktop/src/scene.rs`

```rust
fn glyph_scale(magnitude: f32, values: &[glam::DVec3]) -> f32 {
    let maximum = values.iter().map(|v| v.length() as f32).fold(0.0, f32::max);
    ...
}
```

This is called inside the per-glyph loops of both `append_plane_vectors` and the
sparse-3D branch, so it is O(glyphs × samples) per batch per frame. At the
shipped defaults — a 33×33 transport lattice and arrow density 15 — that is
1089 × 225 ≈ 245,000 length computations per plane per frame, for a quantity
that is constant across the whole batch. Both factors are user-editable and both
are on the same axis, so the cost grows quadratically with a slider the user is
invited to turn up.

There is also a latent inconsistency: `magnitude_colors` normalises against the
maximum over *usable* samples, `glyph_scale` against the maximum over *all*
values. They agree today only because undefined samples carry a zero placeholder.
An equation system that reports a large finite value alongside `Undefined`
validity — which nothing forbids — would make colour and arrow length disagree
about the same sample.

**Disposition: Fixed.** The maximum is computed once per batch as a
`MagnitudeScale`, over usable samples only, and shared by colour and glyph
length.

### F5 — A world command can install a non-unit rotation, which scales attached-probe offsets

`crates/fieldcad-core/src/world.rs`

`Transform::new` validates *and normalises*. `Transform::validate` only
validates. `WorldCommand::SetTransform` and `ObjectSpec` take a `Transform` whose
fields are `pub`, so `Transform { translation, rotation }` constructed literally
bypasses normalisation entirely and `apply_command` stores it as given.

`WorldSnapshot::resolve_probe_position` then computes `rotation * offset`. With
`|q| = k`, glam scales the rotated vector by `k²`. An attached probe would
silently sample the wrong point in space, and would report that value with full
provenance and `Exact` validity — precisely the failure mode the invariant "a
rendered value is attributable to a world revision, plugin version, simulation
time, domain, and numerical configuration" exists to prevent.

Nothing in the application triggers this today; every call site happens to use
`Transform::new`. It is a hole in a validated type, not an active bug, but the
type advertises a guarantee it does not enforce.

**Disposition: Fixed.** Normalisation moved into `Transform::validate`'s
companion `normalized()`, applied at the command boundary for both
`CreateObject` and `SetTransform`, with a test that an object created with a
denormalised quaternion resolves an attached probe to the correct world point.

### F6 — One UI frame can produce several commands, and all but the last are discarded

`apps/fieldcad-desktop/src/ui.rs`

`UiFrameOutput::command` is a single `Option<CommandPayload>` written by
assignment from roughly twenty call sites. Any frame in which two widgets report
`changed()` loses the first edit with no error, no log line, and no visible
symptom other than a control that appears not to have worked.

`object_properties` is the clearest instance: the position grid sets a command
from the closure, then the shape-radius editor may overwrite it, then the charge
editor may overwrite that, and finally the position command is written *after*
the grid closure returns — so an in-frame radius edit is silently lost to a
position edit that may not even have happened this frame.

Single-pointer interaction makes this rare in practice. It is still a design
that discards user intent by default rather than by decision.

**Disposition: Fixed.** `UiFrameOutput::commands` is a `Vec`, appended to by
every producer and executed in submission order — which is also the order ADR
0011 promises for queued edits.

### F7 — `ProbeHistory` retains series for probes that no longer exist

`crates/fieldcad-simulation/src/history.rs`

Each `(ProbeId, ChannelId)` series is correctly bounded. The map of series is
not. Deleting a probe leaves its history keyed forever; probe IDs are minted
monotonically and never reused, so a session that repeatedly adds and removes
probes grows without bound. `WindowState::refresh_world` already prunes stale
selections and `plane_layers` against the current world — probe history is the
one collection it misses.

At `DEFAULT_PROBE_HISTORY = 2048` readings per series this is measured in
megabytes per hundred deleted probes, not a crash, but "bounded" should mean
bounded.

**Disposition: Fixed.** `ProbeHistory::retain_probes` prunes to a live set, and
`refresh_world` calls it alongside the selection pruning it already does.

### F8 — Transport sampling density is fixed at construction and unreachable from the UI

`apps/fieldcad-desktop/src/app.rs`, `crates/fieldcad-simulation/src/runtime.rs`

`SimulationRuntime::set_subscription` exists, is documented as the mechanism by
which "changing it changes how densely a result is observed, never the result
itself", and is covered by a test. The desktop never calls it. The subscription
is set once in `create_local_data_source` to 33×33 plane samples and a
whole-domain stride of 8, and stays there for the life of the process.

The consequence is a real ceiling on the product. `PlaneLayerSettings::magnitude_density`
is a `u32` with no upper bound and the inspector invites the user to raise it,
but above 33 the extra samples are bilinear interpolation of a lattice that
cannot resolve them. The interpolation is deliberate and correctly documented —
it avoids clustered integer decimation and does not claim solver accuracy — but
there is currently *no* way to ask the source for more, which is the other half
of that design.

There is also no `CommandPayload` variant for it, so a remote source could not
be asked either. `CONTEXT.md` lists subscription renewal as part of the
reconnect sequence, so the concept belongs on the command boundary regardless.

**Disposition: Fixed.** `CommandPayload::SetSubscription` was added and is
handled once in `SessionCore` (see F1). The inspector exposes transport plane
density and domain stride, separately from the per-plane presentation densities,
and labels which is which.

### F9 — Gizmo geometry is written twice, once for drawing and once for picking

`apps/fieldcad-desktop/src/scene.rs`

`append_gizmo_plane` and `pick_transform_handle` both compute the plane-handle
quad from `length * 0.18` and `length * 0.42` and both expand the same four
corners, in two places, from two literal copies of the same constants. Changing
the drawn gizmo without changing the picking code produces handles that are
where they look but not where they click — a bug with no compile-time signal and
an unpleasant manual reproduction.

The axis handles have the same shape of problem in a milder form: drawing uses
the full `length`, picking tests the segment from `0.45 * length` to `length`.

**Disposition: Fixed.** `gizmo_plane_corners` and `gizmo_axis_segment` are the
single source of both, used by drawing, picking, and a test that asserts a
pointer at the centroid of the drawn quad picks that handle.

### F10 — `ui.rs` and `scene.rs` have outgrown single files

1,921 and 1,476 lines respectively, each holding several unrelated
responsibilities. `ui.rs` contains the per-frame view model, SI formatting, a
hand-painted plot renderer, four panels, and electrostatics-specific authoring.
`scene.rs` contains field-batch-to-geometry conversion, gizmo drawing, ray
picking, and authoring proxies.

Both are internally well organised, and neither is a defect. But the review's
own navigability suffered: locating the plane-density path required reading
three quarters of `scene.rs`, and the F9 duplication is exactly the kind that a
file boundary between "draw" and "pick" would have made obvious.

**Disposition: Fixed.** Both are now directories of focused modules with
unchanged public surfaces.

---

## Not addressed, deliberately

| Item | Reason |
| --- | --- |
| Asynchronous GPU snapshot publication | ADR 0010 explicitly scopes this to the work preceding Milestone 5, and it is recorded in `PLAN.md`'s next increment. Synchronous readback is correct for a static analytic solver that only recomputes on invalidation. |
| Milestone 3 visual-legibility gate | Requires a native interactive run. Cannot be discharged from a review. |
| Milestone 4 UX acceptance of transport and plot controls | Same. |
| Windows/macOS interactive verification | Same; CI already covers build and test. |
| Splitting `fieldcad-render` / `fieldcad-ui` into crates | `PLAN.md` says to split only when there is a second consumer. There is not. The module split in F10 is the right amount of structure for now. |
| Streamlines, contours, volume rendering | Milestone 3's review gate is meant to decide which layers are legible before more are built. |

---

## Recommended order of work

1. F1 — unify the data sources. Everything else in `fieldcad-simulation` is
   cheaper once command dispatch exists in one place, including F8.
2. F2 — collapse the electrostatics plugin. Independent of F1.
3. F5, F7 — the two contained correctness fixes.
4. F3, F4 — the visualization channel boundary and the glyph-scale cost, which
   touch the same code.
5. F8 — subscription commands, which needs F1 done.
6. F6, F9 — UI command batching and gizmo deduplication.
7. F10 — the mechanical module split, last, so it does not obscure the diffs
   above.
