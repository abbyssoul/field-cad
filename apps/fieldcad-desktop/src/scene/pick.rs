//! Turning a pointer position into a scene selection.
//!
//! Rays are cast through the same `Viewport` the frame was rendered with, so a
//! click lands on the object the user actually sees.

use fieldcad_core::{ObjectId, SlicePlane, WorldSnapshot};
use glam::{Vec2, Vec3};

use super::{ObjectMesh, SceneSelection, SceneVisibility, instances};
use crate::camera::{OrbitCamera, Viewport};

/// The nearest visible authoring entity under a pointer.
pub fn pick_scene(
    world: &WorldSnapshot,
    show: SceneVisibility,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
) -> Option<SceneSelection> {
    let ray = camera.ray_from_viewport(pointer, viewport)?;
    let mut nearest = nearest_object_hit(world, show, ray)
        .map(|(distance, object)| (distance, SceneSelection::Object(object)));

    for plane in world
        .planes()
        .values()
        .filter(|plane| plane.visible && show.planes)
    {
        let Some(distance) = ray_plane_hit(ray, plane) else {
            continue;
        };
        if nearest.is_none_or(|(best, _)| distance < best) {
            nearest = Some((distance, SceneSelection::Plane(plane.id)));
        }
    }

    for probe in world
        .probes()
        .values()
        .filter(|probe| probe.visible && show.probes)
    {
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
    show: SceneVisibility,
    camera: &OrbitCamera,
    viewport: Viewport,
    pointer: Vec2,
) -> Option<ObjectId> {
    let ray = camera.ray_from_viewport(pointer, viewport)?;
    nearest_object_hit(world, show, ray).map(|(_, object)| object)
}

fn nearest_object_hit(
    world: &WorldSnapshot,
    show: SceneVisibility,
    ray: crate::camera::Ray,
) -> Option<(f32, ObjectId)> {
    let mut nearest = None;
    // `instances` is already empty when objects are hidden, so the hit-test
    // inherits the renderer's answer rather than deciding again.
    for instance in instances(world, None, show) {
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

pub(super) fn ray_plane_point(ray: crate::camera::Ray, origin: Vec3, normal: Vec3) -> Option<Vec3> {
    let denominator = ray.direction.dot(normal);
    if denominator.abs() < 1.0e-6 {
        return None;
    }
    let distance = (origin - ray.origin).dot(normal) / denominator;
    (distance >= 0.0).then_some(ray.origin + ray.direction * distance)
}

pub(super) fn project_to_viewport(
    camera: &OrbitCamera,
    viewport: Viewport,
    point: Vec3,
) -> Option<Vec2> {
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

pub(super) fn point_segment_distance(point: Vec2, start: Vec2, end: Vec2) -> f32 {
    let segment = end - start;
    let denominator = segment.length_squared();
    if denominator <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / denominator).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

pub(super) fn point_in_triangle(point: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
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
    use fieldcad_core::{ObjectShape, ObjectSpec, SlicePlaneSpec, Transform, World, WorldCommand};
    use glam::{DQuat, DVec3};

    use super::super::tests::world_with_two_boxes;
    use super::*;
    use crate::camera::AxisView;

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
        let picked = pick_scene(&snapshot, SceneVisibility::ALL, &camera, viewport, centre);

        // From +Y looking back at the origin, the box at y = +3 is nearer.
        assert_eq!(picked, Some(SceneSelection::Object(ObjectId::new(1))));
    }

    /// Hiding a class in the View window must make it unclickable too.
    ///
    /// It was previously only filtered out of rendering, so a user could select
    /// — and then drag — something that was not on screen.
    #[test]
    fn hidden_objects_are_not_selectable() {
        let world = world_with_two_boxes();
        let snapshot = world.snapshot();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let mut camera = OrbitCamera::default();
        camera.focus(Vec3::ZERO, 1.0);
        camera.set_axis_view(AxisView::PositiveY);
        let centre = Vec2::new(400.0, 300.0);

        let hidden = SceneVisibility {
            objects: false,
            ..SceneVisibility::ALL
        };

        assert!(
            pick_scene(&snapshot, SceneVisibility::ALL, &camera, viewport, centre).is_some(),
            "the fixture must be pickable when shown, or this proves nothing"
        );
        assert_eq!(
            pick_scene(&snapshot, hidden, &camera, viewport, centre),
            None
        );
        // The free-drag hit test is a separate entry point and was equally
        // affected.
        assert_eq!(
            pick_object(&snapshot, hidden, &camera, viewport, centre),
            None
        );
    }

    #[test]
    fn hidden_planes_are_not_selectable() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z)
                    .unwrap()
                    .with_half_extent(glam::DVec2::splat(2.0))
                    .unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let mut camera = OrbitCamera::default();
        camera.focus(Vec3::ZERO, 2.0);
        camera.set_axis_view(AxisView::PositiveZ);
        let centre = Vec2::new(400.0, 300.0);

        assert!(pick_scene(&snapshot, SceneVisibility::ALL, &camera, viewport, centre).is_some());
        assert_eq!(
            pick_scene(
                &snapshot,
                SceneVisibility {
                    planes: false,
                    ..SceneVisibility::ALL
                },
                &camera,
                viewport,
                centre
            ),
            None
        );
    }

    #[test]
    fn hidden_probes_are_not_selectable() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(fieldcad_core::ProbeSpec::at(
                "probe",
                DVec3::ZERO,
                Vec::new(),
            ))])
            .unwrap();
        let snapshot = world.snapshot();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let mut camera = OrbitCamera::default();
        camera.focus(Vec3::ZERO, 0.5);
        let centre = Vec2::new(400.0, 300.0);

        assert!(pick_scene(&snapshot, SceneVisibility::ALL, &camera, viewport, centre).is_some());
        assert_eq!(
            pick_scene(
                &snapshot,
                SceneVisibility {
                    probes: false,
                    ..SceneVisibility::ALL
                },
                &camera,
                viewport,
                centre
            ),
            None
        );
    }

    /// Hiding one class must not make the others unpickable.
    #[test]
    fn hiding_one_class_leaves_the_others_selectable() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("body")
                    .with_transform(Transform::at(DVec3::new(0.0, 0.0, 0.6)).unwrap())
                    .with_shape(ObjectShape::sphere(0.25).unwrap()),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let object = *snapshot.objects().keys().next().unwrap();
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };

        assert_eq!(
            pick_scene(
                &snapshot,
                SceneVisibility {
                    probes: false,
                    planes: false,
                    objects: true,
                },
                &OrbitCamera::default(),
                viewport,
                Vec2::new(400.0, 300.0),
            ),
            Some(SceneSelection::Object(object))
        );
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
            pick_scene(
                &world.snapshot(),
                SceneVisibility::ALL,
                &OrbitCamera::default(),
                viewport,
                corner
            ),
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
                SceneVisibility::ALL,
                &camera,
                viewport,
                Vec2::new(400.0, 300.0)
            ),
            Some(SceneSelection::Plane(plane))
        );
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
                SceneVisibility::ALL,
                &OrbitCamera::default(),
                viewport,
                Vec2::new(400.0, 300.0),
            ),
            Some(object)
        );
        assert_eq!(
            pick_object(
                &world.snapshot(),
                SceneVisibility::ALL,
                &OrbitCamera::default(),
                viewport,
                Vec2::new(2.0, 2.0),
            ),
            None
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
        let instance = instances(&snapshot, None, SceneVisibility::ALL)[0];

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
}
