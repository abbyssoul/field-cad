//! The finite region over which a numerical field is represented.
//!
//! `CONTEXT.md` requires that a rendered value be attributable to a domain and a
//! numerical configuration, and that solvers be initialized from a domain rather
//! than inventing one privately. Analytic evaluators may sample outside the
//! domain, but they must still declare the region a snapshot describes.

use glam::{DVec3, UVec3};
use serde::{Deserialize, Serialize};

use crate::GridLattice;

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum DomainError {
    #[error("domain bounds must be finite")]
    NonFiniteBounds,
    #[error("domain bounds must have positive extent on every axis")]
    DegenerateBounds,
    #[error("domain resolution must be at least one cell on every axis")]
    DegenerateResolution,
}

/// An axis-aligned region of space, in metres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DomainBounds {
    min: DVec3,
    max: DVec3,
}

impl DomainBounds {
    pub fn new(min: DVec3, max: DVec3) -> Result<Self, DomainError> {
        if !min.is_finite() || !max.is_finite() {
            return Err(DomainError::NonFiniteBounds);
        }
        if min.cmpge(max).any() {
            return Err(DomainError::DegenerateBounds);
        }
        Ok(Self { min, max })
    }

    /// A cube of `half_extent` metres centred on the origin.
    pub fn centred_cube(half_extent: f64) -> Result<Self, DomainError> {
        Self::new(DVec3::splat(-half_extent), DVec3::splat(half_extent))
    }

    pub const fn min(self) -> DVec3 {
        self.min
    }

    pub const fn max(self) -> DVec3 {
        self.max
    }

    pub fn size(self) -> DVec3 {
        self.max - self.min
    }

    pub fn centre(self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    pub fn contains(self, point: DVec3) -> bool {
        point.cmpge(self.min).all() && point.cmple(self.max).all()
    }
}

/// Cell counts along each axis. Always at least one cell per axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    cells: UVec3,
}

impl Resolution {
    pub fn new(x: u32, y: u32, z: u32) -> Result<Self, DomainError> {
        let cells = UVec3::new(x, y, z);
        if cells.min_element() == 0 {
            return Err(DomainError::DegenerateResolution);
        }
        Ok(Self { cells })
    }

    pub fn uniform(cells: u32) -> Result<Self, DomainError> {
        Self::new(cells, cells, cells)
    }

    pub const fn cells(self) -> UVec3 {
        self.cells
    }

    pub fn cell_count(self) -> u64 {
        u64::from(self.cells.x) * u64::from(self.cells.y) * u64::from(self.cells.z)
    }
}

/// How a solver should treat the field at a domain face.
///
/// Plugins interpret these; the core only records the user's choice so it can be
/// reported alongside a result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoundaryCondition {
    /// The field wraps to the opposite face.
    Periodic,
    /// The value is fixed at the boundary.
    Dirichlet,
    /// The normal derivative is fixed at the boundary.
    Neumann,
    /// Outgoing waves leave without reflection, to the accuracy of the scheme.
    Absorbing,
    /// No constraint; the representation simply ends. Valid for analytic
    /// evaluators that can sample anywhere.
    #[default]
    Open,
}

/// Boundary conditions per axis. Faces of one axis share a condition until a
/// solver demonstrates it needs them to differ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryConditions {
    pub x: BoundaryCondition,
    pub y: BoundaryCondition,
    pub z: BoundaryCondition,
}

impl BoundaryConditions {
    pub const fn uniform(condition: BoundaryCondition) -> Self {
        Self {
            x: condition,
            y: condition,
            z: condition,
        }
    }
}

/// Storage precision of a solver's field representation. Recorded in snapshot
/// metadata so a GPU `f32` result is never mistaken for the `f64` oracle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Precision {
    F32,
    #[default]
    F64,
}

impl Precision {
    pub const fn label(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Domain {
    bounds: DomainBounds,
    resolution: Resolution,
    boundaries: BoundaryConditions,
    precision: Precision,
}

impl Domain {
    pub const fn new(
        bounds: DomainBounds,
        resolution: Resolution,
        boundaries: BoundaryConditions,
        precision: Precision,
    ) -> Self {
        Self {
            bounds,
            resolution,
            boundaries,
            precision,
        }
    }

    /// An open, `f64`, uniformly resolved cube. The default authoring domain
    /// before a plugin states stricter requirements.
    pub fn centred_cube(half_extent: f64, cells: u32) -> Result<Self, DomainError> {
        Ok(Self::new(
            DomainBounds::centred_cube(half_extent)?,
            Resolution::uniform(cells)?,
            BoundaryConditions::default(),
            Precision::default(),
        ))
    }

    pub const fn bounds(&self) -> DomainBounds {
        self.bounds
    }

    pub const fn resolution(&self) -> Resolution {
        self.resolution
    }

    pub const fn boundaries(&self) -> BoundaryConditions {
        self.boundaries
    }

    pub const fn precision(&self) -> Precision {
        self.precision
    }

    /// Spacing between cell centres.
    pub fn cell_size(&self) -> DVec3 {
        self.bounds.size() / self.resolution.cells().as_dvec3()
    }

    /// The cell-centred sampling lattice for this domain.
    pub fn cell_lattice(&self) -> GridLattice {
        let cell = self.cell_size();
        GridLattice::new(
            self.bounds.min() + cell * 0.5,
            cell,
            self.resolution.cells(),
        )
    }

    /// A coarser cell-centred lattice, for whole-domain visualization that must
    /// not draw one glyph per solver cell.
    pub fn decimated_lattice(&self, stride: u32) -> GridLattice {
        let stride = stride.max(1);
        let cell = self.cell_size();
        let counts = (self.resolution.cells() / stride).max(UVec3::ONE);
        GridLattice::new(
            self.bounds.min() + cell * 0.5,
            cell * f64::from(stride),
            counts,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degenerate_domains_are_rejected() {
        assert_eq!(
            DomainBounds::new(DVec3::ZERO, DVec3::new(1.0, 0.0, 1.0)),
            Err(DomainError::DegenerateBounds)
        );
        assert_eq!(
            DomainBounds::new(DVec3::ZERO, DVec3::splat(f64::NAN)),
            Err(DomainError::NonFiniteBounds)
        );
        assert_eq!(
            Resolution::new(4, 0, 4),
            Err(DomainError::DegenerateResolution)
        );
    }

    #[test]
    fn cell_lattice_samples_cell_centres_inside_the_bounds() {
        let domain = Domain::centred_cube(1.0, 2).unwrap();
        let lattice = domain.cell_lattice();

        assert_eq!(domain.cell_size(), DVec3::splat(1.0));
        assert_eq!(lattice.len(), 8);
        assert_eq!(lattice.position(0).unwrap(), DVec3::splat(-0.5));
        assert_eq!(lattice.position(7).unwrap(), DVec3::splat(0.5));
        for index in 0..lattice.len() {
            assert!(domain.bounds().contains(lattice.position(index).unwrap()));
        }
    }

    #[test]
    fn decimation_reduces_sample_count_without_changing_the_domain() {
        let domain = Domain::centred_cube(1.0, 8).unwrap();

        assert_eq!(domain.cell_lattice().len(), 512);
        assert_eq!(domain.decimated_lattice(4).len(), 8);
        assert_eq!(domain.decimated_lattice(1000).len(), 1);
        assert_eq!(domain.resolution().cell_count(), 512);
    }
}
