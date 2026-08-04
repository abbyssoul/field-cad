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
mod gizmo;
mod pick;

pub use authoring::{SceneVisibility, append_authoring_geometry};
pub use field::field_geometry;
pub use gizmo::{
    TransformHandle, TransformPreview, append_transform_gizmo, constrained_translation,
    dragged_plane_normal, pick_transform_handle, plane_normal_label_position, plane_normal_tip,
    selection_gizmo_length, selection_origin, view_plane_translation,
};
pub use pick::{pick_object, pick_scene};

use fieldcad_core::{ObjectId, ObjectShape, PlaneId, ProbeId, WorldObject, WorldSnapshot};
use glam::{Mat4, Quat, Vec3, Vec4};

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
    fn from_object(object: &WorldObject, selected: bool) -> Self {
        let half_extent = object.shape.map_or(
            Vec3::splat(fieldcad_core::DEFAULT_PROXY_RADIUS as f32),
            |shape| shape.half_extent().as_vec3(),
        );
        let translation = object.transform.translation.as_vec3();
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
    pub fn bounding_sphere(&self) -> (Vec3, f32) {
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

/// Whole-domain presentation for one channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldLayerSettings {
    pub vectors: VectorDisplay,
}

impl Default for FieldLayerSettings {
    fn default() -> Self {
        Self {
            // Off by default, and sparser than a plane when switched on: glyphs
            // through a volume occlude each other and the scene behind them, so
            // this is opt-in and starts at a density a user can see through.
            vectors: VectorDisplay::new(false, 6),
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
}

impl Default for PlaneLayerSettings {
    fn default() -> Self {
        Self {
            visible: true,
            magnitude_visible: true,
            magnitude_density: 33,
            vectors: VectorDisplay::default(),
            vector_mode: PlaneVectorMode::InPlane,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldGeometry {
    pub surface_triangles: Vec<ColoredVertex>,
    pub vector_lines: Vec<ColoredVertex>,
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

/// Two triangles from four corners given in winding order.
fn push_quad(triangles: &mut Vec<ColoredVertex>, corners: [Vec3; 4], color: Vec4) {
    for index in [0, 1, 2, 0, 2, 3] {
        triangles.push(ColoredVertex {
            position: corners[index],
            color,
        });
    }
}

fn push_quad_outline(lines: &mut Vec<ColoredVertex>, corners: [Vec3; 4], color: Vec4) {
    for (from, to) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
        push_line(lines, corners[from], corners[to], color);
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
) -> Vec<ObjectInstance> {
    if !show.objects {
        return Vec::new();
    }
    world
        .objects()
        .values()
        .filter(|object| object.visible)
        .map(|object| ObjectInstance::from_object(object, selection == Some(object.id)))
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

        let built = instances(&snapshot, Some(ObjectId::new(1)), SceneVisibility::ALL);

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

        let built = instances(&world.snapshot(), None, SceneVisibility::ALL);

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

        let built = instances(&world.snapshot(), None, SceneVisibility::ALL);

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
        let built = instances(&world.snapshot(), None, SceneVisibility::ALL);
        let (centre, radius) = built[0].bounding_sphere();

        assert_eq!(centre, Vec3::new(0.0, -3.0, 0.0));
        assert!(radius >= 0.5);
    }
}
