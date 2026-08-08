//! Inspector sections for plane, box, and sphere measurement shapes:
//! geometry editors and per-entity field layer display.

use std::collections::BTreeMap;

use fieldcad_core::{
    ChannelId, Dimension, FieldBox, FieldBoxSpec, FieldSphere, FieldSphereSpec, SlicePlane,
    SlicePlaneSpec,
};
use glam::{DQuat, DVec2, DVec3};

use super::scene_tree::entity_actions;
use super::{coordinate_editor, name_editor};
use crate::ui::compute::ComputeView;
use crate::ui::{ChannelLayerSettings, UiFrameOutput};

use crate::scene::{
    BoxLayerSettings, FlowLineDisplay, PlaneVectorMode, SphereLayerSettings, VectorDisplay,
};

pub(super) fn plane_properties(
    ui: &mut egui::Ui,
    plane: &SlicePlane,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("plane_name", plane.id), &plane.name) {
        output.edit(vec![fieldcad_core::WorldCommand::SetPlaneName {
            plane: plane.id,
            name,
        }]);
    }
    ui.small("Drag the dashed purple N arrow to reorient the plane; RGB arrows and squares move its origin.");
    super::section(ui, "inspector_plane_geometry", "Geometry", true, |ui| {
        plane_geometry_editors(ui, plane, output);
    });
    super::section(ui, "inspector_plane_display", "Field display", true, |ui| {
        plane_field_layers(ui, plane, field_layers, compute);
    });

    entity_actions(
        ui,
        output,
        "plane",
        || {
            fieldcad_core::WorldCommand::CreatePlane(
                SlicePlaneSpec::from_plane(plane).with_name(format!("{} copy", plane.name)),
            )
        },
        || fieldcad_core::WorldCommand::RemovePlane(plane.id),
    );
}

fn plane_geometry_editors(ui: &mut egui::Ui, plane: &SlicePlane, output: &mut UiFrameOutput) {
    let mut origin = plane.origin;
    let mut normal = plane.normal;
    let mut half_extent = plane.half_extent;
    let mut changed = false;

    egui::Grid::new("plane_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Origin");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut origin.x, Dimension::LENGTH, editing);
                changed |= coordinate_editor(ui, "y", &mut origin.y, Dimension::LENGTH, editing);
                changed |= coordinate_editor(ui, "z", &mut origin.z, Dimension::LENGTH, editing);
            });
            ui.end_row();

            ui.label("Normal");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |=
                    coordinate_editor(ui, "nx", &mut normal.x, Dimension::DIMENSIONLESS, editing);
                changed |=
                    coordinate_editor(ui, "ny", &mut normal.y, Dimension::DIMENSIONLESS, editing);
                changed |=
                    coordinate_editor(ui, "nz", &mut normal.z, Dimension::DIMENSIONLESS, editing);
            });
            ui.end_row();

            ui.label("Half extent");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |=
                    coordinate_editor(ui, "u", &mut half_extent.x, Dimension::LENGTH, editing);
                changed |=
                    coordinate_editor(ui, "v", &mut half_extent.y, Dimension::LENGTH, editing);
            });
            ui.end_row();
        });

    if changed && let Ok(spec) = plane_spec(plane, origin, normal, half_extent) {
        output.edit(vec![fieldcad_core::WorldCommand::SetPlane {
            plane: plane.id,
            spec,
        }]);
    }

    ui.horizontal(|ui| {
        for (label, normal, u_axis) in [
            ("XY", DVec3::Z, DVec3::X),
            ("XZ", DVec3::Y, DVec3::X),
            ("YZ", DVec3::X, DVec3::Y),
        ] {
            if ui.button(label).clicked()
                && let Ok(spec) = SlicePlaneSpec::new(&plane.name, plane.origin, normal)
                    .and_then(|spec| spec.with_u_axis(u_axis))
                    .and_then(|spec| spec.with_half_extent(plane.half_extent))
            {
                output.edit(vec![fieldcad_core::WorldCommand::SetPlane {
                    plane: plane.id,
                    spec: spec.with_visibility(plane.visible),
                }]);
            }
        }
    });
}

fn plane_spec(
    plane: &SlicePlane,
    origin: DVec3,
    normal: DVec3,
    half_extent: DVec2,
) -> Result<SlicePlaneSpec, fieldcad_core::WorldError> {
    let spec = if normal == plane.normal {
        SlicePlaneSpec::from_plane(plane).with_origin(origin)?
    } else {
        SlicePlaneSpec::new(&plane.name, origin, normal)?.with_visibility(plane.visible)
    };
    spec.with_half_extent(half_extent)
}

fn hidden_everywhere_warning(ui: &mut egui::Ui) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "This field is hidden everywhere. Turn its layer on under Fields \
                 in the View window.",
            )
            .small()
            .color(egui::Color32::from_rgb(230, 180, 80)),
        )
        .wrap(),
    );
}

trait VectorLayerSettings {
    fn visible_mut(&mut self) -> &mut bool;
    fn vectors_mut(&mut self) -> &mut VectorDisplay;
    fn flow_lines_mut(&mut self) -> &mut FlowLineDisplay;
}

impl VectorLayerSettings for BoxLayerSettings {
    fn visible_mut(&mut self) -> &mut bool {
        &mut self.visible
    }
    fn vectors_mut(&mut self) -> &mut VectorDisplay {
        &mut self.vectors
    }
    fn flow_lines_mut(&mut self) -> &mut FlowLineDisplay {
        &mut self.flow_lines
    }
}

impl VectorLayerSettings for SphereLayerSettings {
    fn visible_mut(&mut self) -> &mut bool {
        &mut self.visible
    }
    fn vectors_mut(&mut self) -> &mut VectorDisplay {
        &mut self.vectors
    }
    fn flow_lines_mut(&mut self) -> &mut FlowLineDisplay {
        &mut self.flow_lines
    }
}

struct VolumeFieldLayerText<'a> {
    checkbox_label: &'a str,
    checkbox_hover: &'a str,
    arrow_hover: &'a str,
    flow_line_hover: &'a str,
}

fn volume_field_layers<Id: Ord + Copy, S: VectorLayerSettings + Default>(
    ui: &mut egui::Ui,
    id: Id,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    layer_map: impl Fn(&mut ChannelLayerSettings) -> &mut BTreeMap<Id, S>,
    text: VolumeFieldLayerText<'_>,
) {
    for channel in &compute.vector_channels {
        let name = channel_label(channel, &compute.channel_names);
        let layer = field_layers.entry(channel.clone()).or_default();
        let layer_visible = layer.visible;
        let settings = layer_map(layer).entry(id).or_default();
        ui.collapsing(name, |ui| {
            ui.checkbox(settings.visible_mut(), text.checkbox_label)
                .on_hover_text(text.checkbox_hover);
            if !layer_visible {
                hidden_everywhere_warning(ui);
            }
            ui.add_enabled_ui(*settings.visible_mut(), |ui| {
                super::vector_display_controls(
                    ui,
                    settings.vectors_mut(),
                    "Vector arrows",
                    text.arrow_hover,
                );
                super::flow_line_display_controls(
                    ui,
                    settings.flow_lines_mut(),
                    "Flow lines",
                    text.flow_line_hover,
                );
            });
        });
    }
}

pub(super) fn plane_field_layers(
    ui: &mut egui::Ui,
    plane: &SlicePlane,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
) {
    for channel in &compute.vector_channels {
        let name = channel_label(channel, &compute.channel_names);
        let layer = field_layers.entry(channel.clone()).or_default();
        let layer_visible = layer.visible;
        let settings = layer.planes.entry(plane.id).or_default();
        ui.collapsing(name, |ui| {
            ui.checkbox(&mut settings.visible, "Show on this plane")
                .on_hover_text(
                    "Whether this plane draws this field. Other planes and the \
                     whole-domain arrows are unaffected.",
                );
            if !layer_visible {
                hidden_everywhere_warning(ui);
            }

            ui.add_enabled_ui(settings.visible, |ui| {
                ui.checkbox(&mut settings.magnitude_visible, "Magnitude colour");
                density_editor(
                    ui,
                    "Magnitude density",
                    &mut settings.magnitude_density,
                    settings.magnitude_visible,
                );
                super::vector_display_controls(
                    ui,
                    &mut settings.vectors,
                    "Vector arrows",
                    "Draw the field as arrows on this plane",
                );
                // Always the in-plane projection, regardless of `vector_mode`
                // below: a 2D streamline cannot depict an out-of-plane
                // component either (see `scene::flow_lines`).
                super::flow_line_display_controls(
                    ui,
                    &mut settings.flow_lines,
                    "Flow lines",
                    "Draw the field as continuous flow lines on this plane, independent of \
                     the arrows above",
                );
                ui.horizontal(|ui| {
                    ui.label("Vector component");
                    ui.selectable_value(
                        &mut settings.vector_mode,
                        PlaneVectorMode::InPlane,
                        "In plane",
                    )
                    .on_hover_text("Project vectors into this plane (default)");
                    ui.selectable_value(
                        &mut settings.vector_mode,
                        PlaneVectorMode::Full3d,
                        "Full 3D",
                    )
                    .on_hover_text("Show the component normal to the plane too");
                });
            });
        });
    }
}

pub(super) fn box_properties(
    ui: &mut egui::Ui,
    field_box: &FieldBox,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("box_name", field_box.id), &field_box.name) {
        output.edit(vec![fieldcad_core::WorldCommand::SetBoxName {
            region: field_box.id,
            name,
        }]);
    }
    ui.small("Drag the RGB rings to reorient the box; RGB arrows and squares move its origin.");
    super::section(ui, "inspector_box_geometry", "Geometry", true, |ui| {
        box_geometry_editors(ui, field_box, output);
    });
    super::section(ui, "inspector_box_display", "Field display", true, |ui| {
        box_field_layers(ui, field_box, field_layers, compute);
    });

    entity_actions(
        ui,
        output,
        "box",
        || {
            fieldcad_core::WorldCommand::CreateBox(
                FieldBoxSpec::from_box(field_box).with_name(format!("{} copy", field_box.name)),
            )
        },
        || fieldcad_core::WorldCommand::RemoveBox(field_box.id),
    );
}

fn box_geometry_editors(ui: &mut egui::Ui, field_box: &FieldBox, output: &mut UiFrameOutput) {
    let mut origin = field_box.origin;
    let mut half_extent = field_box.half_extent;
    let mut changed = false;

    egui::Grid::new("box_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Origin");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut origin.x, Dimension::LENGTH, editing);
                changed |= coordinate_editor(ui, "y", &mut origin.y, Dimension::LENGTH, editing);
                changed |= coordinate_editor(ui, "z", &mut origin.z, Dimension::LENGTH, editing);
            });
            ui.end_row();

            ui.label("Half extent");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |=
                    coordinate_editor(ui, "w", &mut half_extent.x, Dimension::LENGTH, editing);
                changed |=
                    coordinate_editor(ui, "h", &mut half_extent.y, Dimension::LENGTH, editing);
                changed |=
                    coordinate_editor(ui, "d", &mut half_extent.z, Dimension::LENGTH, editing);
            });
            ui.end_row();
        });

    if changed
        && let Ok(spec) = FieldBoxSpec::from_box(field_box)
            .with_origin(origin)
            .and_then(|spec| spec.with_half_extent(half_extent))
    {
        output.edit(vec![fieldcad_core::WorldCommand::SetBox {
            region: field_box.id,
            spec,
        }]);
    }

    if ui
        .button("Reset orientation")
        .on_hover_text("Return this box to the world axes")
        .clicked()
        && let Ok(spec) = FieldBoxSpec::from_box(field_box).with_rotation(DQuat::IDENTITY)
    {
        output.edit(vec![fieldcad_core::WorldCommand::SetBox {
            region: field_box.id,
            spec,
        }]);
    }
}

fn box_field_layers(
    ui: &mut egui::Ui,
    field_box: &FieldBox,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
) {
    volume_field_layers(
        ui,
        field_box.id,
        field_layers,
        compute,
        |layer| &mut layer.boxes,
        VolumeFieldLayerText {
            checkbox_label: "Show in this box",
            checkbox_hover: "Whether this box draws this field. Other boxes, spheres, and \
                              planes are unaffected.",
            arrow_hover: "Draw the field as arrows inside this box",
            flow_line_hover: "Draw the field as continuous flow lines inside this box, \
                               independent of the arrows above",
        },
    );
}

pub(super) fn sphere_properties(
    ui: &mut egui::Ui,
    sphere: &FieldSphere,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("sphere_name", sphere.id), &sphere.name) {
        output.edit(vec![fieldcad_core::WorldCommand::SetSphereName {
            sphere: sphere.id,
            name,
        }]);
    }
    ui.small("RGB arrows and squares move its centre; drag the radius below to resize it.");
    super::section(ui, "inspector_sphere_geometry", "Geometry", true, |ui| {
        sphere_geometry_editors(ui, sphere, output);
    });
    super::section(
        ui,
        "inspector_sphere_display",
        "Field display",
        true,
        |ui| {
            sphere_field_layers(ui, sphere, field_layers, compute);
        },
    );

    entity_actions(
        ui,
        output,
        "sphere",
        || {
            fieldcad_core::WorldCommand::CreateSphere(
                FieldSphereSpec::from_sphere(sphere).with_name(format!("{} copy", sphere.name)),
            )
        },
        || fieldcad_core::WorldCommand::RemoveSphere(sphere.id),
    );
}

fn sphere_geometry_editors(ui: &mut egui::Ui, sphere: &FieldSphere, output: &mut UiFrameOutput) {
    let mut origin = sphere.origin;
    let mut radius = sphere.radius;
    let mut changed = false;

    egui::Grid::new("sphere_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Origin");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut origin.x, Dimension::LENGTH, editing);
                changed |= coordinate_editor(ui, "y", &mut origin.y, Dimension::LENGTH, editing);
                changed |= coordinate_editor(ui, "z", &mut origin.z, Dimension::LENGTH, editing);
            });
            ui.end_row();

            ui.label("Radius");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "r", &mut radius, Dimension::LENGTH, editing);
            });
            ui.end_row();
        });

    if changed
        && let Ok(spec) = FieldSphereSpec::from_sphere(sphere)
            .with_origin(origin)
            .and_then(|spec| spec.with_radius(radius))
    {
        output.edit(vec![fieldcad_core::WorldCommand::SetSphere {
            sphere: sphere.id,
            spec,
        }]);
    }
}

fn sphere_field_layers(
    ui: &mut egui::Ui,
    sphere: &FieldSphere,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
) {
    volume_field_layers(
        ui,
        sphere.id,
        field_layers,
        compute,
        |layer| &mut layer.spheres,
        VolumeFieldLayerText {
            checkbox_label: "Show in this sphere",
            checkbox_hover: "Whether this sphere draws this field. Other spheres, boxes, and \
                              planes are unaffected.",
            arrow_hover: "Draw the field as arrows inside this sphere",
            flow_line_hover: "Draw the field as continuous flow lines inside this sphere, \
                               independent of the arrows above",
        },
    );
}

fn channel_label(id: &ChannelId, names: &BTreeMap<ChannelId, String>) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_string())
}

fn density_editor(ui: &mut egui::Ui, label: &str, density: &mut u32, enabled: bool) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(density).speed(1.0).range(0..=u32::MAX),
        )
        .on_hover_text("Enter any non-negative whole number");
    });
}

// note_held_edit, coordinate_editor, name_editor provided by super::
