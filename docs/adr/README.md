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
| [0002](0002-no-ecs-for-the-world-model.md) | A plain object model, not an ECS | accepted |
| [0003](0003-direct-egui-and-wgpu-integration.md) | Integrate `egui` and `wgpu` directly rather than through `eframe` | accepted |
| [0004](0004-si-units-in-the-core.md) | SI in the core, conversion only at display | accepted |
| [0005](0005-defer-runtime-plugins.md) | Defer runtime-loaded plugins; first-party crates until two solvers exist | accepted |
| [0006](0006-columnar-batched-field-sampling.md) | Field values are sampled in columnar batches over a declared geometry | accepted |
| [0007](0007-validate-before-adopting-a-world-edit.md) | Solvers validate a candidate world before it is adopted | accepted |
| [0008](0008-tick-time-from-an-epoch.md) | Simulation time is reconstructed from a tick count and an epoch | accepted |
| [0009](0009-demand-driven-redraw.md) | Redraws are demand-driven, not continuous | accepted |
| [0010](0010-gpu-evaluation-publishes-snapshots.md) | GPU evaluation publishes ordinary field snapshots | accepted |
| [0011](0011-queue-running-edits-at-fixed-tick-boundaries.md) | Running edits enter the world immediately before a fixed tick | accepted |
