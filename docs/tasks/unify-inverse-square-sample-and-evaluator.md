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
`InverseSquareBatchEvaluator` trait replace all four types. The CPU path always
reports `gradient: Some(...)` (the closed-form Jacobian is cheap); the GPU path
reports `gradient: None` (the WGSL shader does not compute it), and the
`sample()` method uses the presence of the gradient to decide whether to attach
a `GradientColumn`.

## Files affected

| File | Change |
|------|--------|
| `crates/fieldcad-superposition/src/lib.rs` | Modify `InverseSquareSample`, add `InverseSquareBatchEvaluator`, add `CpuInverseSquareEvaluator` |
| `crates/fieldcad-superposition/Cargo.toml` | No changes needed |
| `plugins/electrostatics/src/lib.rs` | Remove `ElectrostaticSample`, use `InverseSquareSample`; remove `ElectrostaticBatchEvaluator`, use `InverseSquareBatchEvaluator`; update plugin and solver |
| `plugins/electrostatics/Cargo.toml` | No changes needed (already depends on `fieldcad-superposition`) |
| `plugins/gravity/src/lib.rs` | Remove `NewtonianSample`, use `InverseSquareSample`; remove `GravityBatchEvaluator`, use `InverseSquareBatchEvaluator`; add source-conversion fn; add `fieldcad-superposition` dep; update solver |
| `plugins/gravity/Cargo.toml` | Remove `fieldcad-newtonian-gravity`, add `fieldcad-superposition` |
| `apps/fieldcad-desktop/src/electrostatics_gpu.rs` | Implement `InverseSquareBatchEvaluator` instead of `ElectrostaticBatchEvaluator`; update return mapping |
| `apps/fieldcad-desktop/src/gravity_gpu.rs` | Implement `InverseSquareBatchEvaluator` instead of `GravityBatchEvaluator`; update return mapping; use plugin's `inverse_square_source` |
| `apps/fieldcad-desktop/src/gpu_inverse_square.rs` | Optionally return `Vec<InverseSquareSample>` directly (or keep `GpuInverseSquareSample` and map at wrapper) |
| `apps/fieldcad-desktop/src/app.rs` | Update evaluator creation and plugin wiring to use unified trait |
| `apps/fieldcad-desktop/Cargo.toml` | Remove `fieldcad-newtonian-gravity` |
| `crates/fieldcad-bench/Cargo.toml` | Remove `fieldcad-newtonian-gravity` |
| `Cargo.toml` (workspace root) | Remove `crates/fieldcad-newtonian-gravity` from workspace members |

**Deleted:**
| Path | Reason |
|------|--------|
| `crates/fieldcad-newtonian-gravity/` | Entire crate — responsibility absorbed into `plugins/gravity` + `fieldcad-superposition` |

## Migration sequence

### Phase 1 — `fieldcad-superposition`

**1a. Change `InverseSquareSample.gradient` to `Option<DMat3>`**

The CPU analytical solver always computes the closed-form Jacobian; `evaluate_sources()` wraps it in `Some(...)`. The `undefined()` constructor uses `None`. Doc comment explains the invariant:

```rust
/// The field's own Jacobian (`∂field_i/∂x_j` in column `j`).
///
/// Always `Some` when produced by the CPU analytical solver
/// (`evaluate_sources` in this crate). A GPU backend that has not
/// implemented the derivative math yet reports `None`; the caller (the
/// plugin's `sample()` method) uses this to decide whether to attach a
/// `GradientColumn` to the published `SampledColumn`.
pub gradient: Option<DMat3>,
```

In `fn undefined()`:
```rust
fn undefined(reason: UndefinedReason) -> Self {
    Self {
        field: DVec3::ZERO,
        potential: 0.0,
        gradient: None,
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
/// constant (sign and magnitude) separately.
pub trait InverseSquareBatchEvaluator: Send + Sync {
    /// The numerical precision this evaluator produces (e.g. `F64` for the
    /// CPU oracle, `F32` for the WGSL compute shader).
    fn precision(&self) -> Precision;

    /// Evaluate both vector and scalar (potential) channels at every sample
    /// position described by `geometry`.
    fn evaluate(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
    ) -> Result<Vec<InverseSquareSample>, String>;

    /// [`Self::evaluate`], writing into a caller-owned buffer.
    fn evaluate_into(
        &self,
        coupling_constant: f64,
        sources: &[InverseSquareSource],
        domain: &Domain,
        geometry: &SampleGeometry,
        out: &mut [InverseSquareSample],
    ) -> Result<(), String> {
        out.copy_from_slice(&self.evaluate(coupling_constant, sources, domain, geometry)?);
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
    evaluator: Arc<dyn InverseSquareBatchEvaluator>,
    cache: SampleCache<InverseSquareSample>,
    // ...
}
```

**2f. Update `samples_for`**

Source conversion happens at the plugin level now:
```rust
fn samples_for(&self, geometry: &SampleGeometry) -> Result<Arc<[InverseSquareSample]>, PluginError> {
    let sources: Vec<InverseSquareSource> = self.sources.iter().map(inverse_square_source).collect();
    self.cache.get_or_try_insert_with(
        geometry,
        || {
            let evaluated = self
                .evaluator
                .evaluate(COULOMB_CONSTANT, &sources, &self.domain, geometry)
                .map_err(PluginError::Solver)?;
            if evaluated.len() != geometry.len() {
                return Err(PluginError::Solver(/* length mismatch */));
            }
            Ok(evaluated)
        },
        |out| {
            self.evaluator
                .evaluate_into(COULOMB_CONSTANT, &sources, &self.domain, geometry, out)
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

**3f. Update `samples_for`**

Same pattern as electrostatics — convert sources at plugin level:

```rust
fn samples_for(&self, geometry: &SampleGeometry) -> Result<Arc<[InverseSquareSample]>, PluginError> {
    let sources: Vec<InverseSquareSource> = self.sources.iter().map(inverse_square_source).collect();
    self.cache.get_or_try_insert_with(geometry, || {
        self.evaluator.evaluate(-GRAVITATIONAL_CONSTANT, &sources, &self.domain, geometry)
            .map_err(PluginError::Solver)
    }, |out| {
        self.evaluator.evaluate_into(-GRAVITATIONAL_CONSTANT, &sources, &self.domain, geometry, out)
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
`NewtonianSample` fields to `f32` and back. After unifying to
`InverseSquareSample`, this function is only useful if the evaluator produces
f64 and the plugin needs f32 output. Since `CpuInverseSquareEvaluator` reports
`Precision::F64` and the host decides precision through the evaluator, this
function is no longer needed. Remove it.

**3j. Update `create_solver`**

Precision validator now uses the unified trait — same logic:
```rust
if context.domain.precision() != self.evaluator.precision() {
    return Err(PluginError::InvalidConfiguration(/* ... */));
}
```

### Phase 4 — Desktop GPU code

**4a. `electrostatics_gpu.rs`**

Implement `InverseSquareBatchEvaluator` instead of
`ElectrostaticBatchEvaluator`:

```rust
// Before
impl ElectrostaticBatchEvaluator for GpuElectrostaticEvaluator { ... }

// After
impl InverseSquareBatchEvaluator for GpuElectrostaticEvaluator {
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
        let samples = self.core.evaluate(coupling_constant, sources, geometry)?;
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

Note: source conversion (`ChargeSource` → `InverseSquareSource`) is now the
plugin's responsibility, so the wrapper no longer calls
`inverse_square_source` — it receives pre-converted `&[InverseSquareSource]`.

Update imports:
```rust
// Before
use fieldcad_electrostatics::{
    ElectrostaticBatchEvaluator, ElectrostaticSample, inverse_square_source,
};

// After
use fieldcad_superposition::{
    InverseSquareBatchEvaluator, InverseSquareSample, InverseSquareSource,
};
```

Update test code: `evaluate_sources` still comes from `fieldcad_electrostatics`
but now returns `InverseSquareSample`. Test assertions change:
```rust
// Before
assert_eq!(gpu.electric_field.x, ...);
// After
assert_eq!(gpu.field.x, ...);
```

**4b. `gravity_gpu.rs`**

Same changes as `electrostatics_gpu.rs`:

- Implement `InverseSquareBatchEvaluator` instead of `GravityBatchEvaluator`
- Receive `&[InverseSquareSource]` instead of `&[CoupledSource<MassKg>]`
- Map `GpuInverseSquareSample` → `InverseSquareSample { gradient: None, ... }`
- Remove the private `inverse_square_source` function (now lives in the gravity plugin as `pub`)
- Update tests

Import change:
```rust
// Before
use fieldcad_newtonian_gravity::{GRAVITATIONAL_CONSTANT, NewtonianSample, evaluate_sources};
use fieldcad_gravity::GravityBatchEvaluator;

// After
use fieldcad_gravity::inverse_square_source;  // now pub in the plugin
use fieldcad_superposition::{
    InverseSquareBatchEvaluator, InverseSquareSample, InverseSquareSource,
};
```

For tests that need the CPU oracle, import from the gravity plugin:
```rust
use fieldcad_gravity::evaluate_sources;  // convenience fn, returns InverseSquareSample
```

**4c. Option: eliminate `GpuInverseSquareSample` entirely**

Since both wrappers map `GpuInverseSquareSample` → `InverseSquareSample` with
`gradient: None`, consider whether `GpuInverseSquareEvaluator::evaluate`
should return `Vec<InverseSquareSample>` directly. This would remove the
intermediate type and the mapping loop in each wrapper.

If we do this, each wrapper becomes even thinner — just a call to
`self.core.evaluate(coupling_constant, sources, geometry)`.

Decision: **keep `GpuInverseSquareSample`** as a private intermediate type for
now. It separates the GPU-internal representation from the public API. Remove it
in a follow-up if unnecessary complexity accumulates.

**4d. `gpu_inverse_square.rs`** — no structural changes needed

`GpuInverseSquareEvaluator::evaluate` already accepts `coupling_constant: f64`
and `&[InverseSquareSource]`. It returns `Result<Vec<GpuInverseSquareSample>,
String>`. The trait impl lives in the wrapper files.

### Phase 5 — Remove `crates/fieldcad-newtonian-gravity`

**5a. Delete the directory**
```
rm -rf crates/fieldcad-newtonian-gravity/
```

**5b. Remove from workspace Cargo.toml**

In root `Cargo.toml`, remove `"crates/fieldcad-newtonian-gravity"` from the
`members` list.

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

// After — both use the same trait
let evaluator: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(
    GpuElectrostaticEvaluator::new(compute_device.clone(), compute_queue.clone()),
);
let gravity: Arc<dyn InverseSquareBatchEvaluator> = Arc::new(GpuNewtonianGravityEvaluator::new(
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

Note: if `GpuElectrostaticEvaluator` and `GpuNewtonianGravityEvaluator` become
identical after all changes, they could potentially be replaced by a single
`GpuInverseSquareEvaluator` directly. For now, keep both structs — they carry
different test code and may diverge later.

### Phase 7 — Build and test

**7a. Fix all compilation errors**

The compiler will flag:
- All `.electric_field` access → `.field` (in electrostatics tests and GPU code)
- All `.acceleration` access → `.field` (in gravity tests and GPU code)
- All `ElectrostaticBatchEvaluator`/`GravityBatchEvaluator` → `InverseSquareBatchEvaluator`
- All `ElectrostaticSample`/`NewtonianSample` → `InverseSquareSample`
- Missing imports (now from `fieldcad-superposition` for the kernel types)
- Wrapper struct method signatures

**7b. Run existing tests**

```bash
cargo test -p fieldcad-superposition
cargo test -p fieldcad-electrostatics
cargo test -p fieldcad-gravity
cargo test -p fieldcad-desktop -- --test-threads=1  # GPU tests
```

The superposition tests continue to pass unchanged (they use
`InverseSquareSample` directly).

The electrostatics tests change minimally (`.electric_field` → `.field`).

The gravity plugin tests need more updates (new trait impls, source conversion,
`evaluate_sources` call changes).

The desktop GPU tests need source type changes (need to pass
`&[InverseSquareSource]` instead of `&[ChargeSource]`/`&[CoupledSource<MassKg>]`).

**7c. Verify no broken dependencies**

```bash
cargo check --workspace
```

Confirm the workspace compiles without the removed
`fieldcad-newtonian-gravity` crate.

### Invariants to verify

After each phase, confirm:

- `InverseSquareSample.gradient` is `Some(...)` when produced by CPU,
  `None` when produced by GPU. The `sample()` method handles both correctly.
- Force calculation (`add_forces`) produces identical results (uses
  `field_excluding` directly, same formula with same coupling constant).
- No `ElectrostaticSample` or `NewtonianSample` types remain anywhere in the
  codebase.
- `Cargo.toml` no longer references `fieldcad-newtonian-gravity`.
- Bench crate links without the removed dependency (it never imported it).

## Edge cases and risks

- **Source conversion allocation**: Each `samples_for()` call now converts
  `Vec<ChargeSource/MassKg>` → `Vec<InverseSquareSource>` before calling the
  evaluator. This is one extra `Vec` allocation per geometry evaluation (cache
  miss or refresh). The conversion is O(sources) with trivial per-element cost
  (copy 3 f64s + 1 f64 + 1 enum). The evaluator call itself is O(sources ×
  positions), so this is negligible.

- **Gradient availability mismatch**: If a plugin is configured with a CPU
  evaluator that always gives `Some(gradient)` but the renderer receives
  `gradient: None` through some path, it falls back to trilinear interpolation.
  This is safe — it just reduces visual smoothness. The `sample()` method
  handles this correctly via the `collect::<Option<Vec<_>>>()` pattern.

- **GPU test oracle import**: Tests in `electrostatics_gpu.rs` import
  `evaluate_sources` from the electrostatics plugin. After refactoring, this fn
  returns `InverseSquareSample` with `.field` instead of `.electric_field`. Test
  assertions must be updated consistently.

- **Crate removal from workspace**: `fieldcad-newtonian-gravity` appears in the
  root Cargo.toml workspace `members` list. After removal, `cargo check
  --workspace` must still resolve. If anything else depends on it transitively
  (it shouldn't), the compiler will catch it.

## Later considerations

- **Eliminate `GpuInverseSquareSample`**: After gravity and electrostatics use
  the same `InverseSquareSample` return type, `GpuInverseSquareSample` is just
  an intermediate that gets immediately mapped. Could fold it into
  `InverseSquareSample` directly (with `gradient: None` for the GPU path).

- **Replace wrapper structs with `GpuInverseSquareEvaluator` directly**: If both
  `GpuElectrostaticEvaluator` and `GpuNewtonianGravityEvaluator` become identical
  (same coupling constant pass-through, same source type, same result mapping),
  they could be replaced by a single `GpuInverseSquareEvaluator` implementing
  `InverseSquareBatchEvaluator` directly. The desktop would inject the same
  `Arc<dyn InverseSquareBatchEvaluator>` into both plugins.

- **Generic `field_excluding` helper in the gravity plugin**: Currently uses
  `fieldcad_superposition::field_excluding` directly. If the source conversion
  becomes a pattern, consider a convenience wrapper.
