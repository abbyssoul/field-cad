# Document the mass-aggregate ("center of mass") probe

## Goal

The center-of-mass feature was reworked from a hidden, always-on singleton
into a user-added, generalized probe (`MassAggregateProbe`) with two
membership modes — track everything but an exclusion list (`Universe`), or
track only an explicit selection (`Selection`) — each with computed
position, velocity, momentum, kinetic energy, and a bounded history like
other probes. None of this is documented anywhere outside the code.

## Current limitation

- `CONTEXT.md`'s ubiquitous-language table and `### Probes` section
  (`CONTEXT.md:612-627`) describe `Probe`/point recorders but say nothing
  about `MassAggregateProbe`, `MassSelection`, or the "anchor object"
  pattern it uses to stay attachable (a plane/box/sphere/probe can attach to
  a mass-aggregate probe the same way it attaches to any object).
- `docs/user-guide.md` has no section explaining how to add one from the
  scene tree ("+ Center of mass" / "Selection of objects"), how the
  Universe/Selection checklist works, or what the inspector's live-value
  grid and History plot show.
- No ADR records the design decisions that would be costly to reverse:
  companion `derived` anchor object per probe (chosen to preserve
  attachability without threading a new attachment-target type through
  `validate_attachment`/`ProbePosition`), N-of-M membership semantics
  (unlike `DistanceProbe`'s required two endpoints, removing a member object
  is not rejected — the probe just loses that member), and the legacy-scene
  migration (`World::adopt_legacy_center_of_mass`) that adopts an old
  document's hidden singleton into a real probe on load.

## Required behavior

- Add a `MassAggregateProbe`/`MassSelection` glossary entry to
  `CONTEXT.md`'s ubiquitous-language table, and either fold a short
  paragraph into `### Probes` or give it its own subsection covering the
  anchor-object/attachment pattern and the Universe-vs-Selection modes.
- Add a `docs/user-guide.md` section (near wherever probes are documented)
  covering: adding one, switching modes, the include/exclude checklist,
  reading the live-value grid, and the History plot's four toggleable
  series (position/velocity/momentum/kinetic energy, each plotted as a
  magnitude except kinetic energy).
- Decide whether the anchor-object/N-of-M-membership/migration decisions
  warrant a new ADR (`docs/adr/`) — they're the kind of "expensive to
  reverse" architecture choice ADRs are for, per `AGENTS.md`'s quality bar.

## Tests and acceptance

Documentation only — no test changes. Acceptance is a reviewer confirming
the new glossary entry, user-guide section (and ADR, if written) accurately
describe the shipped behavior in `crates/fieldcad-core/src/world.rs`
(`MassAggregateProbe`, `MassSelection`), `crates/fieldcad-dynamics/src/lib.rs`
(`mass_aggregate`), and the desktop UI
(`apps/fieldcad-desktop/src/ui/panels/mass_aggregate_probe_inspector.rs`,
`apps/fieldcad-desktop/src/ui/panels/scene_tree.rs`).

## Relevant code

- `crates/fieldcad-core/src/world.rs` — `MassSelection`, `MassAggregateProbeSpec`,
  `MassAggregateProbe`, `World::adopt_legacy_center_of_mass`.
- `crates/fieldcad-dynamics/src/lib.rs` — `mass_aggregate`.
- `crates/fieldcad-simulation/src/runtime.rs` — `adopt_world_commands`,
  `publish_snapshot`'s `mass_aggregates` computation.
- `apps/fieldcad-desktop/src/ui/panels/scene_tree.rs` — the "+ Center of
  mass" / "Selection of objects" add button.
- `apps/fieldcad-desktop/src/ui/panels/mass_aggregate_probe_inspector.rs` —
  membership editor, live-value grid, history plot.
