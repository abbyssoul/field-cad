# Performance Bottlenecks & Memory Allocation Audit

Date: 2026-08-08
Workspace: `field-cad`
Scope: Full workspace code-base review (`crates/*`, `plugins/*`, `apps/*`)

## Executive Summary

This report presents a static performance and memory allocation audit of the `field-cad` codebase compiled on **August 8, 2026**.

While the domain modeling and physical type safety are robust, several hot simulation loops and per-frame rendering paths perform unnecessary heap allocations, full-struct/grid clones, and $O(N^2)$ iterations.

### Core Findings
1. **Per-Tick Full Field Grid Cloning**: The electromagnetism Yee solver clones full 3D vector grid arrays (`electric` and `magnetic`) on every single physics time step.
2. **Per-Frame Snapshot & Mesh Cloning**: The desktop application clones the entire `WorldSnapshot` and full `FieldGeometry` mesh buffers (vertices and indices) on every GUI render frame.
3. **Per-Particle Heap Allocations**: Current deposition in particle-grid coupling allocates small `Vec<f64>` scratch vectors for *every particle on every time step*.
4. **Transient WGPU Buffer Creation**: The GPU electrostatic evaluator allocates new WGPU uniform, storage, and staging buffers on every compute dispatch instead of reusing persistent GPU buffers.
5. **Trait Contract Allocation Constraints**: Key solver traits (`EquationSystemSolver::forces`, `sample`) return owned `Vec` objects, forcing allocations across all plugins on every sub-step.

---

## 1. Physics Simulation & Time-Step Hot Loops

### 1.1 `YeeFieldState` Full Grid Cloning Every Physics Step
* **Location**: [`plugins/electromagnetism/src/lib.rs:L1393-L1396`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/lib.rs#L1393-L1396)
* **Severity**: **P0 (Critical)**
* **Analysis**:
  ```rust
  self.core.advance_particles(
      &YeeFieldState {
          electric: self.electric.clone(),
          magnetic: self.magnetic.clone(),
      },
      context.time_step.seconds(),
  )
  ```
  Both `self.electric` and `self.magnetic` are full 3D spatial grids (`Vec<DVec3>`). Calling `.clone()` duplicates millions of 64-bit floats on every time step.
* **Recommendation**: Refactor `YeeFieldState` or `advance_particles` to accept borrowed slices (`&[DVec3]`):
  ```rust
  pub struct YeeFieldStateRef<'a> {
      pub electric: &'a [DVec3],
      pub magnetic: &'a [DVec3],
  }
  ```

---

### 1.2 Per-Particle Current Deposition Scratch Allocations
* **Location**: [`plugins/electromagnetism/src/coupling.rs:L375-L376`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/coupling.rs#L375-L376)
* **Severity**: **P1 (High)**
* **Analysis**:
  ```rust
  let mut delta = vec![0.0; axis_count];
  let mut flux = vec![0.0; axis_count];
  ```
  `deposit_charge_conserving_current` is invoked per particle per step. Allocating two heap `Vec`s per particle starves the heap allocator and hurts CPU L1/L2 cache locality.
* **Recommendation**: Store reusable scratch vectors inside `ParticleCoupling` (or pass `&mut [f64]` slices down from a scratch context):
  ```rust
  self.scratch_delta.clear();
  self.scratch_delta.resize(axis_count, 0.0);
  ```

---

### 1.3 $O(N^2)$ Particle-to-Source Lookup in Physics Loop
* **Location**: [`plugins/electromagnetism/src/lib.rs:L813-L817`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/lib.rs#L813-L817)
* **Severity**: **P1 (High)**
* **Analysis**:
  ```rust
  for source in &mut self.sources {
      if let Some(particle) = coupling.particles().iter().find(|p| p.object == source.object) { ... }
  }
  ```
  Calling `.find()` inside a loop over sources leads to $O(N_{\text{sources}} \times N_{\text{particles}})$ iteration per step.
* **Recommendation**: Maintain aligned arrays, index maps, or sort particles by `ObjectId` during initialization to allow $O(N)$ linear zipping.

---

### 1.4 Dynamic Body Integration & Accumulation Allocations
* **Location**: [`crates/fieldcad-dynamics/src/lib.rs:L47, L74, L97, L121`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-dynamics/src/lib.rs#L47)
* **Severity**: **P1 (High)**
* **Analysis**:
  - `collect_bodies` allocates two vectors (`Vec::new()`) from scratch on every call.
  - `accumulate_forces` allocates `vec![DVec3::ZERO; bodies]` per step.
  - `integrate` and `carry` collect iterator outputs into temporary `Vec<ObjectKinematicsUpdate>` arrays.
* **Recommendation**: Accept `&mut Vec<DynamicBody>` and `&mut [DVec3]` parameters to allow the caller to retain capacity across simulation frames.

---

### 1.5 `BTreeMap` Re-allocation in Simulation Runtime Ticks
* **Location**: [`crates/fieldcad-simulation/src/runtime.rs:L1459, L1497, L1503`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-simulation/src/runtime.rs#L1459)
* **Severity**: **P2 (Medium)**
* **Analysis**:
  - `kinematic_owners` BTreeMap created fresh per tick.
  - `self.last_forces = bodies.iter().zip(...).collect()` drops the old map and allocates a brand new node tree every tick.
  - `kinematics` BTreeMap created fresh per tick.
* **Recommendation**: Retain `self.last_forces` and auxiliary maps in `SimulationRuntime`. Call `.clear()` and `.extend(...)` to reuse existing BTreeMap node allocations.

---

## 2. Desktop App & WGPU Rendering Bottlenecks

### 2.1 Full `WorldSnapshot` Cloned Every Frame
* **Location**: [`apps/fieldcad-desktop/src/app.rs:L697`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/app.rs#L697)
* **Severity**: **P0 (Critical)**
* **Analysis**:
  ```rust
  let world = self.world.clone();
  ```
  `WorldSnapshot` holds scene metadata, object maps, plane definitions, and field states. Deep cloning this object on every GUI tick (60–144Hz) generates high allocator pressure.
* **Recommendation**: Borrow `&self.world` immutably directly from `self` into `ui::show` and `ui::FrameContext`.

---

### 2.2 `FieldGeometry` Mesh Data Cloned on Cache Hit
* **Location**: [`apps/fieldcad-desktop/src/app.rs:L301`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/app.rs#L301) (`compute_field_layer_geometry`)
* **Severity**: **P1 (High)**
* **Analysis**:
  Even when field geometries are cached in `FieldGeometryCache`, retrieving them clones `FieldGeometry`, which contains large `Vec<DVec3>` and `Vec<u32>` vertex/index buffers.
* **Recommendation**: Store mesh geometries in `Arc<FieldGeometry>` or return references `&FieldGeometry` from cache lookups.

---

### 2.3 Per-Dispatch WGPU GPU Buffer Re-creation
* **Location**: [`apps/fieldcad-desktop/src/electrostatics_gpu.rs:L120-L153`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/electrostatics_gpu.rs#L120-L153)
* **Severity**: **P0 (Critical)**
* **Analysis**:
  ```rust
  let params_buffer = self.device.create_buffer_init(...);
  let source_buffer = self.device.create_buffer_init(...);
  let position_buffer = self.device.create_buffer_init(...);
  let output_buffer = self.device.create_buffer(...);
  let staging_buffer = self.device.create_buffer(...);
  ```
  `evaluate_batch` creates 5 new `wgpu::Buffer` objects on every call.
* **Recommendation**: Maintain persistent GPU buffers inside `GpuElectrostaticEvaluator`. Reallocate buffers only when required size exceeds current capacity, and use `queue.write_buffer` for updates.

---

### 2.4 Per-Frame Instance Buffer Vector Collection
* **Location**: [`apps/fieldcad-desktop/src/renderer.rs:L705-L714`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/renderer.rs#L705) & [`apps/fieldcad-desktop/src/scene/mod.rs:L491-L505`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/scene/mod.rs#L491)
* **Severity**: **P1 (High)**
* **Analysis**:
  `SceneRenderer::update` calls `scene.instances().collect::<Vec<InstanceRaw>>()` every frame before uploading to the GPU instance buffer.
* **Recommendation**: Store a persistent `instance_scratch: Vec<InstanceRaw>` in `SceneRenderer`. Call `.clear()` and append instances directly before GPU upload.

---

## 3. API, MCP & Server Serialization Bottlenecks

### 3.1 Redundant JSON Serialization & String Formatting
* **Location**: [`crates/fieldcad-mcp/src/lib.rs:L983-L995`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-mcp/src/lib.rs#L983-L995) & [`crates/fieldcad-simulation/src/source.rs:L419-L426`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-simulation/src/source.rs#L419-L426)
* **Severity**: **P2 (Medium)**
* **Analysis**:
  - `DataSourceStatus::label()` creates owned `String` instances (e.g. `"Ready".to_owned()`, `format!("Failed: {message}")`) on every poll tick, even when status is unchanged.
  - MCP `resource_text` converts snapshots into JSON strings using `serde_json::to_string` and wraps them in `ContentBlock::text`, causing repeated multi-megabyte string allocations.
* **Recommendation**: Use `&'static str` or `Cow<'static, str>` for status labels. Stream JSON output directly into writers where possible.

---

## 4. Solver Trait Contract Recommendations

### 4.1 Transition from Allocating Returns to Out-Parameter Slices
* **Location**: [`crates/fieldcad-plugin-api/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-plugin-api/src/lib.rs) (`EquationSystemSolver`)
* **Current API**:
  ```rust
  fn forces(&self, bodies: &[DynamicBody]) -> Result<Vec<DVec3>, PluginError>;
  fn sample(&self, handle: ChannelHandle, geometry: &SampleGeometry) -> Result<SampledColumn, PluginError>;
  ```
* **Proposed Zero-Allocation API**:
  ```rust
  fn forces(&self, bodies: &[DynamicBody], out_forces: &mut [DVec3]) -> Result<(), PluginError>;
  fn sample(&self, handle: ChannelHandle, geometry: &SampleGeometry, out_column: &mut SampledColumn) -> Result<(), PluginError>;
  ```
* **Benefit**: Allows the runtime simulation loop to allocate buffers *once* at startup and reuse them across millions of time-steps and sub-step integrations (e.g. RK4).

---

## Priority Remediation Matrix

| Priority | Component | File Path | Issue Summary | Recommended Fix |
| :--- | :--- | :--- | :--- | :--- |
| **P0 (Critical)** | `electromagnetism` | [`plugins/electromagnetism/src/lib.rs:L1393`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/lib.rs#L1393) | Full Yee grid `.clone()` per physics step | Pass `&[DVec3]` borrowed slices |
| **P0 (Critical)** | `fieldcad-desktop` | [`apps/fieldcad-desktop/src/app.rs:L697`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/app.rs#L697) | `WorldSnapshot` `.clone()` per frame | Pass by reference `&self.world` into UI |
| **P0 (Critical)** | `electrostatics_gpu` | [`apps/fieldcad-desktop/src/electrostatics_gpu.rs:L120`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/electrostatics_gpu.rs#L120) | Re-creating WGPU buffers per dispatch | Persistent GPU buffers with `queue.write_buffer` |
| **P1 (High)** | `electromagnetism` | [`plugins/electromagnetism/src/coupling.rs:L375`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/coupling.rs#L375) | Current deposition `vec![]` allocs per particle | Add persistent scratch buffers to `ParticleCoupling` |
| **P1 (High)** | `fieldcad-desktop` | [`apps/fieldcad-desktop/src/app.rs:L301`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/app.rs#L301) | `FieldGeometry` mesh vector cloning on hit | Wrap cached geometry in `Arc<FieldGeometry>` |
| **P1 (High)** | `fieldcad-dynamics` | [`crates/fieldcad-dynamics/src/lib.rs:L74`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-dynamics/src/lib.rs#L74) | Temp vectors in `accumulate_forces` & `integrate` | Pass mutable out-slices `&mut [DVec3]` |
| **P2 (Medium)** | `fieldcad-simulation` | [`crates/fieldcad-simulation/src/runtime.rs:L1459`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-simulation/src/runtime.rs#L1459) | BTreeMap dropping and re-creation per tick | Call `.clear()` and `.extend()` on persistent maps |
| **P2 (Medium)** | `plugin-api` | [`crates/fieldcad-plugin-api/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-plugin-api/src/lib.rs) | Trait signatures returning owned `Vec`s | Out-parameter slice parameters (`&mut [T]`) |

---

## 5. Architectural Review of This Report

*Reviewer: Lead Architectural Critic*
*Date: 2026-08-08*
*Method: Static code verification of every cited location; type inspection of every cited struct.*

### 5.1 Factual Error: `WorldSnapshot` Is Not Deep-Cloned (Section 2.1)

**The report's P0 classification for Section 2.1 is wrong.**

`WorldSnapshot` is defined as `pub struct WorldSnapshot(Arc<WorldState>)` (`crates/fieldcad-core/src/world.rs:757`). Calling `.clone()` on it performs an `Arc::clone()` — an atomic reference-count increment, not a deep copy of scene metadata, object maps, or field states. The cost is ~2 atomic instructions, not "high allocator pressure."

The recommendation to borrow `&self.world` directly is still stylistically cleaner (avoids an unnecessary local binding), but the severity should be **P3 (Low)**, not P0. The report's language ("Deep cloning this object on every GUI tick") is materially incorrect and inflates the perceived urgency of the findings.

### 5.2 MCP Serialization Claim Is Overblown (Section 3.1)

The report states that MCP `resource_text` "converts snapshots into JSON strings … causing repeated multi-megabyte string allocations." The cited lines (983–995) show `DiagnosticsResult` serialization — a small struct carrying `snapshot.identity` and a `Vec<SolverDiagnostic>`. This is not a full snapshot dump. The snapshot *is* serialized at the earlier `SESSION_SNAPSHOT_URI` branch (line 975 area), but the report conflates two different code paths without distinguishing their sizes. The `serde_json::to_string` pattern is real but the payloads are diagnostic-sized, not multi-megabyte. Severity should be **P3**.

### 5.3 Existing `&'static str` Variant Missed (Section 3.1)

`source.rs` contains *two* `label()` methods:
- `CommandKind::label()` → `&'static str` (line 205) — already optimal, not mentioned.
- `DataSourceStatus::label()` → `String` (line 419) — the one flagged.

The report's recommendation is correct for the status variant, but failing to note the existing efficient variant weakens the impression of thoroughness.

### 5.4 Simpler Alternatives for Several Findings

#### 5.4.1 Yee Grid Clone (Section 1.1) — Use `Cow` Before Lifetime Propagation

The proposed `YeeFieldStateRef<'a>` introduces a lifetime parameter that propagates through `advance_particles` → `ParticleCoupling::advance` → `deposit_charge_conserving_current` — a non-trivial refactor touching ~4–5 functions. A simpler first step: change `YeeFieldState` to hold `Cow<'_, [DVec3]>`. The caller passes `Cow::Borrowed(&self.electric)` in the hot path, avoiding the full clone without touching every downstream signature. Only if a callee mutates the fields does it clone. **Check whether `advance_particles` or its callees mutate the fields** — if not, `Cow::Borrowed` is zero-cost.

#### 5.4.2 Per-Particle Scratch (Section 1.2) — Stack-Allocate for Small `axis_count`

`axis_count` is `max(counts.x, counts.y, counts.z)` — typically **3** for a 3D grid. A `vec![0.0; axis_count]` heap-allocates 24 bytes per particle per step. Use `SmallVec<[f64; 3]>` or a fixed `[f64; 3]` on the stack. This avoids adding persistent state to `ParticleCoupling` and is a mechanical change.

#### 5.4.3 O(N²) Lookup (Section 1.3) — `HashMap<ObjectId, usize>` Index

Instead of aligning arrays or sorting, build a `HashMap<ObjectId, usize>` from particles once per tick. The `find()` loop becomes `O(N_sources)` hash lookups. This is a ~5-line change and does not restructure the data model.

#### 5.4.4 Dynamics Allocations (Section 1.4) — `&mut Vec` Out-Params

Change `collect_bodies` to accept `&mut Vec<DynamicBody>` and `&mut Vec<DynamicBody>` for the two output partitions. The caller retains capacity across ticks. Same pattern for `accumulate_forces` → `&mut [DVec3]` out-param. This is a quick win that avoids the trait-level change (Section 4.1) for the dynamics crate alone.

### 5.5 Trait Contract Change (Section 4.1) — Defer Until Profiled

The proposed change from `fn forces(&self, bodies: &[DynamicBody]) -> Result<Vec<DVec3>>` to `fn forces(&self, bodies: &[DynamicBody], out_forces: &mut [DVec3]) -> Result<()>` is architecturally significant: it changes the `EquationSystemSolver` trait that every plugin implements. The `sample` change is even more impactful — `SampledColumn` contains a `FieldColumn` enum with `Vec` variants inside.

**Before undertaking this migration, measure whether these allocations appear in profiles.** The runtime calls `forces()` once per tick per plugin, and `sample()` per visible layer per frame. If the allocator handles this without pressure (the `Vec`s are sized to `bodies.len()`, typically small), the trait change adds complexity without benefit. Measure first, optimize second.

### 5.6 Corrected Priority Matrix

| # | Section | Reported Severity | Corrected Severity | Notes |
| :--- | :--- | :--- | :--- | :--- |
| 1.1 | Yee grid clone | P0 | **P1** | Real issue; `Cow` is simpler than lifetime propagation |
| 1.2 | Per-particle scratch | P1 | P1 | Use `SmallVec` or stack array |
| 1.3 | O(N²) lookup | P1 | P1 | HashMap index is simpler |
| 1.4 | Dynamics allocations | P1 | P1 | `&mut Vec` out-params |
| 1.5 | BTreeMap re-allocation | P2 | P2 | Confirmed |
| **2.1** | **WorldSnapshot clone** | **P0** | **P3** | **`Arc` bump, not deep clone — factual error** |
| 2.2 | FieldGeometry cache | P1 | P1 | `Arc` is correct |
| 2.3 | WGPU buffer re-creation | P0 | P0 | Confirmed; most impactful finding |
| 2.4 | Instance buffer collect | P1 | P1 | Confirmed |
| 3.1 | MCP serialization | P2 | **P3** | Not multi-megabyte; conflates two paths |
| 4.1 | Trait contract | unrated | **P2** | Defer until profiling confirms it matters |

### 5.7 Summary

The report correctly identifies several genuine allocation hot-spots (Yee grid clone, WGPU buffer re-creation, O(N²) lookup, dynamics allocations). Its most valuable finding is the WGPU buffer re-creation (Section 2.3), which is a genuine P0. However, the report contains one factual error (WorldSnapshot is `Arc`, not deep-cloned) that inflates its P0 count and undermines credibility. Several recommendations can be implemented with simpler approaches than proposed. The trait contract change (Section 4.1) should be deferred until profiling data justifies the migration cost.
