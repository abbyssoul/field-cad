//! The geometry a physics source superposes over, shared by every coupling
//! that treats an authored object as a point or uniform sphere — electric
//! charge, gravitational mass, and any future one. The physical quantity
//! spread over that geometry (charge, mass, ...) is what differs between
//! them; the geometry question itself is the same one, so it is answered
//! once here rather than once per coupling.

use crate::ObjectShape;

/// A point or uniform sphere source geometry.
///
/// Distinct from [`ObjectShape`]: a `Box` is a valid authored shape but not
/// a valid physics-source geometry, so this only covers the subset of
/// shapes a superposed source resolves to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointOrSphere {
    /// A point source with a declared radius inside which the analytic
    /// field is undefined rather than merely large.
    Point { exclusion_radius: f64 },
    /// A uniformly-distributed sphere, finite (and different from the point
    /// formula) at its interior.
    UniformSphere { radius: f64 },
}

impl PointOrSphere {
    /// Maps an authored object's shape to its physics-source geometry — the
    /// mapping every coupling module needs. A shapeless object is a
    /// legitimate point source at `default_point_radius` (so composing an
    /// object one component at a time never passes through an invalid
    /// intermediate state); a `Sphere` needs a positive radius; a `Box` has
    /// no physics-source counterpart at all.
    pub fn from_shape(
        shape: Option<ObjectShape>,
        default_point_radius: f64,
    ) -> Result<Self, PointOrSphereError> {
        match shape {
            Some(ObjectShape::Point { radius }) => Ok(Self::Point {
                exclusion_radius: radius,
            }),
            Some(ObjectShape::Sphere { radius }) if radius > 0.0 => {
                Ok(Self::UniformSphere { radius })
            }
            Some(ObjectShape::Sphere { .. }) => Err(PointOrSphereError::NonPositiveSphere),
            None => Ok(Self::Point {
                exclusion_radius: default_point_radius,
            }),
            Some(ObjectShape::Box { .. }) => Err(PointOrSphereError::UnsupportedShape),
        }
    }
}

/// Why an authored shape has no physics-source geometry. Deliberately
/// object-name-free and crate-neutral: each caller already reports its own
/// per-crate error carrying the object's name (`MassSourceError`,
/// `ChargeSourceError`, ...), and maps this onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PointOrSphereError {
    #[error("must have a positive radius")]
    NonPositiveSphere,
    #[error("must use a point or sphere shape")]
    UnsupportedShape,
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    #[test]
    fn a_shapeless_object_is_a_point_at_the_default_radius() {
        assert_eq!(
            PointOrSphere::from_shape(None, 0.25).unwrap(),
            PointOrSphere::Point {
                exclusion_radius: 0.25
            }
        );
    }

    #[test]
    fn a_point_shape_keeps_its_own_radius() {
        assert_eq!(
            PointOrSphere::from_shape(Some(ObjectShape::point(0.1).unwrap()), 0.25).unwrap(),
            PointOrSphere::Point {
                exclusion_radius: 0.1
            }
        );
    }

    #[test]
    fn a_positive_radius_sphere_becomes_a_uniform_sphere() {
        assert_eq!(
            PointOrSphere::from_shape(Some(ObjectShape::sphere(2.0).unwrap()), 0.25).unwrap(),
            PointOrSphere::UniformSphere { radius: 2.0 }
        );
    }

    #[test]
    fn a_non_positive_radius_sphere_is_rejected() {
        assert_eq!(
            PointOrSphere::from_shape(Some(ObjectShape::sphere(0.0).unwrap()), 0.25),
            Err(PointOrSphereError::NonPositiveSphere)
        );
    }

    #[test]
    fn a_box_has_no_physics_source_geometry() {
        assert_eq!(
            PointOrSphere::from_shape(Some(ObjectShape::boxed(DVec3::ONE).unwrap()), 0.25),
            Err(PointOrSphereError::UnsupportedShape)
        );
    }
}
