//! Inspector sections for the Simulation (world) node: domain, fields,
//! field systems, transport sampling, and compute status.

use fieldcad_core::{BoundaryCondition, Dimension, ObjectId, Precision};
use fieldcad_simulation::{CommandPayload, IntegrationScheme};

use super::coordinate_editor;
use crate::ui::compute::{ComputeView, validity_note};
use crate::ui::{UiFrameOutput, UiModel};
use fieldcad_core::SnapshotFreshness;

pub(super) fn world_properties(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    compute: &ComputeView,
    edit_in_progress: bool,
    output: &mut UiFrameOutput,
) {
    super::section(
        ui,
        "inspector_numerical_domain",
        "Numerical domain",
        true,
        |ui| {
            numerical_domain_editor(ui, model, compute, edit_in_progress, output);
        },
    );
    super::section(ui, "inspector_dynamics", "Dynamics", true, |ui| {
        integration_scheme_picker(ui, compute, output);
    });
    super::section(ui, "inspector_fields", "Fields", true, |ui| {
        field_controls(ui, compute, output);
    });
    super::section(ui, "inspector_field_systems", "Field systems", true, |ui| {
        field_system_controls(ui, model, compute, edit_in_progress, output);
    });
    super::section(
        ui,
        "inspector_transport_sampling",
        "Transport sampling",
        true,
        |ui| transport_sampling(ui, compute, output),
    );
    super::section(ui, "inspector_compute", "Compute", true, |ui| {
        compute_panel(ui, compute);
    });
}

fn numerical_domain_editor(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    compute: &ComputeView,
    edit_in_progress: bool,
    output: &mut UiFrameOutput,
) {
    let draft = model.domain_draft_for(compute.domain);

    ui.small(
        "Changing this lattice resets the local simulation to t = 0 and leaves it paused. \
         Transport sampling below does not change the solver grid.",
    );
    egui::Grid::new("numerical_domain_editor")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Bounds min");
            ui.horizontal(|ui| {
                domain_coordinate(ui, "x", &mut draft.min.x);
                domain_coordinate(ui, "y", &mut draft.min.y);
                domain_coordinate(ui, "z", &mut draft.min.z);
            });
            ui.end_row();

            ui.label("Bounds max");
            ui.horizontal(|ui| {
                domain_coordinate(ui, "x", &mut draft.max.x);
                domain_coordinate(ui, "y", &mut draft.max.y);
                domain_coordinate(ui, "z", &mut draft.max.z);
            });
            ui.end_row();

            ui.label("Cells");
            ui.horizontal(|ui| {
                domain_cells(ui, "x", &mut draft.cells.x);
                domain_cells(ui, "y", &mut draft.cells.y);
                domain_cells(ui, "z", &mut draft.cells.z);
            });
            ui.end_row();

            ui.label("Boundaries");
            ui.horizontal(|ui| {
                boundary_picker(ui, "x", &mut draft.boundaries.x);
                boundary_picker(ui, "y", &mut draft.boundaries.y);
                boundary_picker(ui, "z", &mut draft.boundaries.z);
            });
            ui.end_row();

            ui.label("Precision");
            egui::ComboBox::from_id_salt("domain_precision")
                .selected_text(match draft.precision {
                    Precision::F32 => "f32",
                    Precision::F64 => "f64",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut draft.precision, Precision::F32, "f32");
                    ui.selectable_value(&mut draft.precision, Precision::F64, "f64");
                });
            ui.end_row();
        });

    let candidate = draft.build();
    match candidate {
        Ok(domain) => {
            let spacing = domain.cell_size();
            let cells = domain.resolution().cell_count();
            let mut summary = format!(
                "cell size {:.4} × {:.4} × {:.4} m · {cells} cells",
                spacing.x, spacing.y, spacing.z,
            );
            // Only a real cost if Maxwell is actually active on this
            // lattice — showing it while the system is disabled reads as a
            // memory commitment the domain isn't actually making.
            let maxwell_active = compute.field_systems.iter().any(|system| {
                system.enabled && system.plugin.id == fieldcad_electromagnetism::plugin_id()
            });
            if maxwell_active {
                let scalar_bytes = match domain.precision() {
                    Precision::F32 => 4_u64,
                    Precision::F64 => 8_u64,
                };
                let minimum_field_bytes = cells.saturating_mul(6).saturating_mul(scalar_bytes);
                summary.push_str(&format!(
                    " · Maxwell E/B minimum {}",
                    format_bytes(minimum_field_bytes),
                ));
            }
            ui.small(summary);
            let changed = domain != compute.domain;
            let response = ui
                .add_enabled(
                    compute.accepts_commands() && changed && !edit_in_progress,
                    egui::Button::new("Apply domain and reset"),
                )
                .on_hover_text(
                    "Validate the whole candidate, rebuild active solvers from the current authored \
                     world, clear run history, and pause at t = 0. If the current dt is unstable, \
                     the source selects 80% of the strictest reported limit.",
                );
            if response.clicked() {
                output.submit(CommandPayload::ReconfigureDomain(domain));
            }
        }
        Err(error) => {
            ui.colored_label(
                egui::Color32::from_rgb(240, 105, 95),
                format!("Invalid domain: {error}"),
            );
        }
    }
    if edit_in_progress {
        ui.small("Finish the scene edit in progress before applying a domain.");
    }
}

fn domain_coordinate(ui: &mut egui::Ui, axis: &str, value: &mut f64) {
    // The domain draft has no held-edit/"scene edit in progress" concept of
    // its own (it's staged, applied only by "Apply domain and reset"), so
    // the per-field editing flag `coordinate_editor` normally reports back
    // through is discarded here rather than threaded anywhere.
    let mut editing = false;
    coordinate_editor(ui, axis, value, Dimension::LENGTH, &mut editing);
}

fn domain_cells(ui: &mut egui::Ui, axis: &str, value: &mut u32) {
    ui.add(
        egui::DragValue::new(value)
            .speed(1.0)
            .prefix(format!("{axis}: "))
            .range(0..=u32::MAX),
    );
}

fn boundary_picker(ui: &mut egui::Ui, axis: &str, value: &mut BoundaryCondition) {
    let label = |boundary| match boundary {
        BoundaryCondition::Periodic => "Periodic",
        BoundaryCondition::Dirichlet => "Dirichlet",
        BoundaryCondition::Neumann => "Neumann",
        BoundaryCondition::Absorbing => "Absorbing",
        BoundaryCondition::Open => "Open",
    };
    egui::ComboBox::from_id_salt(("domain_boundary", axis))
        .selected_text(format!("{axis}: {}", label(*value)))
        .show_ui(ui, |ui| {
            for boundary in [
                BoundaryCondition::Periodic,
                BoundaryCondition::Dirichlet,
                BoundaryCondition::Neumann,
                BoundaryCondition::Absorbing,
                BoundaryCondition::Open,
            ] {
                ui.selectable_value(value, boundary, label(boundary));
            }
        });
}

fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn integration_scheme_picker(ui: &mut egui::Ui, compute: &ComputeView, output: &mut UiFrameOutput) {
    let current = compute.integration_scheme;
    ui.horizontal(|ui| {
        ui.label("Integration scheme");
        ui.add_enabled_ui(compute.accepts_commands(), |ui| {
            egui::ComboBox::from_id_salt("integration_scheme")
                .selected_text(current.label())
                .show_ui(ui, |ui| {
                    for scheme in IntegrationScheme::ALL {
                        if ui
                            .selectable_label(current == scheme, scheme.label())
                            .on_hover_text(scheme.description())
                            .clicked()
                            && scheme != current
                        {
                            output.submit(CommandPayload::SetIntegrationScheme(scheme));
                        }
                    }
                })
                .response
                .on_hover_text(current.description());
        });
    });
}

fn field_controls(ui: &mut egui::Ui, compute: &ComputeView, output: &mut UiFrameOutput) {
    if compute.fields.is_empty() {
        ui.weak("No fields are available. Compose a field system into the scene.");
        return;
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "A field is computed by one model at a time. Choosing another replaces it, \
                 and brings whatever else that model computes with it.",
            )
            .small(),
        )
        .wrap(),
    );
    ui.add_space(4.0);

    let name_of = |plugin: &fieldcad_core::PluginId| {
        compute
            .field_systems
            .iter()
            .find(|system| &system.plugin.id == plugin)
            .map_or_else(
                || plugin.to_string(),
                |system| system.plugin.display_name.clone(),
            )
    };

    for field in &compute.fields {
        ui.push_id(&field.channel, |ui| {
            ui.horizontal(|ui| {
                ui.label(&field.display_name).on_hover_text(format!(
                    "{}\n{}",
                    field.channel,
                    field.kind_label()
                ));

                let selected = match &field.provider {
                    Some(provider) => name_of(provider),
                    None => NOT_COMPUTED.to_owned(),
                };
                let mut chosen: Option<Option<fieldcad_core::PluginId>> = None;
                ui.add_enabled_ui(compute.accepts_commands(), |ui| {
                    egui::ComboBox::from_id_salt(("field_model", &field.channel))
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(field.provider.is_none(), NOT_COMPUTED)
                                .clicked()
                            {
                                chosen = Some(None);
                            }
                            for candidate in &field.candidates {
                                let active = field.provider.as_ref() == Some(candidate);
                                if ui.selectable_label(active, name_of(candidate)).clicked() {
                                    chosen = Some(Some(candidate.clone()));
                                }
                            }
                        })
                        .response
                        .on_hover_text(if field.has_alternatives() {
                            "Which equation system computes this field"
                        } else {
                            "The only model of this field composed into the scene"
                        });
                });
                if let Some(provider) = chosen
                    && provider != field.provider
                {
                    output.submit(CommandPayload::SetFieldModel {
                        channel: field.channel.clone(),
                        provider,
                    });
                }
            });
        });
    }
}

const NOT_COMPUTED: &str = "Not computed";

fn taken_field(
    system: &fieldcad_simulation::FieldSystemStatus,
    compute: &ComputeView,
) -> Option<(String, String)> {
    if system.enabled {
        return None;
    }
    system.channels.iter().find_map(|channel| {
        let field = compute
            .fields
            .iter()
            .find(|field| field.channel == channel.id)?;
        let provider = field.provider.as_ref()?;
        let name = compute
            .field_systems
            .iter()
            .find(|other| &other.plugin.id == provider)
            .map_or_else(
                || provider.to_string(),
                |other| other.plugin.display_name.clone(),
            );
        Some((field.display_name.clone(), name))
    })
}

pub(super) fn field_system_controls(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    compute: &ComputeView,
    edit_in_progress: bool,
    output: &mut UiFrameOutput,
) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Inactive systems do not simulate or publish fields. Their object properties remain available in the scene.",
            )
            .small(),
        )
        .wrap(),
    );

    if compute.field_systems.is_empty() {
        ui.weak("No field systems are available.");
        return;
    }

    for system in &compute.field_systems {
        ui.push_id(&system.plugin.id, |ui| {
            let mut enabled = system.enabled;
            let taken = taken_field(system, compute);
            let response = ui
                .add_enabled(
                    compute.accepts_commands() && (system.enabled || taken.is_none()),
                    egui::Checkbox::new(&mut enabled, &system.plugin.display_name),
                )
                .on_hover_text(format!(
                    "{}\n{} · version {}",
                    system.plugin.description, system.plugin.id, system.plugin.version
                ));
            let response = match taken {
                Some((field, provider)) => response.on_disabled_hover_text(format!(
                    "{field} is computed by {provider}.\n\
                     Choose this system as its model under Fields instead."
                )),
                None => response,
            };
            if response.changed() && enabled != system.enabled {
                output.submit(CommandPayload::SetFieldSystemEnabled {
                    plugin: system.plugin.id.clone(),
                    enabled,
                });
            }

            realtime_control(ui, system, compute, output);

            egui::CollapsingHeader::new("Fields and settings")
                .default_open(false)
                .show(ui, |ui| {
                    for channel in &system.channels {
                        let kind = match channel.value_kind {
                            fieldcad_core::FieldValueKind::Scalar(_) => "scalar",
                            fieldcad_core::FieldValueKind::Vector(_) => "vector",
                        };
                        ui.add_enabled_ui(system.enabled, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{} · {} · {}",
                                        channel.display_name,
                                        kind,
                                        channel.dimension()
                                    ))
                                    .small(),
                                )
                                .wrap(),
                            );
                        });
                    }

                    if !system.configuration_schema.properties.is_empty() {
                        ui.add_space(3.0);
                        ui.strong("Settings");
                        configuration_editor(ui, model, compute, system, edit_in_progress, output);
                    }
                });
        });
    }
}

/// Staged, per-plugin configuration form: reset-class, exactly like the
/// domain editor above, so it stages edits independently of the
/// authoritative value and only submits `SetFieldSystemConfiguration` on an
/// explicit "Apply" click, never per keystroke/drag.
fn configuration_editor(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    compute: &ComputeView,
    system: &fieldcad_simulation::FieldSystemStatus,
    edit_in_progress: bool,
    output: &mut UiFrameOutput,
) {
    let plugin = system.plugin.id.clone();
    let draft = model.field_system_configuration_draft_for(&plugin, &system.configuration);

    // No held-edit concept of its own, same reasoning as `domain_coordinate`:
    // the draft is staged and applied only by the button below.
    let mut editing = false;
    for property in &system.configuration_schema.properties {
        if !property.is_relevant(draft) {
            continue;
        }
        super::object_inspector::property_editor(
            ui,
            ObjectId::new(0),
            property,
            draft,
            &mut editing,
        );
    }

    let valid = system.configuration_schema.validate(draft).is_ok();
    if !valid {
        ui.colored_label(
            egui::Color32::from_rgb(240, 105, 95),
            "Invalid configuration.",
        );
    }
    let dirty = *draft != system.configuration;
    let candidate = draft.clone();
    let response = ui
        .add_enabled(
            compute.accepts_commands() && dirty && valid && !edit_in_progress,
            egui::Button::new("Apply configuration"),
        )
        .on_hover_text(
            "Reset-class, like reconfiguring the domain: rebuilds every active solver from \
             the current authored world and resumes from t = 0.",
        );
    if response.clicked() {
        output.submit(CommandPayload::SetFieldSystemConfiguration {
            plugin,
            configuration: candidate,
        });
    }
    if edit_in_progress {
        ui.small("Finish the scene edit in progress before applying a configuration change.");
    }
}

pub(super) fn realtime_control(
    ui: &mut egui::Ui,
    system: &fieldcad_simulation::FieldSystemStatus,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    ui.indent(("realtime", &system.plugin.id), |ui| {
        let mut realtime = system.realtime;
        let response = ui
            .add_enabled(
                compute.accepts_commands() && system.enabled,
                egui::Checkbox::new(&mut realtime, "Update while editing"),
            )
            .on_hover_text(
                "On: recompute this system for every intermediate value while you drag a body \
                 or type a property.\n\
                 Off: keep the last result until you let go, then recompute once from the \
                 values you committed.\n\
                 Either way the committed scene produces the same field.",
            );
        if response.changed() && realtime != system.realtime {
            output.submit(CommandPayload::SetFieldSystemRealtime {
                plugin: system.plugin.id.clone(),
                realtime,
            });
        }
    });
}

pub(super) fn transport_sampling(
    ui: &mut egui::Ui,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    let mut subscription = compute.subscription;
    let mut changed = false;
    let enabled = compute.accepts_commands();

    if let Some(planes) = density_field(
        ui,
        "Plane samples",
        "Samples per axis the source evaluates on each visible plane",
        enabled,
        0..=1_024,
        subscription.planes.map_or(0, |counts| counts.x),
    ) {
        subscription.planes = (planes > 0).then(|| glam::UVec2::splat(planes));
        changed = true;
    }

    if let Some(stride) = density_field(
        ui,
        "Domain stride",
        "Whole-domain lattice decimation; 0 publishes no 3D grid",
        enabled,
        0..=256,
        subscription.domain_stride.unwrap_or(0),
    ) {
        subscription.domain_stride = (stride > 0).then_some(stride);
        changed = true;
    }

    if let Some(boxes) = density_field(
        ui,
        "Box samples",
        "Samples per axis the source evaluates in each visible field box",
        enabled,
        0..=1_024,
        subscription.boxes.map_or(0, |counts| counts.x),
    ) {
        subscription.boxes = (boxes > 0).then(|| glam::UVec3::splat(boxes));
        changed = true;
    }

    if let Some(spheres) = density_field(
        ui,
        "Sphere samples",
        "Samples per axis the source evaluates over each visible sphere's bounding cube",
        enabled,
        0..=1_024,
        subscription.spheres.unwrap_or(0),
    ) {
        subscription.spheres = (spheres > 0).then_some(spheres);
        changed = true;
    }

    if changed && subscription != compute.subscription {
        output.submit(CommandPayload::SetSubscription(subscription));
    }
}

fn density_field(
    ui: &mut egui::Ui,
    label: &str,
    hover: &str,
    enabled: bool,
    range: std::ops::RangeInclusive<u32>,
    mut count: u32,
) -> Option<u32> {
    let mut result = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui
            .add_enabled(
                enabled,
                egui::DragValue::new(&mut count).speed(1.0).range(range),
            )
            .on_hover_text(hover);
        if response.changed() {
            result = Some(count);
        }
    });
    result
}

fn compute_panel(ui: &mut egui::Ui, compute: &ComputeView) {
    egui::Grid::new("compute_status")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);

            ui.label("Source");
            ui.add(egui::Label::new(&compute.description).truncate())
                .on_hover_text(&compute.description);
            ui.end_row();

            ui.label("State");
            ui.label(compute.status.label());
            ui.end_row();

            ui.label("Mode");
            ui.colored_label(
                compute.workbench_state().color(),
                compute.workbench_state().label(),
            );
            ui.end_row();

            ui.label("Playback");
            ui.monospace(format!("{}×", compute.playback_speed));
            ui.end_row();

            ui.label("Queued edits");
            ui.monospace(compute.pending_commands.to_string());
            ui.end_row();

            ui.label("Tick / time");
            ui.monospace(format!("{} / {:.4} s", compute.tick, compute.time_seconds));
            ui.end_row();

            ui.label("World revision");
            ui.monospace(compute.world_revision.to_string());
            ui.end_row();

            ui.label("Snapshot");
            ui.monospace(
                compute
                    .snapshot_sequence
                    .map_or_else(|| "None".to_owned(), |sequence| format!("#{sequence}")),
            );
            ui.end_row();

            ui.label("Freshness");
            ui.label(
                compute
                    .freshness
                    .map_or("No data", SnapshotFreshness::label),
            );
            ui.end_row();

            ui.label("Domain");
            ui.monospace(&compute.domain_summary)
                .on_hover_text(&compute.domain_summary);
            ui.end_row();

            ui.label("Samples");
            ui.monospace(compute.total_samples.to_string());
            ui.end_row();
        });

    if !compute.probe_readings.is_empty() {
        ui.collapsing("Probe samples", |ui| {
            for reading in &compute.probe_readings {
                ui.label(format!("{} · {}", reading.probe_name, reading.channel_name));
                match validity_note(reading.validity) {
                    Some(note) => {
                        ui.colored_label(egui::Color32::from_rgb(230, 150, 60), note);
                    }
                    None => {
                        ui.monospace(&reading.value);
                    }
                }
            }
        });
    }
}
