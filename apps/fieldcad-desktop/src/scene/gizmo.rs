//! The translation gizmo: how it is drawn, hit-tested, and dragged.
//!
//! Drawing and picking derive their geometry from the same functions here. When
//! each computed its own, a change to the drawn gizmo silently moved the
//! handles away from where they appeared to be.

use fieldcad_core::{SlicePlane, WorldObject};
use glam::{Vec2, Vec3, Vec4};

use super::pick::{
    point_in_triangle, point_segment_distance, project_to_viewport, ray_plane_point,
};
use super::{
    FieldGeometry, SceneSelection, WorldSnapshot, append_arrow, push_line, push_quad,
    push_quad_outline,
};
use crate::camera::{OrbitCamera, Viewport};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformHandle {
    AxisX,
    AxisY,
    AxisZ,
    PlaneXY,
    PlaneYZ,
    PlaneZX,
    /// Reorients a selected slice plane; never translates it.
    PlaneNormal,
}

/// Immediate authoring-helper state while the source is acknowledging a drag.
/// Solver snapshots and world geometry remain authoritative; only the gizmo
/// tracks the pointer optimistically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformPreview {
    pub origin: Vec3,
    pub plane_normal: Option<Vec3>,
}

impl TransformHandle {
    pub const fn label(self) -> &'static str {
        match self {
            Self::AxisX => "X axis",
            Self::AxisY => "Y axis",
            Self::AxisZ => "Z axis",
            Self::PlaneXY => "XY plane",
            Self::PlaneYZ => "YZ plane",
            Self::PlaneZX => "ZX plane",
            Self::PlaneNormal => "plane normal N",
        }
    }

    pub const fn axis(self) -> Option<Vec3> {
        match self {
            Self::AxisX => Some(Vec3::X),
            Self::AxisY => Some(Vec3::Y),
            Self::AxisZ => Some(Vec3::Z),
            Self::PlaneXY | Self::PlaneYZ | Self::PlaneZX | Self::PlaneNormal => None,
        }
    }

    pub const fn plane_normal(self) -> Option<Vec3> {
        match self {
            Self::PlaneXY => Some(Vec3::Z),
            Self::PlaneYZ => Some(Vec3::X),
            Self::PlaneZX => Some(Vec3::Y),
            Self::AxisX | Self::AxisY | Self::AxisZ | Self::PlaneNormal => None,
        }
    }
}

pub fn append_transform_gizmo(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    selection: Option<SceneSelection>,
    active: Option<TransformHandle>,
    preview: Option<TransformPreview>,
) {
    let Some(selection) = selection else {
        return;
    };
    let Some((world_origin, length)) = transform_gizmo(world, selection) else {
        return;
    };
    let origin = preview.map_or(world_origin, |preview| preview.origin);

    append_origin_marker(
        geometry,
        origin,
        selection_marker_radius(world, selection, length),
    );

    const AXIS_COLORS: [Vec4; 3] = [
        Vec4::new(0.95, 0.15, 0.18, 1.0),
        Vec4::new(0.18, 0.9, 0.3, 1.0),
        Vec4::new(0.2, 0.45, 1.0, 1.0),
    ];
    const PLANE_COLORS: [Vec4; 3] = [
        Vec4::new(0.95, 0.84, 0.12, 0.28),
        Vec4::new(0.1, 0.8, 0.8, 0.28),
        Vec4::new(0.85, 0.2, 0.82, 0.28),
    ];

    for ((handle, direction), color) in GIZMO_AXES.into_iter().zip(AXIS_COLORS) {
        append_arrow(
            &mut geometry.vector_lines,
            origin,
            direction,
            length,
            handle_color(color, handle, active, 1.0),
        );
    }

    for ((handle, a, b), color) in GIZMO_PLANES.into_iter().zip(PLANE_COLORS) {
        append_gizmo_plane(
            geometry,
            gizmo_plane_corners(origin, a, b, length),
            handle_color(color, handle, active, 0.72),
        );
    }

    if let SceneSelection::Plane(id) = selection
        && let Some(plane) = world.planes().get(&id)
    {
        append_plane_normal(
            geometry,
            plane,
            origin,
            preview
                .and_then(|preview| preview.plane_normal)
                .unwrap_or(plane.normal.as_vec3()),
            length,
            active == Some(TransformHandle::PlaneNormal),
        );
    }
}

/// Highlight the handle being dragged and dim the others, so a constrained drag
/// says which constraint it is under.
fn handle_color(
    color: Vec4,
    handle: TransformHandle,
    active: Option<TransformHandle>,
    active_alpha: f32,
) -> Vec4 {
    match active {
        Some(active) if active == handle => Vec4::new(1.0, 0.9, 0.18, active_alpha),
        Some(_) => color * Vec4::new(0.45, 0.45, 0.45, 1.0),
        None => color,
    }
}

fn object_gizmo_length(object: &WorldObject) -> f32 {
    object
        .shape
        .map_or(0.5, |shape| shape.bounding_radius() as f32 * 2.0)
        .max(0.9)
}

fn transform_gizmo(world: &WorldSnapshot, selection: SceneSelection) -> Option<(Vec3, f32)> {
    match selection {
        SceneSelection::Object(id) => {
            let object = world.object(id).filter(|object| object.visible)?;
            Some((
                object.transform.translation.as_vec3(),
                object_gizmo_length(object),
            ))
        }
        SceneSelection::Plane(id) => {
            let plane = world.planes().get(&id).filter(|plane| plane.visible)?;
            let length = (plane.half_extent.min_element() as f32 * 0.4).clamp(0.9, 1.8);
            Some((plane.origin.as_vec3(), length))
        }
        SceneSelection::Probe(id) => {
            let probe = world.probe(id).filter(|probe| probe.visible)?;
            Some((world.resolve_probe_position(probe).ok()?.as_vec3(), 0.9))
        }
    }
}

fn selection_marker_radius(
    world: &WorldSnapshot,
    selection: SceneSelection,
    gizmo_length: f32,
) -> f32 {
    match selection {
        SceneSelection::Object(id) => world
            .object(id)
            .map_or(0.16, |object| object.bounding_sphere().1 as f32 * 1.12)
            .max(0.16),
        SceneSelection::Probe(_) => 0.17,
        SceneSelection::Plane(_) => (gizmo_length * 0.12).clamp(0.11, 0.22),
    }
}

pub fn selection_origin(world: &WorldSnapshot, selection: SceneSelection) -> Option<Vec3> {
    transform_gizmo(world, selection).map(|(origin, _)| origin)
}

fn append_origin_marker(geometry: &mut FieldGeometry, origin: Vec3, radius: f32) {
    for (a, b, color) in [
        (Vec3::Y, Vec3::Z, Vec4::new(1.0, 0.32, 0.34, 1.0)),
        (Vec3::Z, Vec3::X, Vec4::new(0.32, 1.0, 0.45, 1.0)),
        (Vec3::X, Vec3::Y, Vec4::new(0.38, 0.62, 1.0, 1.0)),
    ] {
        let mut previous = origin + a * radius;
        for segment in 1..=32 {
            let angle = std::f32::consts::TAU * segment as f32 / 32.0;
            let next = origin + (a * angle.cos() + b * angle.sin()) * radius;
            push_line(&mut geometry.vector_lines, previous, next, color);
            previous = next;
        }
    }
}

/// The four world-space corners of a plane handle's quad.
///
/// Drawing and picking both call this. When they each computed the quad from
/// their own copy of the inset fractions, a change to the drawn gizmo silently
/// moved the handles away from where they appeared to be — a mismatch with no
/// compile-time signal and an unpleasant manual reproduction.
fn gizmo_plane_corners(origin: Vec3, a: Vec3, b: Vec3, length: f32) -> [Vec3; 4] {
    let inner = length * 0.18;
    let outer = length * 0.42;
    [
        origin + a * inner + b * inner,
        origin + a * outer + b * inner,
        origin + a * outer + b * outer,
        origin + a * inner + b * outer,
    ]
}

/// The grabbable span of an axis handle: its outer part, clear of the plane
/// quads that overlap the inner part.
fn gizmo_axis_segment(origin: Vec3, axis: Vec3, length: f32) -> (Vec3, Vec3) {
    (origin + axis * length * 0.45, origin + axis * length)
}

/// The three plane handles and the in-plane axes each is dragged along.
const GIZMO_PLANES: [(TransformHandle, Vec3, Vec3); 3] = [
    (TransformHandle::PlaneXY, Vec3::X, Vec3::Y),
    (TransformHandle::PlaneYZ, Vec3::Y, Vec3::Z),
    (TransformHandle::PlaneZX, Vec3::Z, Vec3::X),
];

const GIZMO_AXES: [(TransformHandle, Vec3); 3] = [
    (TransformHandle::AxisX, Vec3::X),
    (TransformHandle::AxisY, Vec3::Y),
    (TransformHandle::AxisZ, Vec3::Z),
];

fn append_gizmo_plane(geometry: &mut FieldGeometry, corners: [Vec3; 4], color: Vec4) {
    push_quad(&mut geometry.surface_triangles, corners, color);
    push_quad_outline(&mut geometry.vector_lines, corners, color.with_w(0.95));
}

pub fn pick_transform_handle(
    world: &WorldSnapshot,
    selection: SceneSelection,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
) -> Option<TransformHandle> {
    let (origin, length) = transform_gizmo(world, selection)?;

    // The outer part of N is reserved for rotation. For the default XY plane
    // it overlaps the world-Z translation axis, so normal picking must win only
    // near its distinct dashed tip rather than steal the whole axis.
    if let SceneSelection::Plane(id) = selection {
        let plane = world.planes().get(&id)?;
        let normal_length = plane_normal_length(plane, length);
        let start = origin + plane.normal.as_vec3() * normal_length * 0.68;
        let end = origin + plane.normal.as_vec3() * normal_length;
        if let (Some(start), Some(end)) = (
            project_to_viewport(camera, viewport, start),
            project_to_viewport(camera, viewport, end),
        ) && point_segment_distance(pointer, start, end) <= 12.0
        {
            return Some(TransformHandle::PlaneNormal);
        }
    }

    // Plane squares overlap the inner part of the axes, so test their filled
    // screen-space quads first. The quads come from the same function that draws
    // them, so a handle is always where it looks.
    for (handle, a, b) in GIZMO_PLANES {
        let Some(screen) = gizmo_plane_corners(origin, a, b, length)
            .into_iter()
            .map(|point| project_to_viewport(camera, viewport, point))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        if point_in_triangle(pointer, screen[0], screen[1], screen[2])
            || point_in_triangle(pointer, screen[0], screen[2], screen[3])
        {
            return Some(handle);
        }
    }

    let mut nearest: Option<(f32, TransformHandle)> = None;
    for (handle, axis) in GIZMO_AXES {
        let (start, end) = gizmo_axis_segment(origin, axis, length);
        let (Some(start), Some(end)) = (
            project_to_viewport(camera, viewport, start),
            project_to_viewport(camera, viewport, end),
        ) else {
            continue;
        };
        let distance = point_segment_distance(pointer, start, end);
        if distance <= 10.0 && nearest.is_none_or(|(best, _)| distance < best) {
            nearest = Some((distance, handle));
        }
    }
    nearest.map(|(_, handle)| handle)
}

/// Convert one pointer-frame delta into an exactly constrained world-space
/// translation. Axis movement follows the axis' screen projection; plane
/// movement intersects previous/current rays with the selected world plane.
pub fn constrained_translation(
    handle: TransformHandle,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    pointer_delta: Vec2,
    object_origin: Vec3,
    gizmo_length: f32,
) -> Option<Vec3> {
    if let Some(axis) = handle.axis() {
        let screen_origin = project_to_viewport(camera, viewport, object_origin)?;
        let screen_tip =
            project_to_viewport(camera, viewport, object_origin + axis * gizmo_length)?;
        let screen_axis = screen_tip - screen_origin;
        let pixels = screen_axis.length();
        if pixels < 2.0 {
            return None;
        }
        let world_distance = pointer_delta.dot(screen_axis / pixels) * gizmo_length / pixels;
        return Some(axis * world_distance);
    }

    let normal = handle.plane_normal()?;
    let previous = pointer - pointer_delta;
    let previous_ray = camera.ray_from_viewport(previous, viewport)?;
    let current_ray = camera.ray_from_viewport(pointer, viewport)?;
    let previous_hit = ray_plane_point(previous_ray, object_origin, normal)?;
    let current_hit = ray_plane_point(current_ray, object_origin, normal)?;
    Some(current_hit - previous_hit)
}

pub fn selection_gizmo_length(world: &WorldSnapshot, selection: SceneSelection) -> Option<f32> {
    transform_gizmo(world, selection).map(|(_, length)| length)
}

fn plane_normal_length(plane: &SlicePlane, translation_gizmo_length: f32) -> f32 {
    // The proportional term keeps N useful on large planes; the second term
    // ensures its labelled tip remains beyond a coincident translation axis.
    (plane.half_extent.max_element() as f32 * 0.6)
        .max(translation_gizmo_length * 1.35)
        .max(0.65)
}

pub fn plane_normal_tip(
    world: &WorldSnapshot,
    selection: SceneSelection,
    preview: Option<TransformPreview>,
) -> Option<(Vec3, Vec3)> {
    let SceneSelection::Plane(id) = selection else {
        return None;
    };
    let plane = world.planes().get(&id).filter(|plane| plane.visible)?;
    let (world_origin, gizmo_length) = transform_gizmo(world, selection)?;
    let origin = preview.map_or(world_origin, |preview| preview.origin);
    let normal = preview
        .and_then(|preview| preview.plane_normal)
        .unwrap_or(plane.normal.as_vec3());
    let tip = origin + normal * plane_normal_length(plane, gizmo_length);
    Some((origin, tip))
}

pub fn plane_normal_label_position(
    world: &WorldSnapshot,
    selection: SceneSelection,
    camera: &OrbitCamera,
    viewport: Viewport,
    preview: Option<TransformPreview>,
) -> Option<Vec2> {
    let (_, tip) = plane_normal_tip(world, selection, preview)?;
    let position = project_to_viewport(camera, viewport, tip)?;
    viewport.contains(position).then_some(position)
}

fn append_plane_normal(
    geometry: &mut FieldGeometry,
    plane: &SlicePlane,
    origin: Vec3,
    direction: Vec3,
    translation_gizmo_length: f32,
    active: bool,
) {
    let length = plane_normal_length(plane, translation_gizmo_length);
    let color = if active {
        Vec4::new(1.0, 0.9, 0.18, 1.0)
    } else {
        Vec4::new(0.78, 0.48, 1.0, 1.0)
    };
    let tip = origin + direction * length;

    // Dashed purple distinguishes N from solid RGB translation axes and from
    // value glyphs, even when a plane normal happens to equal world X/Y/Z.
    for segment in (0..12).step_by(2) {
        let from = origin.lerp(tip, segment as f32 / 12.0);
        let to = origin.lerp(tip, (segment + 1) as f32 / 12.0);
        push_line(&mut geometry.vector_lines, from, to, color);
    }
    let reference = if direction.z.abs() < 0.9 {
        Vec3::Z
    } else {
        Vec3::X
    };
    let side = direction.cross(reference).normalize_or_zero();
    let head_base = tip - direction * length * 0.2;
    let head_width = length * 0.11;
    push_line(
        &mut geometry.vector_lines,
        tip,
        head_base + side * head_width,
        color,
    );
    push_line(
        &mut geometry.vector_lines,
        tip,
        head_base - side * head_width,
        color,
    );
}

/// Arcball direction for dragging the tip of a plane's normal arrow.
///
/// When the pointer ray crosses the virtual sphere, choose the intersection
/// closest to the current normal so a back-facing normal does not jump to the
/// visible hemisphere. Outside the sphere, use the closest point on the ray,
/// which keeps rotation continuous beyond the silhouette.
pub fn dragged_plane_normal(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    origin: Vec3,
    radius: f32,
    current_normal: Vec3,
) -> Option<Vec3> {
    let ray = camera.ray_from_viewport(pointer, viewport)?;
    let offset = ray.origin - origin;
    let b = offset.dot(ray.direction);
    let c = offset.length_squared() - radius * radius;
    let discriminant = b * b - c;
    if discriminant >= 0.0 {
        let root = discriminant.sqrt();
        let candidates = [-b - root, -b + root]
            .into_iter()
            .filter(|distance| *distance >= 0.0)
            .map(|distance| (ray.origin + ray.direction * distance - origin).normalize_or_zero());
        return candidates.max_by(|a, b| a.dot(current_normal).total_cmp(&b.dot(current_normal)));
    }

    let distance = (origin - ray.origin).dot(ray.direction).max(0.0);
    let closest = ray.origin + ray.direction * distance - origin;
    (closest.length_squared() > f32::EPSILON).then(|| closest.normalize())
}

/// Move parallel to the camera's view plane while retaining the object's depth.
/// This is the free-drag fallback used only after the pointer has hit the
/// selected object's proxy.
pub fn view_plane_translation(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    pointer_delta: Vec2,
    object_origin: Vec3,
) -> Option<Vec3> {
    let normal = (camera.target() - camera.eye()).normalize_or_zero();
    if normal.length_squared() <= f32::EPSILON {
        return None;
    }
    let previous_ray = camera.ray_from_viewport(pointer - pointer_delta, viewport)?;
    let current_ray = camera.ray_from_viewport(pointer, viewport)?;
    let previous_hit = ray_plane_point(previous_ray, object_origin, normal)?;
    let current_hit = ray_plane_point(current_ray, object_origin, normal)?;
    Some(current_hit - previous_hit)
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectShape, ObjectSpec, World, WorldCommand};

    use super::*;

    #[test]
    fn axis_gizmo_pick_and_drag_remain_on_the_selected_axis() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source").with_shape(ObjectShape::sphere(0.25).unwrap()),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let object = snapshot.objects().values().next().unwrap();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let camera = OrbitCamera::default();
        let origin = object.transform.translation.as_vec3();
        let selection = SceneSelection::Object(object.id);
        let length = object_gizmo_length(object);
        let start =
            project_to_viewport(&camera, viewport, origin + Vec3::X * length * 0.6).unwrap();
        let end = project_to_viewport(&camera, viewport, origin + Vec3::X * length * 0.9).unwrap();
        let pointer = start.lerp(end, 0.5);

        assert_eq!(
            pick_transform_handle(&snapshot, selection, &camera, viewport, pointer),
            Some(TransformHandle::AxisX)
        );
        let screen_axis = (end - start).normalize();
        let movement = constrained_translation(
            TransformHandle::AxisX,
            &camera,
            viewport,
            pointer + screen_axis * 12.0,
            screen_axis * 12.0,
            origin,
            length,
        )
        .unwrap();
        assert!(movement.x.abs() > 0.0);
        assert_eq!(movement.y, 0.0);
        assert_eq!(movement.z, 0.0);
    }

    #[test]
    fn plane_gizmo_drag_has_no_component_normal_to_the_plane() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source").with_shape(ObjectShape::sphere(0.25).unwrap()),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let object = snapshot.objects().values().next().unwrap();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let movement = constrained_translation(
            TransformHandle::PlaneXY,
            &OrbitCamera::default(),
            viewport,
            Vec2::new(430.0, 320.0),
            Vec2::new(12.0, 8.0),
            object.transform.translation.as_vec3(),
            object_gizmo_length(object),
        )
        .unwrap();

        assert!(movement.length_squared() > 0.0);
        assert!(movement.z.abs() < 1.0e-5);
    }

    #[test]
    fn free_drag_moves_in_the_camera_plane() {
        let camera = OrbitCamera::default();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let movement = view_plane_translation(
            &camera,
            viewport,
            Vec2::new(430.0, 320.0),
            Vec2::new(12.0, 8.0),
            Vec3::new(0.0, 0.0, 0.6),
        )
        .unwrap();
        let view_normal = (camera.target() - camera.eye()).normalize();

        assert!(movement.length_squared() > 0.0);
        assert!(movement.dot(view_normal).abs() < 1.0e-5);
    }

    #[test]
    fn a_drawn_plane_handle_is_picked_where_it_is_drawn() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source").with_shape(ObjectShape::sphere(0.25).unwrap()),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let object = snapshot.objects().values().next().unwrap();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let camera = OrbitCamera::default();
        let origin = object.transform.translation.as_vec3();
        let corners = gizmo_plane_corners(origin, Vec3::X, Vec3::Y, object_gizmo_length(object));
        let centroid = corners.iter().fold(Vec3::ZERO, |sum, c| sum + *c) / 4.0;
        let pointer = project_to_viewport(&camera, viewport, centroid).unwrap();

        assert_eq!(
            pick_transform_handle(
                &snapshot,
                SceneSelection::Object(object.id),
                &camera,
                viewport,
                pointer,
            ),
            Some(TransformHandle::PlaneXY)
        );
    }

    #[test]
    fn probes_and_planes_share_translation_handles_and_origin_markers() {
        use fieldcad_core::{ProbeSpec, SlicePlaneSpec};
        use glam::DVec3;

        let mut world = World::new();
        let report = world
            .commit([
                WorldCommand::CreateProbe(ProbeSpec::at("probe", DVec3::ZERO, Vec::new())),
                WorldCommand::CreatePlane(
                    SlicePlaneSpec::new("plane", DVec3::ZERO, DVec3::Z).unwrap(),
                ),
            ])
            .unwrap();
        let snapshot = world.snapshot();

        for selection in [
            SceneSelection::Probe(report.created_probes[0]),
            SceneSelection::Plane(report.created_planes[0]),
        ] {
            let mut geometry = FieldGeometry::default();
            append_transform_gizmo(&mut geometry, &snapshot, Some(selection), None, None);
            assert!(geometry.vector_lines.len() > 6 * 32);
            assert_eq!(selection_origin(&snapshot, selection), Some(Vec3::ZERO));
        }
    }

    #[test]
    fn plane_normal_is_proportional_pickable_and_arcball_draggable() {
        use fieldcad_core::SlicePlaneSpec;
        use glam::{DVec2, DVec3};

        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("plane", DVec3::ZERO, DVec3::Z)
                    .unwrap()
                    .with_half_extent(DVec2::splat(4.0))
                    .unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Plane(report.created_planes[0]);
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let camera = OrbitCamera::default();
        let (origin, tip) = plane_normal_tip(&snapshot, selection, None).unwrap();
        assert!(tip.distance(origin) >= 2.4);

        let pointer = project_to_viewport(&camera, viewport, tip * 0.9).unwrap();
        assert_eq!(
            pick_transform_handle(&snapshot, selection, &camera, viewport, pointer),
            Some(TransformHandle::PlaneNormal)
        );

        let dragged = dragged_plane_normal(
            &camera,
            viewport,
            pointer + Vec2::new(30.0, 15.0),
            origin,
            tip.distance(origin),
            Vec3::Z,
        )
        .unwrap();
        assert!(dragged.is_normalized());
        assert!(dragged != Vec3::Z);
    }
}
