//! Shared constant/expression editing primitives used by the Settings
//! window's user-constant library, the Variables inspector panel's document
//! constants, and object property expression editors.
//!
//! The three call sites commit edits through different mechanisms (a local
//! file save, an authoritative `CommitExpressions(AddConstant)` command, or
//! an authoritative `CommitExpressions(SetPropertyExpression)` command), so
//! the widgets here render UI and hand back a finished draft rather than
//! deciding how to commit it.

use fieldcad_core::{PluginId, WorldSnapshot};
use fieldcad_expressions::{
    ConstantDefinition, ConstantId, ConstantOrigin, ConstantScope, EvaluationPlan,
    ExpressionCommand, ExpressionDocument, ExpressionSubject, UserConstantLibrary,
};
use fieldcad_plugin_api::ExportedVariable;
use fieldcad_simulation::CommandPayload;

use super::NoDistanceProvider;
use crate::ui::compute::ComputeView;
use crate::ui::expression_draft::{
    AuthorityDraft, ConstantFields, ExistingConstantDraft, NewConstantDraft, SubmissionState,
    UserLibraryDraft, subject_label,
};
use crate::ui::{UiFrameOutput, UiModel};

/// A brand-new, uncommitted constant has no authoritative resolved value
/// yet at any call site, so its preview always compiles and evaluates
/// locally against the supplied constants plus the drafted candidate.
fn preview_constant(
    scope: ConstantScope,
    name: &str,
    source: &str,
    existing_constants: &[ConstantDefinition],
) -> Option<Result<fieldcad_expressions::ExpressionValue, fieldcad_expressions::ExpressionError>> {
    if name.trim().is_empty() || source.trim().is_empty() {
        return None;
    }
    let candidate_id = ConstantId::new(u64::MAX);
    let mut constants = existing_constants.to_vec();
    constants.push(ConstantDefinition {
        id: candidate_id,
        scope,
        name: name.trim().to_owned(),
        source: source.trim().into(),
        revision: None,
        provenance: None,
        origin: None,
    });
    Some(
        match EvaluationPlan::compile(
            &ExpressionDocument {
                constants,
                bindings: Vec::new(),
            },
            |_| None,
        ) {
            Ok(mut plan) => match plan.evaluate(&NoDistanceProvider) {
                Ok(result) => Ok(result.constants[&candidate_id]),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
    )
}

/// A staged, uncommitted name+source pair for a new constant.
/// Renders a small "Add new" button that reveals a staged name+source row
/// with a resolved/dry-run preview line, and an explicit "Add"/"Cancel"
/// pair. Returns the committed draft exactly once, the frame the user
/// clicks "Add"; the caller allocates an id and decides how to commit it
/// (a local library save, or a `CommitExpressions(AddConstant)` command).
#[allow(clippy::too_many_arguments)]
pub(super) fn add_constant_control(
    ui: &mut egui::Ui,
    id: egui::Id,
    scope: ConstantScope,
    scope_prefix: &str,
    existing_constants: &[ConstantDefinition],
    extra_source_ui: impl FnOnce(&mut egui::Ui, &mut String),
) -> Option<ConstantFields> {
    let open_id = id.with("open");
    let mut open = ui
        .data_mut(|data| data.get_temp::<bool>(open_id))
        .unwrap_or(false);

    if !open {
        if ui
            .button("Add new")
            .on_hover_text(format!(
                "Define a new named constant referenced as {scope_prefix}name"
            ))
            .clicked()
        {
            open = true;
        }
        ui.data_mut(|data| data.insert_temp(open_id, open));
        return None;
    }

    let draft_id = id.with("draft");
    let mut draft = ui
        .data_mut(|data| data.get_temp::<NewConstantDraft>(draft_id))
        .unwrap_or_else(|| {
            NewConstantDraft(AuthorityDraft::new(
                ConstantFields {
                    name: String::new(),
                    source: "1 m".to_owned(),
                },
                "new",
            ))
        });
    let mut result = None;
    let mut preview = None;
    ui.horizontal(|ui| {
        ui.label(scope_prefix);
        let fields = draft.0.edited_mut();
        let name = ui.add(
            egui::TextEdit::singleline(&mut fields.name)
                .id(id.with("name"))
                .hint_text("name")
                .desired_width(90.0),
        );
        let source = ui.add(
            egui::TextEdit::singleline(&mut fields.source)
                .id(id.with("source"))
                .desired_width(120.0),
        );
        extra_source_ui(ui, &mut fields.source);
        preview = preview_constant(scope, &fields.name, &fields.source, existing_constants);
        let can_submit = preview.as_ref().is_some_and(Result::is_ok);
        let enter = (name.lost_focus() || source.lost_focus())
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if (enter
            || ui
                .add_enabled(can_submit, egui::Button::new("Add"))
                .clicked())
            && can_submit
        {
            result = Some(fields.clone());
        }
        let escape = (name.has_focus() || source.has_focus())
            && ui.input(|input| input.key_pressed(egui::Key::Escape));
        if ui.small_button("Cancel").clicked() || escape {
            open = false;
            draft.0.reset();
        }
    });
    match preview {
        Some(Ok(value)) => {
            ui.small(format!(
                "= {} {}",
                value.si_value(),
                value.dimension().unit_symbol()
            ));
        }
        Some(Err(error)) => {
            ui.colored_label(egui::Color32::from_rgb(220, 100, 90), error.to_string());
            if let Some(span) = error.span {
                ui.weak(format!("Source bytes {}..{}", span.start, span.end));
            }
        }
        None => {}
    }

    if result.is_some() {
        open = false;
        draft.0.reset();
    }
    ui.data_mut(|data| {
        data.insert_temp(open_id, open);
        data.insert_temp(draft_id, draft);
    });
    result
}

/// A menu listing every constant already available to the document, plus
/// every not-yet-embedded user-library constant (embedding it on click),
/// appending the chosen symbol to `source`.
#[allow(clippy::too_many_arguments)]
pub(super) fn insert_constant_menu(
    ui: &mut egui::Ui,
    source: &mut String,
    expressions: &ExpressionDocument,
    user_constants: &UserConstantLibrary,
    global_variables: &[(PluginId, ExportedVariable)],
    world: &WorldSnapshot,
    allow_distances: bool,
    output: &mut UiFrameOutput,
) {
    ui.menu_button("Insert", |ui| {
        for constant in &expressions.constants {
            let symbol = format!(
                "{}.{}",
                match constant.scope {
                    ConstantScope::Document => "doc",
                    ConstantScope::User => "user",
                    ConstantScope::Global => "global",
                },
                constant.name
            );
            if ui.button(&symbol).clicked() {
                source.push_str(&symbol);
                ui.close();
            }
        }
        if !global_variables.is_empty() {
            ui.separator();
            for (plugin, variable) in global_variables {
                let symbol = format!("global.{plugin}.{}", variable.property);
                if ui.button(&symbol).clicked() {
                    source.push_str(&symbol);
                    ui.close();
                }
            }
        }
        if !user_constants.constants.is_empty() {
            ui.separator();
            for constant in &user_constants.constants {
                let symbol = format!("user.{}", constant.name);
                if ui.button(&symbol).clicked() {
                    match user_constants.dependency_closure(&constant.name, "user-constants.json") {
                        Ok(closure) => output.submit(CommandPayload::CommitExpressions(vec![
                            ExpressionCommand::ImportUserConstants(closure),
                        ])),
                        Err(error) => {
                            output.scene_edit_in_progress = false;
                            ui.label(error.to_string());
                        }
                    }
                    source.push_str(&symbol);
                    ui.close();
                }
            }
        }
        if allow_distances && !world.distance_probes().is_empty() {
            ui.separator();
            for (label, token) in crate::ui::expression_draft::distance_insertions(world) {
                // The label follows presentation renames; the inserted token
                // is the durable numeric identity.
                if ui.button(label).clicked() {
                    source.push_str(&token);
                    ui.close();
                }
            }
        }
    });
}

/// The "Variables" inspector panel: document and embedded user constants.
pub(super) fn variables_editor(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    compute: &ComputeView,
    user_constants: &UserConstantLibrary,
    output: &mut UiFrameOutput,
) {
    if compute.expressions.constants.is_empty() {
        ui.weak("No document or embedded user constants.");
    }
    for constant in &compute.expressions.constants {
        let id = ui.make_persistent_id(("variable", constant.id));
        let mut draft = ui.data_mut(|data| {
            data.get_temp::<ExistingConstantDraft>(id)
                .unwrap_or_else(|| {
                    ExistingConstantDraft(AuthorityDraft::new(
                        ConstantFields {
                            name: constant.name.clone(),
                            source: constant.source.as_str().to_owned(),
                        },
                        compute.expression_state.graph_hash.clone(),
                    ))
                })
        });
        draft.0.reconcile(
            ConstantFields {
                name: constant.name.clone(),
                source: constant.source.as_str().to_owned(),
            },
            compute.expression_state.graph_hash.clone(),
        );
        ui.group(|ui| {
            let mut enter = false;
            let mut escape = false;
            ui.horizontal(|ui| {
                ui.label(match constant.scope {
                    ConstantScope::Document => "doc.",
                    ConstantScope::User => "user.",
                    ConstantScope::Global => "global.",
                });
                let edited = draft.0.edited_mut();
                let name = ui.add(
                    egui::TextEdit::singleline(&mut edited.name)
                        .id(id.with("name"))
                        .desired_width(90.0),
                );
                let source = ui.add(
                    egui::TextEdit::singleline(&mut edited.source)
                        .id(id.with("source"))
                        .desired_width(130.0),
                );
                insert_constant_menu(
                    ui,
                    &mut edited.source,
                    &compute.expressions,
                    user_constants,
                    &compute.global_variables,
                    world,
                    true,
                    output,
                );
                output.scene_edit_in_progress |= name.has_focus() || source.has_focus();
                enter = (name.lost_focus() || source.lost_focus())
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                escape = (name.has_focus() || source.has_focus())
                    && ui.input(|input| input.key_pressed(egui::Key::Escape));
            });
            let fields = draft.0.edited().clone();
            let mut commands = Vec::new();
            if fields.name != constant.name {
                commands.push(ExpressionCommand::RenameConstant {
                    constant: constant.id,
                    name: fields.name.clone(),
                });
            }
            if fields.source != constant.source.as_str() {
                commands.push(ExpressionCommand::SetConstantSource {
                    constant: constant.id,
                    source: fields.source.as_str().into(),
                });
            }
            let preview = if commands.is_empty() {
                None
            } else {
                Some(match compute.expressions.apply(commands.clone()) {
                    Ok(document) => crate::ui::expression_draft::preview_document(
                        &document,
                        world,
                        ExpressionSubject::Constant(constant.id),
                        &compute.expression_state,
                    ),
                    Err(error) => crate::ui::expression_draft::DraftPreview {
                        values: None,
                        diagnostic: Some(fieldcad_expressions::ExpressionDiagnostic {
                            subject: ExpressionSubject::Constant(constant.id),
                            error,
                        }),
                        dependents: Vec::new(),
                    },
                })
            };
            ui.horizontal(|ui| {
                let valid = preview.as_ref().is_some_and(|preview| preview.valid());
                if (enter || ui.add_enabled(valid, egui::Button::new("Apply")).clicked()) && valid {
                    output.submit(CommandPayload::CommitExpressions(commands.clone()));
                    draft.0.mark_submitted();
                }
                if !commands.is_empty() && (escape || ui.small_button("Reset").clicked()) {
                    draft.0.reset();
                }
                if constant.scope == ConstantScope::Document && ui.small_button("Delete").clicked()
                {
                    output.submit(CommandPayload::CommitExpressions(vec![
                        ExpressionCommand::RemoveConstant(constant.id),
                    ]));
                }
            });
            if draft.0.authority_changed() {
                ui.weak("Authoritative value changed; Reset restores the latest accepted value.");
            } else if draft.0.submission() == SubmissionState::Submitted {
                ui.weak("Awaiting authoritative acknowledgement…");
            }
            if let Some(diagnostic) = preview
                .as_ref()
                .and_then(|preview| preview.diagnostic.as_ref())
            {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 100, 90),
                    diagnostic.error.to_string(),
                );
                if let Some(span) = diagnostic.error.span {
                    ui.weak(format!("Source bytes {}..{}", span.start, span.end));
                }
            }
            if let Some(preview) = &preview
                && !preview.dependents.is_empty()
            {
                ui.weak(format!(
                    "Affected: {}",
                    preview
                        .dependents
                        .iter()
                        .map(|subject| subject_label(subject, &compute.expressions))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(value) = compute.resolved_constants.get(&constant.id) {
                ui.small(format!(
                    "= {} {}",
                    value.si_value(),
                    value.dimension().unit_symbol()
                ));
            }
            if let Some(state) = compute.expression_state.nodes.iter().find(|node| {
                node.subject == fieldcad_expressions::ExpressionSubject::Constant(constant.id)
            }) {
                if !state.dependencies.is_empty() {
                    let dependencies = state
                        .dependencies
                        .iter()
                        .map(|dependency| match dependency {
                            fieldcad_expressions::ExpressionDependency::Constant(id) => compute
                                .expressions
                                .constants
                                .iter()
                                .find(|candidate| candidate.id == *id)
                                .map(|candidate| {
                                    format!(
                                        "{}.{}",
                                        match candidate.scope {
                                            ConstantScope::Document => "doc",
                                            ConstantScope::User => "user",
                                            ConstantScope::Global => "global",
                                        },
                                        candidate.name
                                    )
                                })
                                .unwrap_or_else(|| format!("constant {}", id.get())),
                            fieldcad_expressions::ExpressionDependency::Distance(id) => {
                                format!("distance.{}", id.get())
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.weak(format!("Depends on: {dependencies}"));
                }
                if state.status != fieldcad_expressions::ExpressionNodeStatus::Resolved {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 100, 90),
                        format!("Dependency status: {:?}", state.status),
                    );
                }
            }
            if let Some(provenance) = &constant.provenance {
                ui.weak(format!("Embedded from {provenance}"));
            }
            if constant.scope == ConstantScope::User
                && user_constants
                    .available_updates(&compute.expressions)
                    .contains(&constant.id)
                && ui.button("Refresh embedded dependency closure").clicked()
            {
                match user_constants.dependency_closure(&constant.name, "user-constants.json") {
                    Ok(closure) => output.submit(CommandPayload::CommitExpressions(vec![
                        ExpressionCommand::ImportUserConstants(closure),
                    ])),
                    Err(error) => {
                        ui.colored_label(egui::Color32::from_rgb(220, 100, 90), error.to_string());
                    }
                }
            }
        });
        ui.data_mut(|data| data.insert_temp(id, draft));
    }

    if !compute.global_variables.is_empty() {
        ui.separator();
        ui.label("Global constants");
        ui.small("Registered by active plugins. Import to override a value for this document.");
        for (plugin, variable) in &compute.global_variables {
            ui.horizontal(|ui| {
                let label = ui
                    .label(format!("global.{plugin}.{}", variable.property))
                    .on_hover_text(
                        variable
                            .description
                            .clone()
                            .unwrap_or_else(|| "No description".to_owned()),
                    );
                let _ = label;
                ui.small(format!(
                    "= {} {}",
                    variable.default_value.si_value(),
                    variable.default_value.dimension().unit_symbol()
                ));
                let already_imported = compute.expressions.constants.iter().any(|constant| {
                    matches!(
                        &constant.origin,
                        Some(ConstantOrigin::GlobalVariable {
                            plugin: origin_plugin,
                            property,
                        }) if origin_plugin == plugin && *property == variable.property
                    )
                });
                if already_imported {
                    ui.weak("Imported");
                } else if ui.small_button("Import").clicked() {
                    let next = compute
                        .expressions
                        .constants
                        .iter()
                        .filter(|constant| constant.scope == ConstantScope::Document)
                        .map(|constant| constant.id.get())
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1)
                        .min((1_u64 << 63) - 1);
                    output.submit(CommandPayload::CommitExpressions(vec![
                        ExpressionCommand::ImportGlobalConstants(vec![ConstantDefinition {
                            id: ConstantId::new(next),
                            scope: ConstantScope::Document,
                            name: variable.property.as_str().to_owned(),
                            source: fieldcad_expressions::format_quantity_literal(
                                variable.default_value,
                            ),
                            revision: None,
                            provenance: Some(format!("plugin {plugin}")),
                            origin: Some(ConstantOrigin::GlobalVariable {
                                plugin: plugin.clone(),
                                property: variable.property.clone(),
                            }),
                        }]),
                    ]));
                }
            });
        }
    }

    ui.separator();
    let control_id = ui.make_persistent_id("add-document-variable");
    let committed = add_constant_control(
        ui,
        control_id,
        ConstantScope::Document,
        "doc.",
        &compute.expressions.constants,
        |ui, source| {
            insert_constant_menu(
                ui,
                source,
                &compute.expressions,
                user_constants,
                &compute.global_variables,
                world,
                true,
                output,
            );
        },
    );
    if let Some(draft) = committed {
        let next = compute
            .expressions
            .constants
            .iter()
            .filter(|constant| constant.scope == ConstantScope::Document)
            .map(|constant| constant.id.get())
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .min((1_u64 << 63) - 1);
        output.submit(CommandPayload::CommitExpressions(vec![
            ExpressionCommand::AddConstant(ConstantDefinition {
                id: ConstantId::new(next),
                scope: ConstantScope::Document,
                name: draft.name.trim().to_owned(),
                source: draft.source.trim().into(),
                revision: None,
                provenance: None,
                origin: None,
            }),
        ]));
    }
}

/// The Settings window's "Reusable constants" (user-scoped) library editor.
pub(super) fn user_constants_editor(ui: &mut egui::Ui, model: &mut UiModel) {
    ui.small("Documents embed an explicit dependency closure and never follow this file silently.");
    if let Some(status) = &model.user_constants_status {
        ui.colored_label(egui::Color32::from_rgb(220, 100, 90), status);
    }
    let authoritative_hash = ExpressionDocument {
        constants: model.user_constants.constants.clone(),
        bindings: Vec::new(),
    }
    .content_hash();
    let editor_id = ui.make_persistent_id("user-constant-library-draft");
    let mut draft = ui.data_mut(|data| {
        data.get_temp::<UserLibraryDraft>(editor_id)
            .unwrap_or_else(|| {
                UserLibraryDraft(AuthorityDraft::new(
                    model.user_constants.clone(),
                    authoritative_hash.clone(),
                ))
            })
    });
    draft
        .0
        .reconcile(model.user_constants.clone(), authoritative_hash);
    let mut save_library = false;
    let mut remove = None;
    for constant in &mut draft.0.edited_mut().constants {
        ui.horizontal(|ui| {
            let mut source_text = constant.source.as_str().to_owned();
            ui.label("user.");
            let name = ui.add(egui::TextEdit::singleline(&mut constant.name).desired_width(90.0));
            let source = ui.add(egui::TextEdit::singleline(&mut source_text).desired_width(140.0));
            if source.changed() {
                constant.source = source_text.as_str().into();
            }
            save_library |= name.lost_focus() || source.lost_focus();
            if ui.small_button("Delete").clicked() {
                remove = Some(constant.id);
            }
        });
    }
    if let Some(id) = remove {
        draft.0.edited_mut().constants.retain(|item| item.id != id);
        save_library = true;
    }

    let control_id = ui.make_persistent_id("add-user-constant");
    let committed = add_constant_control(
        ui,
        control_id,
        ConstantScope::User,
        "user.",
        &draft.0.edited().constants,
        |_ui, _source| {},
    );
    if let Some(fields) = committed {
        let next = draft
            .0
            .edited()
            .constants
            .iter()
            .map(|constant| constant.id.get() & ((1_u64 << 63) - 1))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            | (1_u64 << 63);
        draft.0.edited_mut().constants.push(ConstantDefinition {
            id: ConstantId::new(next),
            scope: ConstantScope::User,
            name: fields.name.trim().to_owned(),
            source: fields.source.trim().into(),
            revision: None,
            provenance: None,
            origin: None,
        });
        save_library = true;
    }

    let library_validation = EvaluationPlan::compile(
        &ExpressionDocument {
            constants: draft.0.edited().constants.clone(),
            bindings: Vec::new(),
        },
        |_| None,
    );
    if let Err(error) = &library_validation {
        ui.colored_label(
            egui::Color32::from_rgb(220, 100, 90),
            format!("Library diagnostic: {error}"),
        );
    }

    if save_library && library_validation.is_ok() {
        let candidate = draft.0.edited().clone();
        match crate::user_constants::save(&candidate) {
            Ok(()) => {
                model.user_constants = candidate.clone();
                model.user_constants_status = None;
                draft.0.reconcile(
                    candidate.clone(),
                    ExpressionDocument {
                        constants: candidate.constants,
                        bindings: Vec::new(),
                    }
                    .content_hash(),
                );
            }
            Err(error) => model.user_constants_status = Some(error.to_string()),
        }
    } else if save_library {
        model.user_constants_status = Some("Invalid library draft was not saved".to_owned());
    }
    if draft.0.authority_changed() {
        ui.weak("The library changed on disk; local edits are preserved until saved or reset.");
    }
    if draft.0.dirty() && ui.small_button("Reset library draft").clicked() {
        draft.0.reset();
    }
    ui.data_mut(|data| data.insert_temp(editor_id, draft));
}
