//! Viewport proxies for things a user authors but a solver does not draw.
//!
//! Planes and probes have no rendered body of their own, so they need one to be
//! visible and selectable — independently of whether any field layer is on.

use fieldcad_core::{SlicePlane, WorldSnapshot};
use glam::{Vec3, Vec4};

use super::{FieldGeometry, SceneSelection, push_line, push_quad, push_quad_outline};

/// Which classes of thing the viewport is currently drawing.
///
/// A view filter, not world state: a hidden probe is still recording, a hidden
/// plane is still sampled, and a hidden object still sources its field. The two
/// are kept apart so that turning a class off to see the field cannot change
/// what is simulated or measured.
///
/// Every consumer that decides what the user can *see* takes this — drawing,
/// selection gizmos, and hit-testing alike. That is deliberate: when only the
/// renderer consulted it, hidden objects stayed clickable, and a user could
/// select and drag something invisible. Requiring the argument means a new
/// consumer cannot quietly disagree about what is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneVisibility {
    pub objects: bool,
    pub probes: bool,
    pub planes: bool,
}

impl Default for SceneVisibility {
    fn default() -> Self {
        Self::ALL
    }
}

impl SceneVisibility {
    /// Everything drawn, the state a session opens in.
    pub const ALL: Self = Self {
        objects: true,
        probes: true,
        planes: true,
    };

    /// Whether a particular selection is currently on screen.
    ///
    /// Used to suppress the transform gizmo and its drag handles for something
    /// the user cannot see; the entity stays selected and editable in the
    /// inspector, because hiding a class is a view choice and must not discard
    /// what the user was working on.
    pub const fn shows(self, selection: SceneSelection) -> bool {
        match selection {
            SceneSelection::Object(_) => self.objects,
            SceneSelection::Probe(_) => self.probes,
            SceneSelection::Plane(_) => self.planes,
        }
    }
}

/// Draw authoring proxies independently of whether a field layer is enabled.
/// A translucent plane body makes it selectable; the solid corner tab gives a
/// stable visual anchor when a magnitude map fills the same rectangle.
pub fn append_authoring_geometry(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    selection: Option<SceneSelection>,
    show: SceneVisibility,
) {
    if show.planes {
        for plane in world.planes().values().filter(|plane| plane.visible) {
            append_plane_proxy(
                geometry,
                plane,
                selection == Some(SceneSelection::Plane(plane.id)),
            );
        }
    }
    if !show.probes {
        return;
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

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectId, PlaneId, ProbeId};

    use super::*;

    /// Whether a selection is on screen decides whether it gets a gizmo and
    /// whether a drag may start on it, so each class must answer for itself.
    #[test]
    fn visibility_answers_per_class_for_the_selected_entity() {
        let only_planes = SceneVisibility {
            objects: false,
            probes: false,
            planes: true,
        };

        assert!(!only_planes.shows(SceneSelection::Object(ObjectId::new(0))));
        assert!(!only_planes.shows(SceneSelection::Probe(ProbeId::new(0))));
        assert!(only_planes.shows(SceneSelection::Plane(PlaneId::new(0))));

        for selection in [
            SceneSelection::Object(ObjectId::new(1)),
            SceneSelection::Probe(ProbeId::new(1)),
            SceneSelection::Plane(PlaneId::new(1)),
        ] {
            assert!(SceneVisibility::ALL.shows(selection));
        }
    }

    #[test]
    fn a_session_opens_showing_everything() {
        assert_eq!(SceneVisibility::default(), SceneVisibility::ALL);
    }
}
