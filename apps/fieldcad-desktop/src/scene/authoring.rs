//! Viewport proxies for things a user authors but a solver does not draw.
//!
//! Planes and probes have no rendered body of their own, so they need one to be
//! visible and selectable — independently of whether any field layer is on.

use fieldcad_core::{
    DEFAULT_PROXY_RADIUS, DomainBounds, FieldBox, FieldSphere, ProbePosition, SceneScale,
    SlicePlane, WorldCommand, WorldSnapshot,
};
use glam::{DVec3, Quat, Vec3, Vec4};

use super::{
    FieldGeometry, SceneSelection, push_circle, push_dashed_line, push_line, push_quad,
    push_quad_outline, quat_from_dquat,
};

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
    pub boxes: bool,
    pub spheres: bool,
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
        boxes: true,
        spheres: true,
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
            SceneSelection::Box(_) => self.boxes,
            SceneSelection::Sphere(_) => self.spheres,
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
    scene_scale: SceneScale,
    universe: Option<fieldcad_core::UniverseSummary>,
) {
    if show.planes {
        for plane in world.planes().values().filter(|plane| plane.visible) {
            append_plane_proxy(
                geometry,
                world,
                plane,
                selection == Some(SceneSelection::Plane(plane.id)),
                scene_scale,
            );
        }
    }
    if show.boxes {
        for field_box in world.boxes().values().filter(|region| region.visible) {
            append_box_proxy(
                geometry,
                world,
                field_box,
                selection == Some(SceneSelection::Box(field_box.id)),
                scene_scale,
            );
        }
    }
    if show.spheres {
        for sphere in world.spheres().values().filter(|sphere| sphere.visible) {
            append_sphere_proxy(
                geometry,
                world,
                sphere,
                selection == Some(SceneSelection::Sphere(sphere.id)),
                scene_scale,
            );
        }
    }
    // Always drawn, independent of `show`: the centre of mass isn't a
    // `SceneVisibility` class of its own (it's not user-authored, so there's
    // nothing for a visibility toggle to hide), and it's the one derived
    // object in `world.objects()` — see `WorldObject::derived`.
    if let Some(center_of_mass) = world.objects().values().find(|object| object.derived) {
        let position = scene_scale.to_render_vec3(center_of_mass.transform.translation);
        let radius = 0.1;
        let color = Vec4::new(1.0, 0.82, 0.2, 1.0);
        push_circle(
            &mut geometry.vector_lines,
            position,
            Vec3::X,
            Vec3::Y,
            radius,
            color,
        );
        push_circle(
            &mut geometry.vector_lines,
            position,
            Vec3::Y,
            Vec3::Z,
            radius,
            color,
        );
        push_circle(
            &mut geometry.vector_lines,
            position,
            Vec3::Z,
            Vec3::X,
            radius,
            color,
        );

        if let Some(universe) = universe {
            super::append_arrow(
                &mut geometry.vector_lines,
                position,
                universe.total_momentum.as_vec3(),
                0.25,
                Vec4::new(0.4, 0.85, 1.0, 1.0),
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
        let position = scene_scale.to_render_vec3(position);
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

/// A translucent stand-in for a scene edit the mutation queue is holding
/// while paused (BE-16): the entity hasn't actually moved yet — whatever
/// draws its real body elsewhere still shows it at its current pose — so
/// this draws a dimmed proxy at the pose the edit will land at, plus a
/// dashed line back to the entity's current position, tinted to match the
/// Queue panel's own "paused" indicator so the two read as the same fact.
///
/// Reuses each entity's own translucent-proxy visual (`append_box_visual`,
/// `append_sphere_visual`, the plane quad, the probe cross marker) rather
/// than inventing a second look for "not the real thing yet" — this app
/// already has one.
pub fn append_pending_edit_ghosts(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    edits: &[&WorldCommand],
    scene_scale: SceneScale,
) {
    const GHOST_BODY: Vec4 = Vec4::new(0.92, 0.75, 0.29, 0.16);
    const GHOST_OUTLINE: Vec4 = Vec4::new(0.92, 0.75, 0.29, 0.85);
    const LINK_COLOR: Vec4 = Vec4::new(0.92, 0.75, 0.29, 0.8);

    let link = |geometry: &mut FieldGeometry, current: Vec3, pending: Vec3| {
        push_dashed_line(
            &mut geometry.vector_lines,
            current,
            pending,
            LINK_COLOR,
            0.08,
            0.05,
        );
    };

    for edit in edits {
        match edit {
            WorldCommand::SetTransform { object, transform } => {
                let Some(current) = world.object(*object) else {
                    continue;
                };
                let half_extent = scene_scale.to_render_vec3(
                    current
                        .shape
                        .map_or(DVec3::splat(DEFAULT_PROXY_RADIUS), |shape| {
                            shape.half_extent()
                        }),
                );
                let pending_origin = scene_scale.to_render_vec3(transform.translation);
                append_box_visual(
                    geometry,
                    pending_origin,
                    quat_from_dquat(transform.rotation),
                    half_extent,
                    GHOST_BODY,
                    GHOST_OUTLINE,
                );
                link(
                    geometry,
                    scene_scale.to_render_vec3(current.transform.translation),
                    pending_origin,
                );
            }
            WorldCommand::SetPlane { plane, spec } => {
                let Some(current) = world.planes().get(plane) else {
                    continue;
                };
                let origin = scene_scale.to_render_vec3(spec.origin());
                let normal = spec.normal().as_vec3();
                let u = spec.u_axis().as_vec3();
                let v = normal.cross(u);
                let half = scene_scale.to_render_vec2(spec.half_extent());
                let corners = [
                    origin - u * half.x - v * half.y,
                    origin + u * half.x - v * half.y,
                    origin + u * half.x + v * half.y,
                    origin - u * half.x + v * half.y,
                ];
                push_quad(&mut geometry.surface_triangles, corners, GHOST_BODY);
                push_quad_outline(&mut geometry.vector_lines, corners, GHOST_OUTLINE);
                link(geometry, scene_scale.to_render_vec3(current.origin), origin);
            }
            WorldCommand::SetBox { region, spec } => {
                let Some(current) = world.boxes().get(region) else {
                    continue;
                };
                let origin = scene_scale.to_render_vec3(spec.origin());
                append_box_visual(
                    geometry,
                    origin,
                    quat_from_dquat(spec.rotation()),
                    scene_scale.to_render_vec3(spec.half_extent()),
                    GHOST_BODY,
                    GHOST_OUTLINE,
                );
                link(geometry, scene_scale.to_render_vec3(current.origin), origin);
            }
            WorldCommand::SetSphere { sphere, spec } => {
                let Some(current) = world.spheres().get(sphere) else {
                    continue;
                };
                let origin = scene_scale.to_render_vec3(spec.origin());
                let radius = scene_scale.to_render(spec.radius());
                append_sphere_visual(geometry, origin, radius, GHOST_BODY, GHOST_OUTLINE);
                link(geometry, scene_scale.to_render_vec3(current.origin), origin);
            }
            WorldCommand::SetProbePosition { probe, position } => {
                let Some(current) = world.probe(*probe) else {
                    continue;
                };
                let Ok(current_position) = world.resolve_probe_position(current) else {
                    continue;
                };
                let pending_position = match position {
                    ProbePosition::World(position) => *position,
                    ProbePosition::Attached { object, offset } => {
                        let Some(parent) = world.object(*object) else {
                            continue;
                        };
                        parent.transform.apply(*offset)
                    }
                };
                let origin = scene_scale.to_render_vec3(pending_position);
                let size = 0.09;
                for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                    push_line(
                        &mut geometry.vector_lines,
                        origin - axis * size,
                        origin + axis * size,
                        GHOST_OUTLINE,
                    );
                }
                link(
                    geometry,
                    scene_scale.to_render_vec3(current_position),
                    origin,
                );
            }
            _ => {}
        }
    }
}

fn append_plane_proxy(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    plane: &SlicePlane,
    selected: bool,
    scene_scale: SceneScale,
) {
    let Ok((plane_origin, plane_normal, plane_u_axis)) = world.resolve_plane_frame(plane) else {
        return;
    };
    let plane = SlicePlane {
        origin: plane_origin,
        normal: plane_normal,
        u_axis: plane_u_axis,
        ..plane.clone()
    };
    // `u`, `v`, and `normal` are unit directions, not lengths — cast as-is.
    let (u, v) = plane.basis();
    let u = u.as_vec3();
    let v = v.as_vec3();
    let origin = scene_scale.to_render_vec3(plane.origin);
    let normal = plane.normal.as_vec3();
    let half = scene_scale.to_render_vec2(plane.half_extent);
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

/// The eight world-space corners of a field box, in `±x, ±y, ±z` bit order
/// (matching [`push_quad`]'s winding expectations per face below).
fn box_corners(origin: Vec3, rotation: Quat, half_extent: Vec3) -> [Vec3; 8] {
    let mut corners = [Vec3::ZERO; 8];
    for (index, corner) in corners.iter_mut().enumerate() {
        let signs = Vec3::new(
            if index & 1 == 0 { -1.0 } else { 1.0 },
            if index & 2 == 0 { -1.0 } else { 1.0 },
            if index & 4 == 0 { -1.0 } else { 1.0 },
        );
        *corner = origin + rotation * (half_extent * signs);
    }
    corners
}

/// A translucent volume with a white outline: the box's rights are the same
/// as a slice plane's — selectable, draggable, deletable — but its body has
/// no field-independent purpose beyond marking where it is.
fn append_box_proxy(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    field_box: &FieldBox,
    selected: bool,
    scene_scale: SceneScale,
) {
    let Ok((box_origin, box_rotation)) = world.resolve_box_frame(field_box) else {
        return;
    };
    let body = if selected {
        Vec4::new(1.0, 0.48, 0.08, 0.10)
    } else {
        Vec4::new(0.9, 0.9, 0.95, 0.05)
    };
    let outline = if selected {
        Vec4::new(1.0, 0.48, 0.08, 1.0)
    } else {
        Vec4::new(1.0, 1.0, 1.0, 0.85)
    };
    append_box_visual(
        geometry,
        scene_scale.to_render_vec3(box_origin),
        quat_from_dquat(box_rotation),
        scene_scale.to_render_vec3(field_box.half_extent),
        body,
        outline,
    );
}

/// Draw the active computation's spatial extent separately from authored field
/// boxes. The entry point deliberately accepts the domain's bounds rather than
/// a scene object, leaving room for other domain-shape renderers later.
pub fn append_compute_bounds(
    geometry: &mut FieldGeometry,
    bounds: DomainBounds,
    scene_scale: SceneScale,
) {
    append_box_visual(
        geometry,
        scene_scale.to_render_vec3(bounds.centre()),
        Quat::IDENTITY,
        scene_scale.to_render_vec3(bounds.size() * 0.5),
        Vec4::new(0.25, 0.75, 1.0, 0.035),
        Vec4::new(0.25, 0.75, 1.0, 0.9),
    );
}

fn append_box_visual(
    geometry: &mut FieldGeometry,
    origin: Vec3,
    rotation: Quat,
    half_extent: Vec3,
    body: Vec4,
    outline: Vec4,
) {
    let corners = box_corners(origin, rotation, half_extent);
    // Corner indices per face, wound so `push_quad`'s two triangles face
    // outward; index bits are (x, y, z) as in `box_corners`.
    const FACES: [[usize; 4]; 6] = [
        [0, 2, 6, 4], // -x
        [1, 5, 7, 3], // +x
        [0, 1, 3, 2], // -y
        [4, 6, 7, 5], // +y
        [0, 4, 5, 1], // -z
        [2, 3, 7, 6], // +z
    ];
    for face in FACES {
        let quad = [
            corners[face[0]],
            corners[face[1]],
            corners[face[2]],
            corners[face[3]],
        ];
        push_quad(&mut geometry.surface_triangles, quad, body);
        push_quad_outline(&mut geometry.vector_lines, quad, outline);
    }
}

/// A translucent "crystal ball" shell with a white wireframe: three great
/// circles plus a low-poly latitude/longitude mesh, reusing [`push_circle`]
/// for the wireframe the same way the selection origin marker does.
fn append_sphere_proxy(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    sphere: &FieldSphere,
    selected: bool,
    scene_scale: SceneScale,
) {
    let Ok(sphere_origin) = world.resolve_sphere_origin(sphere) else {
        return;
    };
    let origin = scene_scale.to_render_vec3(sphere_origin);
    let radius = scene_scale.to_render(sphere.radius);
    let body = if selected {
        Vec4::new(1.0, 0.48, 0.08, 0.10)
    } else {
        Vec4::new(0.6, 0.8, 0.95, 0.06)
    };
    let outline = if selected {
        Vec4::new(1.0, 0.48, 0.08, 1.0)
    } else {
        Vec4::new(1.0, 1.0, 1.0, 0.85)
    };

    append_sphere_visual(geometry, origin, radius, body, outline);
}

fn append_sphere_visual(
    geometry: &mut FieldGeometry,
    origin: Vec3,
    radius: f32,
    body: Vec4,
    outline: Vec4,
) {
    for (a, b) in [(Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z), (Vec3::Z, Vec3::X)] {
        push_circle(&mut geometry.vector_lines, origin, a, b, radius, outline);
    }

    const LATITUDES: u32 = 8;
    const LONGITUDES: u32 = 16;
    for lat in 0..LATITUDES {
        let theta0 = std::f32::consts::PI * lat as f32 / LATITUDES as f32;
        let theta1 = std::f32::consts::PI * (lat + 1) as f32 / LATITUDES as f32;
        for lon in 0..LONGITUDES {
            let phi0 = std::f32::consts::TAU * lon as f32 / LONGITUDES as f32;
            let phi1 = std::f32::consts::TAU * (lon + 1) as f32 / LONGITUDES as f32;
            let vertex = |theta: f32, phi: f32| {
                origin
                    + radius
                        * Vec3::new(
                            theta.sin() * phi.cos(),
                            theta.sin() * phi.sin(),
                            theta.cos(),
                        )
            };
            push_quad(
                &mut geometry.surface_triangles,
                [
                    vertex(theta0, phi0),
                    vertex(theta1, phi0),
                    vertex(theta1, phi1),
                    vertex(theta0, phi1),
                ],
                body,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        BoxId, FieldBoxSpec, FieldSphereSpec, ObjectId, ObjectSpec, PlaneId, ProbeId, SphereId,
        Transform as CoreTransform, World, WorldCommand,
    };
    use glam::DVec3;

    use super::*;

    /// Whether a selection is on screen decides whether it gets a gizmo and
    /// whether a drag may start on it, so each class must answer for itself.
    #[test]
    fn visibility_answers_per_class_for_the_selected_entity() {
        let only_planes = SceneVisibility {
            objects: false,
            probes: false,
            planes: true,
            boxes: false,
            spheres: false,
        };

        assert!(!only_planes.shows(SceneSelection::Object(ObjectId::new(0))));
        assert!(!only_planes.shows(SceneSelection::Probe(ProbeId::new(0))));
        assert!(only_planes.shows(SceneSelection::Plane(PlaneId::new(0))));
        assert!(!only_planes.shows(SceneSelection::Box(BoxId::new(0))));
        assert!(!only_planes.shows(SceneSelection::Sphere(SphereId::new(0))));

        for selection in [
            SceneSelection::Object(ObjectId::new(1)),
            SceneSelection::Probe(ProbeId::new(1)),
            SceneSelection::Plane(PlaneId::new(1)),
            SceneSelection::Box(BoxId::new(1)),
            SceneSelection::Sphere(SphereId::new(1)),
        ] {
            assert!(SceneVisibility::ALL.shows(selection));
        }
    }

    #[test]
    fn a_session_opens_showing_everything() {
        assert_eq!(SceneVisibility::default(), SceneVisibility::ALL);
    }

    #[test]
    fn box_and_sphere_proxies_are_drawn_for_visible_regions() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateBox(
                    FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(1.0)).unwrap(),
                ),
                WorldCommand::CreateSphere(FieldSphereSpec::new("ball", DVec3::ZERO, 1.0).unwrap()),
            ])
            .unwrap();
        let snapshot = world.snapshot();

        let mut geometry = FieldGeometry::default();
        append_authoring_geometry(
            &mut geometry,
            &snapshot,
            None,
            SceneVisibility::ALL,
            SceneScale::metre(),
            None,
        );
        assert!(!geometry.surface_triangles.is_empty());
        assert!(!geometry.vector_lines.is_empty());

        let mut hidden = FieldGeometry::default();
        append_authoring_geometry(
            &mut hidden,
            &snapshot,
            None,
            SceneVisibility {
                boxes: false,
                spheres: false,
                ..SceneVisibility::ALL
            },
            SceneScale::metre(),
            None,
        );
        assert!(hidden.surface_triangles.is_empty());
        assert!(hidden.vector_lines.is_empty());
    }

    #[test]
    fn a_derived_object_gets_a_marker_and_a_momentum_arrow_when_universe_is_known() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("Center of mass")
                    .with_transform(CoreTransform::at(DVec3::new(1.0, 2.0, 3.0)).unwrap())
                    .derived(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let universe = fieldcad_core::UniverseSummary {
            center_of_mass: DVec3::new(1.0, 2.0, 3.0),
            total_momentum: DVec3::new(1.0, 0.0, 0.0),
            total_kinetic_energy_j: 4.0,
        };

        let mut without_universe = FieldGeometry::default();
        append_authoring_geometry(
            &mut without_universe,
            &snapshot,
            None,
            SceneVisibility::ALL,
            SceneScale::metre(),
            None,
        );
        // The marker itself doesn't need `universe` — only the arrow does.
        assert!(!without_universe.vector_lines.is_empty());
        let marker_only_lines = without_universe.vector_lines.len();

        let mut with_universe = FieldGeometry::default();
        append_authoring_geometry(
            &mut with_universe,
            &snapshot,
            None,
            SceneVisibility::ALL,
            SceneScale::metre(),
            Some(universe),
        );
        assert!(with_universe.vector_lines.len() > marker_only_lines);
    }

    #[test]
    fn computation_bounds_are_drawn_without_creating_a_scene_object() {
        let mut geometry = FieldGeometry::default();
        append_compute_bounds(
            &mut geometry,
            fieldcad_core::DomainBounds::centred_cube(2.0).unwrap(),
            SceneScale::metre(),
        );

        assert!(!geometry.surface_triangles.is_empty());
        assert!(!geometry.vector_lines.is_empty());
    }
}
