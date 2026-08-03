use glam::{Mat4, Vec2, Vec3, Vec4};

/// Just under 90°, so the orbit camera never reaches a pole where its up-vector
/// becomes degenerate.
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.017_453_3;
const MIN_DISTANCE: f32 = 0.05;
const MAX_DISTANCE: f32 = 100_000.0;

/// The region of the surface the 3D scene occupies, in physical pixels.
///
/// There is deliberately only one viewport type, and it is physical. Deriving
/// the render aspect ratio from rounded pixels while deriving the picking aspect
/// ratio from unrounded logical points casts rays through a slightly different
/// frustum than the one that produced the image — a mismatch that grows with
/// display scaling and is invisible until a click lands on the wrong object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        }
    }
}

impl Viewport {
    /// Convert a logical rectangle and scale factor into whole pixels, clamped
    /// to the surface.
    ///
    /// The rounding goes *inward* — up for the near edges, down for the far
    /// ones — because this rectangle becomes the scissor for the 3D pass, and
    /// the panels are drawn over that pass afterwards. egui lays panels out in
    /// logical points, so a panel edge lands mid-pixel at essentially every
    /// scale factor, including 1.0. Rounding outward hands the scene the pixel
    /// column the panel edge sits in; egui then feathers that edge over it and
    /// the scene shows through the inspector's border as a bright seam.
    /// Rounding inward gives that column to the panel instead, and the most
    /// that costs is a sub-pixel sliver of scene along an edge, against a clear
    /// colour that is the scene's own background.
    pub fn from_logical(min: Vec2, size: Vec2, pixels_per_point: f32, surface: (u32, u32)) -> Self {
        let scale = pixels_per_point.max(0.01);
        let (surface_width, surface_height) = surface;
        // Leaving a pixel of headroom keeps `x + width` inside the surface even
        // when the panels have squeezed the scene to nothing, which a narrow
        // enough window can do. A scissor past the attachment is a validation
        // error, so the degenerate case has to stay in bounds rather than rely
        // on never happening.
        let min_x = (min.x * scale)
            .ceil()
            .clamp(0.0, surface_width.saturating_sub(1) as f32) as u32;
        let min_y = (min.y * scale)
            .ceil()
            .clamp(0.0, surface_height.saturating_sub(1) as f32) as u32;
        let max_x = ((min.x + size.x) * scale)
            .floor()
            .clamp(min_x as f32, surface_width as f32) as u32;
        let max_y = ((min.y + size.y) * scale)
            .floor()
            .clamp(min_y as f32, surface_height as f32) as u32;

        Self {
            x: min_x,
            y: min_y,
            width: (max_x - min_x).max(1),
            height: (max_y - min_y).max(1),
        }
    }

    /// Where a logical pointer position lands in this viewport's pixel space.
    pub fn pointer_to_physical(pointer: Vec2, pixels_per_point: f32) -> Vec2 {
        pointer * pixels_per_point.max(0.01)
    }

    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.x as f32
            && point.y >= self.y as f32
            && point.x < (self.x + self.width) as f32
            && point.y < (self.y + self.height) as f32
    }

    pub fn aspect_ratio(self) -> f32 {
        (self.width as f32 / self.height.max(1) as f32).max(0.01)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisView {
    PositiveX,
    PositiveY,
    PositiveZ,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
}

impl Ray {
    /// Distance along the ray to the nearest intersection with an axis-aligned
    /// box, if the box is not entirely behind the origin.
    pub fn hit_aabb(self, min: Vec3, max: Vec3) -> Option<f32> {
        let mut t_min = 0.0_f32;
        let mut t_max = f32::INFINITY;

        for axis in 0..3 {
            let origin = self.origin[axis];
            let direction = self.direction[axis];

            if direction.abs() < 1.0e-6 {
                if origin < min[axis] || origin > max[axis] {
                    return None;
                }
                continue;
            }

            let inverse = direction.recip();
            let mut near = (min[axis] - origin) * inverse;
            let mut far = (max[axis] - origin) * inverse;
            if near > far {
                std::mem::swap(&mut near, &mut far);
            }

            t_min = t_min.max(near);
            t_max = t_max.min(far);
            if t_min > t_max {
                return None;
            }
        }

        (t_max >= 0.0).then_some(t_min)
    }

    pub fn intersects_aabb(self, min: Vec3, max: Vec3) -> bool {
        self.hit_aabb(min, max).is_some()
    }

    pub fn hit_sphere(self, centre: Vec3, radius: f32) -> Option<f32> {
        let offset = self.origin - centre;
        let a = self.direction.length_squared();
        if a <= f32::EPSILON {
            return None;
        }
        let b = offset.dot(self.direction);
        let c = offset.length_squared() - radius * radius;
        let discriminant = b * b - a * c;
        if discriminant < 0.0 {
            return None;
        }
        let root = discriminant.sqrt();
        let near = (-b - root) / a;
        let far = (-b + root) / a;
        if near >= 0.0 {
            Some(near)
        } else if far >= 0.0 {
            Some(far)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
pub struct OrbitCamera {
    target: Vec3,
    distance: f32,
    yaw: f32,
    pitch: f32,
    vertical_fov: f32,
    near: f32,
    far: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 0.0, 0.6),
            distance: 12.0,
            yaw: -45.0_f32.to_radians(),
            pitch: 32.0_f32.to_radians(),
            vertical_fov: 45.0_f32.to_radians(),
            near: 0.01,
            far: 10_000.0,
        }
    }
}

impl OrbitCamera {
    pub fn target(&self) -> Vec3 {
        self.target
    }

    pub fn distance(&self) -> f32 {
        self.distance
    }

    pub fn eye(&self) -> Vec3 {
        let cos_pitch = self.pitch.cos();
        let offset = Vec3::new(
            cos_pitch * self.yaw.cos(),
            cos_pitch * self.yaw.sin(),
            self.pitch.sin(),
        );
        self.target + offset * self.distance
    }

    pub fn view_projection(&self, aspect_ratio: f32) -> Mat4 {
        // `directx` is glam's name for the WebGPU clip convention — Z in [0, 1],
        // Y-up — which is the one wgpu expects. The `opengl` variant's [-1, 1]
        // depth range would halve the usable precision of the depth buffer and
        // clip everything in front of the camera's midpoint.
        let projection = glam::camera::rh::proj::directx::perspective(
            self.vertical_fov,
            aspect_ratio.max(0.01),
            self.near,
            self.far,
        );
        projection * glam::camera::rh::view::look_at_mat4(self.eye(), self.target, Vec3::Z)
    }

    pub fn orbit(&mut self, pointer_delta: Vec2) {
        self.yaw -= pointer_delta.x * 0.006;
        self.pitch = (self.pitch + pointer_delta.y * 0.006).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    pub fn pan(&mut self, pointer_delta: Vec2, viewport_height: f32) {
        self.target -= self.screen_delta_to_world(pointer_delta, viewport_height);
    }

    /// Convert a pointer delta into a translation parallel to the view plane.
    /// Used by the lightweight object translation gizmo as well as camera pan.
    pub fn screen_delta_to_world(&self, pointer_delta: Vec2, viewport_height: f32) -> Vec3 {
        let forward = (self.target - self.eye()).normalize();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let world_units_per_point =
            2.0 * self.distance * (self.vertical_fov * 0.5).tan() / viewport_height.max(1.0);

        (right * pointer_delta.x - up * pointer_delta.y) * world_units_per_point
    }

    pub fn dolly(&mut self, scroll_delta: f32) {
        self.distance =
            (self.distance * (-scroll_delta * 0.002).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Frame a bounding sphere.
    pub fn focus(&mut self, centre: Vec3, radius: f32) {
        self.target = centre;
        self.distance = (radius.max(0.01) / (self.vertical_fov * 0.5).tan() * 1.5)
            .clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    pub fn set_axis_view(&mut self, view: AxisView) {
        match view {
            AxisView::PositiveX => {
                self.yaw = 0.0;
                self.pitch = 0.0;
            }
            AxisView::PositiveY => {
                self.yaw = std::f32::consts::FRAC_PI_2;
                self.pitch = 0.0;
            }
            AxisView::PositiveZ => {
                self.yaw = -std::f32::consts::FRAC_PI_2;
                self.pitch = PITCH_LIMIT;
            }
        }
    }

    /// A picking ray for a pointer position in the viewport's pixel space.
    ///
    /// Uses the aspect ratio the scene was rendered with, because it is handed
    /// the same `Viewport` value the renderer was.
    pub fn ray_from_viewport(&self, pointer: Vec2, viewport: Viewport) -> Option<Ray> {
        if !viewport.contains(pointer) {
            return None;
        }

        let x = ((pointer.x - viewport.x as f32) / viewport.width as f32) * 2.0 - 1.0;
        let y = 1.0 - ((pointer.y - viewport.y as f32) / viewport.height as f32) * 2.0;
        let inverse = self.view_projection(viewport.aspect_ratio()).inverse();
        if !inverse.is_finite() {
            return None;
        }

        let near = unproject(inverse, Vec3::new(x, y, 0.0));
        let far = unproject(inverse, Vec3::new(x, y, 1.0));
        let direction = (far - near).normalize_or_zero();
        if direction == Vec3::ZERO {
            return None;
        }

        Some(Ray {
            origin: near,
            direction,
        })
    }
}

fn unproject(inverse_view_projection: Mat4, point: Vec3) -> Vec3 {
    let world = inverse_view_projection * Vec4::new(point.x, point.y, point.z, 1.0);
    world.truncate() / world.w
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Viewport = Viewport {
        x: 0,
        y: 0,
        width: 800,
        height: 600,
    };

    #[test]
    fn centre_ray_hits_the_camera_target() {
        let camera = OrbitCamera::default();
        let ray = camera
            .ray_from_viewport(Vec2::new(400.0, 300.0), VIEWPORT)
            .expect("centre is inside viewport");

        let to_target = (camera.target() - ray.origin).normalize();
        assert!(ray.direction.dot(to_target) > 0.999);
    }

    #[test]
    fn ray_aabb_intersection_handles_parallel_axes() {
        let ray = Ray {
            origin: Vec3::new(0.0, -4.0, 0.5),
            direction: Vec3::Y,
        };

        assert!(ray.intersects_aabb(Vec3::ZERO, Vec3::ONE));
        assert!(!ray.intersects_aabb(Vec3::new(2.0, 0.0, 0.0), Vec3::new(3.0, 1.0, 1.0)));
    }

    #[test]
    fn aabb_hit_reports_the_near_distance_so_the_closest_object_wins() {
        let ray = Ray {
            origin: Vec3::new(0.0, -10.0, 0.0),
            direction: Vec3::Y,
        };

        let near = ray
            .hit_aabb(Vec3::new(-1.0, -6.0, -1.0), Vec3::new(1.0, -4.0, 1.0))
            .unwrap();
        let far = ray
            .hit_aabb(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0))
            .unwrap();

        assert!(near < far);
    }

    #[test]
    fn sphere_hit_supports_scaled_non_unit_rays_without_false_positives() {
        let hit = Ray {
            origin: Vec3::new(2.0, 0.0, 0.0),
            direction: Vec3::new(-4.0, 0.0, 0.0),
        };
        let miss = Ray {
            origin: Vec3::new(2.0, 2.0, 0.0),
            direction: Vec3::new(-4.0, 0.0, 0.0),
        };

        assert_eq!(hit.hit_sphere(Vec3::ZERO, 1.0), Some(0.25));
        assert_eq!(miss.hit_sphere(Vec3::ZERO, 1.0), None);
    }

    #[test]
    fn dolly_never_crosses_the_target() {
        let mut camera = OrbitCamera::default();
        camera.dolly(1_000_000.0);

        assert!(camera.distance() >= MIN_DISTANCE);
    }

    #[test]
    fn pan_moves_target_without_rotating_the_view() {
        let mut camera = OrbitCamera::default();
        let original_target = camera.target();
        let original_eye_offset = camera.eye() - camera.target();

        camera.pan(Vec2::new(40.0, -20.0), 600.0);

        assert_ne!(camera.target(), original_target);
        assert!(
            ((camera.eye() - camera.target()) - original_eye_offset).length() < 1.0e-5,
            "pan should translate the camera without rotating it"
        );
    }

    #[test]
    fn top_view_produces_a_finite_matrix() {
        let mut camera = OrbitCamera::default();
        camera.set_axis_view(AxisView::PositiveZ);

        assert!(camera.view_projection(16.0 / 9.0).is_finite());
    }

    #[test]
    fn physical_viewport_is_clamped_to_surface() {
        let viewport = Viewport::from_logical(
            Vec2::new(100.0, 50.0),
            Vec2::new(1_000.0, 900.0),
            2.0,
            (1_600, 1_200),
        );

        assert_eq!(viewport.x, 200);
        assert_eq!(viewport.y, 100);
        assert_eq!(viewport.width, 1_400);
        assert_eq!(viewport.height, 1_100);
    }

    /// The panels are painted over the 3D pass, so a scissor that rounds
    /// outward leaves scene pixels beneath a panel's feathered edge and they
    /// show through as a seam. Panel edges are fractional in physical pixels at
    /// essentially every scale factor, so this has to hold generally.
    #[test]
    fn a_fractional_edge_rounds_away_from_the_neighbouring_panel() {
        let surface = (1_920, 1_080);
        // Deliberately awkward: every edge lands mid-pixel once scaled.
        let min = Vec2::new(180.3, 24.7);
        let size = Vec2::new(843.4, 611.2);

        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let viewport = Viewport::from_logical(min, size, scale, surface);

            assert!(
                viewport.x as f32 >= min.x * scale,
                "scale {scale}: left edge {} reaches into the panel at {}",
                viewport.x,
                min.x * scale
            );
            assert!(
                viewport.y as f32 >= min.y * scale,
                "scale {scale}: top edge {} reaches into the panel at {}",
                viewport.y,
                min.y * scale
            );
            assert!(
                (viewport.x + viewport.width) as f32 <= (min.x + size.x) * scale,
                "scale {scale}: right edge {} reaches into the panel at {}",
                viewport.x + viewport.width,
                (min.x + size.x) * scale
            );
            assert!(
                (viewport.y + viewport.height) as f32 <= (min.y + size.y) * scale,
                "scale {scale}: bottom edge {} reaches into the panel at {}",
                viewport.y + viewport.height,
                (min.y + size.y) * scale
            );
        }
    }

    /// Panels have minimum widths that a narrow enough window cannot satisfy,
    /// which leaves the scene no room at all. A scissor outside the attachment
    /// is a validation error, so the degenerate case still has to be in bounds.
    #[test]
    fn a_scene_squeezed_to_nothing_stays_inside_the_surface() {
        let surface = (400, 300);
        let viewport =
            Viewport::from_logical(Vec2::new(400.0, 300.0), Vec2::new(0.0, 0.0), 1.0, surface);

        assert!(viewport.x + viewport.width <= surface.0);
        assert!(viewport.y + viewport.height <= surface.1);
    }

    #[test]
    fn picking_and_rendering_share_one_aspect_ratio_under_scaling() {
        // A fractional scale factor is where a logical/physical split shows up:
        // the rendered aspect is rounded to whole pixels and a logical picking
        // aspect would not be.
        let viewport = Viewport::from_logical(
            Vec2::new(180.0, 24.0),
            Vec2::new(843.0, 611.0),
            1.5,
            (1_920, 1_080),
        );
        let camera = OrbitCamera::default();

        let centre = Vec2::new(
            viewport.x as f32 + viewport.width as f32 * 0.5,
            viewport.y as f32 + viewport.height as f32 * 0.5,
        );
        let ray = camera.ray_from_viewport(centre, viewport).unwrap();

        // The ray through the centre of the drawn region points at the target
        // the drawn projection was centred on.
        let to_target = (camera.target() - ray.origin).normalize();
        assert!(ray.direction.dot(to_target) > 0.9999);
    }

    #[test]
    fn pointer_outside_the_viewport_produces_no_ray() {
        let camera = OrbitCamera::default();

        assert!(
            camera
                .ray_from_viewport(Vec2::new(900.0, 300.0), VIEWPORT)
                .is_none()
        );
    }
}
