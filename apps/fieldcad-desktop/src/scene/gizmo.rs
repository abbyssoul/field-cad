//! The translation gizmo: how it is drawn, hit-tested, and dragged.
//!
//! Drawing and picking derive their geometry from the same functions here. When
//! each computed its own, a change to the drawn gizmo silently moved the
//! handles away from where they appeared to be.
//!
//! Every size in the gizmo — arrow length, ring radius, the origin dot — is a
//! fixed multiple of one pixel-based length, converted to world units via
//! [`OrbitCamera::world_units_per_pixel`] at the gizmo's own origin. That is
//! what keeps the whole gizmo the same number of screen pixels regardless of
//! camera distance or the scale a scene is authored at, from the metre-scale
//! default scene down to a nanometre-scale one — unlike sizing it from the
//! selected entity's own extent, which is what this used to do and which
//! shrinks to invisible or grows to absurd at either end of that range.

use fieldcad_core::{SceneScale, WorldSnapshot};
use glam::{Quat, Vec2, Vec3, Vec4};

use super::pick::{
    point_in_triangle, point_segment_distance, project_to_viewport, ray_plane_point,
};
use super::{
    FieldGeometry, SceneSelection, append_cone, append_cylinder, append_ring_band, push_line,
    push_quad, push_quad_outline, quat_from_dquat, ring_basis,
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
    /// Rotates a selected field box about its own local X/Y/Z axis; never
    /// translates it.
    RotateX,
    RotateY,
    RotateZ,
    /// Rotates a selected field box about the camera's current view axis;
    /// never translates it. Unlike `RotateX/Y/Z` this axis has no fixed
    /// relationship to the box's own orientation — it is recomputed from the
    /// camera every frame.
    RotateView,
    /// Free trackball rotation of a selected field box: dragging inside the
    /// rotation gizmo's sphere but not on any specific ring. Never translates.
    RotateFree,
}

/// Immediate authoring-helper state while the source is acknowledging a drag.
/// Solver snapshots and world geometry remain authoritative; only the gizmo
/// tracks the pointer optimistically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformPreview {
    pub origin: Vec3,
    pub plane_normal: Option<Vec3>,
    /// A field box's live-dragged orientation, distinct from `plane_normal`
    /// because a box has three independent rotation handles rather than one.
    pub rotation: Option<Quat>,
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
            Self::RotateX => "rotate about local X",
            Self::RotateY => "rotate about local Y",
            Self::RotateZ => "rotate about local Z",
            Self::RotateView => "rotate about the view axis",
            Self::RotateFree => "free rotation",
        }
    }

    pub const fn axis(self) -> Option<Vec3> {
        match self {
            Self::AxisX => Some(Vec3::X),
            Self::AxisY => Some(Vec3::Y),
            Self::AxisZ => Some(Vec3::Z),
            Self::PlaneXY
            | Self::PlaneYZ
            | Self::PlaneZX
            | Self::PlaneNormal
            | Self::RotateX
            | Self::RotateY
            | Self::RotateZ
            | Self::RotateView
            | Self::RotateFree => None,
        }
    }

    pub const fn plane_normal(self) -> Option<Vec3> {
        match self {
            Self::PlaneXY => Some(Vec3::Z),
            Self::PlaneYZ => Some(Vec3::X),
            Self::PlaneZX => Some(Vec3::Y),
            Self::AxisX
            | Self::AxisY
            | Self::AxisZ
            | Self::PlaneNormal
            | Self::RotateX
            | Self::RotateY
            | Self::RotateZ
            | Self::RotateView
            | Self::RotateFree => None,
        }
    }

    /// The box-local axis this handle rotates about, for the three local
    /// rotation rings. `None` for every translation handle and for the view
    /// and free-rotation handles, which have no fixed local axis.
    pub const fn rotation_axis(self) -> Option<Vec3> {
        match self {
            Self::RotateX => Some(Vec3::X),
            Self::RotateY => Some(Vec3::Y),
            Self::RotateZ => Some(Vec3::Z),
            Self::AxisX
            | Self::AxisY
            | Self::AxisZ
            | Self::PlaneXY
            | Self::PlaneYZ
            | Self::PlaneZX
            | Self::PlaneNormal
            | Self::RotateView
            | Self::RotateFree => None,
        }
    }
}

/// Screen-space gizmo sizing. Every other size in this file is a fixed
/// multiple of this one, converted to world units via
/// [`OrbitCamera::world_units_per_pixel`] at the gizmo's origin.
const AXIS_LENGTH_PX: f32 = 120.0;

/// Presentation settings for the scale-independent transform gizmo, expressed
/// in logical screen pixels — the same points egui lays the panels out in,
/// independent of display scaling. They affect neither authored geometry nor
/// the transform produced by a drag. The public functions in this module take
/// the display scale alongside them and convert with
/// [`GizmoDisplay::to_physical`], because the gizmo's math — like the
/// [`Viewport`] — is physical.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GizmoDisplay {
    pub axis_length_px: f32,
    pub axis_thickness_px: f32,
    pub rotation_diameter_px: f32,
    pub rotation_thickness_px: f32,
}

impl Default for GizmoDisplay {
    fn default() -> Self {
        Self {
            axis_length_px: AXIS_LENGTH_PX,
            axis_thickness_px: 4.0,
            rotation_diameter_px: 220.0,
            rotation_thickness_px: 8.0,
        }
    }
}

impl GizmoDisplay {
    /// These settings are logical points, but the gizmo's math is physical
    /// pixels — like the [`Viewport`], and like the pointer position the
    /// picking path hit-tests, both of which arrive already converted. So the
    /// display converts once, here, at each public entry point; conversion is
    /// the functions' job rather than the caller's so that an
    /// already-physical value can never be handed in and doubled by mistake.
    pub fn to_physical(self, pixels_per_point: f32) -> Self {
        let scale = pixels_per_point.max(0.01);
        Self {
            axis_length_px: self.axis_length_px * scale,
            axis_thickness_px: self.axis_thickness_px * scale,
            rotation_diameter_px: self.rotation_diameter_px * scale,
            rotation_thickness_px: self.rotation_thickness_px * scale,
        }
    }

    fn axis_length_px(self) -> f32 {
        self.axis_length_px.max(12.0)
    }
    fn axis_thickness_px(self) -> f32 {
        self.axis_thickness_px.max(0.5)
    }
    fn rotation_radius_px(self) -> f32 {
        (self.rotation_diameter_px * 0.5).max(12.0)
    }
    fn rotation_thickness_px(self) -> f32 {
        self.rotation_thickness_px.max(0.5)
    }
}

/// A fixed world-space light direction for baked flat shading (see the module
/// doc on [`super::push_shaded_triangle`]). Not camera-relative, so a solid
/// shape's shading does not swim as the camera orbits — the same way
/// Blender's own gizmo shading behaves. Left un-normalized: at this magnitude
/// (~1.005) the error is negligible for a baked visual cue.
const GIZMO_LIGHT_DIR: Vec3 = Vec3::new(0.4, -0.6, 0.7);

#[allow(clippy::too_many_arguments)]
pub fn append_transform_gizmo_with_display(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: Option<SceneSelection>,
    active: Option<TransformHandle>,
    preview: Option<TransformPreview>,
    display: GizmoDisplay,
    pixels_per_point: f32,
    scene_scale: SceneScale,
) {
    let display = display.to_physical(pixels_per_point);
    let Some(selection) = selection else {
        return;
    };
    let Some((world_origin, length)) =
        transform_gizmo_with_display(world, camera, viewport, selection, display, scene_scale)
    else {
        return;
    };
    let origin = preview.map_or(world_origin, |preview| preview.origin);
    let scale = camera.world_units_per_pixel(origin, viewport.height as f32);
    let axis_thickness = display.axis_thickness_px() * scale;
    let ring_radius = display.rotation_radius_px() * scale;
    let ring_outer = ring_radius * 1.55 / 1.3;
    let ring_thickness = display.rotation_thickness_px() * scale;

    append_origin_marker(geometry, origin, selection_marker_radius(length));

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
        append_solid_arrow(
            geometry,
            origin,
            direction,
            length,
            axis_thickness,
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
            origin,
            preview
                .and_then(|preview| preview.plane_normal)
                .unwrap_or(plane.normal.as_vec3()),
            length,
            active == Some(TransformHandle::PlaneNormal),
        );
    }

    if let SceneSelection::Box(id) = selection
        && let Some(field_box) = world.boxes().get(&id)
    {
        let rotation = preview
            .and_then(|preview| preview.rotation)
            .unwrap_or_else(|| quat_from_dquat(field_box.rotation));
        append_rotation_rings(
            geometry,
            camera,
            origin,
            rotation,
            ring_radius,
            ring_outer,
            ring_thickness,
            active,
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

/// Where a selection's gizmo is anchored. Sizing is handled separately by
/// [`transform_gizmo`]; visibility is checked here — an invisible entity gets
/// no gizmo and no anchor point at all.
fn selection_origin_point(
    world: &WorldSnapshot,
    selection: SceneSelection,
    scene_scale: SceneScale,
) -> Option<Vec3> {
    let origin = match selection {
        SceneSelection::Object(id) => {
            let object = world.object(id).filter(|object| object.visible)?;
            object.transform.translation
        }
        SceneSelection::Plane(id) => {
            let plane = world.planes().get(&id).filter(|plane| plane.visible)?;
            plane.origin
        }
        SceneSelection::Probe(id) => {
            let probe = world.probe(id).filter(|probe| probe.visible)?;
            world.resolve_probe_position(probe).ok()?
        }
        SceneSelection::Box(id) => {
            let field_box = world.boxes().get(&id).filter(|region| region.visible)?;
            field_box.origin
        }
        SceneSelection::Sphere(id) => {
            let sphere = world.spheres().get(&id).filter(|sphere| sphere.visible)?;
            sphere.origin
        }
    };
    Some(scene_scale.to_render_vec3(origin))
}

/// The gizmo's world-space origin and translation-arrow length. `length` is a
/// fixed screen-pixel size (`AXIS_LENGTH_PX`) converted to world units at
/// `origin`'s own depth, which is what makes the gizmo the same size on screen
/// regardless of camera distance or the scale the scene is authored at. Every
/// other size in this file (`rotation_ring_radius`, `view_ring_radius`,
/// `plane_normal_length`, `selection_marker_radius`) is a fixed multiple of
/// this one `length`.
///
/// Only this module's own tests call the bare (default-display) form now;
/// production code always threads a real [`GizmoDisplay`] through
/// [`transform_gizmo_with_display`] instead — the same fix applied to
/// `plane_normal_tip` for UI-1.
#[cfg(test)]
fn transform_gizmo(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: SceneSelection,
) -> Option<(Vec3, f32)> {
    transform_gizmo_with_display(
        world,
        camera,
        viewport,
        selection,
        GizmoDisplay::default(),
        SceneScale::metre(),
    )
}

/// `display` must already be physical — every public entry point converts the
/// caller's logical [`GizmoDisplay`] with [`GizmoDisplay::to_physical`]
/// before calling here, so the scale arithmetic reads physical pixels
/// throughout this module.
fn transform_gizmo_with_display(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: SceneSelection,
    display: GizmoDisplay,
    scene_scale: SceneScale,
) -> Option<(Vec3, f32)> {
    let origin = selection_origin_point(world, selection, scene_scale)?;
    let scale = camera.world_units_per_pixel(origin, viewport.height as f32);
    Some((origin, display.axis_length_px() * scale))
}

fn selection_marker_radius(length: f32) -> f32 {
    (length * 0.12).max(1.0e-6)
}

pub fn selection_origin(
    world: &WorldSnapshot,
    selection: SceneSelection,
    scene_scale: SceneScale,
) -> Option<Vec3> {
    selection_origin_point(world, selection, scene_scale)
}

fn append_origin_marker(geometry: &mut FieldGeometry, origin: Vec3, radius: f32) {
    for (a, b, color) in [
        (Vec3::Y, Vec3::Z, Vec4::new(1.0, 0.32, 0.34, 1.0)),
        (Vec3::Z, Vec3::X, Vec4::new(0.32, 1.0, 0.45, 1.0)),
        (Vec3::X, Vec3::Y, Vec4::new(0.38, 0.62, 1.0, 1.0)),
    ] {
        super::push_circle(&mut geometry.vector_lines, origin, a, b, radius, color);
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

/// The three local rotation rings, keyed by the box-local axis each rotates
/// about.
const ROTATION_RINGS: [(TransformHandle, Vec3); 3] = [
    (TransformHandle::RotateX, Vec3::X),
    (TransformHandle::RotateY, Vec3::Y),
    (TransformHandle::RotateZ, Vec3::Z),
];

fn append_gizmo_plane(geometry: &mut FieldGeometry, corners: [Vec3; 4], color: Vec4) {
    push_quad(&mut geometry.surface_triangles, corners, color);
    push_quad_outline(&mut geometry.vector_lines, corners, color.with_w(0.95));
}

/// A translation-axis arrow drawn as a solid cylinder shaft and cone head,
/// Blender/Unreal-style, rather than a thin line — reusing the mesh
/// primitives from `super` and baking their flat shading from
/// [`GIZMO_LIGHT_DIR`].
///
/// Field-vector glyphs keep drawing as thin lines via `super::append_arrow`;
/// this is deliberately gizmo-only, since a dense field can draw thousands of
/// glyphs a frame and a solid mesh per glyph would be a different performance
/// question entirely.
fn append_solid_arrow(
    geometry: &mut FieldGeometry,
    origin: Vec3,
    direction: Vec3,
    length: f32,
    shaft_radius: f32,
    color: Vec4,
) {
    if !direction.is_finite() || direction.length_squared() <= f32::EPSILON || length <= 0.0 {
        return;
    }
    let direction = direction.normalize();
    const HEAD_FRACTION: f32 = 0.32;
    let shaft_end = origin + direction * (length * (1.0 - HEAD_FRACTION));
    let tip = origin + direction * length;

    append_cylinder(
        &mut geometry.surface_triangles,
        origin,
        shaft_end,
        shaft_radius,
        color,
        GIZMO_LIGHT_DIR,
    );
    append_cone(
        &mut geometry.surface_triangles,
        shaft_end,
        tip,
        shaft_radius * (0.09 / 0.035),
        color,
        GIZMO_LIGHT_DIR,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn pick_transform_handle_with_display(
    world: &WorldSnapshot,
    selection: SceneSelection,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    display: GizmoDisplay,
    pixels_per_point: f32,
    scene_scale: SceneScale,
) -> Option<TransformHandle> {
    let display = display.to_physical(pixels_per_point);
    let (origin, length) =
        transform_gizmo_with_display(world, camera, viewport, selection, display, scene_scale)?;
    let is_box = matches!(selection, SceneSelection::Box(_));
    let scale = camera.world_units_per_pixel(origin, viewport.height as f32);

    // The outer part of N is reserved for rotation. For the default XY plane
    // it overlaps the world-Z translation axis, so normal picking must win only
    // near its distinct dashed tip rather than steal the whole axis.
    if let SceneSelection::Plane(id) = selection {
        let plane = world.planes().get(&id)?;
        let normal_length = plane_normal_length(length);
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
    //
    // This also has to come before the rotation rings below: "inside this
    // exact quad" is an unambiguous geometric test, while a ring match is a
    // fuzzy "within a few pixels of this line" one — and a ring viewed
    // edge-on (or nearly so) projects to a line that can sweep right through
    // a plane handle's screen position for a wide range of camera angles, not
    // just a rare coincidence. An exact hit must always win over an
    // approximate one, or the plane handle becomes unreliable across most of
    // the camera's orbit.
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

    // The rotation rings sit outside the translation axes (see
    // `rotation_ring_radius`), so they are still checked before the axes
    // below for the same reason `PlaneNormal` is: an outer handle should win
    // over a nearer, but far less deliberately targeted, pixel-distance match
    // on an inner axis. Unlike the plane quads above, an axis's own pick is
    // also a fuzzy line-proximity test, so there is no exactness mismatch to
    // resolve here the way there was against the quads.
    if is_box
        && let SceneSelection::Box(id) = selection
        && let Some(field_box) = world.boxes().get(&id)
    {
        let rotation = quat_from_dquat(field_box.rotation);
        let radius = display.rotation_radius_px() * scale;
        let mut rings: Vec<(TransformHandle, Vec3, f32)> = ROTATION_RINGS
            .into_iter()
            .map(|(handle, local_axis)| {
                (handle, (rotation * local_axis).normalize_or_zero(), radius)
            })
            .collect();
        let view_axis = (camera.target() - camera.eye()).normalize_or_zero();
        if view_axis.length_squared() > f32::EPSILON {
            rings.push((TransformHandle::RotateView, view_axis, radius * 1.55 / 1.3));
        }
        let mut nearest: Option<(f32, TransformHandle)> = None;
        for (handle, axis, ring_radius) in rings {
            let Some(distance) = pick_ring(camera, viewport, pointer, origin, axis, ring_radius)
            else {
                continue;
            };
            if distance <= display.rotation_thickness_px() * 0.5 + 4.0
                && nearest.is_none_or(|(best, _)| distance < best)
            {
                nearest = Some((distance, handle));
            }
        }
        if let Some((_, handle)) = nearest {
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
    if let Some((_, handle)) = nearest {
        return Some(handle);
    }

    // A free trackball drag is the box gizmo's catch-all. Specific translation
    // handles above still win where the configured rings overlap them.
    if is_box {
        let ray = camera.ray_from_viewport(pointer, viewport)?;
        let radius = display.rotation_radius_px() * scale;
        if ray.hit_sphere(origin, radius).is_some() {
            return Some(TransformHandle::RotateFree);
        }
    }
    None
}

/// The screen-space distance from `pointer` to the nearest point on a ring of
/// `radius` centred at `origin`, perpendicular to `axis`. `None` if the ring
/// cannot be projected (camera edge-on to a segment, or behind the camera).
fn pick_ring(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    origin: Vec3,
    axis: Vec3,
    radius: f32,
) -> Option<f32> {
    const SEGMENTS: u32 = 32;
    let (a, b) = ring_basis(axis);
    let point = |segment: u32| {
        let angle = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        project_to_viewport(
            camera,
            viewport,
            origin + (a * angle.cos() + b * angle.sin()) * radius,
        )
    };
    let mut nearest: Option<f32> = None;
    let mut previous = point(0)?;
    for segment in 1..=SEGMENTS {
        let current = point(segment)?;
        let distance = point_segment_distance(pointer, previous, current);
        nearest = Some(nearest.map_or(distance, |best: f32| best.min(distance)));
        previous = current;
    }
    nearest
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

#[allow(clippy::too_many_arguments)]
pub fn selection_gizmo_length_with_display(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: SceneSelection,
    display: GizmoDisplay,
    pixels_per_point: f32,
    scene_scale: SceneScale,
) -> Option<f32> {
    transform_gizmo_with_display(
        world,
        camera,
        viewport,
        selection,
        display.to_physical(pixels_per_point),
        scene_scale,
    )
    .map(|(_, length)| length)
}

/// The rotation gizmo's sphere radius for a box selection — mirrors
/// [`selection_gizmo_length_with_display`], but for the radius the trackball
/// drag needs rather than the translation length.
#[allow(clippy::too_many_arguments)]
pub fn rotation_gizmo_radius_with_display(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: SceneSelection,
    display: GizmoDisplay,
    pixels_per_point: f32,
    scene_scale: SceneScale,
) -> Option<f32> {
    let display = display.to_physical(pixels_per_point);
    transform_gizmo_with_display(world, camera, viewport, selection, display, scene_scale).map(
        |(origin, _)| {
            display.rotation_radius_px()
                * camera.world_units_per_pixel(origin, viewport.height as f32)
        },
    )
}

fn plane_normal_length(translation_gizmo_length: f32) -> f32 {
    // The proportional term keeps N clear of a coincident translation axis.
    translation_gizmo_length * 1.3
}

/// The plane-normal handle's world-space origin and tip, sized from
/// `display` — the caller's configured [`GizmoDisplay`], not a default one.
/// Both this handle's drawn length and its drag-arcball radius derive from
/// the tip this returns, so the two must agree with what
/// [`append_transform_gizmo_with_display`] actually draws for the same
/// `display`.
#[allow(clippy::too_many_arguments)]
pub fn plane_normal_tip(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: SceneSelection,
    preview: Option<TransformPreview>,
    display: GizmoDisplay,
    pixels_per_point: f32,
    scene_scale: SceneScale,
) -> Option<(Vec3, Vec3)> {
    let SceneSelection::Plane(id) = selection else {
        return None;
    };
    let plane = world.planes().get(&id).filter(|plane| plane.visible)?;
    let (world_origin, gizmo_length) = transform_gizmo_with_display(
        world,
        camera,
        viewport,
        selection,
        display.to_physical(pixels_per_point),
        scene_scale,
    )?;
    let origin = preview.map_or(world_origin, |preview| preview.origin);
    let normal = preview
        .and_then(|preview| preview.plane_normal)
        .unwrap_or(plane.normal.as_vec3());
    let tip = origin + normal * plane_normal_length(gizmo_length);
    Some((origin, tip))
}

#[allow(clippy::too_many_arguments)]
pub fn plane_normal_label_position(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    selection: SceneSelection,
    preview: Option<TransformPreview>,
    display: GizmoDisplay,
    pixels_per_point: f32,
    scene_scale: SceneScale,
) -> Option<Vec2> {
    let (_, tip) = plane_normal_tip(
        world,
        camera,
        viewport,
        selection,
        preview,
        display,
        pixels_per_point,
        scene_scale,
    )?;
    let position = project_to_viewport(camera, viewport, tip)?;
    viewport.contains(position).then_some(position)
}

fn append_plane_normal(
    geometry: &mut FieldGeometry,
    origin: Vec3,
    direction: Vec3,
    translation_gizmo_length: f32,
    active: bool,
) {
    let length = plane_normal_length(translation_gizmo_length);
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

#[cfg(test)]
fn rotation_ring_radius(length: f32) -> f32 {
    length * (110.0 / AXIS_LENGTH_PX)
}

/// Radius of the fourth, screen-space rotation ring: larger than the three
/// local rings, per the Blender/Unreal convention of an outer ring for
/// view-axis rotation.
#[cfg(test)]
fn view_ring_radius(length: f32) -> f32 {
    rotation_ring_radius(length) * 1.55 / 1.3
}

#[allow(clippy::too_many_arguments)]
fn append_rotation_rings(
    geometry: &mut FieldGeometry,
    camera: &OrbitCamera,
    origin: Vec3,
    rotation: Quat,
    radius: f32,
    view_radius: f32,
    thickness: f32,
    active: Option<TransformHandle>,
) {
    const RING_COLORS: [Vec4; 3] = [
        Vec4::new(0.95, 0.15, 0.18, 1.0),
        Vec4::new(0.18, 0.9, 0.3, 1.0),
        Vec4::new(0.2, 0.45, 1.0, 1.0),
    ];
    for ((handle, local_axis), color) in ROTATION_RINGS.into_iter().zip(RING_COLORS) {
        let world_axis = (rotation * local_axis).normalize_or_zero();
        append_ring_band(
            &mut geometry.surface_triangles,
            origin,
            world_axis,
            radius,
            thickness,
            handle_color(color, handle, active, 1.0),
            GIZMO_LIGHT_DIR,
        );
    }

    let view_axis = (camera.target() - camera.eye()).normalize_or_zero();
    if view_axis.length_squared() > f32::EPSILON {
        const VIEW_RING_COLOR: Vec4 = Vec4::new(0.92, 0.92, 0.95, 1.0);
        append_ring_band(
            &mut geometry.surface_triangles,
            origin,
            view_axis,
            view_radius,
            thickness,
            handle_color(VIEW_RING_COLOR, TransformHandle::RotateView, active, 1.0),
            GIZMO_LIGHT_DIR,
        );
    }
}

/// Where the pointer's ray crosses a sphere of `radius` centred at `origin`,
/// as a unit direction from `origin` — or, for a pointer outside the sphere's
/// silhouette, the closest point on the ray to `origin`, normalized, which is
/// what keeps a drag continuous past the visible edge instead of sticking.
///
/// `reference` chooses which of two intersections to prefer when the ray
/// crosses the sphere twice, by taking whichever is closer to it. Every
/// caller wants continuity with *something* already in hand — the current
/// normal, or the previous sample in the same drag — rather than an arbitrary
/// near/far rule, so this is a parameter rather than a fixed policy.
fn sphere_drag_point(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    origin: Vec3,
    radius: f32,
    reference: Vec3,
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
        return candidates.max_by(|a, b| a.dot(reference).total_cmp(&b.dot(reference)));
    }

    let distance = (origin - ray.origin).dot(ray.direction).max(0.0);
    let closest = ray.origin + ray.direction * distance - origin;
    (closest.length_squared() > f32::EPSILON).then(|| closest.normalize())
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
    sphere_drag_point(camera, viewport, pointer, origin, radius, current_normal)
}

/// The new absolute rotation after rotating about a known world-space axis by
/// one pointer-frame delta — the core every rotation-ring drag shares. Only
/// *which* axis differs between a box-local ring
/// ([`dragged_box_rotation`]) and the view-axis ring
/// ([`dragged_view_rotation`]).
///
/// The angle is measured between where the previous and current pointer rays
/// cross the rotation plane, so the ring tracks the pointer directly rather
/// than integrating a velocity.
fn rotate_about_world_axis(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    pointer_delta: Vec2,
    origin: Vec3,
    world_axis: Vec3,
    current_rotation: Quat,
) -> Option<Quat> {
    if world_axis.length_squared() <= f32::EPSILON {
        return None;
    }
    let previous = pointer - pointer_delta;
    let previous_ray = camera.ray_from_viewport(previous, viewport)?;
    let current_ray = camera.ray_from_viewport(pointer, viewport)?;
    let previous_radius = ray_plane_point(previous_ray, origin, world_axis)? - origin;
    let current_radius = ray_plane_point(current_ray, origin, world_axis)? - origin;
    if previous_radius.length_squared() <= f32::EPSILON
        || current_radius.length_squared() <= f32::EPSILON
    {
        return None;
    }
    let (a, b) = ring_basis(world_axis);
    let angle_of = |v: Vec3| v.dot(b).atan2(v.dot(a));
    let mut delta_angle = angle_of(current_radius) - angle_of(previous_radius);
    delta_angle = ((delta_angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU))
        - std::f32::consts::PI;
    let incremental = Quat::from_axis_angle(world_axis, delta_angle);
    Some((incremental * current_rotation).normalize())
}

/// Rotate about one of a box's own local axes — the drag behind a local
/// rotation ring.
///
/// Unlike [`dragged_plane_normal`]'s arcball — which can point a normal
/// anywhere on the sphere in one drag — each local ring is constrained to
/// rotate about exactly one axis, `ring_local_axis` in the box's own frame.
/// That constraint is what lets three independent rings compose into a free
/// orientation without fighting each other: dragging the X ring can never
/// smuggle in a Y or Z rotation.
pub fn dragged_box_rotation(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    pointer_delta: Vec2,
    origin: Vec3,
    ring_local_axis: Vec3,
    current_rotation: Quat,
) -> Option<Quat> {
    let world_axis = (current_rotation * ring_local_axis).normalize_or_zero();
    rotate_about_world_axis(
        camera,
        viewport,
        pointer,
        pointer_delta,
        origin,
        world_axis,
        current_rotation,
    )
}

/// Rotate about the camera's current view axis — the drag behind the
/// screen-space ring. Recomputed from the live camera every call, since
/// unlike a local ring this axis has no fixed relationship to the box's own
/// orientation.
pub fn dragged_view_rotation(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    pointer_delta: Vec2,
    origin: Vec3,
    current_rotation: Quat,
) -> Option<Quat> {
    let world_axis = (camera.target() - camera.eye()).normalize_or_zero();
    rotate_about_world_axis(
        camera,
        viewport,
        pointer,
        pointer_delta,
        origin,
        world_axis,
        current_rotation,
    )
}

/// Free trackball rotation: the drag behind the space inside the rotation
/// gizmo's sphere that is not on any specific ring.
///
/// Built from two [`sphere_drag_point`] samples — the classic arcball
/// construction, generalized from `dragged_plane_normal`'s "one point on a
/// sphere" to "the rotation between two such points": the axis is
/// perpendicular to both samples, the angle is between them, applied as one
/// incremental rotation. The current sample prefers continuity with the
/// previous one (rather than an arbitrary near/far rule), since this is one
/// drag sampled twice, not two independent picks.
pub fn dragged_trackball_rotation(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    pointer_delta: Vec2,
    origin: Vec3,
    radius: f32,
    current_rotation: Quat,
) -> Option<Quat> {
    let previous_pointer = pointer - pointer_delta;
    let camera_ward = (camera.eye() - origin).normalize_or_zero();
    let previous = sphere_drag_point(
        camera,
        viewport,
        previous_pointer,
        origin,
        radius,
        camera_ward,
    )?;
    let current = sphere_drag_point(camera, viewport, pointer, origin, radius, previous)?;

    let axis = previous.cross(current);
    if axis.length_squared() <= f32::EPSILON {
        return None;
    }
    let angle = previous.dot(current).clamp(-1.0, 1.0).acos();
    Some((Quat::from_axis_angle(axis.normalize(), angle) * current_rotation).normalize())
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

    const VIEWPORT: Viewport = Viewport {
        x: 0,
        y: 0,
        width: 800,
        height: 600,
    };

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
        let camera = OrbitCamera::default();
        let origin = object.transform.translation.as_vec3();
        let selection = SceneSelection::Object(object.id);
        let (_, length) = transform_gizmo(&snapshot, &camera, VIEWPORT, selection).unwrap();
        let start =
            project_to_viewport(&camera, VIEWPORT, origin + Vec3::X * length * 0.6).unwrap();
        let end = project_to_viewport(&camera, VIEWPORT, origin + Vec3::X * length * 0.9).unwrap();
        let pointer = start.lerp(end, 0.5);

        assert_eq!(
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                VIEWPORT,
                pointer,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
            ),
            Some(TransformHandle::AxisX)
        );
        let screen_axis = (end - start).normalize();
        let movement = constrained_translation(
            TransformHandle::AxisX,
            &camera,
            VIEWPORT,
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
        let camera = OrbitCamera::default();
        let origin = object.transform.translation.as_vec3();
        let (_, length) = transform_gizmo(
            &snapshot,
            &camera,
            VIEWPORT,
            SceneSelection::Object(object.id),
        )
        .unwrap();

        let movement = constrained_translation(
            TransformHandle::PlaneXY,
            &camera,
            VIEWPORT,
            Vec2::new(430.0, 320.0),
            Vec2::new(12.0, 8.0),
            origin,
            length,
        )
        .unwrap();

        assert!(movement.length_squared() > 0.0);
        assert!(movement.z.abs() < 1.0e-5);
    }

    #[test]
    fn free_drag_moves_in_the_camera_plane() {
        let camera = OrbitCamera::default();
        let movement = view_plane_translation(
            &camera,
            VIEWPORT,
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
        let camera = OrbitCamera::default();
        let origin = object.transform.translation.as_vec3();
        let selection = SceneSelection::Object(object.id);
        let (_, length) = transform_gizmo(&snapshot, &camera, VIEWPORT, selection).unwrap();
        let corners = gizmo_plane_corners(origin, Vec3::X, Vec3::Y, length);
        let centroid = corners.iter().fold(Vec3::ZERO, |sum, c| sum + *c) / 4.0;
        let pointer = project_to_viewport(&camera, VIEWPORT, centroid).unwrap();

        assert_eq!(
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                VIEWPORT,
                pointer,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
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
        let camera = OrbitCamera::default();

        for selection in [
            SceneSelection::Probe(report.created_probes[0]),
            SceneSelection::Plane(report.created_planes[0]),
        ] {
            let mut geometry = FieldGeometry::default();
            append_transform_gizmo_with_display(
                &mut geometry,
                &snapshot,
                &camera,
                VIEWPORT,
                Some(selection),
                None,
                None,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
            );
            assert!(!geometry.vector_lines.is_empty());
            assert_eq!(
                selection_origin(&snapshot, selection, SceneScale::metre()),
                Some(Vec3::ZERO)
            );
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
        let camera = OrbitCamera::default();
        let (origin, tip) = plane_normal_tip(
            &snapshot,
            &camera,
            VIEWPORT,
            selection,
            None,
            GizmoDisplay::default(),
            1.0,
            SceneScale::metre(),
        )
        .unwrap();
        assert!(tip.distance(origin) > 0.0);

        let pointer = project_to_viewport(&camera, VIEWPORT, tip * 0.9).unwrap();
        assert_eq!(
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                VIEWPORT,
                pointer,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
            ),
            Some(TransformHandle::PlaneNormal)
        );

        let dragged = dragged_plane_normal(
            &camera,
            VIEWPORT,
            pointer + Vec2::new(30.0, 15.0),
            origin,
            tip.distance(origin),
            Vec3::Z,
        )
        .unwrap();
        assert!(dragged.is_normalized());
        assert!(dragged != Vec3::Z);
    }

    /// UI-1 regression: the plane-normal handle must be sized from the
    /// caller's configured `GizmoDisplay`, not a hardcoded default —
    /// `plane_normal_tip` used to call the bare (default-display)
    /// `transform_gizmo` regardless of what `append_transform_gizmo_with_display`
    /// actually drew, so the N handle rendered at one size while its label
    /// and drag radius were computed at another.
    #[test]
    fn plane_normal_tip_scales_with_the_configured_display() {
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
        let camera = OrbitCamera::default();

        let default_display = GizmoDisplay::default();
        let configured_display = GizmoDisplay {
            axis_length_px: default_display.axis_length_px * 2.5,
            ..default_display
        };

        let (default_origin, default_tip) = plane_normal_tip(
            &snapshot,
            &camera,
            VIEWPORT,
            selection,
            None,
            default_display,
            1.0,
            SceneScale::metre(),
        )
        .unwrap();
        let (configured_origin, configured_tip) = plane_normal_tip(
            &snapshot,
            &camera,
            VIEWPORT,
            selection,
            None,
            configured_display,
            1.0,
            SceneScale::metre(),
        )
        .unwrap();

        assert_eq!(default_origin, configured_origin);
        let default_length = default_tip.distance(default_origin);
        let configured_length = configured_tip.distance(configured_origin);
        assert!(
            (configured_length / default_length - 2.5).abs() < 1.0e-4,
            "the normal handle's tip must scale with axis_length_px like every \
             other gizmo handle does: default {default_length}, configured \
             (2.5x) {configured_length}"
        );
    }

    /// The whole point of the redesign: the gizmo's on-screen size must not
    /// change as the camera dollies, even though the world-space `length`
    /// absolutely does.
    #[test]
    fn the_gizmo_is_the_same_screen_size_at_any_camera_distance() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Object(fieldcad_core::ObjectId::new(0));

        let mut near = OrbitCamera::default();
        near.focus(Vec3::ZERO, 1.0);
        let mut far = OrbitCamera::default();
        far.focus(Vec3::ZERO, 1.0);
        far.dolly(-2_000.0);
        assert!(far.distance() > near.distance() * 1.5);

        let (near_origin, near_length) =
            transform_gizmo(&snapshot, &near, VIEWPORT, selection).unwrap();
        let (far_origin, far_length) =
            transform_gizmo(&snapshot, &far, VIEWPORT, selection).unwrap();
        assert!(
            far_length > near_length * 1.5,
            "world-space length must grow with distance: {near_length} near, {far_length} far"
        );

        let near_tip =
            project_to_viewport(&near, VIEWPORT, near_origin + Vec3::X * near_length).unwrap();
        let near_base = project_to_viewport(&near, VIEWPORT, near_origin).unwrap();
        let far_tip =
            project_to_viewport(&far, VIEWPORT, far_origin + Vec3::X * far_length).unwrap();
        let far_base = project_to_viewport(&far, VIEWPORT, far_origin).unwrap();

        let near_pixels = near_tip.distance(near_base);
        let far_pixels = far_tip.distance(far_base);
        assert!(
            (near_pixels - far_pixels).abs() < 1.0,
            "expected a constant on-screen arrow length: {near_pixels}px near, {far_pixels}px far"
        );
    }

    /// The sibling invariance to the camera-distance one above: the settings
    /// are *logical* pixels, so at 2x display scaling the gizmo must cover
    /// twice as many physical pixels and keep the same world-space size —
    /// the `Viewport` doubles with the scale factor, and both conversions
    /// have to meet in the middle (UI-5).
    #[test]
    fn the_gizmo_keeps_its_logical_size_across_display_scales() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Object(fieldcad_core::ObjectId::new(0));
        let camera = OrbitCamera::default();
        let display = GizmoDisplay::default();

        let hidpi = Viewport {
            x: 0,
            y: 0,
            width: VIEWPORT.width * 2,
            height: VIEWPORT.height * 2,
        };
        let length_1x = selection_gizmo_length_with_display(
            &snapshot,
            &camera,
            VIEWPORT,
            selection,
            display,
            1.0,
            SceneScale::metre(),
        )
        .unwrap();
        let length_2x = selection_gizmo_length_with_display(
            &snapshot,
            &camera,
            hidpi,
            selection,
            display,
            2.0,
            SceneScale::metre(),
        )
        .unwrap();

        assert!(
            (length_2x / length_1x - 1.0).abs() < 1.0e-4,
            "the same logical size must be the same world-space size when the \
             viewport doubles with the scale factor: {length_1x} at 1x, \
             {length_2x} at 2x"
        );

        // And the on-screen size is the same number of *logical* points —
        // twice as many physical pixels.
        let origin = selection_origin(&snapshot, selection, SceneScale::metre()).unwrap();
        let pixels = |viewport: Viewport, length: f32| {
            let base = project_to_viewport(&camera, viewport, origin).unwrap();
            let tip = project_to_viewport(&camera, viewport, origin + Vec3::X * length).unwrap();
            tip.distance(base)
        };
        let physical_1x = pixels(VIEWPORT, length_1x);
        let physical_2x = pixels(hidpi, length_2x);
        assert!(
            (physical_2x / physical_1x - 2.0).abs() < 1.0e-3,
            "a 2x display must draw the gizmo at twice the physical pixels: \
             {physical_1x}px at 1x, {physical_2x}px at 2x"
        );
    }

    /// Drawing and picking share one converted display: at 2x scaling a
    /// pointer at the drawn arrow's physical-pixel position must still pick
    /// it. (Consistency held before UI-5 too — this guards against the
    /// conversions ever diverging.)
    #[test]
    fn picking_matches_the_drawn_gizmo_on_a_hidpi_display() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source").with_shape(ObjectShape::sphere(0.25).unwrap()),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Object(snapshot.objects().values().next().unwrap().id);
        let camera = OrbitCamera::default();
        let display = GizmoDisplay::default();
        let hidpi = Viewport {
            x: 0,
            y: 0,
            width: VIEWPORT.width * 2,
            height: VIEWPORT.height * 2,
        };

        let origin = selection_origin(&snapshot, selection, SceneScale::metre()).unwrap();
        let length = selection_gizmo_length_with_display(
            &snapshot,
            &camera,
            hidpi,
            selection,
            display,
            2.0,
            SceneScale::metre(),
        )
        .unwrap();
        let (start, end) = gizmo_axis_segment(origin, Vec3::X, length);
        let start = project_to_viewport(&camera, hidpi, start).unwrap();
        let end = project_to_viewport(&camera, hidpi, end).unwrap();
        let pointer = start.lerp(end, 0.5);

        assert_eq!(
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                hidpi,
                pointer,
                display,
                2.0,
                SceneScale::metre(),
            ),
            Some(TransformHandle::AxisX),
            "the axis must be pickable where it is drawn at 2x scaling"
        );
    }

    #[test]
    fn display_settings_convert_to_physical_pixels() {
        let display = GizmoDisplay {
            axis_length_px: 120.0,
            axis_thickness_px: 4.0,
            rotation_diameter_px: 220.0,
            rotation_thickness_px: 8.0,
        };

        assert_eq!(display.to_physical(1.0), display, "1x is the identity");
        assert_eq!(
            display.to_physical(2.0),
            GizmoDisplay {
                axis_length_px: 240.0,
                axis_thickness_px: 8.0,
                rotation_diameter_px: 440.0,
                rotation_thickness_px: 16.0,
            }
        );
        // The clamp matches `Viewport::from_logical`/`pointer_to_physical`: a
        // degenerate scale factor is pinned, never propagated.
        assert_eq!(display.to_physical(0.0), display.to_physical(0.01));
    }

    #[test]
    fn display_options_control_arrow_length_and_rotation_diameter() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Object(snapshot.objects().values().next().unwrap().id);
        let camera = OrbitCamera::default();
        let display = GizmoDisplay {
            axis_length_px: 140.0,
            rotation_diameter_px: 320.0,
            ..GizmoDisplay::default()
        };

        let (_, length) = transform_gizmo_with_display(
            &snapshot,
            &camera,
            VIEWPORT,
            selection,
            display,
            SceneScale::metre(),
        )
        .unwrap();
        let expected_scale = camera.world_units_per_pixel(Vec3::ZERO, VIEWPORT.height as f32);
        assert!((length - 140.0 * expected_scale).abs() < 1.0e-5);
        let radius = rotation_gizmo_radius_with_display(
            &snapshot,
            &camera,
            VIEWPORT,
            selection,
            display,
            1.0,
            SceneScale::metre(),
        )
        .unwrap();
        assert!((radius - 160.0 * expected_scale).abs() < 1.0e-5);
    }

    #[test]
    fn gizmo_display_defaults_match_the_workbench_defaults() {
        assert_eq!(
            GizmoDisplay::default(),
            GizmoDisplay {
                axis_length_px: 120.0,
                axis_thickness_px: 4.0,
                rotation_diameter_px: 220.0,
                rotation_thickness_px: 8.0,
            }
        );
    }

    fn box_world() -> (World, fieldcad_core::BoxId) {
        use fieldcad_core::FieldBoxSpec;
        use glam::DVec3;

        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateBox(
                FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(2.0)).unwrap(),
            )])
            .unwrap();
        (world, report.created_boxes[0])
    }

    /// Regression test for a real precedence bug: a plane-drag handle's own
    /// centroid failed to pick that handle for roughly half of a sweep of
    /// camera orbit angles, because the rotation rings were checked first and
    /// a ring viewed edge-on projects to a line that can sweep right through
    /// a plane handle's screen position — not a rare coincidence, but a
    /// systematic conflict across a wide range of angles. "Inside this exact
    /// quad" is unambiguous and now wins over "within a few pixels of a ring
    /// line" for the same reason `PlaneNormal` already wins its own overlap
    /// with the Z axis: an exact, deliberately-shaped target must not lose to
    /// an approximate one.
    ///
    /// This asserts "reliable", not "always": a small number of near-edge-on
    /// angles are a genuine foreshortening limit no picking-order fix can
    /// remove — a flat handle viewed edge-on is hard to click in any tool,
    /// this one included, because it is a hard-to-see sliver on screen, not a
    /// picking bug.
    #[test]
    fn a_plane_handle_is_reliably_pickable_across_a_sweep_of_camera_orbits() {
        let (world, box_id) = box_world();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Box(box_id);

        let mut total = 0;
        let mut correct = 0;
        for yaw_deg in (0..360).step_by(15) {
            for pitch_deg in (-80..=80).step_by(20) {
                let mut camera = OrbitCamera::default();
                let yaw_radians = (yaw_deg as f32).to_radians();
                let pitch_radians = (pitch_deg as f32).to_radians();
                camera.orbit(Vec2::new(-yaw_radians / 0.006, pitch_radians / 0.006));

                let Some((origin, length)) =
                    transform_gizmo(&snapshot, &camera, VIEWPORT, selection)
                else {
                    continue;
                };
                for (handle, a, b) in GIZMO_PLANES {
                    let corners = gizmo_plane_corners(origin, a, b, length);
                    let centroid = corners.iter().fold(Vec3::ZERO, |sum, c| sum + *c) / 4.0;
                    let Some(pointer) = project_to_viewport(&camera, VIEWPORT, centroid) else {
                        continue;
                    };
                    total += 1;
                    if pick_transform_handle_with_display(
                        &snapshot,
                        selection,
                        &camera,
                        VIEWPORT,
                        pointer,
                        GizmoDisplay::default(),
                        1.0,
                        SceneScale::metre(),
                    ) == Some(handle)
                    {
                        correct += 1;
                    }
                }
            }
        }

        let hit_rate = correct as f32 / total as f32;
        assert!(
            hit_rate > 0.85,
            "expected a plane handle's own centroid to pick it reliably, got {correct}/{total} \
             ({:.1}%)",
            hit_rate * 100.0
        );
    }

    #[test]
    fn a_local_rotation_ring_is_picked_where_it_is_drawn_and_rotates_only_about_its_own_axis() {
        let (world, box_id) = box_world();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Box(box_id);
        let camera = OrbitCamera::default();
        let (origin, length) = transform_gizmo(&snapshot, &camera, VIEWPORT, selection).unwrap();
        let radius = rotation_ring_radius(length);

        // At the box's identity rotation, the Z ring is the circle in the XY
        // plane. A point on a coordinate axis (e.g. `origin + X * radius`)
        // would sit on the Z ring *and* the Y ring at once, so this picks a
        // point at 45 degrees, which belongs to the Z ring alone.
        let point_on_z_ring = origin + (Vec3::X + Vec3::Y).normalize() * radius;
        let pointer = project_to_viewport(&camera, VIEWPORT, point_on_z_ring).unwrap();

        assert_eq!(
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                VIEWPORT,
                pointer,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
            ),
            Some(TransformHandle::RotateZ)
        );

        // Two points 30 degrees apart on the same ring, so the screen-space
        // delta between their projections corresponds to a known angle
        // regardless of the default camera's particular viewing direction —
        // `project_to_viewport` and `ray_plane_point` are exact inverses for
        // two points already on the ring's own plane.
        let angle_of = |degrees: f32| {
            let radians = degrees.to_radians();
            origin + Vec3::new(radians.cos(), radians.sin(), 0.0) * radius
        };
        let pointer_before = project_to_viewport(&camera, VIEWPORT, angle_of(45.0)).unwrap();
        let pointer_after = project_to_viewport(&camera, VIEWPORT, angle_of(75.0)).unwrap();
        let delta = pointer_after - pointer_before;

        let dragged = dragged_box_rotation(
            &camera,
            VIEWPORT,
            pointer_after,
            delta,
            origin,
            Vec3::Z,
            Quat::IDENTITY,
        )
        .unwrap();

        // A meaningful rotation happened...
        assert!((dragged * Vec3::X - Vec3::X).length() > 1.0e-4);
        // ...but only about Z: Z itself is fixed, and X never leaves the XY
        // plane the way it would if Y or X rotation had leaked in.
        assert!((dragged * Vec3::Z - Vec3::Z).length() < 1.0e-4);
        assert!((dragged * Vec3::X).z.abs() < 1.0e-4);
    }

    #[test]
    fn the_view_ring_is_picked_and_rotates_about_the_cameras_forward_axis() {
        let (world, box_id) = box_world();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Box(box_id);
        let camera = OrbitCamera::default();
        let (origin, length) = transform_gizmo(&snapshot, &camera, VIEWPORT, selection).unwrap();
        let radius = view_ring_radius(length);
        let view_axis = (camera.target() - camera.eye()).normalize();
        let (a, _) = ring_basis(view_axis);

        let pointer = project_to_viewport(&camera, VIEWPORT, origin + a * radius).unwrap();
        assert_eq!(
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                VIEWPORT,
                pointer,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
            ),
            Some(TransformHandle::RotateView)
        );

        let delta = Vec2::new(24.0, 6.0);
        let dragged = dragged_view_rotation(
            &camera,
            VIEWPORT,
            pointer + delta,
            delta,
            origin,
            Quat::IDENTITY,
        )
        .unwrap();

        // Rotating about the view axis leaves that axis itself fixed.
        assert!((dragged * view_axis - view_axis).length() < 1.0e-3);
        assert!((dragged * Vec3::X - Vec3::X).length() > 1.0e-4);
    }

    #[test]
    fn empty_space_inside_the_rotation_sphere_picks_free_rotation_and_rotates_off_axis() {
        let (world, box_id) = box_world();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Box(box_id);
        let camera = OrbitCamera::default();
        let (origin, length) = transform_gizmo(&snapshot, &camera, VIEWPORT, selection).unwrap();
        let radius = rotation_ring_radius(length);

        // A configured rotation sphere may overlap a translation arrow. Find a
        // visible interior point that is not claimed by one of those more
        // specific handles, then it must be the free trackball target.
        let pointer = [
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(1.0, 1.0, -1.0),
        ]
        .into_iter()
        .flat_map(|direction| {
            [0.35, 0.55, 0.75].map(move |fraction| direction.normalize() * radius * fraction)
        })
        .filter_map(|offset| project_to_viewport(&camera, VIEWPORT, origin + offset))
        .find(|pointer| {
            pick_transform_handle_with_display(
                &snapshot,
                selection,
                &camera,
                VIEWPORT,
                *pointer,
                GizmoDisplay::default(),
                1.0,
                SceneScale::metre(),
            ) == Some(TransformHandle::RotateFree)
        })
        .expect("some unclaimed point inside the rotation sphere must start free rotation");

        let delta = Vec2::new(20.0, 15.0);
        let dragged = dragged_trackball_rotation(
            &camera,
            VIEWPORT,
            pointer + delta,
            delta,
            origin,
            radius,
            Quat::IDENTITY,
        )
        .unwrap();

        // A free trackball drag is not confined to a single world axis: at
        // least two of the three basis vectors must have moved.
        let moved = [Vec3::X, Vec3::Y, Vec3::Z]
            .into_iter()
            .filter(|axis| (dragged * *axis - *axis).length() > 1.0e-3)
            .count();
        assert!(
            moved >= 2,
            "expected an off-axis rotation, dragged = {dragged:?}"
        );
    }

    /// The regression this design specifically guards against: a translation
    /// arrow's tip can extend beyond a deliberately smaller rotation gizmo,
    /// leaving a direct translation target outside the trackball catch-all.
    #[test]
    fn a_translation_arrow_outside_the_rotation_sphere_still_picks_translation() {
        let (world, box_id) = box_world();
        let snapshot = world.snapshot();
        let selection = SceneSelection::Box(box_id);
        let camera = OrbitCamera::default();
        let (origin, length) = transform_gizmo(&snapshot, &camera, VIEWPORT, selection).unwrap();
        let radius = rotation_ring_radius(length);
        assert!(
            length > radius,
            "the translation arrow must extend beyond the rotation sphere"
        );

        // Whichever axis's grabbable segment the default oblique view happens
        // to keep clear of all four rings' screen-projected silhouettes — an
        // edge-on ring collapses to a line that can run right along an axis
        // arrow for some specific views, so this is a property of "some axis,
        // some point on its segment, for a camera actually looking at the
        // gizmo" rather than one hand-picked axis and point.
        let mut hit = None;
        'search: for axis in [
            (TransformHandle::AxisX, Vec3::X),
            (TransformHandle::AxisY, Vec3::Y),
            (TransformHandle::AxisZ, Vec3::Z),
        ] {
            let (start, end) = gizmo_axis_segment(origin, axis.1, length);
            for t in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                let pointer = project_to_viewport(&camera, VIEWPORT, start.lerp(end, t)).unwrap();
                if pick_transform_handle_with_display(
                    &snapshot,
                    selection,
                    &camera,
                    VIEWPORT,
                    pointer,
                    GizmoDisplay::default(),
                    1.0,
                    SceneScale::metre(),
                ) == Some(axis.0)
                {
                    hit = Some(());
                    break 'search;
                }
            }
        }
        assert!(
            hit.is_some(),
            "expected at least one point on some translation axis segment to pick that axis, \
             not a ring or free rotation"
        );
    }
}
