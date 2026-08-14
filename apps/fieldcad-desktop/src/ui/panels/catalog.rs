//! Catalog discovery window. Source editing is intentionally owned by the
//! application shell; this panel only renders current catalog state and emits
//! source-qualified user intent.

use fieldcad_catalog::{LoadResult, TemplateComponentInstance, TemplateShape};
use fieldcad_core::quantities::{LengthMetres, SiScalar};
use fieldcad_core::{Dimension, ObjectId};

use crate::ui::{CatalogAction, FrameContext, UiFrameOutput, UiModel};

use super::scene_tree::catalog_object_command;

pub fn catalog_window(
    root: &egui::Context,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    if !model.catalog_visible {
        return;
    }
    let mut open = model.catalog_visible;
    egui::Window::new("Catalog")
        .open(&mut open)
        .default_width(560.0)
        .default_height(440.0)
        .show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label("Search");
                ui.add(
                    egui::TextEdit::singleline(&mut model.catalog_filter)
                        .hint_text("name, catalog, label, source"),
                );
                if ui.button("Reload").clicked() {
                    output.app_action = Some(crate::ui::AppAction::ReloadCatalog);
                }
            });
            ui.horizontal(|ui| {
                ui.label("New global template");
                ui.add(
                    egui::TextEdit::singleline(&mut model.catalog_new_catalog).hint_text("catalog"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut model.catalog_new_template)
                        .hint_text("template"),
                );
                let create_ready = !model.catalog_new_catalog.trim().is_empty()
                    && !model.catalog_new_template.trim().is_empty();
                if ui
                    .add_enabled(create_ready, egui::Button::new("Create YAML"))
                    .clicked()
                {
                    output.catalog_action = Some(CatalogAction::CreateGlobal {
                        catalog: model.catalog_new_catalog.clone(),
                        template: model.catalog_new_template.clone(),
                    });
                }
                if ui
                    .add_enabled(create_ready, egui::Button::new("Create document entry"))
                    .on_disabled_hover_text("Enter both a catalog and template name first.")
                    .clicked()
                {
                    output.catalog_action = Some(CatalogAction::CreateDocument {
                        catalog: model.catalog_new_catalog.clone(),
                        template: model.catalog_new_template.clone(),
                    });
                }
            });
            ui.separator();
            let query = model.catalog_filter.to_ascii_lowercase();
            ui.columns(2, |columns| {
                let list = &mut columns[0];
                egui::ScrollArea::vertical().show(list, |ui| {
                    for entry in &frame.catalog.entries {
                        let Some(reference) = entry.reference.as_ref() else {
                            if query.is_empty() {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    format!("{} — invalid", entry.source.file.display()),
                                );
                            }
                            continue;
                        };
                        let matches = query.is_empty()
                            || reference.template.to_ascii_lowercase().contains(&query)
                            || reference.catalog.to_ascii_lowercase().contains(&query)
                            || entry
                                .source
                                .file
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .contains(&query);
                        if !matches {
                            continue;
                        }
                        let (state, available, detail) = match &entry.result {
                            LoadResult::Available { metadata, .. } => (
                                "available",
                                true,
                                metadata.description.clone().unwrap_or_default(),
                            ),
                            LoadResult::Unavailable { reasons, .. } => (
                                "unavailable",
                                false,
                                reasons
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            ),
                            LoadResult::Invalid { diagnostics } => (
                                "invalid",
                                false,
                                diagnostics
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            ),
                        };
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(
                                        model.catalog_selected.as_ref() == Some(reference),
                                        format!("{}/{}", reference.catalog, reference.template),
                                    )
                                    .clicked()
                                {
                                    output.catalog_action =
                                        Some(CatalogAction::Open(Some(reference.clone())));
                                }
                                ui.weak(state);
                                if ui.small_button("Open").clicked() {
                                    output.catalog_action =
                                        Some(CatalogAction::Open(Some(reference.clone())));
                                }
                                if ui
                                    .add_enabled(available, egui::Button::new("Add"))
                                    .clicked()
                                {
                                    output.submit(catalog_object_command(frame, reference));
                                }
                                let hidden = frame.quick_add_hidden.contains(reference);
                                if ui
                                    .selectable_label(
                                        hidden,
                                        if hidden { "Hidden" } else { "Quick add" },
                                    )
                                    .clicked()
                                {
                                    output.catalog_action =
                                        Some(CatalogAction::SetQuickAddHidden {
                                            entry: reference.clone(),
                                            hidden: !hidden,
                                        });
                                }
                            });
                            ui.weak(format!(
                                "{} (document #{})",
                                entry.source.file.display(),
                                entry.source.document_ordinal
                            ));
                            if !detail.is_empty() {
                                ui.label(detail);
                            }
                        });
                    }
                });
                catalog_editor(&mut columns[1], model, frame, output);
            });
        });
    model.catalog_visible = open;
}

fn catalog_editor(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let Some(selected) = &model.catalog_selected else {
        ui.weak("Select a catalog entry to inspect or edit it.");
        return;
    };
    let Some(entry) = frame
        .catalog
        .entries
        .iter()
        .find(|entry| entry.reference.as_ref() == Some(selected))
    else {
        ui.weak("The selected entry is no longer available.");
        return;
    };
    ui.heading(format!("Edit {}/{}", selected.catalog, selected.template));
    match &entry.result {
        LoadResult::Available { .. } | LoadResult::Unavailable { .. } => {
            let Some(draft) = model
                .catalog_editor
                .as_mut()
                .filter(|draft| &draft.source == selected)
            else {
                ui.weak("Preparing editor draft…");
                return;
            };
            let read_only = matches!(selected.origin, fieldcad_core::CatalogOrigin::Global { .. })
                && std::fs::metadata(&entry.source.file)
                    .is_ok_and(|meta| meta.permissions().readonly());
            if read_only {
                ui.colored_label(egui::Color32::YELLOW, "This catalog source is read-only.");
            }
            if let LoadResult::Unavailable { reasons, .. } = &entry.result {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "This template is currently unavailable:",
                );
                for reason in reasons {
                    ui.weak(reason.to_string());
                }
            }
            egui::ScrollArea::vertical()
                .id_salt("catalog_editor_scroll")
                .show(ui, |ui| {
                    ui.add_enabled_ui(!read_only, |ui| {
                        identity_editor(ui, draft);
                        ui.separator();
                        ui.label("Description");
                        ui.add(egui::TextEdit::multiline(&mut draft.description).desired_rows(3));
                        ui.label("Author");
                        ui.text_edit_singleline(&mut draft.author);
                        map_editor(ui, "Labels", &mut draft.labels);
                        map_editor(ui, "Annotations", &mut draft.annotations);
                        ui.separator();
                        ui.label("Object kind");
                        ui.text_edit_singleline(&mut draft.object_kind);
                        shape_editor(ui, draft);
                        ui.separator();
                        component_editor(ui, draft, frame);
                    });
                });
            let errors = draft_errors(draft, frame);
            if !errors.is_empty() {
                for error in &errors {
                    ui.colored_label(egui::Color32::YELLOW, error);
                }
            }
            if let Some(status) = &model.catalog_status {
                ui.colored_label(egui::Color32::YELLOW, status);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!read_only && errors.is_empty(), egui::Button::new("Save"))
                    .clicked()
                {
                    output.catalog_action = Some(CatalogAction::SaveEntry {
                        entry: selected.clone(),
                        catalog: draft.catalog.clone(),
                        template: draft.template.clone(),
                        metadata: draft.metadata(),
                        spec: draft.spec(),
                    });
                }
                if ui
                    .add_enabled(!read_only, egui::Button::new("Delete entry"))
                    .clicked()
                {
                    output.catalog_action = Some(CatalogAction::DeleteEntry {
                        entry: selected.clone(),
                    });
                }
            });
        }
        LoadResult::Invalid { diagnostics } => {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Invalid YAML cannot be edited structurally.",
            );
            for diagnostic in diagnostics {
                ui.label(diagnostic.to_string());
            }
        }
    }
}

fn identity_editor(ui: &mut egui::Ui, draft: &mut crate::ui::CatalogEditorDraft) {
    ui.horizontal(|ui| {
        ui.label("Catalog");
        ui.text_edit_singleline(&mut draft.catalog);
    });
    ui.horizontal(|ui| {
        ui.label("Template name");
        ui.text_edit_singleline(&mut draft.template);
    });
}

fn map_editor(ui: &mut egui::Ui, title: &str, rows: &mut Vec<(String, String)>) {
    ui.label(title);
    let mut remove = None;
    for (index, (key, value)) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(key).hint_text("key"));
            ui.add(egui::TextEdit::singleline(value).hint_text("value"));
            if ui.small_button("−").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        rows.remove(index);
    }
    if ui.small_button(format!("+ Add {title}")).clicked() {
        rows.push((String::new(), String::new()));
    }
}

fn shape_editor(ui: &mut egui::Ui, draft: &mut crate::ui::CatalogEditorDraft) {
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        None,
        Point,
        Sphere,
        Box,
    }
    let mut kind = match draft.shape {
        None => Kind::None,
        Some(TemplateShape::Point { .. }) => Kind::Point,
        Some(TemplateShape::Sphere { .. }) => Kind::Sphere,
        Some(TemplateShape::Box { .. }) => Kind::Box,
    };
    let before = kind;
    egui::ComboBox::from_id_salt("catalog_shape_kind")
        .selected_text(match kind {
            Kind::None => "None",
            Kind::Point => "Point",
            Kind::Sphere => "Sphere",
            Kind::Box => "Box",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut kind, Kind::None, "None");
            ui.selectable_value(&mut kind, Kind::Point, "Point");
            ui.selectable_value(&mut kind, Kind::Sphere, "Sphere");
            ui.selectable_value(&mut kind, Kind::Box, "Box");
        });
    if kind != before {
        draft.shape = match kind {
            Kind::None => None,
            Kind::Point => Some(TemplateShape::Point {
                exclusion_radius: LengthMetres::from_si(0.1),
            }),
            Kind::Sphere => Some(TemplateShape::Sphere {
                radius: LengthMetres::from_si(0.1),
            }),
            Kind::Box => Some(TemplateShape::Box {
                half_extent: glam::DVec3::splat(0.1),
            }),
        };
    }
    let mut editing = false;
    match &mut draft.shape {
        Some(TemplateShape::Point { exclusion_radius }) => {
            let mut value = exclusion_radius.into_si();
            if super::coordinate_editor(
                ui,
                "exclusion radius",
                &mut value,
                Dimension::LENGTH,
                &mut editing,
            ) {
                *exclusion_radius = LengthMetres::from_si(value);
            }
        }
        Some(TemplateShape::Sphere { radius }) => {
            let mut value = radius.into_si();
            if super::coordinate_editor(ui, "radius", &mut value, Dimension::LENGTH, &mut editing) {
                *radius = LengthMetres::from_si(value);
            }
        }
        Some(TemplateShape::Box { half_extent }) => {
            ui.horizontal(|ui| {
                for (label, value) in [
                    ("x", &mut half_extent.x),
                    ("y", &mut half_extent.y),
                    ("z", &mut half_extent.z),
                ] {
                    super::coordinate_editor(ui, label, value, Dimension::LENGTH, &mut editing);
                }
            });
        }
        None => {}
    }
}

fn component_editor(
    ui: &mut egui::Ui,
    draft: &mut crate::ui::CatalogEditorDraft,
    frame: &FrameContext<'_>,
) {
    ui.label("Components");
    let available: Vec<_> = frame
        .world
        .component_schemas()
        .values()
        .filter(|schema| {
            !draft
                .components
                .iter()
                .any(|component| component.component_type == schema.id)
        })
        .collect();
    ui.menu_button("+ Add component", |ui| {
        for schema in available {
            match schema
                .default_properties()
                .ok()
                .and_then(|bag| fieldcad_catalog::property_bag_to_template(schema, &bag).ok())
            {
                Some(properties) => {
                    if ui.button(&schema.display_name).clicked() {
                        draft.components.push(TemplateComponentInstance {
                            component_type: schema.id.clone(),
                            properties,
                        });
                        ui.close();
                    }
                }
                None => {
                    ui.add_enabled(false, egui::Button::new(&schema.display_name))
                        .on_disabled_hover_text("This component has no schema defaults.");
                }
            }
        }
    });
    let mut remove = None;
    for (index, component) in draft.components.iter_mut().enumerate() {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(component.component_type.to_string());
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
            let Some(schema) = frame
                .world
                .component_schemas()
                .get(&component.component_type)
            else {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Schema unavailable; this raw component will be preserved until removed.",
                );
                return;
            };
            match fieldcad_catalog::template_properties_to_bag(schema, &component.properties) {
                Ok(mut bag) => {
                    let mut changed = false;
                    let mut editing = false;
                    for property in &schema.properties {
                        if property.is_relevant(&bag) {
                            changed |= super::object_inspector::property_editor(
                                ui,
                                ObjectId::new(index as u64 + 1),
                                property,
                                &mut bag,
                                &mut editing,
                            );
                        }
                    }
                    if changed {
                        if let Ok(values) = fieldcad_catalog::property_bag_to_template(schema, &bag)
                        {
                            for (key, value) in values {
                                component.properties.insert(key, value);
                            }
                        }
                    }
                }
                Err(errors) => {
                    for error in errors {
                        ui.colored_label(egui::Color32::YELLOW, error.to_string());
                    }
                }
            }
        });
    }
    if let Some(index) = remove {
        draft.components.remove(index);
    }
}

fn draft_errors(draft: &crate::ui::CatalogEditorDraft, frame: &FrameContext<'_>) -> Vec<String> {
    let mut errors = Vec::new();
    let catalog = match fieldcad_catalog::CatalogScopeName::new(draft.catalog.trim()) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(format!("Catalog: {error}"));
            None
        }
    };
    let template = match fieldcad_catalog::TemplateName::new(draft.template.trim()) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(format!("Template name: {error}"));
            None
        }
    };
    if let (Some(catalog), Some(template)) = (catalog, template) {
        let identity = fieldcad_catalog::TemplateIdentity { catalog, template };
        if frame.catalog.entries.iter().any(|entry| {
            entry.identity.as_ref() == Some(&identity)
                && entry.reference.as_ref() != Some(&draft.source)
        }) {
            errors.push(format!(
                "{identity} already exists in the effective catalog"
            ));
        }
    }
    for (name, rows) in [("label", &draft.labels), ("annotation", &draft.annotations)] {
        let mut keys = std::collections::BTreeSet::new();
        for (key, _) in rows {
            if key.trim().is_empty() {
                errors.push(format!("{name} keys cannot be blank"));
            } else if !keys.insert(key.trim()) {
                errors.push(format!("duplicate {name} key '{key}'"));
            }
        }
    }
    if draft.object_kind.trim().is_empty() {
        errors.push("Object kind cannot be blank".to_owned());
    }
    if let Some(shape) = &draft.shape {
        let values: Vec<f64> = match shape {
            TemplateShape::Point { exclusion_radius } => vec![exclusion_radius.into_si()],
            TemplateShape::Sphere { radius } => vec![radius.into_si()],
            TemplateShape::Box { half_extent } => vec![half_extent.x, half_extent.y, half_extent.z],
        };
        if values
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            errors.push("Shape extents must be finite and positive".to_owned());
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for component in &draft.components {
        if !ids.insert(&component.component_type) {
            errors.push(format!("Duplicate component {}", component.component_type));
        }
        if let Some(schema) = frame
            .world
            .component_schemas()
            .get(&component.component_type)
        {
            match fieldcad_catalog::template_properties_to_bag(schema, &component.properties) {
                Ok(bag) => {
                    if let Err(error) = schema.validate(&bag) {
                        errors.push(format!("{}: {error}", component.component_type));
                    }
                }
                Err(component_errors) => errors.extend(
                    component_errors
                        .into_iter()
                        .map(|error| format!("{}: {error}", component.component_type)),
                ),
            }
        }
    }
    errors
}
