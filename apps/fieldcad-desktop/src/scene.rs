//! Turns an immutable world snapshot into things the viewport can draw and pick.
//!
//! This is the seam between the headless model and the renderer. It is pure
//! geometry over a `WorldSnapshot`, so selection and framing are testable without
//! a window or a GPU device.

use std::collections::BTreeMap;

use fieldcad_core::{
    FieldColumn, FieldSnapshot, ObjectId, ObjectShape, PlaneId, ProbeId, SampleGeometry,
    SampleValidity, SlicePlane, WorldObject, WorldSnapshot,
};
use fieldcad_electrostatics::electric_field_channel_id;
use glam::{DVec3, Mat4, Quat, Vec2, Vec3, Vec4};

use crate::camera::{OrbitCamera, Viewport};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformHandle {
    AxisX,
    AxisY,
    AxisZ,
    PlaneXY,
    PlaneYZ,
    PlaneZX,
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
        }
    }

    pub const fn axis(self) -> Option<Vec3> {
        match self {
            Self::AxisX => Some(Vec3::X),
            Self::AxisY => Some(Vec3::Y),
            Self::AxisZ => Some(Vec3::Z),
            _ => None,
        }
    }

    pub const fn plane_normal(self) -> Option<Vec3> {
        match self {
            Self::PlaneXY => Some(Vec3::Z),
            Self::PlaneYZ => Some(Vec3::X),
            Self::PlaneZX => Some(Vec3::Y),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoredVertex {
    pub position: Vec3,
    pub color: Vec4,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FieldLayerSettings {
    pub domain_vectors: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaneVectorMode {
    #[default]
    InPlane,
    Full3d,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneLayerSettings {
    pub magnitude_visible: bool,
    pub vectors_visible: bool,
    /// Target samples along the larger plane axis used to draw the colour mesh.
    pub magnitude_density: u32,
    /// Target arrows along the larger plane axis.
    pub vector_density: u32,
    pub vector_mode: PlaneVectorMode,
}

impl Default for PlaneLayerSettings {
    fn default() -> Self {
        Self {
            magnitude_visible: true,
            vectors_visible: true,
            magnitude_density: 33,
            vector_density: 15,
            vector_mode: PlaneVectorMode::InPlane,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldGeometry {
    pub surface_triangles: Vec<ColoredVertex>,
    pub vector_lines: Vec<ColoredVertex>,
}

/// Convert the electric-field batches in one immutable snapshot into generic
/// coloured triangles and line glyphs. Undefined samples are omitted rather
/// than clamped into something that looks measured.
pub fn field_geometry(
    snapshot: &FieldSnapshot,
    settings: FieldLayerSettings,
    plane_layers: &BTreeMap<PlaneId, PlaneLayerSettings>,
) -> FieldGeometry {
    let Some(channel) = snapshot.channel(&electric_field_channel_id()) else {
        return FieldGeometry::default();
    };
    let mut output = FieldGeometry::default();

    for batch in channel.batches.iter() {
        let FieldColumn::Vector(values) = batch.values() else {
            continue;
        };
        match batch.geometry() {
            SampleGeometry::Plane { plane, lattice } => {
                let plane_settings = plane_layers.get(plane).copied().unwrap_or_default();
                let normal = lattice_normal(*lattice);
                let displayed_values: Vec<_> = values
                    .iter()
                    .map(|value| displayed_plane_vector(*value, normal, plane_settings.vector_mode))
                    .collect();
                let colors = magnitude_colors(&displayed_values, batch.validity());
                let offset = normal * 0.006;
                if plane_settings.magnitude_visible {
                    append_plane_surface(
                        &mut output.surface_triangles,
                        *lattice,
                        batch.validity(),
                        &colors,
                        offset,
                        plane_settings.magnitude_density,
                    );
                }
                if plane_settings.vectors_visible {
                    append_plane_vectors(
                        &mut output.vector_lines,
                        *lattice,
                        &displayed_values,
                        batch.validity(),
                        &colors,
                        offset + normal * 0.008,
                        plane_settings.vector_density,
                    );
                }
            }
            SampleGeometry::Grid(lattice) if settings.domain_vectors => {
                let colors = magnitude_colors(values, batch.validity());
                let characteristic_length = lattice.step().min_element().abs() as f32;
                for index in 0..lattice.len() {
                    if !batch.validity()[index].is_usable() {
                        continue;
                    }
                    let vector = values[index].as_vec3();
                    append_arrow(
                        &mut output.vector_lines,
                        lattice
                            .position(index)
                            .expect("index is inside lattice")
                            .as_vec3(),
                        vector,
                        characteristic_length * glyph_scale(vector.length(), values),
                        colors[index].extend(1.0),
                    );
                }
            }
            _ => {}
        }
    }
    output
}

fn displayed_plane_vector(value: DVec3, normal: Vec3, mode: PlaneVectorMode) -> DVec3 {
    match mode {
        PlaneVectorMode::InPlane => value - normal.as_dvec3() * value.dot(normal.as_dvec3()),
        PlaneVectorMode::Full3d => value,
    }
}

pub fn append_translation_gizmo(
    geometry: &mut FieldGeometry,
    world: &WorldSnapshot,
    selection: Option<ObjectId>,
    active: Option<TransformHandle>,
) {
    let Some(object) = selection
        .and_then(|id| world.object(id))
        .filter(|object| object.visible)
    else {
        return;
    };
    let origin = object.transform.translation.as_vec3();
    let length = gizmo_length(object);
    for (handle, direction, color) in [
        (
            TransformHandle::AxisX,
            Vec3::X,
            Vec4::new(0.95, 0.15, 0.18, 1.0),
        ),
        (
            TransformHandle::AxisY,
            Vec3::Y,
            Vec4::new(0.18, 0.9, 0.3, 1.0),
        ),
        (
            TransformHandle::AxisZ,
            Vec3::Z,
            Vec4::new(0.2, 0.45, 1.0, 1.0),
        ),
    ] {
        let color = if active == Some(handle) {
            Vec4::new(1.0, 0.9, 0.18, 1.0)
        } else if active.is_some() {
            color * Vec4::new(0.45, 0.45, 0.45, 1.0)
        } else {
            color
        };
        append_arrow(&mut geometry.vector_lines, origin, direction, length, color);
    }

    for (handle, a, b, color) in [
        (
            TransformHandle::PlaneXY,
            Vec3::X,
            Vec3::Y,
            Vec4::new(0.95, 0.84, 0.12, 0.28),
        ),
        (
            TransformHandle::PlaneYZ,
            Vec3::Y,
            Vec3::Z,
            Vec4::new(0.1, 0.8, 0.8, 0.28),
        ),
        (
            TransformHandle::PlaneZX,
            Vec3::Z,
            Vec3::X,
            Vec4::new(0.85, 0.2, 0.82, 0.28),
        ),
    ] {
        let color = if active == Some(handle) {
            Vec4::new(1.0, 0.9, 0.18, 0.72)
        } else if active.is_some() {
            color * Vec4::new(0.45, 0.45, 0.45, 1.0)
        } else {
            color
        };
        append_gizmo_plane(geometry, origin, a, b, length, color);
    }
}

fn gizmo_length(object: &WorldObject) -> f32 {
    object
        .shape
        .map_or(0.5, |shape| shape.bounding_radius() as f32 * 2.0)
        .max(0.9)
}

fn append_plane_surface(
    triangles: &mut Vec<ColoredVertex>,
    lattice: fieldcad_core::PlaneLattice,
    validity: &[SampleValidity],
    colors: &[Vec3],
    offset: Vec3,
    density: u32,
) {
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, density);
    let ys = uniform_axis(counts.y, density);
    for y_pair in ys.windows(2) {
        for x_pair in xs.windows(2) {
            let sample = |u, v| {
                let interpolation = plane_interpolation(lattice, u, v)?;
                interpolation.is_usable(validity).then_some(ColoredVertex {
                    position: interpolation.position + offset,
                    color: interpolation.vec3(colors).extend(0.78),
                })
            };
            let (Some(lower_left), Some(lower_right), Some(upper_right), Some(upper_left)) = (
                sample(x_pair[0], y_pair[0]),
                sample(x_pair[1], y_pair[0]),
                sample(x_pair[1], y_pair[1]),
                sample(x_pair[0], y_pair[1]),
            ) else {
                continue;
            };
            triangles.extend([
                lower_left,
                lower_right,
                upper_right,
                lower_left,
                upper_right,
                upper_left,
            ]);
        }
    }
}

fn append_plane_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: fieldcad_core::PlaneLattice,
    values: &[glam::DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    offset: Vec3,
    density: u32,
) {
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, density);
    let ys = uniform_axis(counts.y, density);
    let step_length = uniform_glyph_spacing(lattice, &xs, &ys);
    for &y in &ys {
        for &x in &xs {
            let Some(interpolation) = plane_interpolation(lattice, x, y) else {
                continue;
            };
            if !interpolation.is_usable(validity) {
                continue;
            }
            let vector = interpolation.dvec3(values).as_vec3();
            append_arrow(
                lines,
                interpolation.position + offset,
                vector,
                step_length * glyph_scale(vector.length(), values),
                interpolation.vec3(colors).extend(1.0),
            );
        }
    }
}

/// Coordinates in snapshot-lattice space, distributed uniformly across its
/// complete extent. Fractional coordinates deliberately support a display
/// density above or between the published sample counts without clustering on
/// integer sample indices.
fn uniform_axis(count: u32, target: u32) -> Vec<f64> {
    if target == 0 {
        return Vec::new();
    }
    let extent = f64::from(count.saturating_sub(1));
    if target == 1 {
        return vec![extent * 0.5];
    }
    (0..target)
        .map(|index| f64::from(index) * extent / f64::from(target - 1))
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct PlaneInterpolation {
    position: Vec3,
    indices: [usize; 4],
    weights: [f64; 4],
}

impl PlaneInterpolation {
    fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

fn plane_interpolation(
    lattice: fieldcad_core::PlaneLattice,
    u: f64,
    v: f64,
) -> Option<PlaneInterpolation> {
    let counts = lattice.counts();
    let u = u.clamp(0.0, f64::from(counts.x.saturating_sub(1)));
    let v = v.clamp(0.0, f64::from(counts.y.saturating_sub(1)));
    let left = u.floor() as usize;
    let right = u.ceil() as usize;
    let lower = v.floor() as usize;
    let upper = v.ceil() as usize;
    let u_fraction = u - left as f64;
    let v_fraction = v - lower as f64;
    let width = counts.x as usize;
    let indices = [
        lower * width + left,
        lower * width + right,
        upper * width + right,
        upper * width + left,
    ];
    let weights = [
        (1.0 - u_fraction) * (1.0 - v_fraction),
        u_fraction * (1.0 - v_fraction),
        u_fraction * v_fraction,
        (1.0 - u_fraction) * v_fraction,
    ];
    let position = indices
        .into_iter()
        .zip(weights)
        .map(|(index, weight)| lattice.position(index).map(|point| point * weight))
        .sum::<Option<DVec3>>()?
        .as_vec3();
    Some(PlaneInterpolation {
        position,
        indices,
        weights,
    })
}

fn uniform_glyph_spacing(lattice: fieldcad_core::PlaneLattice, xs: &[f64], ys: &[f64]) -> f32 {
    let mut spacings = Vec::with_capacity(2);
    if xs.len() > 1
        && let (Some(first), Some(second)) = (
            plane_interpolation(lattice, xs[0], ys[0]),
            plane_interpolation(lattice, xs[1], ys[0]),
        )
    {
        spacings.push(first.position.distance(second.position));
    }
    if ys.len() > 1
        && let (Some(first), Some(second)) = (
            plane_interpolation(lattice, xs[0], ys[0]),
            plane_interpolation(lattice, xs[0], ys[1]),
        )
    {
        spacings.push(first.position.distance(second.position));
    }
    spacings
        .into_iter()
        .filter(|spacing| *spacing > f32::EPSILON)
        .reduce(f32::min)
        .unwrap_or(0.25)
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

fn append_gizmo_plane(
    geometry: &mut FieldGeometry,
    origin: Vec3,
    a: Vec3,
    b: Vec3,
    length: f32,
    color: Vec4,
) {
    let inner = length * 0.18;
    let outer = length * 0.42;
    let corners = [
        origin + a * inner + b * inner,
        origin + a * outer + b * inner,
        origin + a * outer + b * outer,
        origin + a * inner + b * outer,
    ];
    for index in [0, 1, 2, 0, 2, 3] {
        geometry.surface_triangles.push(ColoredVertex {
            position: corners[index],
            color,
        });
    }
    let outline = color.with_w(0.95);
    for edge in [(0, 1), (1, 2), (2, 3), (3, 0)] {
        push_line(
            &mut geometry.vector_lines,
            corners[edge.0],
            corners[edge.1],
            outline,
        );
    }
}

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
    for index in [0, 1, 2, 0, 2, 3] {
        geometry.surface_triangles.push(ColoredVertex {
            position: corners[index],
            color: body,
        });
    }
    let outline = if selected {
        Vec4::new(1.0, 0.48, 0.08, 1.0)
    } else {
        Vec4::new(0.55, 0.78, 0.92, 0.9)
    };
    for edge in [(0, 1), (1, 2), (2, 3), (3, 0)] {
        push_line(
            &mut geometry.vector_lines,
            corners[edge.0],
            corners[edge.1],
            outline,
        );
    }

    let tab = half.min_element().max(0.01) * 0.12;
    let corner = corners[0] + normal * 0.003;
    let tab_corners = [
        corner,
        corner + u * tab,
        corner + u * tab + v * tab,
        corner + v * tab,
    ];
    for index in [0, 1, 2, 0, 2, 3] {
        geometry.surface_triangles.push(ColoredVertex {
            position: tab_corners[index],
            color: outline,
        });
    }
}

fn lattice_normal(lattice: fieldcad_core::PlaneLattice) -> Vec3 {
    let origin = lattice.position(0).unwrap_or_default();
    let u = lattice.position(1).unwrap_or(origin + glam::DVec3::X) - origin;
    let v_index = lattice.counts().x as usize;
    let v = lattice.position(v_index).unwrap_or(origin + glam::DVec3::Y) - origin;
    u.cross(v).normalize_or_zero().as_vec3()
}

fn magnitude_colors(values: &[glam::DVec3], validity: &[SampleValidity]) -> Vec<Vec3> {
    let maximum = values
        .iter()
        .zip(validity)
        .filter(|(_, validity)| validity.is_usable())
        .map(|(value, _)| value.length())
        .fold(0.0_f64, f64::max);
    values
        .iter()
        .zip(validity)
        .map(|(value, validity)| {
            if !validity.is_usable() {
                return Vec3::ZERO;
            }
            field_color(normalized_log_magnitude(value.length(), maximum))
        })
        .collect()
}

fn normalized_log_magnitude(magnitude: f64, maximum: f64) -> f32 {
    if magnitude <= 0.0 || maximum <= 0.0 {
        return 0.0;
    }
    let floor = (maximum * 1.0e-4).max(f64::MIN_POSITIVE);
    ((magnitude.max(floor).ln() - floor.ln()) / (maximum.ln() - floor.ln()).max(1.0e-12))
        .clamp(0.0, 1.0) as f32
}

fn glyph_scale(magnitude: f32, values: &[glam::DVec3]) -> f32 {
    let maximum = values
        .iter()
        .map(|value| value.length() as f32)
        .fold(0.0_f32, f32::max);
    0.18 + normalized_log_magnitude(magnitude as f64, maximum as f64) * 0.62
}

fn field_color(value: f32) -> Vec3 {
    let deep_blue = Vec3::new(0.02, 0.10, 0.42);
    let cyan = Vec3::new(0.02, 0.82, 0.88);
    let yellow = Vec3::new(1.0, 0.84, 0.12);
    let red = Vec3::new(0.95, 0.12, 0.04);
    if value < 0.4 {
        deep_blue.lerp(cyan, value / 0.4)
    } else if value < 0.75 {
        cyan.lerp(yellow, (value - 0.4) / 0.35)
    } else {
        yellow.lerp(red, (value - 0.75) / 0.25)
    }
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

/// Every drawable object in the world, in stable identifier order.
pub fn instances(world: &WorldSnapshot, selection: Option<ObjectId>) -> Vec<ObjectInstance> {
    world
        .objects()
        .values()
        .filter(|object| object.visible)
        .map(|object| ObjectInstance::from_object(object, selection == Some(object.id)))
        .collect()
}

/// The nearest visible authoring entity under a pointer.
pub fn pick_scene(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
) -> Option<SceneSelection> {
    let ray = camera.ray_from_viewport(pointer, viewport)?;
    let mut nearest = nearest_object_hit(world, ray)
        .map(|(distance, object)| (distance, SceneSelection::Object(object)));

    for plane in world.planes().values().filter(|plane| plane.visible) {
        let Some(distance) = ray_plane_hit(ray, plane) else {
            continue;
        };
        if nearest.is_none_or(|(best, _)| distance < best) {
            nearest = Some((distance, SceneSelection::Plane(plane.id)));
        }
    }

    for probe in world.probes().values().filter(|probe| probe.visible) {
        let Ok(position) = world.resolve_probe_position(probe) else {
            continue;
        };
        if let Some(distance) = ray.hit_sphere(position.as_vec3(), 0.13)
            && nearest.is_none_or(|(best, _)| distance < best)
        {
            nearest = Some((distance, SceneSelection::Probe(probe.id)));
        }
    }

    nearest.map(|(_, selection)| selection)
}

/// The nearest visible object proxy under a pointer. This intentionally ignores
/// plane/probe authoring geometry so an already-selected object's own visible
/// body remains a reliable free-drag target.
pub fn pick_object(
    world: &WorldSnapshot,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
) -> Option<ObjectId> {
    let ray = camera.ray_from_viewport(pointer, viewport)?;
    nearest_object_hit(world, ray).map(|(_, object)| object)
}

fn nearest_object_hit(world: &WorldSnapshot, ray: crate::camera::Ray) -> Option<(f32, ObjectId)> {
    let mut nearest = None;
    for instance in instances(world, None) {
        let inverse = instance.model.inverse();
        if !inverse.is_finite() {
            continue;
        }
        let local = crate::camera::Ray {
            origin: inverse.transform_point3(ray.origin),
            direction: inverse.transform_vector3(ray.direction),
        };
        let distance = match instance.mesh {
            ObjectMesh::Box => local.hit_aabb(Vec3::NEG_ONE, Vec3::ONE),
            ObjectMesh::Sphere => local.hit_sphere(Vec3::ZERO, 1.0),
        };
        let Some(distance) = distance else {
            continue;
        };
        let world_hit = instance
            .model
            .transform_point3(local.origin + local.direction * distance);
        let world_distance = world_hit.distance(ray.origin);
        if nearest.is_none_or(|(best, _)| world_distance < best) {
            nearest = Some((world_distance, instance.id));
        }
    }
    nearest
}

fn ray_plane_hit(ray: crate::camera::Ray, plane: &SlicePlane) -> Option<f32> {
    let normal = plane.normal.as_vec3();
    let denominator = ray.direction.dot(normal);
    if denominator.abs() < 1.0e-6 {
        return None;
    }
    let distance = (plane.origin.as_vec3() - ray.origin).dot(normal) / denominator;
    if distance < 0.0 {
        return None;
    }
    let hit = ray.origin + ray.direction * distance - plane.origin.as_vec3();
    let (u, v) = plane.basis();
    (hit.dot(u.as_vec3()).abs() <= plane.half_extent.x as f32
        && hit.dot(v.as_vec3()).abs() <= plane.half_extent.y as f32)
        .then_some(distance)
}

pub fn pick_transform_handle(
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
    object: &WorldObject,
) -> Option<TransformHandle> {
    if !object.visible {
        return None;
    }
    let origin = object.transform.translation.as_vec3();
    let length = gizmo_length(object);

    // Plane squares overlap the inner part of the axes, so test their filled
    // screen-space quads first.
    for (handle, a, b) in [
        (TransformHandle::PlaneXY, Vec3::X, Vec3::Y),
        (TransformHandle::PlaneYZ, Vec3::Y, Vec3::Z),
        (TransformHandle::PlaneZX, Vec3::Z, Vec3::X),
    ] {
        let inner = length * 0.18;
        let outer = length * 0.42;
        let world = [
            origin + a * inner + b * inner,
            origin + a * outer + b * inner,
            origin + a * outer + b * outer,
            origin + a * inner + b * outer,
        ];
        let Some(screen) = world
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
    for (handle, axis) in [
        (TransformHandle::AxisX, Vec3::X),
        (TransformHandle::AxisY, Vec3::Y),
        (TransformHandle::AxisZ, Vec3::Z),
    ] {
        let Some(start) = project_to_viewport(camera, viewport, origin + axis * length * 0.45)
        else {
            continue;
        };
        let Some(end) = project_to_viewport(camera, viewport, origin + axis * length) else {
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
    object: &WorldObject,
) -> Option<Vec3> {
    if let Some(axis) = handle.axis() {
        let length = gizmo_length(object);
        let screen_origin = project_to_viewport(camera, viewport, object_origin)?;
        let screen_tip = project_to_viewport(camera, viewport, object_origin + axis * length)?;
        let screen_axis = screen_tip - screen_origin;
        let pixels = screen_axis.length();
        if pixels < 2.0 {
            return None;
        }
        let world_distance = pointer_delta.dot(screen_axis / pixels) * length / pixels;
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

fn ray_plane_point(ray: crate::camera::Ray, origin: Vec3, normal: Vec3) -> Option<Vec3> {
    let denominator = ray.direction.dot(normal);
    if denominator.abs() < 1.0e-6 {
        return None;
    }
    let distance = (origin - ray.origin).dot(normal) / denominator;
    (distance >= 0.0).then_some(ray.origin + ray.direction * distance)
}

fn project_to_viewport(camera: &OrbitCamera, viewport: Viewport, point: Vec3) -> Option<Vec2> {
    let clip = camera.view_projection(viewport.aspect_ratio()) * point.extend(1.0);
    if !clip.is_finite() || clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(Vec2::new(
        viewport.x as f32 + (ndc.x + 1.0) * 0.5 * viewport.width as f32,
        viewport.y as f32 + (1.0 - ndc.y) * 0.5 * viewport.height as f32,
    ))
}

fn point_segment_distance(point: Vec2, start: Vec2, end: Vec2) -> f32 {
    let segment = end - start;
    let denominator = segment.length_squared();
    if denominator <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / denominator).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

fn point_in_triangle(point: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let sign = |p: Vec2, q: Vec2, r: Vec2| (p.x - r.x) * (q.y - r.y) - (q.x - r.x) * (p.y - r.y);
    if sign(a, b, c).abs() < 1.0 {
        return false;
    }
    let d1 = sign(point, a, b);
    let d2 = sign(point, b, c);
    let d3 = sign(point, c, a);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        ObjectShape, ObjectSpec, PlaneLattice, SampleValidity, SlicePlaneSpec, Transform,
        UndefinedReason, World, WorldCommand,
    };
    use glam::{DQuat, DVec3, UVec2};

    use super::*;
    use crate::camera::AxisView;

    fn world_with_two_boxes() -> World {
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

        let built = instances(&snapshot, Some(ObjectId::new(1)));

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

        let built = instances(&world.snapshot(), None);

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

        let built = instances(&world.snapshot(), None);

        assert_eq!(built.len(), 2);
        assert!(
            built
                .iter()
                .all(|instance| instance.mesh == ObjectMesh::Sphere)
        );
    }

    #[test]
    fn picking_returns_the_nearest_object_along_the_ray() {
        let world = world_with_two_boxes();
        let snapshot = world.snapshot();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let mut camera = OrbitCamera::default();
        // Look down -Y from +Y so both boxes line up, far one behind near one.
        camera.focus(Vec3::ZERO, 1.0);
        camera.set_axis_view(AxisView::PositiveY);

        let centre = Vec2::new(400.0, 300.0);
        let picked = pick_scene(&snapshot, &camera, viewport, centre);

        // From +Y looking back at the origin, the box at y = +3 is nearer.
        assert_eq!(picked, Some(SceneSelection::Object(ObjectId::new(1))));
    }

    #[test]
    fn clicking_empty_space_selects_nothing() {
        let world = world_with_two_boxes();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };

        let corner = Vec2::new(2.0, 2.0);
        assert_eq!(
            pick_scene(&world.snapshot(), &OrbitCamera::default(), viewport, corner),
            None
        );
    }

    #[test]
    fn visible_plane_is_selectable_in_the_viewport() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z)
                    .unwrap()
                    .with_half_extent(glam::DVec2::splat(2.0))
                    .unwrap(),
            )])
            .unwrap();
        let plane = *world.snapshot().planes().keys().next().unwrap();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let mut camera = OrbitCamera::default();
        camera.focus(Vec3::ZERO, 2.0);
        camera.set_axis_view(AxisView::PositiveZ);

        assert_eq!(
            pick_scene(
                &world.snapshot(),
                &camera,
                viewport,
                Vec2::new(400.0, 300.0)
            ),
            Some(SceneSelection::Plane(plane))
        );
    }

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
        let length = gizmo_length(object);
        let start =
            project_to_viewport(&camera, viewport, origin + Vec3::X * length * 0.6).unwrap();
        let end = project_to_viewport(&camera, viewport, origin + Vec3::X * length * 0.9).unwrap();
        let pointer = start.lerp(end, 0.5);

        assert_eq!(
            pick_transform_handle(&camera, viewport, pointer, object),
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
            object,
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
            object,
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
    fn free_drag_hit_testing_starts_only_on_an_object_proxy() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::at(DVec3::new(0.0, 0.0, 0.6)).unwrap())
                    .with_shape(ObjectShape::sphere(0.25).unwrap()),
            )])
            .unwrap();
        let object = *world.snapshot().objects().keys().next().unwrap();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };

        assert_eq!(
            pick_object(
                &world.snapshot(),
                &OrbitCamera::default(),
                viewport,
                Vec2::new(400.0, 300.0),
            ),
            Some(object)
        );
        assert_eq!(
            pick_object(
                &world.snapshot(),
                &OrbitCamera::default(),
                viewport,
                Vec2::new(2.0, 2.0),
            ),
            None
        );
    }

    #[test]
    fn plane_vectors_default_to_the_in_plane_component() {
        let value = DVec3::new(1.0, 2.0, 3.0);

        assert_eq!(
            displayed_plane_vector(value, Vec3::Z, PlaneVectorMode::InPlane),
            DVec3::new(1.0, 2.0, 0.0)
        );
        assert_eq!(
            displayed_plane_vector(value, Vec3::Z, PlaneVectorMode::Full3d),
            value
        );
    }

    #[test]
    fn picking_respects_object_rotation() {
        let mut world = World::new();
        // A long thin slab, rotated so its long axis lies along X.
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("slab")
                    .with_transform(
                        Transform::new(
                            DVec3::ZERO,
                            DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2),
                        )
                        .unwrap(),
                    )
                    .with_shape(ObjectShape::boxed(DVec3::new(0.1, 4.0, 0.1)).unwrap()),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let instance = instances(&snapshot, None)[0];

        // A ray along +Y at x = 3 misses the unrotated slab but hits the rotated
        // one, which now extends along X.
        let ray = crate::camera::Ray {
            origin: Vec3::new(3.0, -10.0, 0.0),
            direction: Vec3::Y,
        };
        let inverse = instance.model.inverse();
        let local = crate::camera::Ray {
            origin: inverse.transform_point3(ray.origin),
            direction: inverse.transform_vector3(ray.direction),
        };

        assert!(local.hit_aabb(Vec3::NEG_ONE, Vec3::ONE).is_some());
    }

    #[test]
    fn instance_bounding_spheres_frame_the_drawn_geometry() {
        let world = world_with_two_boxes();
        let built = instances(&world.snapshot(), None);
        let (centre, radius) = built[0].bounding_sphere();

        assert_eq!(centre, Vec3::new(0.0, -3.0, 0.0));
        assert!(radius >= 0.5);
    }

    #[test]
    fn magnitude_surface_omits_cells_with_undefined_corners() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(2));
        let colors = vec![Vec3::ONE; 4];
        let mut triangles = Vec::new();

        append_plane_surface(
            &mut triangles,
            lattice,
            &[
                SampleValidity::Exact,
                SampleValidity::Exact,
                SampleValidity::Undefined(UndefinedReason::InsideSourceRadius),
                SampleValidity::Exact,
            ],
            &colors,
            Vec3::ZERO,
            2,
        );

        assert!(triangles.is_empty());
    }

    #[test]
    fn a_valid_plane_cell_becomes_two_coloured_triangles() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(2));
        let colors = vec![Vec3::new(0.2, 0.4, 0.8); 4];
        let mut triangles = Vec::new();

        append_plane_surface(
            &mut triangles,
            lattice,
            &[SampleValidity::Exact; 4],
            &colors,
            Vec3::ZERO,
            2,
        );

        assert_eq!(triangles.len(), 6);
        assert!(triangles.iter().all(|vertex| vertex.position.is_finite()));
        assert!(triangles.iter().all(|vertex| vertex.color.is_finite()));
    }

    #[test]
    fn magnitude_density_reduces_only_the_draw_mesh() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(9));
        let colors = vec![Vec3::ONE; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let mut triangles = Vec::new();

        append_plane_surface(&mut triangles, lattice, &validity, &colors, Vec3::ZERO, 3);

        assert_eq!(triangles.len(), 4 * 6);
    }

    #[test]
    fn vector_density_places_glyphs_uniformly_when_it_does_not_divide_the_snapshot() {
        let lattice = PlaneLattice::new(
            DVec3::new(-4.0, -4.0, 0.0),
            DVec3::new(0.25, 0.0, 0.0),
            DVec3::new(0.0, 0.25, 0.0),
            UVec2::splat(33),
        );
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let mut lines = Vec::new();

        append_plane_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            Vec3::ZERO,
            25,
        );

        let origins: Vec<_> = lines
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();
        assert_eq!(origins.len(), 25 * 25);
        let expected_step = 8.0 / 24.0;
        for pair in origins[..25].windows(2) {
            assert!((pair[1].x - pair[0].x - expected_step).abs() < 1.0e-5);
        }
        assert!((origins[0].x + 4.0).abs() < 1.0e-5);
        assert!((origins[24].x - 4.0).abs() < 1.0e-5);
    }

    #[test]
    fn vector_density_can_be_zero_or_exceed_the_snapshot_density() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(5));
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let mut lines = Vec::new();

        append_plane_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            Vec3::ZERO,
            0,
        );
        assert!(lines.is_empty());

        append_plane_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            Vec3::ZERO,
            8,
        );
        assert_eq!(lines.len() / 6, 8 * 8);
    }
}
