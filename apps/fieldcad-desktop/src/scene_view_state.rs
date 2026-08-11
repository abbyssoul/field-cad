//! Bridges this app's live camera/view types and
//! `fieldcad_scene_document`'s plain, serializable mirrors of them (see that
//! crate's `view` module for why they're separate types rather than shared
//! ones) — `capture` for `WindowState::save_scene`, the `restore_*`
//! functions for `WindowState::replace_session`.

use std::collections::BTreeMap;

use fieldcad_core::{ChannelId, ObjectId};
use fieldcad_scene_document::{
    CameraProjection, CameraState, ChannelViewState, FieldLayerViewState, FlowLineDisplayState,
    GizmoDisplayState, PlaneVectorModeState, PlaneViewState, RegionViewState, SceneViewState,
    TrajectoryDisplayState, VectorDisplayState, ViewOptionsState,
};

use crate::camera::{OrbitCamera, Projection};
use crate::scene::{
    BoxLayerSettings, FieldLayerSettings, FlowLineDisplay, GizmoDisplay, PlaneLayerSettings,
    PlaneVectorMode, SphereLayerSettings, TrajectoryDisplay, VectorDisplay,
};
use crate::ui::{ChannelLayerSettings, ViewOptions};

pub fn capture(
    camera: &OrbitCamera,
    following: Option<ObjectId>,
    view: &ViewOptions,
    field_layers: &BTreeMap<ChannelId, ChannelLayerSettings>,
    object_trajectories: &BTreeMap<ObjectId, TrajectoryDisplay>,
) -> SceneViewState {
    SceneViewState {
        camera: Some(capture_camera(camera)),
        following,
        view_options: Some(capture_view_options(view)),
        channels: field_layers
            .iter()
            .map(|(id, settings)| (id.clone(), capture_channel(settings)))
            .collect(),
        objects: object_trajectories
            .iter()
            .map(|(id, display)| (*id, capture_trajectory(*display)))
            .collect(),
    }
}

fn capture_trajectory(display: TrajectoryDisplay) -> TrajectoryDisplayState {
    TrajectoryDisplayState {
        visible: display.visible,
        trail_seconds: display.trail_seconds,
        thickness_px: display.thickness_px,
        animated: display.animated,
        speed: display.speed,
    }
}

fn capture_camera(camera: &OrbitCamera) -> CameraState {
    let target = camera.target();
    CameraState {
        target: [target.x, target.y, target.z],
        distance: camera.distance(),
        yaw: camera.yaw(),
        pitch: camera.pitch(),
        projection: match camera.projection() {
            Projection::Perspective => CameraProjection::Perspective,
            Projection::Orthographic => CameraProjection::Orthographic,
        },
    }
}

fn capture_view_options(view: &ViewOptions) -> ViewOptionsState {
    ViewOptionsState {
        grid: view.grid,
        axes: view.axes,
        objects: view.objects,
        auxiliary_objects: view.auxiliary_objects,
        compute_bounds: view.compute_bounds,
        gizmo_display: capture_gizmo_display(view.gizmo_display),
        probes: view.probes,
        planes: view.planes,
        boxes: view.boxes,
        spheres: view.spheres,
    }
}

fn capture_gizmo_display(gizmo: GizmoDisplay) -> GizmoDisplayState {
    GizmoDisplayState {
        axis_length_px: gizmo.axis_length_px,
        axis_thickness_px: gizmo.axis_thickness_px,
        rotation_diameter_px: gizmo.rotation_diameter_px,
        rotation_thickness_px: gizmo.rotation_thickness_px,
    }
}

fn capture_vectors(vectors: VectorDisplay) -> VectorDisplayState {
    VectorDisplayState {
        visible: vectors.visible,
        density: vectors.density,
        scale: vectors.scale,
    }
}

fn capture_flow_lines(flow_lines: FlowLineDisplay) -> FlowLineDisplayState {
    FlowLineDisplayState {
        visible: flow_lines.visible,
        density: flow_lines.density,
        thickness_px: flow_lines.thickness_px,
        animated: flow_lines.animated,
        speed: flow_lines.speed,
    }
}

fn capture_whole_domain(whole_domain: FieldLayerSettings) -> FieldLayerViewState {
    FieldLayerViewState {
        vectors: capture_vectors(whole_domain.vectors),
        flow_lines: capture_flow_lines(whole_domain.flow_lines),
    }
}

fn capture_plane(plane: PlaneLayerSettings) -> PlaneViewState {
    PlaneViewState {
        visible: plane.visible,
        magnitude_visible: plane.magnitude_visible,
        magnitude_density: plane.magnitude_density,
        vectors: capture_vectors(plane.vectors),
        vector_mode: match plane.vector_mode {
            PlaneVectorMode::InPlane => PlaneVectorModeState::InPlane,
            PlaneVectorMode::Full3d => PlaneVectorModeState::Full3d,
        },
        flow_lines: capture_flow_lines(plane.flow_lines),
    }
}

fn capture_box(region: BoxLayerSettings) -> RegionViewState {
    RegionViewState {
        visible: region.visible,
        vectors: capture_vectors(region.vectors),
        flow_lines: capture_flow_lines(region.flow_lines),
    }
}

fn capture_sphere(region: SphereLayerSettings) -> RegionViewState {
    RegionViewState {
        visible: region.visible,
        vectors: capture_vectors(region.vectors),
        flow_lines: capture_flow_lines(region.flow_lines),
    }
}

fn capture_channel(settings: &ChannelLayerSettings) -> ChannelViewState {
    ChannelViewState {
        visible: settings.visible,
        whole_domain: capture_whole_domain(settings.whole_domain),
        planes: settings
            .planes
            .iter()
            .map(|(id, plane)| (*id, capture_plane(*plane)))
            .collect(),
        boxes: settings
            .boxes
            .iter()
            .map(|(id, region)| (*id, capture_box(*region)))
            .collect(),
        spheres: settings
            .spheres
            .iter()
            .map(|(id, region)| (*id, capture_sphere(*region)))
            .collect(),
    }
}

/// Apply a saved camera framing via `OrbitCamera`'s own setters, starting
/// from `OrbitCamera::default()` so `near`/`far`/`vertical_fov` — fixed
/// constants with no setter and no UI control anywhere — stay at their
/// usual values rather than needing a place in `CameraState` at all.
pub fn restore_camera(camera: &mut OrbitCamera, state: &CameraState) {
    *camera = OrbitCamera::default();
    let [x, y, z] = state.target;
    camera.set_target(glam::Vec3::new(x, y, z));
    camera.set_distance(state.distance);
    camera.set_yaw(state.yaw);
    camera.set_pitch(state.pitch);
    camera.set_projection(match state.projection {
        CameraProjection::Perspective => Projection::Perspective,
        CameraProjection::Orthographic => Projection::Orthographic,
    });
}

pub fn restore_view_options(state: ViewOptionsState) -> ViewOptions {
    ViewOptions {
        grid: state.grid,
        axes: state.axes,
        objects: state.objects,
        auxiliary_objects: state.auxiliary_objects,
        compute_bounds: state.compute_bounds,
        gizmo_display: restore_gizmo_display(state.gizmo_display),
        probes: state.probes,
        planes: state.planes,
        boxes: state.boxes,
        spheres: state.spheres,
    }
}

fn restore_gizmo_display(state: GizmoDisplayState) -> GizmoDisplay {
    GizmoDisplay {
        axis_length_px: state.axis_length_px,
        axis_thickness_px: state.axis_thickness_px,
        rotation_diameter_px: state.rotation_diameter_px,
        rotation_thickness_px: state.rotation_thickness_px,
    }
}

fn restore_vectors(state: VectorDisplayState) -> VectorDisplay {
    VectorDisplay {
        visible: state.visible,
        density: state.density,
        scale: state.scale,
    }
}

fn restore_flow_lines(state: FlowLineDisplayState) -> FlowLineDisplay {
    FlowLineDisplay {
        visible: state.visible,
        density: state.density,
        thickness_px: state.thickness_px,
        animated: state.animated,
        speed: state.speed,
    }
}

fn restore_whole_domain(state: FieldLayerViewState) -> FieldLayerSettings {
    FieldLayerSettings {
        vectors: restore_vectors(state.vectors),
        flow_lines: restore_flow_lines(state.flow_lines),
    }
}

fn restore_plane(state: PlaneViewState) -> PlaneLayerSettings {
    PlaneLayerSettings {
        visible: state.visible,
        magnitude_visible: state.magnitude_visible,
        magnitude_density: state.magnitude_density,
        vectors: restore_vectors(state.vectors),
        vector_mode: match state.vector_mode {
            PlaneVectorModeState::InPlane => PlaneVectorMode::InPlane,
            PlaneVectorModeState::Full3d => PlaneVectorMode::Full3d,
        },
        flow_lines: restore_flow_lines(state.flow_lines),
    }
}

fn restore_box(state: RegionViewState) -> BoxLayerSettings {
    BoxLayerSettings {
        visible: state.visible,
        vectors: restore_vectors(state.vectors),
        flow_lines: restore_flow_lines(state.flow_lines),
    }
}

fn restore_sphere(state: RegionViewState) -> SphereLayerSettings {
    SphereLayerSettings {
        visible: state.visible,
        vectors: restore_vectors(state.vectors),
        flow_lines: restore_flow_lines(state.flow_lines),
    }
}

pub fn restore_field_layers(
    channels: BTreeMap<ChannelId, ChannelViewState>,
) -> BTreeMap<ChannelId, ChannelLayerSettings> {
    channels
        .into_iter()
        .map(|(id, state)| {
            let settings = ChannelLayerSettings {
                visible: state.visible,
                whole_domain: restore_whole_domain(state.whole_domain),
                planes: state
                    .planes
                    .into_iter()
                    .map(|(id, plane)| (id, restore_plane(plane)))
                    .collect(),
                boxes: state
                    .boxes
                    .into_iter()
                    .map(|(id, region)| (id, restore_box(region)))
                    .collect(),
                spheres: state
                    .spheres
                    .into_iter()
                    .map(|(id, region)| (id, restore_sphere(region)))
                    .collect(),
            };
            (id, settings)
        })
        .collect()
}

fn restore_trajectory(state: TrajectoryDisplayState) -> TrajectoryDisplay {
    TrajectoryDisplay {
        visible: state.visible,
        trail_seconds: state.trail_seconds,
        thickness_px: state.thickness_px,
        animated: state.animated,
        speed: state.speed,
    }
}

pub fn restore_object_trajectories(
    objects: BTreeMap<ObjectId, TrajectoryDisplayState>,
) -> BTreeMap<ObjectId, TrajectoryDisplay> {
    objects
        .into_iter()
        .map(|(id, state)| (id, restore_trajectory(state)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{BoxId, PlaneId, SphereId};

    #[test]
    fn camera_round_trips_through_capture_and_restore() {
        let mut camera = OrbitCamera::default();
        camera.set_target(glam::Vec3::new(1.0, 2.0, 3.0));
        camera.set_distance(42.0);
        camera.set_yaw(0.5);
        camera.set_pitch(-0.25);
        camera.set_projection(Projection::Orthographic);

        let state = capture_camera(&camera);
        let mut restored = OrbitCamera::default();
        restore_camera(&mut restored, &state);

        assert_eq!(restored.target(), camera.target());
        assert_eq!(restored.distance(), camera.distance());
        assert_eq!(restored.yaw(), camera.yaw());
        assert_eq!(restored.pitch(), camera.pitch());
        assert_eq!(restored.projection(), camera.projection());
    }

    #[test]
    fn view_options_round_trip_through_capture_and_restore() {
        let view = ViewOptions {
            grid: false,
            gizmo_display: GizmoDisplay {
                axis_length_px: 99.0,
                ..GizmoDisplay::default()
            },
            ..ViewOptions::default()
        };

        let restored = restore_view_options(capture_view_options(&view));

        assert_eq!(restored, view);
    }

    #[test]
    fn per_channel_display_settings_round_trip_non_default_values() {
        let plane_id = PlaneId::new(7);
        let box_id = BoxId::new(3);
        let sphere_id = SphereId::new(1);
        let channel = ChannelId::new(fieldcad_core::PluginId::new("test").unwrap(), "field")
            .expect("valid channel id");

        let mut settings = ChannelLayerSettings {
            visible: true,
            ..Default::default()
        };
        settings.planes.insert(
            plane_id,
            PlaneLayerSettings {
                magnitude_visible: false,
                ..Default::default()
            },
        );
        settings.boxes.insert(
            box_id,
            BoxLayerSettings {
                flow_lines: FlowLineDisplay {
                    visible: true,
                    animated: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        settings
            .spheres
            .insert(sphere_id, SphereLayerSettings::default());

        let mut field_layers = BTreeMap::new();
        field_layers.insert(channel.clone(), settings.clone());

        let restored = restore_field_layers(
            capture(
                &OrbitCamera::default(),
                None,
                &ViewOptions::default(),
                &field_layers,
                &BTreeMap::new(),
            )
            .channels,
        );

        assert_eq!(restored.get(&channel), Some(&settings));
    }

    #[test]
    fn per_object_trajectory_display_round_trips_non_default_values() {
        let object = fieldcad_core::ObjectId::new(4);
        let display = TrajectoryDisplay {
            visible: true,
            trail_seconds: 12.5,
            thickness_px: 3.0,
            animated: true,
            speed: 2.0,
        };
        let mut object_trajectories = BTreeMap::new();
        object_trajectories.insert(object, display);

        let restored = restore_object_trajectories(
            capture(
                &OrbitCamera::default(),
                None,
                &ViewOptions::default(),
                &BTreeMap::new(),
                &object_trajectories,
            )
            .objects,
        );

        assert_eq!(restored.get(&object), Some(&display));
    }
}
