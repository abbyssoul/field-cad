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

/// A standard orthographic-style viewpoint, looking down one world axis.
///
/// Both signs of each axis are offered because "front" and "back" of an
/// arrangement are different questions, and a user who can only reach three of
/// the six faces has to orbit past the geometry to see the other side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisView {
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

impl AxisView {
    /// The six views in the order a view panel should present them.
    pub const ALL: [Self; 6] = [
        Self::PositiveX,
        Self::NegativeX,
        Self::PositiveY,
        Self::NegativeY,
        Self::PositiveZ,
        Self::NegativeZ,
    ];

    /// The short button label, naming the axis the camera looks *along*.
    pub const fn label(self) -> &'static str {
        match self {
            Self::PositiveX => "+X",
            Self::NegativeX => "−X",
            Self::PositiveY => "+Y",
            Self::NegativeY => "−Y",
            Self::PositiveZ => "+Z",
            Self::NegativeZ => "−Z",
        }
    }

    /// What the user will be looking at, in the language of a drawing.
    pub const fn description(self) -> &'static str {
        match self {
            Self::PositiveX => "Look along +X (YZ plane, right)",
            Self::NegativeX => "Look along −X (YZ plane, left)",
            Self::PositiveY => "Look along +Y (XZ plane, back)",
            Self::NegativeY => "Look along −Y (XZ plane, front)",
            Self::PositiveZ => "Look along +Z (XY plane, top)",
            Self::NegativeZ => "Look along −Z (XY plane, bottom)",
        }
    }
}

/// How the camera maps the scene onto the screen.
///
/// Both are wanted for different reading tasks and neither replaces the other.
/// A perspective view shows depth, which is how an arrangement in space is
/// understood. An orthographic one removes it, so equal lengths measure equal on
/// screen wherever they are — which is what makes a field's decay comparable
/// across a slice, and what makes an axis view an engineering drawing rather
/// than a photograph of one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Projection {
    #[default]
    Perspective,
    Orthographic,
}

impl Projection {
    pub const ALL: [Self; 2] = [Self::Perspective, Self::Orthographic];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Perspective => "Perspective",
            Self::Orthographic => "Orthographic",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Perspective => {
                "Distant things are smaller. Shows depth, so the arrangement of a scene in \
                 space reads directly."
            }
            Self::Orthographic => {
                "No foreshortening. Equal lengths measure equal anywhere on screen, so values \
                 across a slice are comparable and an axis view reads as a drawing."
            }
        }
    }
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
    projection: Projection,
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
            projection: Projection::default(),
        }
    }
}

impl OrbitCamera {
    pub fn target(&self) -> Vec3 {
        self.target
    }

    /// Move the orbit's centre without touching distance, yaw, or pitch —
    /// unlike [`Self::focus`], which reframes all three from a bounding
    /// radius. This is the follow-camera primitive: retargeting every frame
    /// to a moving object's current position, with the eye offset held
    /// fixed, is what makes the object sit still on screen while the world
    /// moves around it.
    pub fn set_target(&mut self, target: Vec3) {
        self.target = target;
    }

    pub fn distance(&self) -> f32 {
        self.distance
    }

    pub fn set_distance(&mut self, distance: f32) {
        self.distance = distance.clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    pub fn set_yaw(&mut self, yaw: f32) {
        self.yaw = yaw;
    }

    pub fn pitch(&self) -> f32 {
        self.pitch
    }

    pub fn set_pitch(&mut self, pitch: f32) {
        self.pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
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

    pub const fn projection(&self) -> Projection {
        self.projection
    }

    /// Switch projection without disturbing the framing.
    ///
    /// The two share the orbit, the target, and the distance, and the
    /// orthographic extent is derived from that same distance, so a scene keeps
    /// its size and position on screen across the change. Toggling to compare
    /// the two readings of one arrangement is the point; having to re-frame
    /// afterwards would defeat it.
    pub const fn set_projection(&mut self, projection: Projection) {
        self.projection = projection;
    }

    /// Half the world-space height the viewport spans at the target distance.
    ///
    /// The single quantity both projections are built from. Deriving the
    /// orthographic extent from it rather than storing a separate zoom is what
    /// lets dolly, focus, pan, and the framing itself mean the same thing in
    /// either mode — including `screen_delta_to_world`, which needs no branch of
    /// its own because the answer is identical.
    fn half_extent_at_target(&self) -> f32 {
        self.distance * (self.vertical_fov * 0.5).tan()
    }

    pub fn view_projection(&self, aspect_ratio: f32) -> Mat4 {
        // `directx` is glam's name for the WebGPU clip convention — Z in [0, 1],
        // Y-up — which is the one wgpu expects. The `opengl` variant's [-1, 1]
        // depth range would halve the usable precision of the depth buffer and
        // clip everything in front of the camera's midpoint.
        let aspect_ratio = aspect_ratio.max(0.01);
        let projection = match self.projection {
            Projection::Perspective => glam::camera::rh::proj::directx::perspective(
                self.vertical_fov,
                aspect_ratio,
                self.near,
                self.far,
            ),
            Projection::Orthographic => {
                let half_height = self.half_extent_at_target();
                let half_width = half_height * aspect_ratio;
                // The depth range is symmetric about the eye rather than
                // starting just in front of it. Without foreshortening, dollying
                // is a zoom and not an approach, so a near plane at the eye
                // would silently cut the scene in half as the user zoomed in —
                // clipping geometry that has not moved and is not in front of
                // anything.
                glam::camera::rh::proj::directx::orthographic(
                    -half_width,
                    half_width,
                    -half_height,
                    half_height,
                    -self.far,
                    self.far,
                )
            }
        };
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
        // Deliberately not branched on projection: an orthographic view spans
        // exactly what the perspective one spans at the target distance, so a
        // pointer delta is worth the same in either.
        let world_units_per_point = 2.0 * self.half_extent_at_target() / viewport_height.max(1.0);

        (right * pointer_delta.x - up * pointer_delta.y) * world_units_per_point
    }

    /// World units spanned by one screen pixel at `point`'s depth.
    ///
    /// The single conversion a screen-space-constant gizmo needs: multiply this
    /// by a desired pixel size to get a world-space length that will occupy
    /// exactly that many pixels, however far away `point` is and however the
    /// scene is scaled. Perspective depth is measured along the view axis (not
    /// straight-line distance to the eye) so an off-axis point is not
    /// foreshortened relative to one on it; orthographic is depth-independent,
    /// like `screen_delta_to_world`.
    pub fn world_units_per_pixel(&self, point: Vec3, viewport_height: f32) -> f32 {
        let extent = match self.projection {
            Projection::Perspective => {
                let forward = (self.target - self.eye()).normalize_or_zero();
                let depth = (point - self.eye()).dot(forward).max(self.near);
                depth * (self.vertical_fov * 0.5).tan()
            }
            Projection::Orthographic => self.half_extent_at_target(),
        };
        2.0 * extent / viewport_height.max(1.0)
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
        // The top and bottom views clamp to `PITCH_LIMIT` rather than a true
        // pole: looking exactly along Z would make the up vector parallel to the
        // view direction and the look-at matrix degenerate.
        (self.yaw, self.pitch) = match view {
            AxisView::PositiveX => (0.0, 0.0),
            AxisView::NegativeX => (std::f32::consts::PI, 0.0),
            AxisView::PositiveY => (std::f32::consts::FRAC_PI_2, 0.0),
            AxisView::NegativeY => (-std::f32::consts::FRAC_PI_2, 0.0),
            AxisView::PositiveZ => (-std::f32::consts::FRAC_PI_2, PITCH_LIMIT),
            AxisView::NegativeZ => (-std::f32::consts::FRAC_PI_2, -PITCH_LIMIT),
        };
    }

    /// Return to the framing a new session opens with, keeping nothing of the
    /// current orbit. A user who has lost the scene off-screen needs one control
    /// that is guaranteed to bring it back.
    ///
    /// Projection survives, because it is not framing: a user who has chosen to
    /// read the scene without foreshortening has not asked to stop.
    pub fn reset(&mut self) {
        *self = Self {
            projection: self.projection,
            ..Self::default()
        };
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

    /// Picking is built from the inverse of whatever matrix drew the frame, so
    /// it follows the projection without knowing which one it is. What changes
    /// is the shape of the answer: perspective rays fan out from one eye,
    /// orthographic rays are parallel and start where the pointer is.
    #[test]
    fn picking_follows_the_projection_it_was_drawn_with() {
        let mut camera = OrbitCamera::default();
        camera.set_projection(Projection::Orthographic);

        let centre = camera
            .ray_from_viewport(Vec2::new(400.0, 300.0), VIEWPORT)
            .expect("centre is inside viewport");
        let corner = camera
            .ray_from_viewport(Vec2::new(120.0, 90.0), VIEWPORT)
            .expect("corner is inside viewport");

        // Parallel, and still pointing at the scene.
        assert!(
            centre.direction.dot(corner.direction) > 0.9999,
            "orthographic rays must not fan out"
        );
        assert!(
            centre
                .direction
                .dot((camera.target() - centre.origin).normalize())
                > 0.999
        );
        // ...from different places, which is what makes them able to hit
        // different things.
        assert!(centre.origin.distance(corner.origin) > 0.1);

        // The perspective camera at the same framing does fan out.
        camera.set_projection(Projection::Perspective);
        let centre = camera
            .ray_from_viewport(Vec2::new(400.0, 300.0), VIEWPORT)
            .unwrap();
        let corner = camera
            .ray_from_viewport(Vec2::new(120.0, 90.0), VIEWPORT)
            .unwrap();
        assert!(centre.direction.dot(corner.direction) < 0.99);
    }

    /// Switching projection is for comparing two readings of one arrangement, so
    /// it must not also move the scene. Both spans are built from the same
    /// distance, so what fills the viewport at the target stays put.
    #[test]
    fn switching_projection_keeps_the_framing() {
        let mut camera = OrbitCamera::default();
        camera.focus(Vec3::new(1.0, -2.0, 0.5), 3.0);
        let eye = camera.eye();
        let target = camera.target();

        // A point one target-plane half-height above the target sits at the top
        // of the viewport in either projection.
        let up = (camera.target() - camera.eye())
            .normalize()
            .cross(Vec3::Z)
            .normalize()
            .cross((camera.target() - camera.eye()).normalize())
            .normalize();
        let edge = target + up * camera.half_extent_at_target();
        let ndc_y = |camera: &OrbitCamera| {
            let clip = camera.view_projection(VIEWPORT.aspect_ratio()) * edge.extend(1.0);
            clip.y / clip.w
        };

        let perspective = ndc_y(&camera);
        camera.set_projection(Projection::Orthographic);
        let orthographic = ndc_y(&camera);

        assert_eq!(camera.eye(), eye, "the viewpoint must not move");
        assert_eq!(camera.target(), target);
        assert!(
            (perspective - 1.0).abs() < 1.0e-4,
            "expected the top edge, got {perspective}"
        );
        assert!(
            (orthographic - perspective).abs() < 1.0e-4,
            "the same point left the viewport edge: {perspective} then {orthographic}"
        );
    }

    /// Without foreshortening, dollying is a zoom rather than an approach, so it
    /// must not clip away the half of the scene it has zoomed past.
    #[test]
    fn zooming_an_orthographic_view_does_not_clip_the_scene_in_half() {
        let mut camera = OrbitCamera::default();
        camera.set_projection(Projection::Orthographic);
        camera.dolly(4_000.0);
        assert!(camera.distance() < 1.0, "the camera should be zoomed in");

        // A point well behind the eye, which a near plane at the eye would cut.
        let behind = camera.eye() + (camera.eye() - camera.target()).normalize() * 50.0;
        let clip = camera.view_projection(VIEWPORT.aspect_ratio()) * behind.extend(1.0);
        let depth = clip.z / clip.w;

        assert!(
            (0.0..=1.0).contains(&depth),
            "geometry behind the zoomed viewpoint was clipped away: depth {depth}"
        );
    }

    #[test]
    fn a_scene_reset_reframes_without_changing_how_the_scene_is_projected() {
        let mut camera = OrbitCamera::default();
        camera.set_projection(Projection::Orthographic);
        camera.orbit(Vec2::new(120.0, 65.0));

        camera.reset();

        assert_eq!(camera.projection(), Projection::Orthographic);
        assert_eq!(camera.eye(), {
            let mut default = OrbitCamera::default();
            default.set_projection(Projection::Orthographic);
            default.eye()
        });
    }

    #[test]
    fn every_projection_produces_a_finite_matrix_from_every_axis_view() {
        for projection in Projection::ALL {
            for view in AxisView::ALL {
                let mut camera = OrbitCamera::default();
                camera.set_projection(projection);
                camera.set_axis_view(view);

                assert!(
                    camera.view_projection(16.0 / 9.0).is_finite(),
                    "{} in {} produced a degenerate matrix",
                    view.label(),
                    projection.label()
                );
            }
        }
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

    /// The whole point of the conversion: a fixed pixel size must map to a
    /// world length that grows with a *point's own depth* in perspective
    /// (otherwise a gizmo on a distant object would shrink relative to one on
    /// a near object at the same camera state), and must be the same for
    /// every point in orthographic, where there is no foreshortening to
    /// account for. (The camera's overall zoom/`distance` naturally scales
    /// both projections together — that is `dolly`, not what this checks.)
    #[test]
    fn world_units_per_pixel_accounts_for_perspective_depth_but_not_in_ortho() {
        let mut camera = OrbitCamera::default();
        camera.focus(Vec3::ZERO, 1.0);
        let forward = (camera.target() - camera.eye()).normalize();
        let near_point = camera.eye() + forward * 2.0;
        let far_point = camera.eye() + forward * 20.0;

        let near_units = camera.world_units_per_pixel(near_point, 600.0);
        let far_units = camera.world_units_per_pixel(far_point, 600.0);
        assert!(
            far_units > near_units * 9.0,
            "expected roughly 10x growth with depth: {near_units} near, {far_units} far"
        );

        camera.set_projection(Projection::Orthographic);
        let near_ortho = camera.world_units_per_pixel(near_point, 600.0);
        let far_ortho = camera.world_units_per_pixel(far_point, 600.0);
        assert!(
            (near_ortho - far_ortho).abs() < 1.0e-6,
            "orthographic must not depend on a point's own depth: {near_ortho} vs {far_ortho}"
        );
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

    /// Every standard viewpoint has to be usable, including the two that look
    /// along the up axis and would make the look-at matrix degenerate if they
    /// reached the pole exactly.
    #[test]
    fn all_six_axis_views_are_distinct_and_finite() {
        let mut eyes: Vec<Vec3> = Vec::new();
        for view in AxisView::ALL {
            let mut camera = OrbitCamera::default();
            camera.set_axis_view(view);

            assert!(
                camera.view_projection(16.0 / 9.0).is_finite(),
                "{} produced a degenerate view matrix",
                view.label()
            );
            let eye = camera.eye();
            assert!(
                eyes.iter().all(|seen| (*seen - eye).length() > 1.0e-3),
                "{} put the camera where another view already was",
                view.label()
            );
            eyes.push(eye);
        }
        assert_eq!(eyes.len(), 6);
    }

    /// Opposite views must sit on opposite sides of the target, or the pair of
    /// buttons is not showing the user the front and the back of the scene.
    #[test]
    fn opposite_axis_views_look_from_opposite_sides() {
        for (one, other) in [
            (AxisView::PositiveX, AxisView::NegativeX),
            (AxisView::PositiveY, AxisView::NegativeY),
            (AxisView::PositiveZ, AxisView::NegativeZ),
        ] {
            let mut first = OrbitCamera::default();
            first.set_axis_view(one);
            let mut second = OrbitCamera::default();
            second.set_axis_view(other);

            let a = (first.eye() - first.target()).normalize();
            let b = (second.eye() - second.target()).normalize();
            assert!(
                a.dot(b) < -0.9,
                "{} and {} are not opposed (dot {})",
                one.label(),
                other.label(),
                a.dot(b)
            );
        }
    }

    #[test]
    fn reset_restores_the_default_framing_from_any_orbit() {
        let mut camera = OrbitCamera::default();
        camera.orbit(Vec2::new(120.0, 65.0));
        camera.pan(Vec2::new(40.0, -20.0), 600.0);
        camera.focus(Vec3::new(9.0, -4.0, 2.0), 3.0);
        assert_ne!(camera.target(), OrbitCamera::default().target());

        camera.reset();

        assert_eq!(camera.target(), OrbitCamera::default().target());
        assert_eq!(camera.distance(), OrbitCamera::default().distance());
        assert_eq!(camera.eye(), OrbitCamera::default().eye());
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

    /// The literal bug `SceneScale` fixes: `MIN_DISTANCE`/`MAX_DISTANCE` are
    /// fixed constants with no notion of a scene's physical scale. A
    /// nanometre-scale object's radius, cast straight into `focus()` without
    /// going through `SceneScale::to_render`, collapses to ~1e-9 and clamps
    /// to `MIN_DISTANCE` — the camera cannot approach any closer, regardless
    /// of how far a user scrolls in. Converting the same radius through the
    /// render-space boundary first brings it to unit magnitude, and `focus()`
    /// lands well clear of the clamp.
    #[test]
    fn nanometre_scale_focus_does_not_collapse_to_the_minimum_distance() {
        let radius_metres = 2.0e-9;
        let mut camera = OrbitCamera::default();

        // The old, unscaled behaviour: a bare cast pins the camera to the
        // floor it can never dolly past.
        camera.focus(Vec3::ZERO, radius_metres as f32);
        assert_eq!(camera.distance(), MIN_DISTANCE);

        // Through `SceneScale::nanometre`, the same physical object renders
        // at unit magnitude, and `focus` places the camera comfortably
        // inside its distance window instead of pinned to the floor.
        let scale = fieldcad_core::SceneScale::nanometre();
        camera.focus(Vec3::ZERO, scale.to_render(radius_metres));
        assert!(
            camera.distance() > MIN_DISTANCE * 10.0,
            "expected the camera to clear the minimum-distance floor, got {}",
            camera.distance()
        );
        assert!(camera.distance() < MAX_DISTANCE);
    }

    /// The follow-camera primitive: retargeting must never perturb distance,
    /// yaw, or pitch, or a followed object would drift on screen as it moves
    /// rather than sitting still.
    #[test]
    fn set_target_moves_the_target_without_touching_the_rest_of_the_frame() {
        let mut camera = OrbitCamera::default();
        camera.orbit(Vec2::new(40.0, -20.0));
        camera.dolly(-500.0);
        let (distance, yaw, pitch) = (camera.distance(), camera.yaw(), camera.pitch());

        camera.set_target(Vec3::new(3.0, -1.0, 7.0));

        assert_eq!(camera.target(), Vec3::new(3.0, -1.0, 7.0));
        assert_eq!(camera.distance(), distance);
        assert_eq!(camera.yaw(), yaw);
        assert_eq!(camera.pitch(), pitch);
    }

    #[test]
    fn set_distance_and_set_pitch_clamp_to_the_same_bounds_as_dolly_and_orbit() {
        let mut camera = OrbitCamera::default();

        camera.set_distance(f32::MAX);
        assert_eq!(camera.distance(), MAX_DISTANCE);
        camera.set_distance(-5.0);
        assert_eq!(camera.distance(), MIN_DISTANCE);

        camera.set_pitch(std::f32::consts::PI);
        assert_eq!(camera.pitch(), PITCH_LIMIT);
        camera.set_pitch(-std::f32::consts::PI);
        assert_eq!(camera.pitch(), -PITCH_LIMIT);
    }

    #[test]
    fn set_yaw_is_unclamped_since_orbit_wraps_freely() {
        let mut camera = OrbitCamera::default();
        camera.set_yaw(12.5);
        assert_eq!(camera.yaw(), 12.5);
    }
}
