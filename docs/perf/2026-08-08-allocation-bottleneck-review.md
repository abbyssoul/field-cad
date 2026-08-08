# Memory Allocation Bottleneck Review

**Date:** 2026-08-08
**Scope:** Entire workspace (17 crates, ~25 KLOC Rust)
**Method:** Static analysis of allocation patterns on hot paths (ticks, frames, snapshot publications, sampling)
**Status:** For review; no code modified

---

## TIER 1 — Grid-Sized Per-Tick Allocations (Critical)

These allocate memory proportional to the Yee lattice cell count **every simulation tick**. At a 64³ grid (~262k cells) this is multiple MB freed and reallocated per tick; at 128³ (~2M cells) it is 30+ MB per tick.

### 1.1 Full grid clone of E/B on every CPU Maxwell tick with particles

| | |
|---|---|
| **File** | `plugins/electromagnetism/src/lib.rs:1394-1395` |
| **Pattern** | `self.electric.clone()` + `self.magnetic.clone()` |
| **Size** | 2 × cell_count × 24 bytes |
| **Frequency** | Every `step()` when particle coupling is active |

The Yee field is cloned solely to pass it to `advance_particles` which only reads the fields. Use `Cow<[DVec3]>` in `YeeFieldState` — the caller passes `Cow::Borrowed(&self.electric)`, avoiding the full clone without threading a lifetime through every downstream signature. If `advance_particles` or its callees never mutate the fields, `Cow::Borrowed` is zero-cost.

### 1.2 Grid-sized scratch buffers in `advance_particles`

| | |
|---|---|
| **File** | `plugins/electromagnetism/src/coupling.rs:134,136,183` |
| **Pattern** | `zero_vector_grid(domain)` → `Vec<DVec3>`, `deposit_particle_charge()` × 2 → `Vec<f64>` |
| **Size** | 1 × cell_count × 24 + 2 × cell_count × 8 bytes |
| **Frequency** | Every `advance_particles()` call (once per tick when particles exist) |

Three new grid-sized buffers allocated every tick: `current_density`, `old_charge`, `new_charge`. Pre-allocate scratch buffers in `ParticleCoupling` and clear/reuse.

### 1.3 `YeeFieldView` allocates centred fields per sample call

| | |
|---|---|
| **File** | `plugins/electromagnetism/src/lib.rs:1054-1055` |
| **Pattern** | `Vec::with_capacity(expected)` × 2 for `centred_electric` and `centred_magnetic` |
| **Size** | 2 × cell_count × 24 bytes |
| **Frequency** | Every `sample_yee_fields()` call (per channel per sample geometry per snapshot) |

The centred-field interpolation stores the result in full-grid Vecs even when sampling only a small geometry (e.g., probe points or a slice plane). Compute centre values lazily only for the cells touched by the sample geometry, rather than the full grid. For a probe (1 cell) or a slice plane (O(N²) cells), this reduces allocation from O(N³) to O(sample size). This is the **highest-impact finding across both reports**.

---

## TIER 2 — WorldState Deep Copy on Every Edit (High Impact)

### 2.1 `WorldState` deep-cloned inside `commit()` on every edit

| | |
|---|---|
| **File** | `crates/fieldcad-core/src/world.rs:933` |
| **Pattern** | `let mut candidate = (*self.state).clone()` inside `World::commit()` |
| **Frequency** | Every `adopt_world_commands()` call (authored edit, tick with kinematics, brush stroke) |

**Correction from earlier draft**: `World` contains `Arc<WorldState>`, so `self.world.clone()` (at `runtime.rs:1686`) is just an atomic refcount bump + Copy of 5 `u64` counters. The actual deep clone is inside `World::commit()` at `world.rs:933`, where `(*self.state).clone()` copies every `BTreeMap` (objects, planes, probes, spheres, boxes, component schemas). The diagnosis is directionally correct (deep copy on every edit) but the mechanism was misattributed.

The deep clone is inherent in the copy-on-write design: `commit` needs a mutable copy to apply commands, then wraps it in a new `Arc`. To avoid this, `commit` could apply commands directly to a clone of only the parts of `WorldState` that actually change, or use `Arc::make_mut` with shared-state mutation.

---

## TIER 3 — Per-Frame UI/View Allocations (Medium Impact)

These allocate every frame even when nothing has changed, adding allocation/free pressure at ~60 Hz.

### 3.1 `field_systems()` clones channel schemas every frame

| | |
|---|---|
| **File** | `crates/fieldcad-simulation/src/runtime.rs:734-762` |
| **Pattern** | `channel.as_ref().clone()` on every `Arc<ChannelSchema>`; multiple `.collect()` calls |
| **Frequency** | Every UI frame via `ComputeView::build` |

Each `ChannelSchema` contains `id: ChannelId(QualifiedName { PluginId(String), String })` and `display_name: String` — ~3 heap allocations per channel per frame. A cache invalidated by a revision counter would cut this from 60 Hz to edit frequency. The cache is non-trivial: invalidation triggers include plugin activation/deactivation, channel registration, config changes, and realtime toggles.

### 3.2 `ComputeView::build` allocates new collections every frame

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/ui/compute.rs:224-328` |
| **Pattern** | `Vec::new()` for `probe_readings`, `diagnostics`, `vector_channels`; `BTreeMap::new()` for `channel_names`; `.collect()` for field rows |
| **Frequency** | Every frame |

### 3.3 Per-frame clones in panels

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/ui/panels.rs:1697,1830,2140,2546` |
| **Pattern** | `.clone()` on property bags, channel IDs, probe channel lists, field-layer entries |
| **Frequency** | Every frame |

### 3.4 Per-batch Vec allocations in field scene rendering

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/scene/field.rs:67-70,102,129,156` |
| **Pattern** | `Vec::collect()` for displayed values; `scale.colors()` returning a new `Vec<Vec3>` per batch |
| **Frequency** | Per visible plane/domain batch per frame |

### 3.5 Per-frame renderer allocations

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/renderer.rs:705,830,890` |
| **Pattern** | `Vec::collect()` for `InstanceRaw` from `scene.instances()`; `vertices.iter().copied().map(Vertex::from).collect()`; `Vec::new()` in `grid_vertices()` |
| **Frequency** | Every render frame |

Store a persistent `instance_scratch: Vec<InstanceRaw>` in `SceneRenderer`. Call `.clear()` and extend from instances directly before GPU upload.

### 3.6 `FieldGeometry` mesh cloned on cache hit

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/app.rs:301` (`compute_field_layer_geometry`) |
| **Pattern** | `return (cache.geometry.clone(), None)` — returns a cloned `FieldGeometry` even on cache hit |
| **Size** | Contains `surface_triangles: Vec<ColoredVertex>` and `vector_lines: Vec<ColoredVertex>` |
| **Frequency** | Every frame when the scene is unchanged (most frames) |

`FieldGeometryCache` stores owned `FieldGeometry`. On hit, it clones the entire geometry. Store `Arc<FieldGeometry>` in the cache and return `Arc::clone()` (refcount bump) instead.

---

## TIER 4 — Triple-Collect in Analytic Plugins (Medium Impact)

### 4.1 Electrostatics `sample()` iterates samples three times

| | |
|---|---|
| **File** | `plugins/electrostatics/src/lib.rs:236-243` |
| **Pattern** | Three `.collect()` calls from the same `samples.iter()` — `validity`, `electric_field`, `potential` |
| **Size** | 3 × geometry_len × (1 + 24 + 8) bytes |
| **Frequency** | Every `sample()` call (per channel per geometry per publication) |

### 4.2 Gravity `sample()` same pattern

| | |
|---|---|
| **File** | `plugins/gravity/src/lib.rs:125-132` |
| **Pattern** | Same triple-`.collect()` for `validity`, `acceleration`, `potential` |
| **Frequency** | Every `sample()` call |

Note: `samples` is already a `Vec` from `samples_for(geometry)`. The three `.collect()` calls allocate new Vecs of the same length. A single-pass approach would halve the allocation count but not total bytes — the data must still be stored. Impact is modest; severity **P2** is appropriate.

---

## TIER 5 — Per-Tick Vec/BTreeMap Allocations in Runtime (Low-Medium Impact)

### 5.1 `apply_tick_inner` allocates new collections every tick

| File | Line | Pattern | Frequency |
|---|---|---|---|
| `runtime.rs` | 1459 | `BTreeMap::new()` for `kinematic_owners` | Every tick |
| `runtime.rs` | 1488-1491 | `.collect()` for filtered `bodies` | Every tick |
| `runtime.rs` | 1492 | `Vec::new()` for `contributions` | Every tick |
| `runtime.rs` | 1497-1501 | `.collect()` for `last_forces` | Every tick |
| `runtime.rs` | 1503 | `BTreeMap::new()` for `kinematics` | Every tick |
| `runtime.rs` | 1531-1534 | `.collect()` for `carried` | Every tick |

Upper bounds are known: plugin count (<= 10), body count. Pre-size Vecs. For `last_forces`, retain the BTreeMap in `SimulationRuntime` and use `.clear()` + `.extend()` to reuse node allocations.

### 5.2 `publish_snapshot` allocates new collections per publication

| File | Line | Pattern | Frequency |
|---|---|---|---|
| `runtime.rs` | 1773 | `BTreeMap::new()` for `channels` | Every publication |
| `runtime.rs` | 1774 | `Vec::new()` for `diagnostics` | Every publication |
| `runtime.rs` | 1813 | `Vec::new()` for `batches` (inside channel loop) | Every publication |

### 5.3 `collect_bodies` without preallocation

| | |
|---|---|
| **File** | `crates/fieldcad-dynamics/src/lib.rs:50-51` |
| **Pattern** | `Vec::new()` for `dynamic` and `carried` — called every tick from `apply_tick_inner` |
| **Capacity known** | `world.objects_with(&inertial_mass_component_id()).count()` |

### 5.4 Dynamic integration `collect()` return allocations

| | |
|---|---|
| **File** | `crates/fieldcad-dynamics/src/lib.rs:74,97,121` |
| **Pattern** | `vec![DVec3::ZERO; bodies]` in `accumulate_forces`; `.collect()` returns in `integrate` and `carry` |
| **Frequency** | Every tick |

Accept `&mut Vec<DynamicBody>` and `&mut [DVec3]` parameters to allow the caller to retain capacity across simulation frames. This is a quick win that avoids a trait-level change for the dynamics crate alone (the trait is internal, not the `EquationSystemSolver` boundary).

---

## TIER 6 — GPU Buffer & Readback Allocations (Medium Impact)

### 6.1 `field_state()` allocates decoded Vecs on every cache miss

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/electromagnetism_gpu.rs:237-332` |
| **Pattern** | `decode_fields()` → allocates `Vec<DVec3>` for both E and B; wraps in `Arc::new()` |
| **Size** | 2 × cell_count × 24 bytes |
| **Frequency** | First `sample()` or `diagnostics()` after each GPU tick (cached thereafter) |

The `Mutex<Option<Arc<...>>>` caching is good. The decode allocation is unavoidable for CPU read, but could come from a pre-allocated pool.

### 6.2 Electrostatic GPU evaluator re-creates all wgpu buffers per dispatch

| | |
|---|---|
| **File** | `apps/fieldcad-desktop/src/electrostatics_gpu.rs:120-153` |
| **Pattern** | 5 `device.create_buffer(_init|)` per `evaluate_batch` call (params, sources, positions, output, staging) |
| **Frequency** | Every `evaluate_batch()` call |

`wgpu::Buffer` creation is a device-level operation that can involve GPU-side allocation. Maintain persistent GPU buffers inside `GpuElectrostaticEvaluator`. Reallocate only when required size exceeds current capacity; use `queue.write_buffer` for data updates. Severity **P0** — this is a genuine GPU-side allocation hot-spot that the sibling report correctly identified.

---

## TIER 7 — O(n·m) Patterns in Force Evaluation

### 7.1 Electrostatic `forces()` linear-scans sources per body

| | |
|---|---|
| **File** | `plugins/electrostatics/src/lib.rs:264-285` |
| **Pattern** | Two linear scans over `self.sources` per body: `find()` for charge lookup then `filter()` for self-exclusion |
| **Complexity** | O(2·n·m) — n bodies × m charge sources |

### 7.2 Gravity `forces()` same pattern

| | |
|---|---|
| **File** | `plugins/gravity/src/lib.rs:139-165` |
| **Pattern** | Linear scan and filter for mass lookup and self-exclusion |
| **Complexity** | O(2·n·m) |

Both become O(n) with a `HashMap<ObjectId, Source>` for source lookup (O(1) per body instead of O(m)).

### 7.3 O(N²) particle-to-source lookup in EM source update

| | |
|---|---|
| **File** | `plugins/electromagnetism/src/lib.rs:813-817` |
| **Pattern** | `for source in &mut self.sources { if let Some(particle) = coupling.particles().iter().find(\|p\| p.object == source.object) { ... } }` |
| **Complexity** | O(N_sources × N_particles) |

Build a `HashMap<ObjectId, usize>` from particles once per tick. The linear `find()` becomes O(N_sources) hash lookups.

---

## TIER 8 — API/MCP/Server Serialization (Low Impact)

### 8.1 `DataSourceStatus::label()` allocates per poll tick

| | |
|---|---|
| **File** | `crates/fieldcad-simulation/src/source.rs:419-426` |
| **Pattern** | `"Ready".to_owned()`, `format!("Failed: {message}")` on every status poll |
| **Frequency** | Every poll tick |

Use `&'static str` or `Cow<'static, str>` for status labels. Note: `CommandKind::label()` (line 205 in the same file) already returns `&'static str` — the efficient pattern exists but isn't used consistently.

### 8.2 MCP snapshot serialization allocates JSON strings

| | |
|---|---|
| **File** | `crates/fieldcad-mcp/src/lib.rs:983-995` |
| **Pattern** | `serde_json::to_string` on snapshot/diagnostic data |
| **Frequency** | Per snapshot resource read |

The report from the sibling review correctly notes that the cited lines handle `DiagnosticsResult` (small struct), not full snapshot dumps. Impact is overstated in that report — severity **P3**.

---

## TIER 9 — Per-Particle Scratch Allocations in Coupling (Low Impact)

### 9.1 `vec![]` per particle in `deposit_charge_conserving_current`

| | |
|---|---|
| **File** | `plugins/electromagnetism/src/coupling.rs:375-376` |
| **Pattern** | `let mut delta = vec![0.0; axis_count]; let mut flux = vec![0.0; axis_count];` |
| **Size** | 2 × axis_count × 8 bytes (axis_count ≈ 3 for a 3D grid = 48 bytes) |
| **Frequency** | Per particle per tick |

`axis_count` is `max(counts.x, counts.y, counts.z)` — typically **3**. Use a fixed `[f64; 3]` stack array or `SmallVec<[f64; 3]>` instead of heap Vecs. This avoids adding persistent state to `ParticleCoupling` and is a mechanical change.

---

## Cross-Cutting Observations

### No arena / object-pool pattern exists
Every per-tick buffer is allocated fresh and drops at end of scope. The `wgpu` staging buffer and `Encoder` are the only reused resources.

### No `Vec::reserve()` on the runtime tick path
There is one `reserve` in `YeeFieldView::new` at `lib.rs:1054` and several `Vec::with_capacity` calls, but the runtime tick path (`apply_tick_inner`, `publish_snapshot`, dynamics) uses `Vec::new()` + `push` throughout.

### Snapshot delivery is correctly `Arc`-based
`Arc<FieldSnapshot>` crosses the runtime→UI boundary with no deep copy after publication — the right pattern.

### `format!()` is pervasive in hot paths
String formatting in `panels.rs`, `plot.rs`, `coupling.rs`, and `runtime.rs` allocates per frame/tick on paths where a static string or `Cow` would serve. Example: `diagnostic_summary()` at `coupling.rs:196` builds a large diagnostic string via `format!()` on every diagnostics call.

### Trait contract (`EquationSystemSolver::forces`/`sample` returning owned Vecs)
Changing from `-> Result<Vec<DVec3>>` to `out_forces: &mut [DVec3] -> Result<()>` would remove allocation at the trait boundary. **Defer until profiled** — the runtime calls `forces()` once per tick per plugin and `sample()` per visible layer per frame. If the allocator handles this without pressure (Vecs sized to body count, typically small), the trait change adds complexity without benefit.

---

## Remediation Priority

| Prio | Finding | File:Line | Est. Impact |
|------|---------|-----------|-------------|
| P0 | `YeeFieldView` centred fields per sample | `lib.rs:1054-1055` | 2× grid alloc removed per sample; highest impact |
| P0 | Grid scratch buffers in `advance_particles` | `coupling.rs:134,136,183` | 3× grid alloc removed per tick |
| P0 | Electrostatic GPU buffers re-created per dispatch | `electrostatics_gpu.rs:120-153` | 5 GPU buffer allocs per evaluate |
| P0 | E/B grid clone on particle-coupled tick | `lib.rs:1394-1395` | 2× grid alloc removed per tick |
| P1 | `field_systems()` clones per frame | `runtime.rs:734-762` | O(channels) allocs per frame |
| P1 | `FieldGeometry` cloned on cache hit | `app.rs:301` | Full mesh clone removed per frame |
| P1 | Per-frame instance buffer collect | `renderer.rs:705` | Multi-element Vec alloc per frame |
| P1 | WorldState deep copy on edit | `world.rs:933` | O(world size) alloc per edit |
| P2 | Triple-collect in analytic plugin sample | `electrostatics.rs:236-243` | 2× redundant Vec per sample |
| P2 | Dynamics integration Vec returns | `dynamics.rs:50,74,97,121` | Per-tick Vec allocs (body-sized) |
| P2 | BTreeMap re-creation per tick | `runtime.rs:1459,1497,1503` | Small per-tick allocs |
| P3 | O(n·m) force evaluation in analytic plugins | `electrostatics.rs:264-285` | O(m)→O(1) per body at high counts |
| P3 | Per-particle scratch Vec in coupling | `coupling.rs:375-376` | Small (48 bytes) per particle per tick |
| P4 | MCP/server string serialization | `source.rs:419-426` | Per-poll String alloc |

---

## Architectural Self-Review

### Corrected Errors

| Error | Original Claim | Correction | Impact on Report |
|-------|---------------|------------|------------------|
| **2.1** | `self.world.clone()` at `runtime.rs:1686` deep-clones World | `World` contains `Arc<WorldState>`; `.clone()` is an Arc bump + 5×u64 Copy. The deep clone is inside `World::commit()` at `world.rs:933` | Severity stays P1 but line number and mechanism were wrong |
| **Cross-cutting "no `reserve`"** | No `Vec::reserve()` usage in codebase | There are `with_capacity` calls in `YeeFieldView::new`, `coupling.rs`, `runtime.rs`. The claim should be "no `reserve()` on the runtime tick path" | Overstated; corrected |

### Findings Merged from Sibling Report

| Finding | Source | Priority | Added As |
|---------|--------|----------|----------|
| Electrostatic GPU buffer re-creation per dispatch | `electrostatics_gpu.rs:120-153` | P0 | Tier 6.2 |
| Per-particle `vec![]` scratch in coupling | `coupling.rs:375-376` | P3 | Tier 9.1 |
| O(N²) particle-to-source lookup in EM | `lib.rs:813-817` | P3 | Tier 7.3 |
| `FieldGeometry` mesh cloned on cache hit | `app.rs:301` | P1 | Tier 3.6 |
| Per-frame instance buffer collect | `renderer.rs:705`, `scene/mod.rs:491` | P1 | Expanded Tier 3.5 |
| MCP/server serialization | `source.rs:419-426`, `mcp/lib.rs:983-995` | P4 | Tier 8 |

### Simpler Alternatives Identified

| Finding | Complex Fix | Simpler Fix |
|---------|-------------|-------------|
| 1.1 E/B grid clone | `YeeFieldStateRef<'a>` with lifetime propagation | `Cow<[DVec3]>` — zero-cost if fields are never mutated; no new lifetimes |
| 1.3 YeeFieldView centred fields | Cache across sample calls within tick | Compute centre values on-the-fly only for cells touched by sample geometry |
| 7.3 O(N²) lookup | Aligned arrays or sorted initialization | `HashMap<ObjectId, usize>` index — ~5 line change |
| 9.1 Per-particle scratch | Persistent scratch buffers in `ParticleCoupling` | `SmallVec<[f64; 3]>` or fixed `[f64; 3]` on stack — axis_count ≈ 3 |
| Trait contract change | `out_forces: &mut [DVec3]` on `EquationSystemSolver` | Defer until profiled; dynamics-internal `&mut Vec` is a quick win without touching the trait |

### What Should Be Deferred

| Proposal | Reason to Defer |
|----------|-----------------|
| Change `EquationSystemSolver::forces` to out-param | Runtime calls it once per tick per plugin with body-sized Vecs. Measure first — the allocator may handle this without pressure |
| Field systems cache in runtime | Invalidation logic is non-trivial (activation/deactivation, channel registration, config changes, realtime toggles). Measure per-frame allocation cost first |
| Copy-on-write `World::commit` | The deep clone only happens on actual edits (not every frame). Profile to confirm it's a bottleneck before refactoring |

### Comparison with Sibling Report (`docs/2026-08-08-performance-memory-audit.md`)

| Dimension | This Report | Sibling Report |
|---|---|---|
| **Factual errors** | 1 (World clone mechanism misattributed — corrected) | 1 (WorldSnapshot deep-clone claim — uncorrected) |
| **Grid-sized findings** | 4 (Tiers 1.1, 1.2, 1.3, 6.2) | 2 (Sections 1.1, 2.3) |
| **Grid issues unique to this report** | advance_particles buffers, YeeFieldView centred alloc | — |
| **Grid issues unique to sibling** | — | Electrostatic GPU buffer re-creation (P0, merged) |
| **Per-frame findings** | 6 (Tiers 3.1–3.6) | 3 (Sections 2.1, 2.2, 2.4) |
| **Trait contract change proposed** | Defer until profiled | Proposed unconditionally |
| **Cross-cutting observations** | Yes | No |
| **Simpler alternatives provided** | Yes (Cow, SmallVec, HashMap index) | No |
| **Structure** | Tiered by impact | Flat by subsystem |
| **Self-review section** | Yes (Architectural Self-Review) | Yes (§5) |

### Recommended Remediation Order

| Order | Finding | Fix | Est. Effort |
|---|---|---|---|
| 1 | 1.3 YeeFieldView centred fields | On-demand centre computation for touched cells only | Medium |
| 2 | 6.2 Electrostatic GPU buffer re-creation | Persistent buffers + `queue.write_buffer` | Small |
| 3 | 1.2 Grid scratch buffers in advance_particles | Pre-allocated scratch in ParticleCoupling | Small |
| 4 | 1.1 E/B grid clone on particle-coupled tick | `Cow<[DVec3]>` in YeeFieldState | Small |
| 5 | 3.6 FieldGeometry mesh cloned on cache hit | `Arc<FieldGeometry>` in cache | Small |
| 6 | 9.1 Per-particle scratch Vec in coupling | `SmallVec<[f64; 3]>` or `[f64; 3]` stack array | Trivial |
| 7 | 3.5 Per-frame instance buffer collect | Persistent `instance_scratch` with clear/extend | Trivial |
| 8 | 7.3 O(N²) lookup in EM source update | `HashMap<ObjectId, usize>` index | Trivial |
| 9 | 5.3–5.4 Dynamics Vec preallocation | `&mut Vec` out-params | Small |
| 10 | 7.1–7.2 O(n·m) force evaluation | HashMap-indexed source lookup | Small |
