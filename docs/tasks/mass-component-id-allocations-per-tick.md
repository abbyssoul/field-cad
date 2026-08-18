# `fieldcad-sources` mass component/property IDs allocate fresh every call

## Goal

`fieldcad-sources`' `schema_namespace_id`, `inertial_mass_component_id`,
`gravitational_mass_component_id`, `mass_property_id`, and
`follows_inertial_property_id` (`crates/fieldcad-sources/src/lib.rs:37-57`)
each rebuild a `PluginId`/`ComponentTypeId`/`PropertyId` from a `&'static
str` constant on every call, and each of those `::new` constructors takes
`impl Into<String>`, so every call heap-allocates a `String` even though
the input is always the same literal. These are the dominant allocation
source in a per-tick profile of a real scene and should be reduced to a
one-time cost.

## Current limitation

Backtrace-sampled allocation profiling (`cargo flamegraph` plus a temporary
sampling `GlobalAlloc` wrapper) against
`~/Documents/field-cad/earth-moon-titan.fcscene` — a scene with several
gravitating bodies — showed these five functions responsible for roughly
half of all sampled allocation call sites, called from:

- `fieldcad_sources::collect_gravity_sources` (once per gravity source, per
  tick, via `gravitational_mass_of` → `inertial_mass_of` →
  `inertial_mass_component_id`/`gravitational_mass_component_id`/
  `mass_property_id`/`follows_inertial_property_id`)
- `fieldcad_dynamics::collect_mass_bearing_bodies` and
  `fieldcad_dynamics::collect_bodies` (once per body, per tick)

Each of those call chains allocates 3-4 `String`s (one per ID level:
`PluginId` → `ComponentTypeId`/`PropertyId`) to look up a component that
never has a different namespace or name at runtime. `fieldcad-electromagnetic-sources`
and any other crate following the same `fn xyz_id() -> XyzId { XyzId::new(...) }`
pattern likely has the same shape and is worth checking once this is fixed
here, so the fix generalizes rather than treating this one crate as special.

## Required behavior

- Each of these ID-returning functions should compute its `PluginId`/
  `ComponentTypeId`/`PropertyId` once and hand back a cheap clone (or a
  `&'static` reference, if the ID types support that) on every subsequent
  call — e.g. via `std::sync::OnceLock` seeded on first use, so the
  `.expect("static ... is valid")` panic path still only runs once and
  still fails fast in the same way it does today.
- No change to what the functions return — same `PluginId`/`ComponentTypeId`/
  `PropertyId` values, same panic behavior on the (should-be-impossible)
  invalid-static-string path — only how often the allocation backing them
  happens.
- Worth checking whether `PluginId`/`ComponentTypeId`/`PropertyId`
  (`crates/fieldcad-core/src/ids.rs`) could store an `Arc<str>` instead of
  `String` internally, which would make *this* fix (and every other crate's
  analogous `_id()` helper) a cheap `Arc::clone` instead of a fresh
  allocation without each call site needing its own cache.

## Tests and acceptance

- A regression test (or a `#[global_allocator]`-counting test, matching the
  pattern in `crates/fieldcad-bench/examples/profile_scene.rs`) asserting
  that N calls to `inertial_mass_component_id()` allocate O(1), not O(N).
- Re-run `cargo run --release -p fieldcad-bench --example profile_scene --
  ~/Documents/field-cad/earth-moon-titan.fcscene 2000` before/after: this
  session's baseline was 167.013 allocations/tick; expect a measurable drop
  once these functions stop allocating on every call.
- Full `cargo test --workspace` plus `cargo clippy --workspace --all-targets
  -- -D warnings` unaffected.

## Relevant code

- `crates/fieldcad-sources/src/lib.rs:37-57` — the five allocating ID
  functions.
- `crates/fieldcad-sources/src/lib.rs` (search `collect_gravity_sources`,
  `gravitational_mass_of`, `inertial_mass_of`) — the per-source, per-tick
  call sites.
- `crates/fieldcad-dynamics` (search `collect_mass_bearing_bodies`,
  `collect_bodies`) — the per-body, per-tick call sites.
- `crates/fieldcad-core/src/ids.rs:35,68,112` — `QualifiedName::new`,
  `ComponentTypeId::new`, `PropertyId::new`/`PluginId::new`, all taking
  `impl Into<String>`, which is the actual allocation.
- `crates/fieldcad-bench/README.md` (`## Profiling an authored scene`) —
  how to reproduce the profile: `examples/profile_scene.rs` for the
  allocation *count*, `cargo flamegraph` (or a temporary sampling
  `GlobalAlloc` capturing `std::backtrace::Backtrace` on 1-in-N allocations)
  to name the call *site*, since a cycles-sampling flamegraph alone under-
  samples cheap, fast `malloc` calls relative to the CPU-bound analytic
  solver math that otherwise dominates the profile.
