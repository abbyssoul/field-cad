# Architecture decision records

Each record states one decision that would be expensive to reverse, the forces
that produced it, and what it costs. They exist so that a future reader — human
or agent — can tell a deliberate constraint from an accident, and so that
re-litigating a settled question requires new evidence rather than fresh opinion.

Format: context, decision, consequences, status. Keep them short. A decision that
needs pages of justification is usually two decisions.

Status values: `accepted`, `superseded by NNNN`, `revisited NNNN`.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-field-data-source-boundary.md) | The visualizer consumes a field data source, never a solver | accepted |
| [0002](0002-no-ecs-for-the-world-model.md) | A plain object model, not an ECS | accepted, revisited [0021](0021-objects-are-composed-from-independent-components.md) |
| [0003](0003-direct-egui-and-wgpu-integration.md) | Integrate `egui` and `wgpu` directly rather than through `eframe` | accepted |
| [0004](0004-si-units-in-the-core.md) | SI in the core, conversion only at display | accepted |
| [0005](0005-defer-runtime-plugins.md) | Defer runtime-loaded plugins; first-party crates until two solvers exist | accepted |
| [0006](0006-columnar-batched-field-sampling.md) | Field values are sampled in columnar batches over a declared geometry | accepted |
| [0007](0007-validate-before-adopting-a-world-edit.md) | Solvers validate a candidate world before it is adopted | accepted |
| [0008](0008-tick-time-from-an-epoch.md) | Simulation time is reconstructed from a tick count and an epoch | accepted |
| [0009](0009-demand-driven-redraw.md) | Redraws are demand-driven, not continuous | accepted |
| [0010](0010-gpu-evaluation-publishes-snapshots.md) | GPU evaluation publishes ordinary field snapshots | accepted |
| [0011](0011-queue-running-edits-at-fixed-tick-boundaries.md) | Running edits enter the world immediately before a fixed tick | accepted, revisited [0023](0023-an-interactive-edit-suspends-the-run.md) |
| [0012](0012-background-local-compute.md) | Local compute does not run on the window thread | accepted |
| [0013](0013-validate-time-step-before-adoption.md) | Equation systems validate a time step before adoption | accepted |
| [0014](0014-scene-level-field-system-activation.md) | Field-system activation is scene state, separate from object schemas | accepted, revisited [0025](0025-a-field-is-shared-a-model-is-chosen.md) |
| [0015](0015-host-owned-gpu-maxwell-backend.md) | Maxwell uses a host-owned GPU backend and publishes ordinary snapshots | accepted |
| [0016](0016-static-charges-constrain-the-default-maxwell-field.md) | Stationary charges constrain the default Maxwell field | accepted |
| [0017](0017-share-physical-source-schemas-across-equation-systems.md) | Physical-source schemas are shared across equation systems | accepted, generalised [0025](0025-a-field-is-shared-a-model-is-chosen.md) |
| [0018](0018-solvers-return-narrow-kinematic-outcomes.md) | Solvers return narrow kinematic outcomes through the runtime | accepted, revisited [0022](0022-dynamics-is-a-first-party-system.md) |
| [0019](0019-generic-particle-catalog-is-data.md) | The particle catalog creates one generic representation | accepted, revisited [0021](0021-objects-are-composed-from-independent-components.md), [0022](0022-dynamics-is-a-first-party-system.md) |
| [0020](0020-charge-conserving-periodic-particle-coupling.md) | Moving particles use charge-conserving periodic coupling | accepted |
| [0021](0021-objects-are-composed-from-independent-components.md) | Objects are composed from independent components | accepted |
| [0022](0022-dynamics-is-a-first-party-system.md) | Dynamics is a first-party system, coupled by force | accepted |
| [0023](0023-an-interactive-edit-suspends-the-run.md) | An interactive edit suspends the run and may defer a system | accepted |
| [0024](0024-undo-restores-a-captured-scene.md) | Undo restores a captured scene, forwards | accepted |
| [0025](0025-a-field-is-shared-a-model-is-chosen.md) | A field is shared; the model that computes it is chosen | accepted |
