# Remediation report — Milestone 3 and 4 review

Date: 2026-08-03
Companion document: [the review](2026-08-03-milestone-3-4-review.md) whose
findings this addresses.

## Summary

All ten findings were addressed. Nothing was deferred except the items the
review had already recorded as out of scope.

| | Before | After |
| --- | --- | --- |
| Tests passing | 152 | 162 |
| Rust source | 13,444 lines | 14,060 lines |
| Rust files | 26 | 33 |
| Largest file | 1,921 lines (`ui.rs`) | 1,036 lines (`ui/panels.rs`) |
| `cargo clippy --workspace --all-targets` | clean | clean |
| `cargo fmt --all --check` | clean | clean |

The line count rose despite three deduplications because the new tests, the
`SessionCore` and `MagnitudeScale` documentation, and the module headers added
more than the removed duplication took away. The duplication itself is gone:
`source.rs` lost about 130 lines of parallel implementation, the electrostatics
plugin about 120.

## What changed, by finding

### F1 — one implementation of the session semantics

`crates/fieldcad-simulation/src/source.rs`

A new private `SessionCore` owns the runtime, the tick pacer, the playback
multiplier, and the queue of edits awaiting a boundary. It implements command
dispatch (`execute`) and wall-clock pacing (`advance`) once. ADR 0011's rules —
what applies immediately, what queues, when a queue flushes, how elapsed time
becomes whole fixed ticks — now exist in one place.

`LocalDataSource` and `LoopbackDataSource` compose it and differ only in
delivery: immediate publication through the mailbox, versus a link plus a
believed client replica. The duplicated `CommandPayload` match, the duplicated
`apply_pending_world_edits`, the duplicated `poll` body, and the receipt
construction that loopback repeated verbatim within one function are all gone.

One subtlety was preserved deliberately rather than by accident. The previous
loopback code adopted the server's status *before* advancing ticks, so the
client never claimed a tick whose snapshot had not arrived. `TickProgress` now
carries `status_after_flush` — the authoritative state after queued edits were
applied but before any tick — and that is what the client adopts. Adopting the
post-advance status would have let a remote client report ticks it had not been
told about, which is precisely the confusion the loopback source exists to
prevent.

### F2 — one electrostatics plugin

`plugins/electrostatics/src/lib.rs`

`CpuBatchEvaluator` now implements `ElectrostaticBatchEvaluator` at `f64` by
delegating to `evaluate_sources`. There is one `ElectrostaticsPlugin` holding an
`Arc<dyn ElectrostaticBatchEvaluator>` (`::new()` for the reference evaluator,
`::with_evaluator()` for a host-owned backend) and one `ElectrostaticsSolver`.
`AcceleratedElectrostaticsPlugin` and `AcceleratedElectrostaticsSolver` are gone.

The latent defect is closed by construction: there is no second type that can
forget to delegate `configuration_schema` or `default_configuration`.
`every_evaluator_backing_declares_the_same_contract` asserts that all five
contract methods agree regardless of which evaluator was injected.

Two behaviours strengthened as a side effect, both desirable:

- The precision guard — evaluator precision must match the domain's declared
  precision — now covers the reference path too, so an `f64` evaluator cannot
  quietly publish into a snapshot labelled `f32`.
- The reference path now shares the per-geometry cache, so `E` and the potential
  are evaluated once per geometry instead of twice.

`evaluate_sources` remains a free function, so the analytic oracle is still
directly callable by tests and by future numerical solvers, as the invariants
require. `the_reference_evaluator_agrees_with_the_analytic_oracle` pins that the
batched wrapper and the oracle produce identical values.

### F3 — the renderer no longer names an equation system

`crates/fieldcad-core/src/snapshot.rs`, `apps/fieldcad-desktop/src/scene/field.rs`,
`apps/fieldcad-desktop/src/ui/`

`FieldSnapshot::vector_channels` enumerates the published channels whose declared
`FieldValueKind` is `Vector`. `scene::field_geometry` takes the channel to draw
as a parameter. `apps/fieldcad-desktop/src/scene/` no longer references
`fieldcad-electrostatics` at all.

`UiModel::field_channel` holds the user's choice and
`UiModel::resolved_field_channel` falls back to the first published vector
channel — so the viewport draws something without the user choosing, and a
channel that stops being published falls back rather than blanking the viewport.
The View panel shows a channel selector only when more than one vector channel
exists, so the electrostatic slice gains no ceremony.

The desktop still depends on `fieldcad-electrostatics` for *authoring* — the +Q
button, the charge editor — which is plugin-specific UI and was never the
problem.

### F4 — glyph scaling is now O(samples), not O(glyphs × samples)

`apps/fieldcad-desktop/src/scene/field.rs`

`MagnitudeScale` computes the logarithmic normalization once per batch, over
usable samples only, and both colour and glyph length read it. At the shipped
defaults this removes roughly 245,000 redundant length computations per plane
per frame, and the cost no longer grows quadratically as a user raises the
density sliders.

`magnitude_scale_ignores_undefined_placeholders` pins the other half: an
undefined sample's placeholder does not set the scale, and colour and arrow
length cannot disagree about the same sample because they share one value.

`PlaneField` was extracted while doing this — the surface and vector layers took
five and eight positional arguments respectively, several of them parallel
slices that had to stay aligned by hand. They now take one value.

### F5 — rotations are unit at the world boundary

`crates/fieldcad-core/src/world.rs`

`Transform::normalized` is applied in `apply_command` for both `CreateObject` and
`SetTransform`. A quaternion of length `k` scales rotated vectors by `k²`, so a
denormalised transform reaching the world through a literal struct or through
`Deserialize` would have moved an attached probe's sample point while the reading
still claimed `Exact` validity.

`a_denormalised_rotation_cannot_scale_an_attached_probe_offset` constructs the
transform literally — the only way the value could arise — and asserts the probe
resolves to the correct world point through both commands.

### F6 — a frame's commands are all kept

`apps/fieldcad-desktop/src/ui/`, `apps/fieldcad-desktop/src/app.rs`

`UiFrameOutput::commands` is a `Vec`. The twenty-odd producers call `submit` or
`edit` — the latter wrapping a world transaction — instead of overwriting a
single `Option`. `WindowState::redraw` executes them in submission order, which
is also the order ADR 0011 promises for queued edits.

### F7 — probe history is bounded in both dimensions

`crates/fieldcad-simulation/src/history.rs`, `apps/fieldcad-desktop/src/app.rs`

`ProbeHistory::retain_probes` prunes series whose probe no longer exists;
`refresh_world` calls it beside the selection and plane-layer pruning it already
did. Each series was already bounded; the set of series now is too.

### F8 — subscriptions are commands, and reachable

`crates/fieldcad-simulation/src/source.rs`, `apps/fieldcad-desktop/src/ui/panels.rs`

`CommandPayload::SetSubscription` is handled once in `SessionCore` and is always
`Applied`, never queued: it cannot make a solver observe half an edit, so there
is no boundary for it to be atomic with. `FieldDataSource::subscription` reports
what the source has acknowledged it is sampling.

The inspector's new "Transport sampling" section edits plane samples per axis and
the whole-domain stride, labelled and placed separately from the per-plane
presentation densities, because the two mean different things: presentation
density interpolates the published lattice and claims no extra accuracy, while
transport density asks for samples that were actually evaluated.

`local_subscriptions_change_density_but_not_physics` and its loopback twin assert
that the sample count rises while the world revision and the domain do not.

### F9 — the gizmo is drawn and picked from the same geometry

`apps/fieldcad-desktop/src/scene/gizmo.rs`

`gizmo_plane_corners` and `gizmo_axis_segment` are the single source of both
paths; `GIZMO_PLANES` and `GIZMO_AXES` are the single source of which handles
exist. `handle_color` replaced the highlight/dim expression that was written
twice. `push_quad` and `push_quad_outline` replaced three copies of the
four-corners-to-triangles-and-outline pattern.

`a_drawn_plane_handle_is_picked_where_it_is_drawn` projects the centroid of the
drawn quad and asserts the pointer there picks that handle — a test that fails if
drawing and picking ever diverge again.

### F10 — two directories of focused modules

| Was | Now |
| --- | --- |
| `scene.rs`, 1,476 lines | `scene/mod.rs` 295, `field.rs` 568, `gizmo.rs` 399, `pick.rs` 306, `authoring.rs` 87 |
| `ui.rs`, 1,921 lines | `ui/mod.rs` 522, `panels.rs` 1,036, `compute.rs` 353, `plot.rs` 234 |

The public surfaces are unchanged: `scene::field_geometry`, `scene::pick_scene`,
`ui::show`, `ui::ComputeView` and the rest resolve exactly as before, through
re-exports. Tests moved with the code they cover; the two shared world fixtures
stayed in the respective `mod.rs` and are imported by the submodule that also
needs them.

`ui/panels.rs` is still large. It is a list of independent panel functions with
no shared state beyond `UiFrameOutput`, which is the shape egui code takes; a
further split by panel would produce four files of one function each. It is worth
revisiting if the inspector grows a second equation system's authoring UI.

## Verification

```
cargo build --workspace --all-targets   clean
cargo test  --workspace                 162 passed, 0 failed
cargo clippy --workspace --all-targets  clean
cargo fmt --all --check                 clean
```

New tests, by finding:

| Finding | Test |
| --- | --- |
| F2 | `every_evaluator_backing_declares_the_same_contract` |
| F2 | `the_reference_evaluator_agrees_with_the_analytic_oracle` |
| F3 | `the_drawn_channel_comes_from_the_snapshot_not_from_a_named_plugin` |
| F4 | `magnitude_scale_ignores_undefined_placeholders` |
| F5 | `a_denormalised_rotation_cannot_scale_an_attached_probe_offset` |
| F6 | `every_command_a_frame_produces_survives_in_submission_order` |
| F7 | `deleted_probes_do_not_retain_their_history_forever` |
| F8 | `local_subscriptions_change_density_but_not_physics` |
| F8 | `loopback_subscriptions_change_density_but_not_physics` |
| F9 | `a_drawn_plane_handle_is_picked_where_it_is_drawn` |

F1 and F10 are covered by the existing suite, which is the point of both: the
interchangeability script, the queued-edit tests run against both sources, and
the deterministic replay tests all pass unchanged against the unified
implementation, and the module split changed no behaviour.

## Still open

Unchanged from the review, and recorded here so the next reader does not have to
re-derive it:

- **Asynchronous GPU snapshot publication.** ADR 0010 requires this before a
  time-stepped GPU solver; `electrostatics_gpu.rs` still blocks on
  `device.poll(Wait)`. Correct for a static analytic solver that recomputes only
  on invalidation, and already recorded as the next increment in `PLAN.md`.
- **Milestone 3's visual-legibility gate** and **Milestone 4's UX acceptance** of
  the transport and plot controls. Both need a native interactive run. The
  transport-sampling control added under F8 is new UI and should be included in
  that pass.
- **Windows and macOS interactive verification.** CI covers build and test.
