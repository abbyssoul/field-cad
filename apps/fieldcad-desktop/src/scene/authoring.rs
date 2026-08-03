//! Viewport proxies for things a user authors but a solver does not draw.
//!
//! Planes and probes have no rendered body of their own, so they need one to be
//! visible and selectable — independently of whether any field layer is on.

use fieldcad_core::{SlicePlane, WorldSnapshot};
use glam::{Vec3, Vec4};

use super::{FieldGeometry, SceneSelection, push_line, push_quad, push_quad_outline};

/// Draw authoring proxies independently of whether a field layer is enabled.
/// A translucent plane body makes it selectable; the solid corner tab gives a
/// stable visual anchor when a magnitude map fills the same rectangle.
pub fn append_authoring_geometry(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    selection: Option<SceneSelection>,
) {
    for plane in world.planes().values().filter(|plane| plane.visible) {
        append_plane_proxy(
            geometry,
            plane,
            selection == Some(SceneSelection::Plane(plane.id)),
        );
    }
    for probe in world.probes().values().filter(|probe| probe.visible) {
        let Ok(position) = world.resolve_probe_position(probe) else {
            continue;
        };
        let position = position.as_vec3();
        let size = 0.09;
        let color = if selection == Some(SceneSelection::Probe(probe.id)) {
            Vec4::new(1.0, 0.55, 0.08, 1.0)
        } else {
            Vec4::new(0.95, 0.95, 0.95, 1.0)
        };
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            push_line(
                &mut geometry.vector_lines,
                position - axis * size,
                position + axis * size,
                color,
            );
        }
    }
}

fn append_plane_proxy(geometry: &mut FieldGeometry, plane: &SlicePlane, selected: bool) {
    let (u, v) = plane.basis();
    let u = u.as_vec3();
    let v = v.as_vec3();
    let origin = plane.origin.as_vec3();
    let normal = plane.normal.as_vec3();
    let half = plane.half_extent.as_vec2();
    let offset = normal * 0.002;
    let corners = [
        origin - u * half.x - v * half.y + offset,
        origin + u * half.x - v * half.y + offset,
        origin + u * half.x + v * half.y + offset,
        origin - u * half.x + v * half.y + offset,
    ];
    let body = if selected {
        Vec4::new(1.0, 0.48, 0.08, 0.12)
    } else {
        Vec4::new(0.5, 0.72, 0.86, 0.055)
    };
    let outline = if selected {
        Vec4::new(1.0, 0.48, 0.08, 1.0)
    } else {
        Vec4::new(0.55, 0.78, 0.92, 0.9)
    };
    push_quad(&mut geometry.surface_triangles, corners, body);
    push_quad_outline(&mut geometry.vector_lines, corners, outline);

    let tab = half.min_element().max(0.01) * 0.12;
    let corner = corners[0] + normal * 0.003;
    push_quad(
        &mut geometry.surface_triangles,
        [
            corner,
            corner + u * tab,
            corner + u * tab + v * tab,
            corner + v * tab,
        ],
        outline,
    );
}
