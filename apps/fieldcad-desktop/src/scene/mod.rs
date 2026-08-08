//! Turns an immutable world snapshot into things the viewport can draw and pick.
//!
//! This is the seam between the headless model and the renderer. It is pure
//! geometry over a `WorldSnapshot`, so selection, framing, and layer output are
//! testable without a window or a GPU device.
//!
//! The submodules divide the work by what it is for: [`field`] turns published
//! snapshot batches into colour and glyphs, [`gizmo`] draws and hit-tests the
//! translation handles, [`authoring`] draws plane and probe proxies, and
//! [`pick`] turns a pointer into a scene selection. This module owns the types
//! they share and the vertex primitives they all append to.

mod authoring;
mod field;
mod flow_lines;
mod gizmo;
mod interpolation;
mod pick;

pub use authoring::{
    SceneVisibility, append_authoring_geometry, append_compute_bounds, append_pending_edit_ghosts,
};
pub use field::field_geometry;
pub use gizmo::{
    GizmoDisplay, TransformHandle, TransformPreview, append_transform_gizmo_with_display,
    constrained_translation, dragged_box_rotation, dragged_plane_normal,
    dragged_trackball_rotation, dragged_view_rotation, pick_transform_handle_with_display,
    plane_normal_label_position, plane_normal_tip, rotation_gizmo_radius_with_display,
    selection_gizmo_length_with_display, selection_origin, view_plane_translation,
};
pub use pick::pick_scene;

use fieldcad_core::{
    BoxId, ObjectId, ObjectShape, PlaneId, ProbeId, SceneScale, SphereId, WorldObject,
    WorldSnapshot,
};
use glam::{DQuat, DVec3, Mat4, Quat, Vec3, Vec4};

/// One world object prepared for drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectInstance {
    pub id: ObjectId,
    /// Object space to world space.
    pub model: Mat4,
    /// Object-space half-extent of the unit box the mesh is scaled to.
    pub half_extent: Vec3,
    pub mesh: ObjectMesh,
    pub selected: bool,
}

impl ObjectInstance {
    fn from_object(object: &WorldObject, selected: bool, scene_scale: SceneScale) -> Self {
        let half_extent = scene_scale.to_render_vec3(
            object
                .shape
                .map_or(DVec3::splat(fieldcad_core::DEFAULT_PROXY_RADIUS), |shape| {
                    shape.half_extent()
                }),
        );
        let translation = scene_scale.to_render_vec3(object.transform.translation);
        let rotation = Quat::from_xyzw(
            object.transform.rotation.x as f32,
            object.transform.rotation.y as f32,
            object.transform.rotation.z as f32,
            object.transform.rotation.w as f32,
        )
        .normalize();
        let mesh = match object.shape {
            Some(ObjectShape::Point { .. } | ObjectShape::Sphere { .. }) => ObjectMesh::Sphere,
            _ => ObjectMesh::Box,
        };

        Self {
            id: object.id,
            model: Mat4::from_scale_rotation_translation(half_extent, rotation, translation),
            half_extent,
            mesh,
            selected,
        }
    }

    /// World-space centre and radius, for camera framing.
    ///
    /// Not `pub`, and only compiled for tests: no production caller uses
    /// this today — `app.rs`'s own camera-framing code goes through
    /// `WorldObject::bounding_sphere` (`fieldcad-core`) directly, not this
    /// `ObjectInstance` method.
    #[cfg(test)]
    fn bounding_sphere(&self) -> (Vec3, f32) {
        (
            self.model.w_axis.truncate(),
            self.half_extent.length().max(0.01),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObjectMesh {
    Box,
    Sphere,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneSelection {
    Object(ObjectId),
    Plane(PlaneId),
    Box(BoxId),
    Sphere(SphereId),
    Probe(ProbeId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoredVertex {
    pub position: Vec3,
    pub color: Vec4,
}

/// How one vector field is drawn over one region.
///
/// The same three questions arise wherever vectors are drawn — whether to draw
/// them, how many, and how long — so a slice plane and the whole domain settle
/// them with one value and one control rather than each growing its own. What
/// differs between regions is where the arrows go, not how they are configured.
///
/// Presentation only: density resamples the published lattice by interpolation
/// and claims no accuracy the solver did not produce, and scale changes nothing
/// but a length on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorDisplay {
    pub visible: bool,
    /// Target arrows along the longest axis of the region being drawn.
    pub density: u32,
    /// Multiplier on the automatic arrow length.
    ///
    /// The automatic length already fits the glyph spacing, so this exists for
    /// the cases that fit does not serve: reading direction in a dense field, or
    /// magnitude in a sparse one.
    pub scale: f32,
}

impl VectorDisplay {
    pub const fn new(visible: bool, density: u32) -> Self {
        Self {
            visible,
            density,
            scale: 1.0,
        }
    }
}

impl Default for VectorDisplay {
    fn default() -> Self {
        Self::new(true, 15)
    }
}

/// How one vector field's flow lines are drawn over one region.
///
/// Independent of [`VectorDisplay`]: a region can show arrows, flow lines,
/// both, or neither. Unlike arrows, flow lines are off by default everywhere
/// — tracing is far costlier than sampling a point (a streamline is many RK4
/// evaluations, run synchronously on the render thread), so a new, heavier
/// display mode should not turn itself on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowLineDisplay {
    pub visible: bool,
    /// Target streamline seeds along the longest axis of the region.
    pub density: u32,
    /// Ribbon width, in screen pixels.
    pub thickness_px: f32,
    pub animated: bool,
    /// Scroll rate along the ribbon, in ribbon-lengths per second. Only
    /// meaningful while `animated` is set.
    pub speed: f32,
}

impl FlowLineDisplay {
    pub const fn new(visible: bool, density: u32) -> Self {
        Self {
            visible,
            density,
            thickness_px: 1.5,
            animated: false,
            speed: 1.0,
        }
    }
}

impl Default for FlowLineDisplay {
    fn default() -> Self {
        Self::new(false, 12)
    }
}

/// Whole-domain presentation for one channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldLayerSettings {
    pub vectors: VectorDisplay,
    pub flow_lines: FlowLineDisplay,
}

impl Default for FieldLayerSettings {
    fn default() -> Self {
        Self {
            // Off by default, and sparser than a plane when switched on: glyphs
            // through a volume occlude each other and the scene behind them, so
            // this is opt-in and starts at a density a user can see through.
            vectors: VectorDisplay::new(false, 6),
            // Sparser still: a traced line through a volume is far noisier than
            // a point glyph at the same density (see the reference image this
            // feature was modeled on, which is inherently a 2D field).
            flow_lines: FlowLineDisplay::new(false, 4),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaneVectorMode {
    #[default]
    InPlane,
    Full3d,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneLayerSettings {
    /// Whether *this plane* draws this field.
    ///
    /// Independent of the channel's own visibility, which decides whether the
    /// field is drawn anywhere at all. Both have to be on for anything to
    /// appear, and neither is reachable from the other's control: a plane set up
    /// to show `E` keeps that arrangement while the layer is hidden, and hiding
    /// one plane's copy of a field says nothing about the others.
    pub visible: bool,
    pub magnitude_visible: bool,
    /// Target samples along the larger plane axis used to draw the colour mesh.
    pub magnitude_density: u32,
    pub vectors: VectorDisplay,
    /// The one setting a plane has and a volume does not: a 2D view cannot
    /// depict depth it has no room for, so vectors project into the plane
    /// unless asked otherwise.
    pub vector_mode: PlaneVectorMode,
    /// A plane's flow lines always trace the in-plane projection of the
    /// field, regardless of `vector_mode` — a 2D streamline cannot depict an
    /// out-of-plane component either.
    pub flow_lines: FlowLineDisplay,
}

impl Default for PlaneLayerSettings {
    fn default() -> Self {
        Self {
            visible: true,
            magnitude_visible: true,
            magnitude_density: 33,
            vectors: VectorDisplay::default(),
            vector_mode: PlaneVectorMode::InPlane,
            flow_lines: FlowLineDisplay::default(),
        }
    }
}

/// How one field box draws one channel.
///
/// Arrows only: unlike a plane, a volume's interior has no natural surface to
/// flatten a magnitude map onto, and there is no in-plane-vs-3D choice to make
/// when the region is already three-dimensional.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxLayerSettings {
    pub visible: bool,
    pub vectors: VectorDisplay,
    pub flow_lines: FlowLineDisplay,
}

impl Default for BoxLayerSettings {
    fn default() -> Self {
        Self {
            visible: true,
            vectors: VectorDisplay::default(),
            flow_lines: FlowLineDisplay::default(),
        }
    }
}

/// How one field sphere draws one channel. See [`BoxLayerSettings`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SphereLayerSettings {
    pub visible: bool,
    pub vectors: VectorDisplay,
    pub flow_lines: FlowLineDisplay,
}

impl Default for SphereLayerSettings {
    fn default() -> Self {
        Self {
            visible: true,
            vectors: VectorDisplay::default(),
            flow_lines: FlowLineDisplay::default(),
        }
    }
}

/// One vertex of a flow-line ribbon.
///
/// A ribbon segment is a screen-space quad expanded in the vertex shader, not
/// a fixed-width world-space strip: `neighbor` (the segment's other endpoint)
/// and `side` (which edge, -1 or +1) are what the shader needs to build that
/// quad facing the camera, and `arclength` (cumulative world-space distance
/// from the streamline's seed) is what drives the animated scroll.
/// `thickness_px`/`speed` are baked in per-vertex from the layer's
/// [`FlowLineDisplay`] rather than carried in a uniform, so streamlines from
/// differently configured layers can share one buffer, pipeline, and draw
/// call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowRibbonVertex {
    pub position: Vec3,
    pub neighbor: Vec3,
    pub side: f32,
    pub arclength: f32,
    pub thickness_px: f32,
    pub speed: f32,
    pub color: Vec4,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldGeometry {
    pub surface_triangles: Vec<ColoredVertex>,
    pub vector_lines: Vec<ColoredVertex>,
    pub flow_ribbons: Vec<FlowRibbonVertex>,
}

/// One channel's per-region layer settings, borrowed from its owner (the
/// desktop keeps them inside `ui::ChannelLayerSettings`). The three maps
/// travel together through the drawing path, so they are one parameter —
/// and being borrowed, passing them costs nothing.
#[derive(Clone, Copy)]
pub struct RegionLayers<'a> {
    pub planes: &'a std::collections::BTreeMap<PlaneId, PlaneLayerSettings>,
    pub boxes: &'a std::collections::BTreeMap<BoxId, BoxLayerSettings>,
    pub spheres: &'a std::collections::BTreeMap<SphereId, SphereLayerSettings>,
}

fn append_arrow(
    lines: &mut Vec<ColoredVertex>,
    origin: Vec3,
    vector: Vec3,
    length: f32,
    color: Vec4,
) {
    if !vector.is_finite() || vector.length_squared() <= f32::EPSILON || length <= 0.0 {
        return;
    }
    let direction = vector.normalize();
    let tip = origin + direction * length;
    push_line(lines, origin, tip, color);

    let reference = if direction.z.abs() < 0.9 {
        Vec3::Z
    } else {
        Vec3::X
    };
    let side = direction.cross(reference).normalize();
    let head_length = length * 0.28;
    let head_width = length * 0.14;
    let head_base = tip - direction * head_length;
    push_line(lines, tip, head_base + side * head_width, color);
    push_line(lines, tip, head_base - side * head_width, color);
}

fn push_line(lines: &mut Vec<ColoredVertex>, from: Vec3, to: Vec3, color: Vec4) {
    lines.extend([
        ColoredVertex {
            position: from,
            color,
        },
        ColoredVertex {
            position: to,
            color,
        },
    ]);
}

/// A dash pattern is geometric, not a shader effect: the gaps are simply
/// absent line segments, so this reuses the existing (opaque) `line_pipeline`
/// rather than needing a transparency-aware one. Ghost preview links (see
/// [`authoring::append_object_ghost`]) are the only current caller.
fn push_dashed_line(
    lines: &mut Vec<ColoredVertex>,
    from: Vec3,
    to: Vec3,
    color: Vec4,
    dash_length: f32,
    gap_length: f32,
) {
    let span = to - from;
    let total = span.length();
    if total <= f32::EPSILON || dash_length <= 0.0 {
        push_line(lines, from, to, color);
        return;
    }
    let direction = span / total;
    let period = dash_length + gap_length.max(0.0);
    let mut travelled = 0.0;
    while travelled < total {
        let dash_end = (travelled + dash_length).min(total);
        push_line(
            lines,
            from + direction * travelled,
            from + direction * dash_end,
            color,
        );
        travelled += period;
    }
}

/// Two triangles from four corners given in winding order.
fn push_quad(triangles: &mut Vec<ColoredVertex>, corners: [Vec3; 4], color: Vec4) {
    for index in [0, 1, 2, 0, 2, 3] {
        triangles.push(ColoredVertex {
            position: corners[index],
            color,
        });
    }
}

/// The single-precision rotation a render-space consumer needs from a
/// world-space [`DQuat`]. Shared by the box authoring proxy and its rotation
/// gizmo, which must agree on which way "up" is drawn.
pub(crate) fn quat_from_dquat(rotation: DQuat) -> Quat {
    Quat::from_xyzw(
        rotation.x as f32,
        rotation.y as f32,
        rotation.z as f32,
        rotation.w as f32,
    )
    .normalize()
}

fn push_quad_outline(lines: &mut Vec<ColoredVertex>, corners: [Vec3; 4], color: Vec4) {
    for (from, to) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
        push_line(lines, corners[from], corners[to], color);
    }
}

/// A circle of `origin + (a*cos + b*sin) * radius`, as a closed line loop.
///
/// Shared by the selection origin marker's three great circles and the sphere
/// authoring proxy's wireframe, which draw the same shape at different radii
/// and colours.
fn push_circle(
    lines: &mut Vec<ColoredVertex>,
    origin: Vec3,
    a: Vec3,
    b: Vec3,
    radius: f32,
    color: Vec4,
) {
    const SEGMENTS: u32 = 32;
    let mut previous = origin + a * radius;
    for segment in 1..=SEGMENTS {
        let angle = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        let next = origin + (a * angle.cos() + b * angle.sin()) * radius;
        push_line(lines, previous, next, color);
        previous = next;
    }
}

/// Two vectors perpendicular to `axis` and to each other, used to parametrize
/// a ring or a cylinder/cone cross-section around that axis.
fn ring_basis(axis: Vec3) -> (Vec3, Vec3) {
    let reference = if axis.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
    let a = axis.cross(reference).normalize_or_zero();
    let b = axis.cross(a).normalize_or_zero();
    (a, b)
}

/// A flat-shaded triangle: the facet normal decides how bright `color` reads,
/// baked straight into the vertex color at generation time.
///
/// The renderer has no lighting of its own — `ColoredVertex` is position plus
/// flat RGBA, and the shared shader's fragment stage just returns the vertex
/// color — so this is the one place a solid shape gets to look like more than
/// a flat silhouette: every facet of a cylinder or cone calls this once, and
/// the varying baked brightness across facets is what reads as curvature.
fn push_shaded_triangle(
    triangles: &mut Vec<ColoredVertex>,
    a: Vec3,
    b: Vec3,
    c: Vec3,
    color: Vec4,
    light_dir: Vec3,
) {
    let normal = (b - a).cross(c - a).normalize_or_zero();
    let intensity = 0.55 + 0.45 * normal.dot(light_dir).max(0.0);
    let shaded = Vec4::new(
        color.x * intensity,
        color.y * intensity,
        color.z * intensity,
        color.w,
    );
    for position in [a, b, c] {
        triangles.push(ColoredVertex {
            position,
            color: shaded,
        });
    }
}

/// A solid cylinder from `base` to `top`, `radius` wide, flat-shaded per facet.
/// The side wall only — no caps, since every caller so far attaches one end to
/// another solid shape (a cone head) or to the origin marker, where a cap
/// would never be seen.
fn append_cylinder(
    triangles: &mut Vec<ColoredVertex>,
    base: Vec3,
    top: Vec3,
    radius: f32,
    color: Vec4,
    light_dir: Vec3,
) {
    let Some(axis) = (top - base).try_normalize() else {
        return;
    };
    const SEGMENTS: u32 = 10;
    let (a, b) = ring_basis(axis);
    let ring = |centre: Vec3, segment: u32| {
        let angle = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        centre + (a * angle.cos() + b * angle.sin()) * radius
    };
    for segment in 0..SEGMENTS {
        let next = segment + 1;
        let (base0, base1) = (ring(base, segment), ring(base, next));
        let (top0, top1) = (ring(top, segment), ring(top, next));
        push_shaded_triangle(triangles, base0, top0, top1, color, light_dir);
        push_shaded_triangle(triangles, base0, top1, base1, color, light_dir);
    }
}

/// A solid cone from a `base` circle to `tip`, `radius` wide at the base,
/// flat-shaded per facet, with a base cap (unlike [`append_cylinder`]: a cone
/// head's wider base is visible past the narrower shaft it sits on).
fn append_cone(
    triangles: &mut Vec<ColoredVertex>,
    base: Vec3,
    tip: Vec3,
    radius: f32,
    color: Vec4,
    light_dir: Vec3,
) {
    let Some(axis) = (tip - base).try_normalize() else {
        return;
    };
    const SEGMENTS: u32 = 12;
    let (a, b) = ring_basis(axis);
    let ring = |segment: u32| {
        let angle = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        base + (a * angle.cos() + b * angle.sin()) * radius
    };
    for segment in 0..SEGMENTS {
        let next = segment + 1;
        let (p0, p1) = (ring(segment), ring(next));
        push_shaded_triangle(triangles, p0, p1, tip, color, light_dir);
        push_shaded_triangle(triangles, base, p1, p0, color, light_dir);
    }
}

/// A flat ribbon around `origin`, perpendicular to `axis`, spanning
/// `radius - width/2` to `radius + width/2` — the solid-band analogue of
/// [`push_circle`], used wherever a rotation ring needs to be more than a
/// single-pixel line to stay legible and easy to grab.
fn append_ring_band(
    triangles: &mut Vec<ColoredVertex>,
    origin: Vec3,
    axis: Vec3,
    radius: f32,
    width: f32,
    color: Vec4,
    light_dir: Vec3,
) {
    const SEGMENTS: u32 = 48;
    let (a, b) = ring_basis(axis);
    let half_width = width * 0.5;
    let point = |t: f32, r: f32| origin + (a * t.cos() + b * t.sin()) * r;
    let mut previous_inner = point(0.0, radius - half_width);
    let mut previous_outer = point(0.0, radius + half_width);
    for segment in 1..=SEGMENTS {
        let t = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        let inner = point(t, radius - half_width);
        let outer = point(t, radius + half_width);
        push_shaded_triangle(
            triangles,
            previous_inner,
            previous_outer,
            outer,
            color,
            light_dir,
        );
        push_shaded_triangle(triangles, previous_inner, outer, inner, color, light_dir);
        previous_inner = inner;
        previous_outer = outer;
    }
}

/// Every drawable object in the world, in stable identifier order.
///
/// Empty when the view is hiding objects. Returning nothing rather than leaving
/// the filter to each caller is what keeps the renderer and the hit-test
/// agreeing about what is on screen.
pub fn instances(
    world: &WorldSnapshot,
    selection: Option<ObjectId>,
    show: SceneVisibility,
    scene_scale: SceneScale,
) -> Vec<ObjectInstance> {
    if !show.objects {
        return Vec::new();
    }
    world
        .objects()
        .values()
        .filter(|object| object.visible)
        .map(|object| {
            ObjectInstance::from_object(object, selection == Some(object.id), scene_scale)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectShape, ObjectSpec, Transform, World, WorldCommand};
    use glam::DVec3;

    use super::*;

    pub(super) fn world_with_two_boxes() -> World {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(
                    ObjectSpec::new("near")
                        .with_transform(Transform::at(DVec3::new(0.0, -3.0, 0.0)).unwrap())
                        .with_shape(ObjectShape::boxed(DVec3::splat(0.5)).unwrap()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("far")
                        .with_transform(Transform::at(DVec3::new(0.0, 3.0, 0.0)).unwrap())
                        .with_shape(ObjectShape::boxed(DVec3::splat(0.5)).unwrap()),
                ),
            ])
            .unwrap();
        world
    }

    #[test]
    fn instances_follow_the_world_not_a_hardcoded_placeholder() {
        let world = world_with_two_boxes();
        let snapshot = world.snapshot();

        let built = instances(
            &snapshot,
            Some(ObjectId::new(1)),
            SceneVisibility::ALL,
            SceneScale::metre(),
        );

        assert_eq!(built.len(), 2);
        assert_eq!(built[0].id, ObjectId::new(0));
        assert!(!built[0].selected);
        assert!(built[1].selected);
        assert_eq!(built[1].model.w_axis.truncate(), Vec3::new(0.0, 3.0, 0.0));
    }

    #[test]
    fn an_object_with_no_shape_still_gets_a_selectable_proxy() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("bare"))])
            .unwrap();

        let built = instances(
            &world.snapshot(),
            None,
            SceneVisibility::ALL,
            SceneScale::metre(),
        );

        assert_eq!(built.len(), 1);
        assert!(built[0].half_extent.min_element() > 0.0);
    }

    #[test]
    fn point_and_sphere_sources_use_sphere_meshes_and_hidden_objects_are_omitted() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(
                    ObjectSpec::new("point").with_shape(ObjectShape::point(0.1).unwrap()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("sphere").with_shape(ObjectShape::sphere(0.2).unwrap()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("hidden box")
                        .with_shape(ObjectShape::boxed(DVec3::ONE).unwrap())
                        .with_visibility(false),
                ),
            ])
            .unwrap();

        let built = instances(
            &world.snapshot(),
            None,
            SceneVisibility::ALL,
            SceneScale::metre(),
        );

        assert_eq!(built.len(), 2);
        assert!(
            built
                .iter()
                .all(|instance| instance.mesh == ObjectMesh::Sphere)
        );
    }

    #[test]
    fn instance_bounding_spheres_frame_the_drawn_geometry() {
        let world = world_with_two_boxes();
        let built = instances(
            &world.snapshot(),
            None,
            SceneVisibility::ALL,
            SceneScale::metre(),
        );
        let (centre, radius) = built[0].bounding_sphere();

        assert_eq!(centre, Vec3::new(0.0, -3.0, 0.0));
        assert!(radius >= 0.5);
    }
}
