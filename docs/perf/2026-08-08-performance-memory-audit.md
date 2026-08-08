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
