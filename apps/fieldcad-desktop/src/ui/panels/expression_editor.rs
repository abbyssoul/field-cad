//! Shared constant/expression editing primitives used by the Settings
//! window's user-constant library, the Variables inspector panel's document
//! constants, and object property expression editors.
//!
//! The three call sites commit edits through different mechanisms (a local
//! file save, an authoritative `CommitExpressions(AddConstant)` command, or
//! an authoritative `CommitExpressions(SetPropertyExpression)` command), so
//! the widgets here render UI and hand back a finished draft rather than
//! deciding how to commit it.

use fieldcad_expressions::{
    ConstantDefinition, ConstantId, ConstantScope, EvaluationPlan, ExpressionCommand,
    ExpressionDocument, UserConstantLibrary,
};
use fieldcad_simulation::CommandPayload;

use super::NoDistanceProvider;
use crate::ui::compute::ComputeView;
use crate::ui::{UiFrameOutput, UiModel};

/// A brand-new, uncommitted constant has no authoritative resolved value
/// yet at any call site, so its preview always compiles and evaluates
/// locally against the supplied constants plus the drafted candidate.
fn preview_line(
    ui: &mut egui::Ui,
    scope: ConstantScope,
    name: &str,
    source: &str,
    existing_constants: &[ConstantDefinition],
) {
    if name.trim().is_empty() || source.trim().is_empty() {
        return;
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
    match EvaluationPlan::compile(
        &ExpressionDocument {
            constants,
            bindings: Vec::new(),
        },
        |_| None,
    ) {
        Ok(mut plan) => match plan.evaluate(&NoDistanceProvider) {
            Ok(result) => {
                if let Some(value) = result.constants.get(&candidate_id) {
                    ui.small(format!(
                        "= {} {}",
                        value.si_value(),
                        value.dimension().unit_symbol()
                    ));
                }
            }
            Err(error) => {
                ui.colored_label(egui::Color32::from_rgb(220, 100, 90), error.to_string());
            }
        },
        Err(error) => {
            ui.colored_label(egui::Color32::from_rgb(220, 100, 90), error.to_string());
        }
    }
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
        let can_submit = !draft.name.trim().is_empty() && !draft.source.trim().is_empty();
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
    preview_line(ui, scope, &draft.name, &draft.source, existing_constants);

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
                let submit = (name.lost_focus() || source.lost_focus())
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if submit {
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
                    if !commands.is_empty() {
                        output.submit(CommandPayload::CommitExpressions(commands));
                    }
                }
                if constant.scope == ConstantScope::Document && ui.small_button("Delete").clicked()
                {
                    output.submit(CommandPayload::CommitExpressions(vec![
                        ExpressionCommand::RemoveConstant(constant.id),
                    ]));
                }
            });
            if let Some(value) = compute.resolved_constants.get(&constant.id) {
                ui.small(format!(
                    "= {} {}",
                    value.si_value(),
                    value.dimension().unit_symbol()
                ));
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
    if let Err(error) = EvaluationPlan::compile(
        &ExpressionDocument {
            constants: model.user_constants.constants.clone(),
            bindings: Vec::new(),
        },
        |_| None,
    ) {
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

    if save_library {
        match crate::user_constants::save(&model.user_constants) {
            Ok(()) => model.user_constants_status = None,
            Err(error) => model.user_constants_status = Some(error.to_string()),
        }
    }
}
