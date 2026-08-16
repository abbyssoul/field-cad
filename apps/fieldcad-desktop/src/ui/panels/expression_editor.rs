//! Shared constant/expression editing primitives used by the Settings
//! window's user-constant library, the Variables inspector panel's document
//! constants, and object property expression editors.
//!
//! The three call sites commit edits through different mechanisms (a local
//! file save, an authoritative `CommitExpressions(AddConstant)` command, or
//! an authoritative `CommitExpressions(SetPropertyExpression)` command), so
//! the widgets here render UI and hand back a finished draft rather than
//! deciding how to commit it.

use fieldcad_core::{PropertyKind, WorldSnapshot};
use fieldcad_expressions::{
    ConstantDefinition, ConstantId, ConstantScope, EvaluationPlan, ExpressionCommand,
    ExpressionDocument, UserConstantLibrary,
};
use fieldcad_simulation::CommandPayload;

use super::NoDistanceProvider;
use crate::ui::compute::ComputeView;
use crate::ui::{UiFrameOutput, UiModel};

pub(super) struct WorldDistanceProvider<'a>(pub &'a WorldSnapshot);

impl fieldcad_expressions::ValueProvider for WorldDistanceProvider<'_> {
    fn distance(&self, probe: fieldcad_core::DistanceProbeId) -> Option<f64> {
        self.0
            .distance_probe(probe)
            .and_then(|probe| self.0.resolve_distance(probe).ok())
    }
}

pub(super) fn preview_document(
    document: &ExpressionDocument,
    world: &WorldSnapshot,
) -> Result<fieldcad_expressions::EvaluationResult, fieldcad_expressions::ExpressionError> {
    let mut plan = EvaluationPlan::compile(document, |target| {
        let object = world.object(target.object)?;
        object.components.get(&target.component)?;
        let schema = world.component_schemas().get(&target.component)?;
        let property = schema
            .properties
            .iter()
            .find(|property| property.id == target.property)?;
        let PropertyKind::Scalar(dimension) = property.kind else {
            return None;
        };
        Some(fieldcad_expressions::PropertyBindingSchema {
            dimension,
            live_binding: property.live_binding,
        })
    })?;
    plan.evaluate(&WorldDistanceProvider(world))
}

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
#[derive(Clone, Debug)]
pub(super) struct ConstantDraft {
    pub name: String,
    pub source: String,
}

impl Default for ConstantDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            source: "1 m".to_owned(),
        }
    }
}

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
) -> Option<ConstantDraft> {
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
        .data_mut(|data| data.get_temp::<ConstantDraft>(draft_id))
        .unwrap_or_default();
    let preview = preview_constant(scope, &draft.name, &draft.source, existing_constants);

    let mut result = None;
    ui.horizontal(|ui| {
        ui.label(scope_prefix);
        ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .id(id.with("name"))
                .hint_text("name")
                .desired_width(90.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut draft.source)
                .id(id.with("source"))
                .desired_width(120.0),
        );
        extra_source_ui(ui, &mut draft.source);
        let can_submit = preview.as_ref().is_some_and(Result::is_ok);
        if ui
            .add_enabled(can_submit, egui::Button::new("Add"))
            .clicked()
        {
            result = Some(draft.clone());
        }
        if ui.small_button("Cancel").clicked() {
            open = false;
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
        draft = ConstantDraft::default();
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
pub(super) fn insert_constant_menu(
    ui: &mut egui::Ui,
    source: &mut String,
    expressions: &ExpressionDocument,
    user_constants: &UserConstantLibrary,
    output: &mut UiFrameOutput,
) {
    ui.menu_button("Insert", |ui| {
        for constant in &expressions.constants {
            let symbol = format!(
                "{}.{}",
                match constant.scope {
                    ConstantScope::Document => "doc",
                    ConstantScope::User => "user",
                },
                constant.name
            );
            if ui.button(&symbol).clicked() {
                source.push_str(&symbol);
                ui.close();
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
    });
}

#[derive(Clone, Debug)]
struct VariableDraft {
    name: String,
    source: String,
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
            data.get_temp::<VariableDraft>(id)
                .unwrap_or_else(|| VariableDraft {
                    name: constant.name.clone(),
                    source: constant.source.as_str().to_owned(),
                })
        });
        ui.group(|ui| {
            let mut commands = Vec::new();
            if draft.name != constant.name {
                commands.push(ExpressionCommand::RenameConstant {
                    constant: constant.id,
                    name: draft.name.clone(),
                });
            }
            if draft.source != constant.source.as_str() {
                commands.push(ExpressionCommand::SetConstantSource {
                    constant: constant.id,
                    source: draft.source.as_str().into(),
                });
            }
            let preview = if commands.is_empty() {
                None
            } else {
                Some(
                    compute
                        .expressions
                        .apply(commands.clone())
                        .and_then(|document| preview_document(&document, world)),
                )
            };
            ui.horizontal(|ui| {
                ui.label(match constant.scope {
                    ConstantScope::Document => "doc.",
                    ConstantScope::User => "user.",
                });
                let name = ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .id(id.with("name"))
                        .desired_width(90.0),
                );
                let source = ui.add(
                    egui::TextEdit::singleline(&mut draft.source)
                        .id(id.with("source"))
                        .desired_width(130.0),
                );
                insert_constant_menu(
                    ui,
                    &mut draft.source,
                    &compute.expressions,
                    user_constants,
                    output,
                );
                output.scene_edit_in_progress |= name.has_focus() || source.has_focus();
                let enter = (name.lost_focus() || source.lost_focus())
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let valid = preview.as_ref().is_some_and(Result::is_ok);
                if (enter || ui.add_enabled(valid, egui::Button::new("Apply")).clicked()) && valid {
                    output.submit(CommandPayload::CommitExpressions(commands.clone()));
                }
                if !commands.is_empty() && ui.small_button("Reset").clicked() {
                    draft.name = constant.name.clone();
                    draft.source = constant.source.as_str().to_owned();
                }
                if constant.scope == ConstantScope::Document && ui.small_button("Delete").clicked()
                {
                    output.submit(CommandPayload::CommitExpressions(vec![
                        ExpressionCommand::RemoveConstant(constant.id),
                    ]));
                }
            });
            if let Some(Err(error)) = &preview {
                ui.colored_label(egui::Color32::from_rgb(220, 100, 90), error.to_string());
                if let Some(span) = error.span {
                    ui.weak(format!("Source bytes {}..{}", span.start, span.end));
                }
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

    ui.separator();
    let control_id = ui.make_persistent_id("add-document-variable");
    let committed = add_constant_control(
        ui,
        control_id,
        ConstantScope::Document,
        "doc.",
        &compute.expressions.constants,
        |ui, source| {
            insert_constant_menu(ui, source, &compute.expressions, user_constants, output);
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
    let mut save_library = false;
    let mut remove = None;
    for constant in &mut model.user_constants.constants {
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
    let library_validation = EvaluationPlan::compile(
        &ExpressionDocument {
            constants: model.user_constants.constants.clone(),
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
    if let Some(id) = remove {
        model.user_constants.constants.retain(|item| item.id != id);
        save_library = true;
    }

    let control_id = ui.make_persistent_id("add-user-constant");
    let committed = add_constant_control(
        ui,
        control_id,
        ConstantScope::User,
        "user.",
        &model.user_constants.constants,
        |_ui, _source| {},
    );
    if let Some(draft) = committed {
        let next = model
            .user_constants
            .constants
            .iter()
            .map(|constant| constant.id.get() & ((1_u64 << 63) - 1))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            | (1_u64 << 63);
        model.user_constants.constants.push(ConstantDefinition {
            id: ConstantId::new(next),
            scope: ConstantScope::User,
            name: draft.name.trim().to_owned(),
            source: draft.source.trim().into(),
            revision: None,
            provenance: None,
        });
        save_library = true;
    }

    if save_library && library_validation.is_ok() {
        match crate::user_constants::save(&model.user_constants) {
            Ok(()) => model.user_constants_status = None,
            Err(error) => model.user_constants_status = Some(error.to_string()),
        }
    } else if save_library {
        model.user_constants_status = Some("Invalid library draft was not saved".to_owned());
    }
}
