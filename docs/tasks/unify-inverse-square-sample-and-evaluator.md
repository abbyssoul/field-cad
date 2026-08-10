# Task: unify inverse-square sample types and evaluator traits

## Short prompt

Merge `ElectrostaticSample` and `NewtonianSample` into the single
`InverseSquareSample` type (already in `fieldcad-superposition`), unify the
per-plugin `*BatchEvaluator` traits into a shared
`InverseSquareBatchEvaluator` in `fieldcad-superposition`, and eliminate
`crates/fieldcad-newtonian-gravity`. Both plugins (electrostatics and gravity)
end up with identical structure — a thin plugin wrapper over the shared
superposition kernel — differing only in source type, coupling constant, and
channel identity.

## Motivation

Electrostatics (Coulomb's law) and Newtonian gravity are the same
inverse-square coupling law with a different constant and sign. The codebase
reflected this in two different ways:

- **Electrostatics** (`plugins/electrostatics`): monolithic, with its own
  `ElectrostaticSample` type and `ElectrostaticBatchEvaluator` trait, calling
  `fieldcad-superposition` directly.
- **Gravity**: split across `crates/fieldcad-newtonian-gravity` (kernel crate
  with `NewtonianSample`) and `plugins/gravity` (plugin wrapper with
  `GravityBatchEvaluator`), with the kernel crate doing an extra wrapping
  layer over `fieldcad-superposition`.

This asymmetry adds unnecessary types (`ElectrostaticSample`, `NewtonianSample`,
duplicate evaluator traits) and an extra crate with no unique responsibility.
The concrete sample types never leave the plugin boundary anyway — they're
converted to generic `FieldColumn` at `sample()`.

A single `InverseSquareSample` (with `gradient: Option<DMat3>`) and a single
`InverseSquareBatchEvaluator` trait replace all four types. The CPU path
advertises a batch gradient (the closed-form Jacobian is cheap); the GPU path
does not (the WGSL shader does not compute it). Gradient availability is a
per-evaluator, per-batch capability, not a property that an undefined point may
silently withdraw from the rest of the batch. `sample()` uses that capability to
decide whether to attach a `GradientColumn`.

The desktop also has one GPU adapter, `GpuInverseSquareEvaluator`, directly
implementing the shared trait. The electrostatic and gravity GPU wrappers are
deleted: after source conversion and coupling selection move to the plugins,
they contain no equation-system-specific behaviour.

## Files affected

| File | Change |
|------|--------|
| `crates/fieldcad-superposition/src/lib.rs` | Modify `InverseSquareSample`, add `InverseSquareBatchEvaluator`, add `CpuInverseSquareEvaluator` |
| `crates/fieldcad-superposition/Cargo.toml` | No changes needed |
| `plugins/electrostatics/src/lib.rs` | Remove `ElectrostaticSample`, use `InverseSquareSample`; remove `ElectrostaticBatchEvaluator`, use `InverseSquareBatchEvaluator`; update plugin and solver |
| `plugins/electrostatics/Cargo.toml` | No changes needed (already depends on `fieldcad-superposition`) |
| `plugins/gravity/src/lib.rs` | Remove `NewtonianSample`, use `InverseSquareSample`; remove `GravityBatchEvaluator`, use `InverseSquareBatchEvaluator`; add source-conversion fn; add `fieldcad-superposition` dep; update solver |
| `plugins/gravity/Cargo.toml` | Remove `fieldcad-newtonian-gravity`, add `fieldcad-superposition` |
| `apps/fieldcad-desktop/src/electrostatics_gpu.rs` | Delete — its adapter has no remaining electrostatics-specific behaviour |
| `apps/fieldcad-desktop/src/gravity_gpu.rs` | Delete — its adapter has no remaining gravity-specific behaviour |
| `apps/fieldcad-desktop/src/gpu_inverse_square.rs` | Implement `InverseSquareBatchEvaluator` directly; return `InverseSquareSample` with `gradient: None` |
| `apps/fieldcad-desktop/src/app.rs` | Update evaluator creation and plugin wiring to use unified trait |
| `apps/fieldcad-desktop/Cargo.toml` | Remove `fieldcad-newtonian-gravity` |
| `crates/fieldcad-bench/Cargo.toml` | Remove `fieldcad-newtonian-gravity` |
| `Cargo.toml` (workspace root) | Remove `crates/fieldcad-newtonian-gravity` from workspace members and workspace dependencies |
| `Cargo.lock` | Regenerate after deleting the crate |
| `PLAN.md`, `docs/orishu-integration-plan.md` | Update current-state references to the deleted crate; preserve dated review records as history |

**Deleted:**
| Path | Reason |
|------|--------|
| `crates/fieldcad-newtonian-gravity/` | Entire crate — responsibility absorbed into `plugins/gravity` + `fieldcad-superposition` |
| `apps/fieldcad-desktop/src/electrostatics_gpu.rs` | Duplicate GPU adapter absorbed into `gpu_inverse_square.rs` |
| `apps/fieldcad-desktop/src/gravity_gpu.rs` | Duplicate GPU adapter absorbed into `gpu_inverse_square.rs` |

## Migration sequence

### Phase 1 — `fieldcad-superposition`

**1a. Change `InverseSquareSample.gradient` to `Option<DMat3>`**

The CPU analytical solver always computes the closed-form Jacobian;
`evaluate_sources()` wraps it in `Some(...)`. An undefined sample retains a
finite placeholder (`Some(DMat3::ZERO)`): its derivative is not meaningful, but
its `SampleValidity` already marks that and the placeholder prevents one
undefined position from removing the CPU gradient capability from every other
position in the batch. The GPU adapter alone reports `None` for every sample.
Doc comment explains the invariant:

```rust
/// The field's own Jacobian (`∂field_i/∂x_j` in column `j`).
///
/// `Some` for every sample from the CPU analytical solver
/// (`evaluate_sources` in this crate), including an undefined sample whose
/// validity makes its zero placeholder unusable. An evaluator without
/// derivative support (the current GPU adapter) reports `None` for every
/// sample in a batch. Callers use that batch-wide capability to decide whether
/// to attach a `GradientColumn` to the published `SampledColumn`.
pub gradient: Option<DMat3>,
```

In `fn undefined()`:
```rust
fn undefined(reason: UndefinedReason) -> Self {
    Self {
        field: DVec3::ZERO,
        potential: 0.0,
        gradient: Some(DMat3::ZERO),
        validity: SampleValidity::Undefined(reason),
    }
}
```

In `evaluate_sources()` at the point where `InverseSquareSample` is constructed:
```rust
InverseSquareSample {
    field,
    potential,
    gradient: Some(gradient),  // wrap in Some
    validity,
}
```

Also update the `field` field doc: was `electric_field` / `acceleration` in
the plugin types; now it's just `field` (already what `InverseSquareSample`
uses).

**1b. Add `InverseSquareBatchEvaluator` trait**

```rust
/// Batch evaluator for an inverse-square coupling law.
///
/// A single evaluator can serve both electrostatic (Coulomb) and
/// gravitational equation systems — the caller supplies the coupling
/// constant (sign and magnitude) separately. A successful batch reports
/// gradient availability uniformly: every sample has `Some(gradient)`, or
/// every sample has `None`.
pub trait InverseSquareBatchEvaluator: Send + Sync {
    /// The numerical precision this evaluator produces (e.g. `F64` for the
    /// CPU oracle, `F32` for the WGSL compute shader).
    fn precision(&self) -> Precision;

    /// Evaluate both vector and scalar (potential) channels at every sample
    /// position described by `geometry`. On success, the returned vector has
    /// exactly `geometry.len()` entries; a backend that cannot meet that
    /// contract returns `Err`, never a partial result.
    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String>;

    /// [`Self::evaluate`], writing into a caller-owned buffer.
    ///
    /// `out.len()` must equal `geometry.len()`. Implementations return `Err`
    /// for a mismatch rather than panicking or leaving part of `out` stale.
    fn evaluate_into(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
        out: &mut [InverseSquareSample],
    ) -> Result<(), String> {
        if out.len() != geometry.len() {
            return Err(format!(
                "inverse-square output buffer has length {}, expected {}",
                out.len(),
                geometry.len()
            ));
        }
        let evaluated = self.evaluate(coupling_constant, sources, domain, geometry)?;
        if evaluated.len() != geometry.len() {
            return Err(format!(
                "inverse-square evaluator returned {} samples for a geometry of length {}",
                evaluated.len(),
                geometry.len()
            ));
        }
        out.copy_from_slice(&evaluated);
        Ok(())
    }
}
```

Needs these imports:
```rust
use fieldcad_core::{Domain, Precision, SampleGeometry};
```

**1c. Add `CpuInverseSquareEvaluator`**

```rust
/// The reference CPU `f64` oracle. Iterates `geometry.positions()`, calls
/// [`evaluate_sources`] for each with the given coupling constant.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuInverseSquareEvaluator;

impl InverseSquareBatchEvaluator for CpuInverseSquareEvaluator {
    fn precision(&self) -> Precision {
        Precision::F64
    }

    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String> {
        Ok(geometry
            .positions()
            .map(|position| evaluate_sources(coupling_constant, sources.iter().copied(), position))
            .collect())
    }

    fn evaluate_into(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
        out: &mut [InverseSquareSample],
    ) -> Result<(), String> {
        if out.len() != geometry.len() {
            return Err(format!(
                "inverse-square output buffer has length {}, expected {}",
                out.len(),
                geometry.len()
            ));
        }
        for (position, out) in geometry.positions().zip(out) {
            *out = evaluate_sources(coupling_constant, sources.iter().copied(), position);
        }
        Ok(())
    }
}
```

### Phase 2 — `plugins/electrostatics`

**2a. Replace `ElectrostaticSample` with `InverseSquareSample`**

Delete the `ElectrostaticSample` struct definition. All uses of
`ElectrostaticSample` become `InverseSquareSample` from
`fieldcad-superposition`.

Import change:
```rust
// Before
use fieldcad_superposition::InverseSquareSource;

// After
use fieldcad_superposition::{InverseSquareBatchEvaluator, InverseSquareSample, InverseSquareSource};
```

**2b. Replace `ElectrostaticBatchEvaluator` trait**

Remove the trait definition. The plugin stores `Arc<dyn InverseSquareBatchEvaluator>` instead.

Plugin struct change:
```rust
// Before
pub struct ElectrostaticsPlugin {
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
}

// After
pub struct ElectrostaticsPlugin {
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
}
```

Constructor:
```rust
impl ElectrostaticsPlugin {
    pub fn new() -> Self {
        Self {
            evaluator: Arc::new(fieldcad_superposition::CpuInverseSquareEvaluator),
        }
    }

    pub fn with_evaluator(evaluator: Arc<dyn InverseSquareBatchEvaluator>) -> Self {
        Self { evaluator }
    }
}
```

(Note: `CpuInverseSquareEvaluator` is `Default` so can be written as
`CpuInverseSquareEvaluator` or `CpuInverseSquareEvaluator::default()`.)

**2c. Replace `CpuBatchEvaluator`**

The struct and its `ElectrostaticBatchEvaluator` impl are removed. The CPU path
is now `CpuInverseSquareEvaluator` from `fieldcad-superposition`, which the
plugin uses via `Arc::new(CpuInverseSquareEvaluator)`.

**2d. Update `evaluate_sources` convenience fn**

```rust
// Before
pub fn evaluate_sources(sources: &[ChargeSource], position: DVec3) -> ElectrostaticSample {
    let sample = fieldcad_superposition::evaluate_sources(
        COULOMB_CONSTANT,
        sources.iter().map(inverse_square_source),
        position,
    );
    ElectrostaticSample {
        electric_field: sample.field,
        potential: sample.potential,
        gradient: Some(sample.gradient),
        validity: sample.validity,
    }
}

// After — returns InverseSquareSample directly
pub fn evaluate_sources(sources: &[ChargeSource], position: DVec3) -> InverseSquareSample {
    fieldcad_superposition::evaluate_sources(
        COULOMB_CONSTANT,
        sources.iter().map(inverse_square_source),
        position,
    )
}
```

The gradient is already `Some(...)` from `fieldcad_superposition::evaluate_sources`.

**2e. Update `Solver`**

Change cache type and evaluator field:
```rust
// Before
struct ElectrostaticsSolver {
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
    cache: SampleCache<ElectrostaticSample>,
    // ...
}

// After
struct ElectrostaticsSolver {
    // Rebuilt with the object-indexed sources on creation/world changes; this
    // is the cache-local input shape for the shared evaluator.
    inverse_square_sources: Vec<InverseSquareSource>,
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    cache: SampleCache<InverseSquareSample>,
    // ...
}
```

Build `inverse_square_sources` when constructing the solver and rebuild it in
`on_world_changed`, immediately after replacing `self.sources`. Do not convert
inside `samples_for`: that method runs for both channels even when the sample
cache is a hit.

**2f. Update `samples_for`**

Source conversion happens at the plugin level now:
```rust
fn samples_for(&self, geometry: &SampleGeometry) -> Result<Arc<[InverseSquareSample]>, PluginError> {
    self.cache.get_or_try_insert_with(
        geometry,
        || {
            let evaluated = self
                .evaluator
                .evaluate(COULOMB_CONSTANT, &self.inverse_square_sources, &self.domain, geometry)
                .map_err(PluginError::Solver)?;
            if evaluated.len() != geometry.len() {
                return Err(PluginError::Solver(/* length mismatch */));
            }
            Ok(evaluated)
        },
        |out| {
            self.evaluator
                .evaluate_into(COULOMB_CONSTANT, &self.inverse_square_sources, &self.domain, geometry, out)
                .map_err(PluginError::Solver)
        },
    )
}
```

**2g. Update `sample()`**

```rust
fn sample(&self, channel: ChannelHandle, geometry: &SampleGeometry) -> Result<SampledColumn, PluginError> {
    let samples = self.samples_for(geometry)?;
    let validity = samples.iter().map(|s| s.validity).collect();
    let gradients = samples.iter().map(|s| s.gradient).collect::<Option<Vec<_>>>();
    match channel {
        ELECTRIC_FIELD_HANDLE => {
            let column = SampledColumn::new(
                FieldColumn::vectors(samples.iter().map(|s| s.field).collect()),
                validity,
            );
            Ok(match gradients {
                Some(jacobians) => column.with_gradient(GradientColumn::Vector(jacobians.into())),
                None => column,
            })
        }
        ELECTRIC_POTENTIAL_HANDLE => {
            let column = SampledColumn::new(
                FieldColumn::scalars(samples.iter().map(|s| s.potential).collect()),
                validity,
            );
            // ∇φ = −E
            Ok(match gradients {
                Some(_) => column.with_gradient(GradientColumn::Scalar(
                    samples.iter().map(|s| -s.field).collect(),
                )),
                None => column,
            })
        }
        other => Err(PluginError::UnknownChannel(other.index())),
    }
}
```

Changes:
- `s.electric_field` → `s.field`
- No change to gradient logic

**2h. Update `create_solver`**

```rust
// Validator: `context.domain.precision() != self.evaluator.precision()` — unchanged
```

Only the type in `Ok(Box::new(ElectrostaticsSolver { ... }))` changes because
`SampleCache<InverseSquareSample>` has a different type param. Same capacity,
same structure.

**2i. Update tests**

All test assertions that access `.electric_field` → `.field`:

```rust
// Before
assert_eq!(sample.electric_field.x, COULOMB_CONSTANT * 2.0e-9, 1.0e-14);
assert_eq!(sample.electric_field.y, 0.0);

// After
assert_eq!(sample.field.x, COULOMB_CONSTANT * 2.0e-9, 1.0e-14);
assert_eq!(sample.field.y, 0.0);
```

The `CountingEvaluator` test mock needs updating:
```rust
// Before
impl ElectrostaticBatchEvaluator for CountingEvaluator { ... }

// After — implement InverseSquareBatchEvaluator
impl InverseSquareBatchEvaluator for CountingEvaluator {
    fn precision(&self) -> Precision { Precision::F32 }
    fn evaluate(&self, _coupling_constant: f64, _sources: &[InverseSquareSource], _domain: &Domain, geometry: &SampleGeometry) -> Result<Vec<InverseSquareSample>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![
            InverseSquareSample {
                field: DVec3::X,
                potential: 2.0,
                gradient: None,
                validity: SampleValidity::Exact,
            };
            geometry.len()
        ])
    }
}
```

### Phase 3 — `plugins/gravity`

**3a. Update `Cargo.toml`**

```toml
# Before
[dependencies]
fieldcad-core = { workspace = true }
fieldcad-sources = { workspace = true }
fieldcad-newtonian-gravity = { workspace = true }
fieldcad-plugin-api = { workspace = true }
glam = { workspace = true }

# After
[dependencies]
fieldcad-core = { workspace = true }
fieldcad-sources = { workspace = true }
fieldcad-superposition = { workspace = true }
fieldcad-plugin-api = { workspace = true }
glam = { workspace = true }
```

**3b. Add source-conversion function**

Make a public `inverse_square_source` (mirroring the electrostatics plugin's):

```rust
/// Map a [`CoupledSource<MassKg>`] into the kernel's generic source type.
pub fn inverse_square_source(source: &CoupledSource<MassKg>) -> InverseSquareSource {
    InverseSquareSource {
        position: source.position,
        strength: source.coupling_value.into_si(),
        distribution: source.distribution,
    }
}
```

Also add a convenience `evaluate_sources` (mirroring electrostatics):

```rust
/// Evaluate gravitational field and potential at a single position from all
/// given mass sources.
pub fn evaluate_sources(sources: &[CoupledSource<MassKg>], position: DVec3) -> InverseSquareSample {
    fieldcad_superposition::evaluate_sources(
        -GRAVITATIONAL_CONSTANT,
        sources.iter().map(inverse_square_source),
        position,
    )
}
```

**3c. Remove `NewtonianSample` usage**

All `NewtonianSample` references become `InverseSquareSample`.

Import change:
```rust
// Before
use fieldcad_newtonian_gravity::{
    NewtonianSample, evaluate_acceleration_excluding, evaluate_geometry, evaluate_geometry_into,
};

// After
use fieldcad_superposition::{
    InverseSquareBatchEvaluator, InverseSquareSample, InverseSquareSource,
};
```

**3d. Remove `GravityBatchEvaluator` trait and `CpuGravityEvaluator`**

Replace the trait impl with `CpuInverseSquareEvaluator` from
`fieldcad-superposition` (same as electrostatics).

Plugin struct:
```rust
// Before
pub struct NewtonianGravityPlugin {
    evaluator: Arc<dyn GravityBatchEvaluator>,
}

// After
pub struct NewtonianGravityPlugin {
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
}
```

Constructor:
```rust
impl NewtonianGravityPlugin {
    pub fn new() -> Self {
        Self {
            evaluator: Arc::new(fieldcad_superposition::CpuInverseSquareEvaluator),
        }
    }

    pub fn with_evaluator(evaluator: Arc<dyn InverseSquareBatchEvaluator>) -> Self {
        Self { evaluator }
    }
}
```

**3e. Update solver**

```rust
// Before
struct NewtonianGravitySolver {
    sources: ObjectIndex<CoupledSource<MassKg>>,
    // Rebuilt with the object-indexed sources on creation/world changes; this
    // is the cache-local input shape for the shared evaluator.
    inverse_square_sources: Vec<InverseSquareSource>,
    evaluator: Arc<dyn GravityBatchEvaluator>,
    cache: SampleCache<NewtonianSample>,
    // ...
}

// After
struct NewtonianGravitySolver {
    sources: ObjectIndex<CoupledSource<MassKg>>,
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    cache: SampleCache<InverseSquareSample>,
    // ...
}
```

Build `inverse_square_sources` when constructing the solver and rebuild it in
`on_world_changed`, immediately after replacing `self.sources`. The
object-indexed sources remain necessary for force ownership/exclusion; the
converted vector is the evaluator's cache-local input shape.

**3f. Update `samples_for`**

Same pattern as electrostatics — convert sources at plugin level:

```rust
fn samples_for(&self, geometry: &SampleGeometry) -> Result<Arc<[InverseSquareSample]>, PluginError> {
    self.cache.get_or_try_insert_with(geometry, || {
        self.evaluator.evaluate(-GRAVITATIONAL_CONSTANT, &self.inverse_square_sources, &self.domain, geometry)
            .map_err(PluginError::Solver)
    }, |out| {
        self.evaluator.evaluate_into(-GRAVITATIONAL_CONSTANT, &self.inverse_square_sources, &self.domain, geometry, out)
            .map_err(PluginError::Solver)
    })
}
```

**3g. Update `sample()`**

```rust
fn sample(&self, channel: ChannelHandle, geometry: &SampleGeometry) -> Result<SampledColumn, PluginError> {
    let samples = self.samples_for(geometry)?;
    let validity = samples.iter().map(|s| s.validity).collect();
    // Gradients: optional — CPU gives Some, GPU gives None
    let gradients = samples.iter().map(|s| s.gradient).collect::<Option<Vec<_>>>();
    match channel {
        GRAVITATIONAL_ACCELERATION_HANDLE => {
            let column = SampledColumn::new(
                FieldColumn::vectors(samples.iter().map(|s| s.field).collect()),
                validity,
            );
            Ok(match gradients {
                Some(jacobians) => column.with_gradient(GradientColumn::Vector(jacobians.into())),
                None => column,
            })
        }
        GRAVITATIONAL_POTENTIAL_HANDLE => {
            let column = SampledColumn::new(
                FieldColumn::scalars(samples.iter().map(|s| s.potential).collect()),
                validity,
            );
            // ∇φ = −g (potential's gradient is minus the acceleration)
            Ok(match gradients {
                Some(_) => column.with_gradient(GradientColumn::Scalar(
                    samples.iter().map(|s| -s.field).collect(),
                )),
                None => column,
            })
        }
        other => Err(PluginError::UnknownChannel(other.index())),
    }
}
```

Key changes from the existing gravity `sample()`:
- `.acceleration` → `.field`
- Now conditionally attaches gradients (matching electrostatics)
- Uses `.gradient` from `InverseSquareSample`

**3h. Update `add_forces`**

Replace the `fieldcad_newtonian_gravity::evaluate_acceleration_excluding` call:

```rust
// Before
let acceleration = evaluate_acceleration_excluding(
    self.sources.iter_excluding(body.object),
    body.position,
)
.ok_or_else(|| { /* overflow */ })?;
*out_force += acceleration * mass;

// After — call fieldcad_superposition::field_excluding directly
let acceleration = fieldcad_superposition::field_excluding(
    -GRAVITATIONAL_CONSTANT,
    self.sources.iter_excluding(body.object).map(inverse_square_source),
    body.position,
)
.ok_or_else(|| {
    PluginError::Solver("gravitational acceleration overflowed to a non-finite value".to_owned())
})?;
*out_force += acceleration * mass;
```

**3i. Remove the `f32` quantization function**

The gravity plugin currently has a `quantize()` function that converts
`NewtonianSample` fields to `f32` and back. Remove it only together with a
precision validator matching electrostatics: an `F64` CPU evaluator on an `F32`
domain must now be rejected, rather than silently quantized. This is an
intentional behaviour change, not merely cleanup.

**3j. Update `create_solver`**

Precision validator now uses the unified trait — same logic:
```rust
if context.domain.precision() != self.evaluator.precision() {
    return Err(PluginError::InvalidConfiguration(/* ... */));
}
```

### Phase 4 — Desktop GPU code

**4a. Delete the equation-system GPU wrappers**

Delete `electrostatics_gpu.rs` and `gravity_gpu.rs`, remove their module
declarations, and move their tests to `gpu_inverse_square.rs`. Neither wrapper
has a remaining source conversion, fixed coupling constant, or result type that
differs from the other; retaining two adapters would preserve a shallow seam
with no independent responsibility.

**4b. Implement the shared trait directly in `gpu_inverse_square.rs`**

Keep `GpuInverseSquareSample` private if it helps isolate raw GPU readback, but
implement the public evaluator interface on `GpuInverseSquareEvaluator` and map
the private values there:

```rust
impl InverseSquareBatchEvaluator for GpuInverseSquareEvaluator {
    fn precision(&self) -> Precision {
        Precision::F32
    }

    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        _domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String> {
        let samples = self.evaluate_raw(coupling_constant, sources, geometry)?;
        Ok(samples
            .into_iter()
            .map(|sample| InverseSquareSample {
                field: sample.field,
                potential: sample.potential,
                gradient: None,  // GPU shader does not compute Jacobian
                validity: sample.validity,
            })
            .collect())
    }
}
```

Rename the existing inherent `evaluate` to `evaluate_raw` to avoid recursive
dispatch. This shared adapter receives already-converted generic sources and
the caller's coupling constant, so one `Arc<dyn InverseSquareBatchEvaluator>`
can be injected into both plugins. Update its module documentation to describe
the direct shared adapter rather than two wrappers.

### Phase 5 — Remove `crates/fieldcad-newtonian-gravity`

**5a. Delete the directory**
```
rm -rf crates/fieldcad-newtonian-gravity/
```

**5b. Remove from workspace Cargo.toml**

In root `Cargo.toml`, remove `"crates/fieldcad-newtonian-gravity"` from the
`members` list **and** remove its `workspace.dependencies` entry. Regenerate
`Cargo.lock` as part of the dependency update.

**5c. Remove from desktop Cargo.toml**

In `apps/fieldcad-desktop/Cargo.toml`, remove line:
```
fieldcad-newtonian-gravity = { workspace = true }
```
(No longer needed — `fieldcad-gravity` already provides the plugin, and the
only things used from `fieldcad-newtonian-gravity` were `NewtonianSample`,
`GRAVITATIONAL_CONSTANT`, and `evaluate_sources` — all now in the gravity
plugin or `fieldcad-superposition`.)

**5d. Remove from bench Cargo.toml**

In `crates/fieldcad-bench/Cargo.toml`, remove line:
```
fieldcad-newtonian-gravity = { workspace = true }
```
(Confirmed unused — the bench crate never imports symbols from it.)

**5e. Update current architecture documentation**

Update `PLAN.md` and `docs/orishu-integration-plan.md`, which currently
describe `fieldcad-newtonian-gravity` as a present reusable kernel. Do not edit
dated reviews that mention it: those are historical evidence of the architecture
at the time of review.

### Phase 6 — Wire desktop application

**6a. Update evaluator creation in `app.rs`**

```rust
// Before
let evaluator: Arc<dyn ElectrostaticBatchEvaluator> = Arc::new(
    GpuElectrostaticEvaluator::new(compute_device.clone(), compute_queue.clone()),
);
let gravity: Arc<dyn GravityBatchEvaluator> = Arc::new(GpuNewtonianGravityEvaluator::new(
    compute_device.clone(),
    compute_queue.clone(),
));

// After — one shared GPU adapter type is injected into both plugins
let evaluator: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
    GpuInverseSquareEvaluator::new(compute_device.clone(), compute_queue.clone()),
);
let gravity: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(GpuInverseSquareEvaluator::new(
    compute_device.clone(),
    compute_queue.clone(),
));
```

**6b. Update `desktop_plugin_catalog` signature**

```rust
// Before
fn desktop_plugin_catalog(
    evaluator: Arc<dyn ElectrostaticBatchEvaluator>,
    gravity: Arc<dyn GravityBatchEvaluator>,
    maxwell: Arc<dyn MaxwellSolverBackend>,
) -> Vec<PluginRegistration>

// After — both take the unified trait
fn desktop_plugin_catalog(
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    gravity: Arc<dyn InverseSquareBatchEvaluator>,
    maxwell: Arc<dyn MaxwellSolverBackend>,
) -> Vec<PluginRegistration>
```

The source conversion and coupling constant now belong to the plugins, so the
desktop has no reason to retain equation-system-specific GPU adapters.

### Phase 7 — Build and test

**7a. Fix all compilation errors**

The compiler will flag:
- All `.electric_field` access → `.field` (in electrostatics tests)
- All `.acceleration` access → `.field` (in gravity tests)
- All `ElectrostaticBatchEvaluator`/`GravityBatchEvaluator` → `InverseSquareBatchEvaluator`
- All `ElectrostaticSample`/`NewtonianSample` → `InverseSquareSample`
- Missing imports (now from `fieldcad-superposition` for the kernel types)
- Deleted GPU-wrapper module declarations/imports and the renamed raw GPU method

**7b. Run existing tests**

```bash
cargo test -p fieldcad-superposition
cargo test -p fieldcad-electrostatics
cargo test -p fieldcad-gravity
cargo test -p fieldcad-desktop -- --test-threads=1  # GPU tests
```

Update the superposition tests for `Option<DMat3>` and add coverage for the
CPU gradient/undefined-sample invariant.

The electrostatics tests change minimally (`.electric_field` → `.field`).

The gravity plugin tests need more updates (new trait impls, source conversion,
`evaluate_sources` call changes).

Move the desktop GPU tests beside the shared adapter. They pass
`&[InverseSquareSource]` and both Coulomb and gravitational coupling constants
to the same evaluator implementation.

**7c. Add contract and regression tests**

- A CPU batch containing both exact and undefined positions still publishes a
  gradient column; validity, not gradient availability, marks the undefined
  entry unusable.
- The GPU adapter returns `gradient: None` for every sample and both plugin
  channels omit gradients consistently.
- A deliberately malformed evaluator returning too few/many samples and an
  `evaluate_into` call with a wrong-sized buffer return `Err` rather than panic
  or retain stale output.
- Gravity rejects a domain whose declared precision differs from its evaluator,
  matching electrostatics; retain a test for an accepted F32 GPU evaluator and
  F64 CPU evaluator.
- Both plugins retain force results and CPU/GPU parity after receiving the same
  generic sources and their respective coupling constants.

**7d. Verify no broken dependencies**

```bash
cargo check --workspace
```

Confirm the workspace compiles without the removed
`fieldcad-newtonian-gravity` crate.

### Invariants to verify

After each phase, confirm:

- CPU samples, including invalid placeholders, carry `Some(...)`; GPU samples
  carry `None` for the whole batch. A CPU batch with one undefined position
  still publishes gradients for its valid positions.
- Force calculation (`add_forces`) produces identical results (uses
  `field_excluding` directly, same formula with same coupling constant).
- No `ElectrostaticSample` or `NewtonianSample` definitions or production Rust
  references remain; this task and dated review records may retain their names.
- Active manifests, `Cargo.lock`, and current-state documentation no longer
  reference `fieldcad-newtonian-gravity`; dated review records remain intact.
- Bench crate links without the removed dependency (it never imported it).

## Edge cases and risks

- **Source conversion storage**: Convert `ChargeSource`/`CoupledSource<MassKg>`
  into `InverseSquareSource` when the solver adopts a world and retain that
  vector beside the object index. This avoids allocation and conversion on both
  channel reads and cache hits while preserving object-indexed sources for
  force exclusion.

- **Gradient capability is batch-wide**: `SampledColumn` can only publish one
  gradient-column decision for the batch. The shared evaluator interface must
  therefore require a backend to report gradients for every sample in a batch
  or none of them. Undefined CPU samples use finite placeholders and validity;
  a GPU backend reports none for the whole batch.

- **Crate removal from workspace**: Remove the crate from both root workspace
  lists, regenerate the lockfile, and search production code/manifests before
  deletion. `cargo check --workspace` then proves no live dependency remains.

## Later considerations

- **Eliminate `GpuInverseSquareSample`**: It may remain as a private raw-readback
  shape inside `gpu_inverse_square.rs`. If it becomes only a one-to-one mapping,
  fold it into `InverseSquareSample` there; no equation-system wrapper should
  be reintroduced for that purpose.

- **Generic `field_excluding` helper in the gravity plugin**: Currently uses
  `fieldcad_superposition::field_excluding` directly. If the source conversion
  becomes a pattern, consider a convenience wrapper.
