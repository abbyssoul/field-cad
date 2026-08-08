//! Tensor-product cubic Hermite interpolation of a vector field within one
//! lattice cell, using only a value and a Jacobian at each corner.
//!
//! A full tricubic (Lekien-Marsden style) reconstruction needs mixed
//! second partials ("twist" terms) at each corner for exact C1 continuity;
//! no solver in this codebase computes those. This instead collapses one
//! axis at a time — cubic Hermite for the value being collapsed, linear
//! blend for how the *other* axes' tangents vary along it, since nothing
//! better is available for that — a standard, well-known simplification
//! (a degenerate/zero-twist Coons patch). It degenerates back toward plain
//! trilinear when every tangent collapses to an endpoint-difference slope;
//! the two differ only by the correction the gradient actually supplies.
//!
//! Shared by the desktop scene's `grid_interpolation`/`box_interpolation`/
//! `plane_interpolation` and the Yee/Maxwell plugin's own off-grid
//! reconstruction, so both consumers of a gradient-carrying batch use
//! exactly the same reconstruction.

use glam::{DMat3, DVec2, DVec3};

/// The four cubic Hermite basis functions evaluated at `t ∈ [0, 1]`.
fn hermite_basis(t: f64) -> (f64, f64, f64, f64) {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    (h00, h10, h01, h11)
}

/// One cubic Hermite blend between `p0` (`t = 0`) and `p1` (`t = 1`), given
/// tangents `m0`/`m1` already scaled to the same `t ∈ [0, 1]` parameter
/// range as the blend itself (the chain-rule scaling callers apply before
/// calling this: `tangent = jacobian * axis_step_vector`).
fn hermite_1d(t: f64, p0: DVec3, m0: DVec3, p1: DVec3, m1: DVec3) -> DVec3 {
    let (h00, h10, h01, h11) = hermite_basis(t);
    p0 * h00 + m0 * h10 + p1 * h01 + m1 * h11
}

/// Interpolate a vector field within a 4-corner (2D) cell.
///
/// `values`/`gradients` are in the same "quad winding" order
/// `PlaneInterpolation` already builds its `indices` in —
/// `[(u0,v0), (u1,v0), (u1,v1), (u0,v1)]` (bottom-left, bottom-right,
/// top-right, top-left) — rather than a binary-counter bit order, since
/// that is what the bilinear weights it falls back to already assume.
///
/// `gradients[i]` is the field's Jacobian at that corner (column `j` is
/// `∂field/∂x_j`); `axis_steps` are the cell's world-space edge vectors
/// along local u and v, used to turn each Jacobian into the directional
/// derivative the Hermite basis needs (`jacobian * axis_step`).
/// `fraction` is the query point's fractional position within the cell,
/// `(0, 0)` at the first corner and `(1, 1)` at the third.
pub fn hermite_cell_2d(
    values: [DVec3; 4],
    gradients: [DMat3; 4],
    fraction: DVec2,
    axis_steps: [DVec3; 2],
) -> DVec3 {
    let tangent_along = |corner: usize, axis: usize| gradients[corner] * axis_steps[axis];

    // Collapse v for the low-u edge (corners 0 and 3) and the high-u edge
    // (corners 1 and 2), each producing a cubic-blended value and a
    // linearly-propagated u tangent along that edge.
    let collapse_v = |low_v: usize, high_v: usize| {
        let value = hermite_1d(
            fraction.y,
            values[low_v],
            tangent_along(low_v, 1),
            values[high_v],
            tangent_along(high_v, 1),
        );
        let tangent_u = tangent_along(low_v, 0).lerp(tangent_along(high_v, 0), fraction.y);
        (value, tangent_u)
    };
    let (value_low_u, tangent_u_low) = collapse_v(0, 3);
    let (value_high_u, tangent_u_high) = collapse_v(1, 2);

    hermite_1d(
        fraction.x,
        value_low_u,
        tangent_u_low,
        value_high_u,
        tangent_u_high,
    )
}

/// Interpolate a vector field within an 8-corner (3D) cell.
///
/// `values`/`gradients` are in the binary-counter bit order
/// `GridInterpolation`/`BoxInterpolation` already build their `indices`
/// in: corner index `i + 2j + 4k` where `i`/`j`/`k` are the 0/1 bit along
/// local u/v/w respectively — `[(0,0,0), (1,0,0), (0,1,0), (1,1,0),
/// (0,0,1), (1,0,1), (0,1,1), (1,1,1)]`.
///
/// `gradients[i]` is the field's Jacobian at that corner (column `j` is
/// `∂field/∂x_j`); `axis_steps` are the cell's world-space edge vectors
/// along local u/v/w. `fraction` is the query point's fractional position
/// within the cell, each component in `[0, 1]`.
pub fn hermite_cell_3d(
    values: [DVec3; 8],
    gradients: [DMat3; 8],
    fraction: DVec3,
    axis_steps: [DVec3; 3],
) -> DVec3 {
    let index = |i: usize, j: usize, k: usize| i + 2 * j + 4 * k;
    let tangent_along = |corner: usize, axis: usize| gradients[corner] * axis_steps[axis];

    // Collapse w (axis 2): for each of the 4 (i, j) corners of the u-v
    // face, cubic-blend between k=0 and k=1, producing a value and
    // linearly-propagated u/v tangents at that face point.
    let mut face_value = [[DVec3::ZERO; 2]; 2];
    let mut face_tangent_u = [[DVec3::ZERO; 2]; 2];
    let mut face_tangent_v = [[DVec3::ZERO; 2]; 2];
    for (i, row) in face_value.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let low = index(i, j, 0);
            let high = index(i, j, 1);
            *cell = hermite_1d(
                fraction.z,
                values[low],
                tangent_along(low, 2),
                values[high],
                tangent_along(high, 2),
            );
            face_tangent_u[i][j] = tangent_along(low, 0).lerp(tangent_along(high, 0), fraction.z);
            face_tangent_v[i][j] = tangent_along(low, 1).lerp(tangent_along(high, 1), fraction.z);
        }
    }

    // Collapse v (axis 1): for each of the 2 u edges, cubic-blend between
    // j=0 and j=1 using the propagated v tangent, producing a value and a
    // further-propagated u tangent along that edge.
    let mut edge_value = [DVec3::ZERO; 2];
    let mut edge_tangent_u = [DVec3::ZERO; 2];
    for i in 0..2 {
        edge_value[i] = hermite_1d(
            fraction.y,
            face_value[i][0],
            face_tangent_v[i][0],
            face_value[i][1],
            face_tangent_v[i][1],
        );
        edge_tangent_u[i] = face_tangent_u[i][0].lerp(face_tangent_u[i][1], fraction.y);
    }

    // Collapse u (axis 0): the final cubic blend.
    hermite_1d(
        fraction.x,
        edge_value[0],
        edge_tangent_u[0],
        edge_value[1],
        edge_tangent_u[1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An affine field (`f(p) = M·p + c`) has a constant Jacobian `M`
    /// everywhere, so the cubic Hermite reconstruction — which matches
    /// value and derivative exactly at both ends of every collapsed axis —
    /// must reproduce it exactly, not merely approximate it.
    #[test]
    fn hermite_cell_3d_reproduces_a_planted_affine_field_exactly() {
        let m = DMat3::from_cols(
            DVec3::new(1.0, 2.0, 0.0),
            DVec3::new(0.5, -1.0, 3.0),
            DVec3::new(-2.0, 0.0, 1.0),
        );
        let c = DVec3::new(0.3, -0.2, 0.1);
        let f = |p: DVec3| m * p + c;

        let axis_steps = [DVec3::X, DVec3::Y, DVec3::Z];
        let mut values = [DVec3::ZERO; 8];
        let gradients = [m; 8];
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    let index = i + 2 * j + 4 * k;
                    values[index] = f(DVec3::new(i as f64, j as f64, k as f64));
                }
            }
        }

        let fraction = DVec3::new(0.3, 0.7, 0.4);
        let result = hermite_cell_3d(values, gradients, fraction, axis_steps);
        let expected = f(fraction);

        assert!(
            (result - expected).length() < 1.0e-9,
            "expected {expected:?}, got {result:?}"
        );
    }

    /// A per-axis-separable quadratic field (`f(p) = (x², y², z²)`) is not
    /// affine, so plain trilinear blending of the corner values alone
    /// (linear per axis) cannot reproduce it — but supplying the exact
    /// corner gradients lets cubic Hermite reconstruct it exactly, since a
    /// quadratic is the unique cubic satisfying the same value+derivative
    /// constraints at both endpoints of each axis.
    #[test]
    fn hermite_cell_3d_reconstructs_a_curved_field_that_trilinear_gets_wrong() {
        let axis_steps = [DVec3::X, DVec3::Y, DVec3::Z];
        let mut values = [DVec3::ZERO; 8];
        let mut gradients = [DMat3::ZERO; 8];
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    let index = i + 2 * j + 4 * k;
                    let (x, y, z) = (i as f64, j as f64, k as f64);
                    values[index] = DVec3::new(x * x, y * y, z * z);
                    gradients[index] = DMat3::from_diagonal(DVec3::new(2.0 * x, 2.0 * y, 2.0 * z));
                }
            }
        }

        let fraction = DVec3::splat(0.5);
        let hermite_result = hermite_cell_3d(values, gradients, fraction, axis_steps);
        let expected = DVec3::splat(0.25); // f(0.5, 0.5, 0.5) = (0.25, 0.25, 0.25)

        // Plain trilinear blend of the same corner values, for comparison —
        // equal weight on every corner along each axis at the cell's exact
        // midpoint, i.e. the mean of the 8 corner values.
        let trilinear_result: DVec3 = values.iter().copied().sum::<DVec3>() / 8.0;

        let hermite_error = (hermite_result - expected).length();
        let trilinear_error = (trilinear_result - expected).length();

        assert!(
            hermite_error < 1.0e-9,
            "Hermite should reconstruct this field exactly, error {hermite_error}"
        );
        assert!(
            trilinear_error > 0.2,
            "trilinear should visibly miss the curvature, error {trilinear_error}"
        );
    }

    /// The 2D quad-winding corner order must be handled correctly — an
    /// affine field is the simplest check that `collapse_v(0, 3)` /
    /// `collapse_v(1, 2)` pair up the right corners.
    #[test]
    fn hermite_cell_2d_reproduces_a_planted_affine_field_exactly() {
        let m = DMat3::from_cols(
            DVec3::new(1.0, -1.0, 0.5),
            DVec3::new(2.0, 0.0, -0.5),
            DVec3::ZERO,
        );
        let c = DVec3::new(0.1, 0.2, 0.3);
        let f = |p: DVec3| m * p + c;

        let axis_steps = [DVec3::X, DVec3::Y];
        // Quad order: (u0,v0), (u1,v0), (u1,v1), (u0,v1).
        let corners = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
        ];
        let values = corners.map(f);
        let gradients = [m; 4];

        let fraction = DVec2::new(0.25, 0.9);
        let result = hermite_cell_2d(values, gradients, fraction, axis_steps);
        let expected = f(DVec3::new(fraction.x, fraction.y, 0.0));

        assert!(
            (result - expected).length() < 1.0e-9,
            "expected {expected:?}, got {result:?}"
        );
    }
}
