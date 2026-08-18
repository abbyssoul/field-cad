//! How a batch of field values is laid out, and where each value was taken.
//!
//! Values are stored columnar rather than as a sequence of `Quantity`. A channel
//! already declares its dimension and scalar/vector shape once, so repeating that
//! per value costs memory and buys nothing. Columnar storage is also the layout a
//! GPU upload and a network chunk both want.
//!
//! Positions are described by a *geometry* rather than stored per sample, so a
//! plane or grid batch carries three vectors and a count instead of one position
//! per cell.

use std::sync::Arc;

use glam::{DMat3, DVec2, DVec3, UVec2, UVec3};
use serde::{Deserialize, Serialize};

use crate::{
    BoxId, Dimension, FieldValue, FieldValueKind, PlaneId, ProbeId, Quantity, SphereId,
    VectorQuantity,
};

/// How a returned value was obtained, and whether it means anything.
///
/// `CONTEXT.md` requires that point-source singularities be marked undefined
/// inside a declared source radius rather than silently clamped, and that a probe
/// report its interpolation method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleValidity {
    /// Evaluated exactly at the requested position.
    #[default]
    Exact,
    /// Reconstructed from nearby stored samples.
    Interpolated(InterpolationMethod),
    /// The field has no defined value here.
    Undefined(UndefinedReason),
}

impl SampleValidity {
    pub const fn is_usable(self) -> bool {
        !matches!(self, Self::Undefined(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterpolationMethod {
    Nearest,
    Bilinear,
    Trilinear,
    /// Reconstructed from published values *and* a per-sample gradient
    /// (a tensor-product cubic Hermite blend), rather than values alone.
    Hermite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UndefinedReason {
    /// Inside a point source's declared radius, where the analytic field diverges.
    InsideSourceRadius,
    /// Outside the region the solver represents.
    OutsideDomain,
    /// The solver did not reach its convergence criterion here.
    NotConverged,
    /// Arithmetic overflowed the representation's declared precision.
    NumericalOverflow,
    /// The value would have to be read across a periodic wrap that the
    /// solver's initial state does not satisfy.
    ///
    /// A periodic lattice can only carry a field whose potential is periodic.
    /// When a solver is constrained by a source whose analytic field is not —
    /// an isolated charge, for example — the outermost layer of the lattice
    /// differences two opposite faces of the domain. That is a fabricated
    /// value, not an approximation, so it is reported as undefined rather than
    /// drawn as though it were measured.
    AcrossPeriodicSeam,
}

/// A regular lattice of samples on a slice plane.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaneLattice {
    origin: DVec3,
    u_step: DVec3,
    v_step: DVec3,
    counts: UVec2,
}

impl PlaneLattice {
    /// `origin` is the position of sample `(0, 0)`; the step vectors separate
    /// adjacent samples along each in-plane axis.
    pub fn new(origin: DVec3, u_step: DVec3, v_step: DVec3, counts: UVec2) -> Self {
        Self {
            origin,
            u_step,
            v_step,
            counts: counts.max(UVec2::ONE),
        }
    }

    pub const fn counts(self) -> UVec2 {
        self.counts
    }

    /// World-space step between adjacent samples along the plane's local u axis.
    pub const fn u_step(self) -> DVec3 {
        self.u_step
    }

    /// World-space step between adjacent samples along the plane's local v axis.
    pub const fn v_step(self) -> DVec3 {
        self.v_step
    }

    pub fn len(self) -> usize {
        self.counts.x as usize * self.counts.y as usize
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn position(self, index: usize) -> Option<DVec3> {
        if index >= self.len() {
            return None;
        }
        let width = self.counts.x as usize;
        let u = (index % width) as f64;
        let v = (index / width) as f64;
        Some(self.origin + self.u_step * u + self.v_step * v)
    }

    /// Inverse of [`Self::position`]: the fractional in-plane index a world
    /// point projects onto.
    ///
    /// `u_step`/`v_step` are mutually orthogonal by construction
    /// (`SlicePlane::lattice` builds them from an orthonormal in-plane
    /// basis), so each axis's index is recovered independently by
    /// projection, and any out-of-plane component of `point` is ignored
    /// rather than folded into the result.
    pub fn fractional_coordinates(self, point: DVec3) -> DVec2 {
        let offset = point - self.origin;
        DVec2::new(
            offset.dot(self.u_step) / self.u_step.length_squared(),
            offset.dot(self.v_step) / self.v_step.length_squared(),
        )
    }
}

/// A regular three-dimensional lattice of samples. Index order is x fastest.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridLattice {
    origin: DVec3,
    step: DVec3,
    counts: UVec3,
}

impl GridLattice {
    pub fn new(origin: DVec3, step: DVec3, counts: UVec3) -> Self {
        Self {
            origin,
            step,
            counts: counts.max(UVec3::ONE),
        }
    }

    pub const fn counts(self) -> UVec3 {
        self.counts
    }

    pub const fn step(self) -> DVec3 {
        self.step
    }

    pub fn len(self) -> usize {
        self.counts.x as usize * self.counts.y as usize * self.counts.z as usize
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn position(self, index: usize) -> Option<DVec3> {
        if index >= self.len() {
            return None;
        }
        let width = self.counts.x as usize;
        let height = self.counts.y as usize;
        let x = (index % width) as f64;
        let y = ((index / width) % height) as f64;
        let z = (index / (width * height)) as f64;
        Some(self.origin + self.step * DVec3::new(x, y, z))
    }

    /// Inverse of [`Self::position`]: the fractional index a world point
    /// corresponds to. Exact, since the lattice is axis-aligned with a
    /// uniform step per axis.
    pub fn fractional_coordinates(self, point: DVec3) -> DVec3 {
        (point - self.origin) / self.step
    }
}

/// A regular, arbitrarily oriented three-dimensional lattice of samples.
///
/// The natural generalization of [`PlaneLattice`] to a third in-region axis:
/// `origin` is sample `(0, 0, 0)` and `u_step`/`v_step`/`w_step` need not be
/// axis-aligned, which is what lets an oriented [`FieldBox`](crate::FieldBox)
/// reuse the same construction a [`SlicePlane`](crate::SlicePlane) already
/// uses rather than needing a rotation carried alongside an axis-aligned grid.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoxLattice {
    origin: DVec3,
    u_step: DVec3,
    v_step: DVec3,
    w_step: DVec3,
    counts: UVec3,
}

impl BoxLattice {
    pub fn new(origin: DVec3, u_step: DVec3, v_step: DVec3, w_step: DVec3, counts: UVec3) -> Self {
        Self {
            origin,
            u_step,
            v_step,
            w_step,
            counts: counts.max(UVec3::ONE),
        }
    }

    pub const fn counts(self) -> UVec3 {
        self.counts
    }

    /// World-space step between adjacent samples along the box's local u axis.
    pub const fn u_step(self) -> DVec3 {
        self.u_step
    }

    /// World-space step between adjacent samples along the box's local v axis.
    pub const fn v_step(self) -> DVec3 {
        self.v_step
    }

    /// World-space step between adjacent samples along the box's local w axis.
    pub const fn w_step(self) -> DVec3 {
        self.w_step
    }

    pub fn len(self) -> usize {
        self.counts.x as usize * self.counts.y as usize * self.counts.z as usize
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn position(self, index: usize) -> Option<DVec3> {
        if index >= self.len() {
            return None;
        }
        let width = self.counts.x as usize;
        let height = self.counts.y as usize;
        let u = (index % width) as f64;
        let v = ((index / width) % height) as f64;
        let w = (index / (width * height)) as f64;
        Some(self.origin + self.u_step * u + self.v_step * v + self.w_step * w)
    }

    /// Inverse of [`Self::position`]: the fractional index a world point
    /// projects onto. `u_step`/`v_step`/`w_step` are mutually orthogonal by
    /// construction (`FieldBox::lattice` scales a rotated orthonormal basis
    /// independently per axis), so each axis's index is recovered
    /// independently by projection.
    pub fn fractional_coordinates(self, point: DVec3) -> DVec3 {
        let offset = point - self.origin;
        DVec3::new(
            offset.dot(self.u_step) / self.u_step.length_squared(),
            offset.dot(self.v_step) / self.v_step.length_squared(),
            offset.dot(self.w_step) / self.w_step.length_squared(),
        )
    }
}

/// A regular lattice over a sphere's bounding cube, plus the sphere itself.
///
/// The lattice is evaluated over the whole cube — the same simple
/// axis-aligned construction as [`GridLattice`] — rather than a
/// variable-length shape that only carries in-sphere points. A drawer that
/// wants a spherical silhouette instead of a cubic one uses [`Self::centre`]
/// and [`Self::radius`] to cull points at display time; the solver evaluates
/// the same batch either way.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SphereLattice {
    grid: GridLattice,
    centre: DVec3,
    radius: f64,
}

impl SphereLattice {
    pub fn new(origin: DVec3, step: DVec3, counts: UVec3, centre: DVec3, radius: f64) -> Self {
        Self {
            grid: GridLattice::new(origin, step, counts),
            centre,
            radius,
        }
    }

    pub const fn counts(self) -> UVec3 {
        self.grid.counts()
    }

    /// The bounding-cube lattice itself, for a drawer that wants to
    /// interpolate the published grid before culling to the sphere.
    pub const fn grid(self) -> GridLattice {
        self.grid
    }

    pub fn len(self) -> usize {
        self.grid.len()
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn position(self, index: usize) -> Option<DVec3> {
        self.grid.position(index)
    }

    pub const fn centre(self) -> DVec3 {
        self.centre
    }

    pub const fn radius(self) -> f64 {
        self.radius
    }

    /// Whether a world point lies inside the sphere this lattice bounds.
    pub fn contains(self, point: DVec3) -> bool {
        (point - self.centre).length_squared() <= self.radius * self.radius
    }
}

/// Where the values in a batch were sampled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SampleGeometry {
    /// One sample per probe, in the order the probes were requested.
    Probes {
        ids: Arc<[ProbeId]>,
        positions: Arc<[DVec3]>,
    },
    Plane {
        plane: PlaneId,
        lattice: PlaneLattice,
    },
    Grid(GridLattice),
    Box {
        region: BoxId,
        lattice: BoxLattice,
    },
    Sphere {
        region: SphereId,
        lattice: SphereLattice,
    },
}

impl SampleGeometry {
    pub fn probes(ids: Vec<ProbeId>, positions: Vec<DVec3>) -> Result<Self, SamplingError> {
        if ids.len() != positions.len() {
            return Err(SamplingError::LengthMismatch {
                geometry: ids.len(),
                values: positions.len(),
            });
        }
        Ok(Self::Probes {
            ids: ids.into(),
            positions: positions.into(),
        })
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Probes { ids, .. } => ids.len(),
            Self::Plane { lattice, .. } => lattice.len(),
            Self::Grid(lattice) => lattice.len(),
            Self::Box { lattice, .. } => lattice.len(),
            Self::Sphere { lattice, .. } => lattice.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn position(&self, index: usize) -> Option<DVec3> {
        match self {
            Self::Probes { positions, .. } => positions.get(index).copied(),
            Self::Plane { lattice, .. } => lattice.position(index),
            Self::Grid(lattice) => lattice.position(index),
            Self::Box { lattice, .. } => lattice.position(index),
            Self::Sphere { lattice, .. } => lattice.position(index),
        }
    }

    /// Every position this geometry describes, in index order.
    ///
    /// `map`, not `filter_map`: `position(index)` is `None` only for
    /// `index >= self.len()`, which `0..self.len()` never produces, so the
    /// `None` arm is unreachable here by construction. `filter_map` cannot
    /// be `ExactSizeIterator` (a predicate might drop items), which used to
    /// stop every `.collect()` over this iterator — the per-sample
    /// evaluation buffer, once per solver publication — from specializing
    /// to a single upfront allocation; it grew by repeated reallocation
    /// instead. `Map` over `Range<usize>` (already `ExactSizeIterator`) is
    /// `ExactSizeIterator` too, so `.collect()` sizes the buffer once.
    /// Every position this geometry describes, in index order.
    ///
    /// `map`, not `filter_map`: `position(index)` is `None` only for
    /// `index >= self.len()`, which `0..self.len()` never produces, so the
    /// `None` arm is unreachable here by construction. `filter_map` cannot
    /// be `ExactSizeIterator` (a predicate might drop items), which used to
    /// stop every `.collect()` over this iterator — the per-sample
    /// evaluation buffer, once per solver publication — from specializing
    /// to a single upfront allocation; it grew by repeated reallocation
    /// instead. `Map` over `Range<usize>` (already `ExactSizeIterator`) is
    /// `ExactSizeIterator` too, so `.collect()` sizes the buffer once.
    pub fn positions(&self) -> impl Iterator<Item = DVec3> + '_ {
        (0..self.len()).map(|index| {
            self.position(index)
                .expect("index < self.len() always has a position")
        })
    }

    pub const fn plane_id(&self) -> Option<PlaneId> {
        match self {
            Self::Plane { plane, .. } => Some(*plane),
            _ => None,
        }
    }

    /// The index at which `probe` appears, if this geometry is probe-shaped.
    pub fn probe_index(&self, probe: ProbeId) -> Option<usize> {
        match self {
            Self::Probes { ids, .. } => ids.iter().position(|id| *id == probe),
            _ => None,
        }
    }
}

/// A column of field values of one shape. Dimension is carried by the channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldColumn {
    Scalar(Arc<[f64]>),
    Vector(Arc<[DVec3]>),
}

impl FieldColumn {
    pub fn scalars(values: Vec<f64>) -> Self {
        Self::Scalar(values.into())
    }

    pub fn vectors(values: Vec<DVec3>) -> Self {
        Self::Vector(values.into())
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Scalar(values) => values.len(),
            Self::Vector(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The first element that cannot be represented as a physical number.
    ///
    /// Undefined samples still carry a finite placeholder and explain why the
    /// value must not be used through [`SampleValidity`]. Keeping NaN and
    /// infinity out of snapshots prevents them leaking into colour mapping,
    /// reductions, GPU uploads, or network consumers that do not reconstruct a
    /// [`FieldValue`] first.
    pub fn first_non_finite(&self) -> Option<usize> {
        match self {
            Self::Scalar(values) => values.iter().position(|value| !value.is_finite()),
            Self::Vector(values) => values.iter().position(|value| !value.is_finite()),
        }
    }

    /// Whether this column's shape matches a channel's declared value kind.
    pub fn matches(&self, kind: FieldValueKind) -> bool {
        matches!(
            (self, kind),
            (Self::Scalar(_), FieldValueKind::Scalar(_))
                | (Self::Vector(_), FieldValueKind::Vector(_))
        )
    }

    /// Recombine one element with its channel dimension into a display-facing
    /// value. Returns `None` if the index is out of range or the stored number is
    /// not finite.
    pub fn value_at(&self, index: usize, dimension: Dimension) -> Option<FieldValue> {
        match self {
            Self::Scalar(values) => Quantity::new(*values.get(index)?, dimension)
                .ok()
                .map(FieldValue::Scalar),
            Self::Vector(values) => VectorQuantity::new(*values.get(index)?, dimension)
                .ok()
                .map(FieldValue::Vector),
        }
    }
}

/// The spatial derivative of a channel's value, one entry per sample,
/// shaped the same way [`FieldColumn`] is shaped: `Scalar` accompanies a
/// scalar-valued channel (an ordinary gradient, `∇φ`) and `Vector`
/// accompanies a vector-valued channel (a full Jacobian, `∂F_i/∂x_j`).
///
/// A solver reports this only when it can — most cannot or do not bother,
/// and every consumer treats its absence as "use today's reconstruction,"
/// the same optional-capability idiom the rest of the plugin surface uses
/// (`EquationSystemSolver::time_step_limit`, for instance).
///
/// Convention for the `Vector` case: column `j` of the Jacobian is
/// `∂F/∂x_j`, so `jacobian * step_vector` is the directional derivative of
/// `F` along `step_vector` — exactly the operation a gradient-aware
/// interpolator needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GradientColumn {
    Scalar(Arc<[DVec3]>),
    Vector(Arc<[DMat3]>),
}

impl GradientColumn {
    pub fn len(&self) -> usize {
        match self {
            Self::Scalar(values) => values.len(),
            Self::Vector(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The first entry that cannot be represented as a physical number, for
    /// the same reason [`FieldColumn::first_non_finite`] exists.
    pub fn first_non_finite(&self) -> Option<usize> {
        match self {
            Self::Scalar(values) => values.iter().position(|value| !value.is_finite()),
            Self::Vector(values) => values.iter().position(|value| {
                !value.x_axis.is_finite() || !value.y_axis.is_finite() || !value.z_axis.is_finite()
            }),
        }
    }
}

/// One channel's values over one geometry, with per-sample validity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldBatch {
    geometry: SampleGeometry,
    values: FieldColumn,
    validity: Arc<[SampleValidity]>,
    gradient: Option<GradientColumn>,
}

impl FieldBatch {
    /// Lengths are checked once per batch here, rather than once per value at
    /// every call site that reads the batch. `validity` is taken as shared
    /// parts when the caller already holds an `Arc` column (a solver's
    /// memoized derivation) and as an owned `Vec` otherwise.
    pub fn new(
        geometry: SampleGeometry,
        values: FieldColumn,
        validity: impl Into<Arc<[SampleValidity]>>,
    ) -> Result<Self, SamplingError> {
        let validity = validity.into();
        if values.len() != geometry.len() {
            return Err(SamplingError::LengthMismatch {
                geometry: geometry.len(),
                values: values.len(),
            });
        }
        if validity.len() != geometry.len() {
            return Err(SamplingError::ValidityLengthMismatch {
                geometry: geometry.len(),
                validity: validity.len(),
            });
        }
        if let Some(index) = values.first_non_finite() {
            return Err(SamplingError::NonFiniteValue { index });
        }
        Ok(Self {
            geometry,
            values,
            validity,
            gradient: None,
        })
    }

    /// Every sample was evaluated exactly.
    pub fn exact(geometry: SampleGeometry, values: FieldColumn) -> Result<Self, SamplingError> {
        let validity = vec![SampleValidity::Exact; geometry.len()];
        Self::new(geometry, values, validity)
    }

    /// Attach a per-sample derivative to an already-built batch.
    ///
    /// Optional: nothing calls this unless the solver that produced the
    /// batch chose to report a gradient, and every consumer already treats
    /// [`Self::gradient`] returning `None` as "use today's reconstruction."
    pub fn with_gradient(mut self, gradient: GradientColumn) -> Result<Self, SamplingError> {
        if gradient.len() != self.values.len() {
            return Err(SamplingError::GradientLengthMismatch {
                values: self.values.len(),
                gradient: gradient.len(),
            });
        }
        if !matches!(
            (&self.values, &gradient),
            (FieldColumn::Scalar(_), GradientColumn::Scalar(_))
                | (FieldColumn::Vector(_), GradientColumn::Vector(_))
        ) {
            return Err(SamplingError::GradientShapeMismatch);
        }
        if let Some(index) = gradient.first_non_finite() {
            return Err(SamplingError::NonFiniteGradient { index });
        }
        self.gradient = Some(gradient);
        Ok(self)
    }

    pub const fn geometry(&self) -> &SampleGeometry {
        &self.geometry
    }

    pub const fn values(&self) -> &FieldColumn {
        &self.values
    }

    pub fn validity(&self) -> &[SampleValidity] {
        &self.validity
    }

    pub fn gradient(&self) -> Option<&GradientColumn> {
        self.gradient.as_ref()
    }

    pub fn len(&self) -> usize {
        self.geometry.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn sample(&self, index: usize, dimension: Dimension) -> Option<Sample> {
        Some(Sample {
            position: self.geometry.position(index)?,
            value: self.values.value_at(index, dimension)?,
            validity: *self.validity.get(index)?,
        })
    }
}

/// One reconstructed, display-facing sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub position: DVec3,
    pub value: FieldValue,
    pub validity: SampleValidity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SamplingError {
    #[error("geometry describes {geometry} samples but {values} values were supplied")]
    LengthMismatch { geometry: usize, values: usize },
    #[error("geometry describes {geometry} samples but {validity} validity flags were supplied")]
    ValidityLengthMismatch { geometry: usize, validity: usize },
    #[error("field column contains a non-finite value at index {index}")]
    NonFiniteValue { index: usize },
    #[error("value column has {values} samples but its gradient column has {gradient}")]
    GradientLengthMismatch { values: usize, gradient: usize },
    #[error("gradient column's shape does not match the value column's shape")]
    GradientShapeMismatch,
    #[error("gradient column contains a non-finite value at index {index}")]
    NonFiniteGradient { index: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_index_order_is_x_fastest() {
        let lattice = GridLattice::new(DVec3::ZERO, DVec3::ONE, UVec3::new(2, 2, 2));

        assert_eq!(lattice.len(), 8);
        assert_eq!(lattice.position(0).unwrap(), DVec3::new(0.0, 0.0, 0.0));
        assert_eq!(lattice.position(1).unwrap(), DVec3::new(1.0, 0.0, 0.0));
        assert_eq!(lattice.position(2).unwrap(), DVec3::new(0.0, 1.0, 0.0));
        assert_eq!(lattice.position(4).unwrap(), DVec3::new(0.0, 0.0, 1.0));
        assert!(lattice.position(8).is_none());
    }

    #[test]
    fn grid_lattice_fractional_coordinates_inverts_position() {
        let lattice = GridLattice::new(
            DVec3::new(-2.0, 3.0, 0.5),
            DVec3::new(0.5, 1.5, 2.0),
            UVec3::new(4, 3, 2),
        );

        for index in [0usize, 1, 5, 23] {
            let point = lattice.position(index).unwrap();
            let recovered = lattice.fractional_coordinates(point);
            let width = lattice.counts().x as usize;
            let height = lattice.counts().y as usize;
            let expected = DVec3::new(
                (index % width) as f64,
                ((index / width) % height) as f64,
                (index / (width * height)) as f64,
            );
            assert!((recovered - expected).length() < 1.0e-9);
        }

        // A fractional (non-integer) query round-trips too, since a tracer
        // asks about arbitrary points, not just published lattice cells.
        let midpoint = lattice.position(0).unwrap() + DVec3::new(0.25, 0.75, 1.0);
        let recovered = lattice.fractional_coordinates(midpoint);
        assert!((recovered - DVec3::new(0.5, 0.5, 0.5)).length() < 1.0e-9);
    }

    #[test]
    fn plane_lattice_walks_u_before_v() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::new(3, 2));

        assert_eq!(lattice.len(), 6);
        assert_eq!(lattice.position(2).unwrap(), DVec3::new(2.0, 0.0, 0.0));
        assert_eq!(lattice.position(3).unwrap(), DVec3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn plane_lattice_fractional_coordinates_inverts_position_and_ignores_out_of_plane_offset() {
        let lattice = PlaneLattice::new(
            DVec3::new(-1.0, -1.0, 5.0),
            DVec3::new(0.5, 0.0, 0.0),
            DVec3::new(0.0, 0.25, 0.0),
            UVec2::splat(9),
        );

        for index in [0usize, 3, 8, 40] {
            let point = lattice.position(index).unwrap();
            let recovered = lattice.fractional_coordinates(point);
            let width = lattice.counts().x as usize;
            let expected = DVec2::new((index % width) as f64, (index / width) as f64);
            assert!((recovered - expected).length() < 1.0e-9);
        }

        // A point offset out of the plane still resolves the same in-plane
        // coordinate: the tracer that calls this projects the field, not the
        // point, into the plane.
        let on_plane = lattice.position(10).unwrap();
        let off_plane = on_plane + DVec3::new(0.0, 0.0, 42.0);
        assert!(
            (lattice.fractional_coordinates(off_plane) - lattice.fractional_coordinates(on_plane))
                .length()
                < 1.0e-9
        );
    }

    #[test]
    fn plane_sample_geometry_retains_its_authoring_identity() {
        let plane = PlaneId::new(7);
        let geometry = SampleGeometry::Plane {
            plane,
            lattice: PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(2)),
        };

        assert_eq!(geometry.plane_id(), Some(plane));
        assert_eq!(geometry.len(), 4);
    }

    #[test]
    fn batches_reject_mismatched_lengths_once_instead_of_per_value() {
        let geometry = SampleGeometry::Grid(GridLattice::new(
            DVec3::ZERO,
            DVec3::ONE,
            UVec3::new(2, 1, 1),
        ));

        assert_eq!(
            FieldBatch::exact(geometry.clone(), FieldColumn::scalars(vec![1.0])),
            Err(SamplingError::LengthMismatch {
                geometry: 2,
                values: 1
            })
        );
        assert_eq!(
            FieldBatch::new(
                geometry,
                FieldColumn::scalars(vec![1.0, 2.0]),
                vec![SampleValidity::Exact]
            ),
            Err(SamplingError::ValidityLengthMismatch {
                geometry: 2,
                validity: 1
            })
        );
    }

    #[test]
    fn batches_reject_non_finite_values_even_when_marked_undefined() {
        let geometry = SampleGeometry::Grid(GridLattice::new(DVec3::ZERO, DVec3::ONE, UVec3::ONE));

        assert_eq!(
            FieldBatch::new(
                geometry,
                FieldColumn::scalars(vec![f64::NAN]),
                vec![SampleValidity::Undefined(UndefinedReason::NotConverged)],
            ),
            Err(SamplingError::NonFiniteValue { index: 0 })
        );
    }

    #[test]
    fn undefined_samples_survive_reconstruction() {
        let geometry = SampleGeometry::probes(
            vec![ProbeId::new(0), ProbeId::new(1)],
            vec![DVec3::ZERO, DVec3::X],
        )
        .unwrap();
        let batch = FieldBatch::new(
            geometry,
            FieldColumn::scalars(vec![0.0, 2.0]),
            vec![
                SampleValidity::Undefined(UndefinedReason::InsideSourceRadius),
                SampleValidity::Exact,
            ],
        )
        .unwrap();

        let singular = batch.sample(0, Dimension::ELECTRIC_POTENTIAL).unwrap();
        assert!(!singular.validity.is_usable());
        assert!(
            batch
                .sample(1, Dimension::ELECTRIC_POTENTIAL)
                .unwrap()
                .validity
                .is_usable()
        );
    }

    #[test]
    fn box_lattice_walks_u_before_v_before_w() {
        let lattice = BoxLattice::new(
            DVec3::ZERO,
            DVec3::X,
            DVec3::Y,
            DVec3::Z,
            UVec3::new(3, 2, 2),
        );

        assert_eq!(lattice.len(), 12);
        assert_eq!(lattice.position(2).unwrap(), DVec3::new(2.0, 0.0, 0.0));
        assert_eq!(lattice.position(3).unwrap(), DVec3::new(0.0, 1.0, 0.0));
        assert_eq!(lattice.position(6).unwrap(), DVec3::new(0.0, 0.0, 1.0));
        assert!(lattice.position(12).is_none());
    }

    #[test]
    fn box_lattice_need_not_be_axis_aligned() {
        // A lattice whose u/v/w axes are rotated 90 degrees about Z: local X
        // becomes world Y and local Y becomes world -X.
        let lattice = BoxLattice::new(
            DVec3::ZERO,
            DVec3::Y,
            DVec3::NEG_X,
            DVec3::Z,
            UVec3::splat(2),
        );

        assert_eq!(lattice.position(1).unwrap(), DVec3::Y);
        assert_eq!(lattice.position(2).unwrap(), DVec3::NEG_X);
    }

    #[test]
    fn box_lattice_fractional_coordinates_inverts_position_even_when_not_axis_aligned() {
        // The same 90-degree-about-Z rotated basis as
        // `box_lattice_need_not_be_axis_aligned`.
        let lattice = BoxLattice::new(
            DVec3::splat(1.0),
            DVec3::Y,
            DVec3::NEG_X,
            DVec3::Z,
            UVec3::new(3, 2, 2),
        );

        for index in [0usize, 2, 5, 11] {
            let point = lattice.position(index).unwrap();
            let recovered = lattice.fractional_coordinates(point);
            let width = lattice.counts().x as usize;
            let height = lattice.counts().y as usize;
            let expected = DVec3::new(
                (index % width) as f64,
                ((index / width) % height) as f64,
                (index / (width * height)) as f64,
            );
            assert!((recovered - expected).length() < 1.0e-9);
        }
    }

    #[test]
    fn sphere_lattice_covers_its_bounding_cube_and_reports_containment() {
        let lattice = SphereLattice::new(
            DVec3::splat(-1.0),
            DVec3::splat(2.0),
            UVec3::splat(2),
            DVec3::ZERO,
            1.0,
        );

        assert_eq!(lattice.len(), 8);
        assert_eq!(lattice.centre(), DVec3::ZERO);
        assert_eq!(lattice.radius(), 1.0);
        // A cube corner lies outside the inscribed sphere; the centre does not.
        assert!(!lattice.contains(lattice.position(0).unwrap()));
        assert!(lattice.contains(DVec3::ZERO));
    }

    #[test]
    fn box_and_sphere_sample_geometry_retain_their_authoring_identity() {
        let region = BoxId::new(3);
        let box_geometry = SampleGeometry::Box {
            region,
            lattice: BoxLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, DVec3::Z, UVec3::splat(2)),
        };
        assert_eq!(box_geometry.len(), 8);
        assert_eq!(box_geometry.position(0), Some(DVec3::ZERO));

        let sphere = SphereId::new(4);
        let sphere_geometry = SampleGeometry::Sphere {
            region: sphere,
            lattice: SphereLattice::new(
                DVec3::splat(-1.0),
                DVec3::splat(2.0),
                UVec3::splat(2),
                DVec3::ZERO,
                1.0,
            ),
        };
        assert_eq!(sphere_geometry.len(), 8);
        assert_eq!(sphere_geometry.position(0), Some(DVec3::splat(-1.0)));
    }

    #[test]
    fn field_batch_accepts_a_gradient_column_matching_its_geometry_length() {
        let geometry = SampleGeometry::Grid(GridLattice::new(
            DVec3::ZERO,
            DVec3::ONE,
            UVec3::new(2, 1, 1),
        ));
        let batch = FieldBatch::exact(geometry, FieldColumn::vectors(vec![DVec3::X, DVec3::Y]))
            .unwrap()
            .with_gradient(GradientColumn::Vector(
                vec![DMat3::IDENTITY, DMat3::ZERO].into(),
            ))
            .unwrap();

        assert!(batch.gradient().is_some());
    }

    #[test]
    fn field_batch_rejects_a_gradient_column_of_the_wrong_length() {
        let geometry = SampleGeometry::Grid(GridLattice::new(
            DVec3::ZERO,
            DVec3::ONE,
            UVec3::new(2, 1, 1),
        ));
        let batch =
            FieldBatch::exact(geometry, FieldColumn::vectors(vec![DVec3::X, DVec3::Y])).unwrap();

        assert_eq!(
            batch.with_gradient(GradientColumn::Vector(vec![DMat3::IDENTITY].into())),
            Err(SamplingError::GradientLengthMismatch {
                values: 2,
                gradient: 1,
            })
        );
    }

    #[test]
    fn field_batch_rejects_a_gradient_column_whose_shape_disagrees_with_the_value_column() {
        let geometry = SampleGeometry::Grid(GridLattice::new(DVec3::ZERO, DVec3::ONE, UVec3::ONE));
        let batch = FieldBatch::exact(geometry, FieldColumn::vectors(vec![DVec3::X])).unwrap();

        assert_eq!(
            batch.with_gradient(GradientColumn::Scalar(vec![DVec3::X].into())),
            Err(SamplingError::GradientShapeMismatch)
        );
    }

    #[test]
    fn field_batch_rejects_a_non_finite_gradient() {
        let geometry = SampleGeometry::Grid(GridLattice::new(DVec3::ZERO, DVec3::ONE, UVec3::ONE));
        let batch = FieldBatch::exact(geometry, FieldColumn::vectors(vec![DVec3::X])).unwrap();

        assert_eq!(
            batch.with_gradient(GradientColumn::Vector(
                vec![DMat3::from_cols(DVec3::NAN, DVec3::ZERO, DVec3::ZERO)].into()
            )),
            Err(SamplingError::NonFiniteGradient { index: 0 })
        );
    }
}
