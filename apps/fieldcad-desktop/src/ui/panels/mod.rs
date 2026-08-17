//! Sub-module panel files and shared panel helpers.
//!
//! This module re-exports everything the top-level UI layout needs from the
//! individual panel files and provides shared helper functions used across
//! multiple panel sub-modules.

mod catalog;
mod diagnostics;
mod distance_probe_inspector;
mod expression_editor;
mod inspector;
mod mass_aggregate_probe_inspector;
mod mcp;
mod menu_bar;
mod object_inspector;
mod probe_inspector;
mod queue;
mod scene_tree;
mod settings;
mod shape_inspector;
mod world_inspector;

// ── Public surface for ui/mod.rs ───────────────────────────────────────────

pub use catalog::{catalog_propagation_window, catalog_window};
pub use diagnostics::diagnostics_window;
pub use inspector::inspector;
pub use mcp::mcp_window;
pub use menu_bar::{field_brush_dialog, menu_bar};
pub use queue::queue_window;
pub use scene_tree::scene_tree;
pub use settings::settings_window;

// ── Re-exports for sibling sub-modules ─────────────────────────────────────
// Used via `super::section(...)` etc. in function-body path expressions.
pub(super) use super::{
    CameraAction, flow_line_display_controls, section, split_add_button,
    trajectory_display_controls, vector_display_controls,
};

// ── Helpers used by tests ──────────────────────────────────────────────────
// Each sub-module has its own private copy; these are here so that
// `#[cfg(test)] mod tests` can reference them through `super::*`.
pub(super) fn note_held_edit(response: &egui::Response, editing: &mut bool) -> bool {
    *editing |= response.dragged() || response.has_focus();
    response.changed()
}

pub(super) fn coordinate_editor(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    dimension: fieldcad_core::Dimension,
    editing: &mut bool,
) -> bool {
    let mut drag = egui::DragValue::new(value)
        .speed(0.02)
        .prefix(format!("{label}: "));

    if dimension.si_prefix_root().is_some() {
        // SI prefix supported — formatter includes the unit, no suffix.
        drag = drag
            .custom_formatter(move |val, _| fieldcad_core::format_si_value(val, dimension).unwrap())
            .custom_parser(move |text| {
                fieldcad_core::parse_si_value(text, dimension)
                    .or_else(|| text.trim().parse().ok())
                    .or_else(|| evaluate_literal_expression(text, dimension))
            });
    } else {
        // Compound dimension — just show the unit symbol as a suffix.
        let symbol = dimension.unit_symbol();
        let suffix = if symbol.is_empty() {
            symbol
        } else {
            format!(" {symbol}")
        };
        drag = drag.suffix(suffix).custom_parser(move |text| {
            text.trim()
                .parse()
                .ok()
                .or_else(|| evaluate_literal_expression(text, dimension))
        });
    }

    let response = ui.add(drag);
    note_held_edit(&response, editing)
}

/// One-shot dimension-checked calculator for plain numeric-entry fields
/// (position, velocity, extent, domain bounds, …): parses `text` as an
/// authored arithmetic/unit-literal expression and returns its SI
/// magnitude if it evaluates to exactly `dimension`. Unlike
/// `expression_editor`'s component-property bindings, the result is not
/// persisted as a formula — only the literal number is kept, and there is
/// no `doc.`/`user.` constant reference support, matching the "type 3/2 mm
/// and it evaluates" convenience these fields need rather than the full
/// authoritative expression-binding UX.
pub(super) fn evaluate_literal_expression(
    text: &str,
    dimension: fieldcad_core::Dimension,
) -> Option<f64> {
    evaluate_expression_text(text, dimension)
        .or_else(|| evaluate_trailing_unit_expression(text, dimension))
}

/// Compile and evaluate `text` as a single self-contained expression,
/// accepting it only if it resolves to exactly `dimension`.
fn evaluate_expression_text(text: &str, dimension: fieldcad_core::Dimension) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let candidate_id = fieldcad_expressions::ConstantId::new(0);
    let document = fieldcad_expressions::ExpressionDocument {
        constants: vec![fieldcad_expressions::ConstantDefinition {
            id: candidate_id,
            scope: fieldcad_expressions::ConstantScope::Document,
            name: "value".to_owned(),
            source: text.into(),
            revision: None,
            provenance: None,
        }],
        bindings: Vec::new(),
    };
    let mut plan = fieldcad_expressions::EvaluationPlan::compile(&document, |_| None).ok()?;
    let result = plan.evaluate(&NoDistanceProvider).ok()?;
    let value = result.constants.get(&candidate_id)?;
    (value.dimension() == dimension).then_some(value.si_value())
}

/// Natural shorthand where a single trailing unit scales the whole authored
/// expression, e.g. `"120 + 15 Mm"` meaning `"(120 + 15) Mm"`.
///
/// `fieldcad-expressions` binds a unit identifier at multiplicative
/// precedence, so it multiplies only the term immediately to its left —
/// correct and necessary for genuinely mixed-dimension sums like
/// `"5 km + 200 m"`, but it rejects `"120 + 15 Mm"` outright, since `120`
/// alone is dimensionless and can't add to a length. This fallback only
/// runs after that strict evaluation has already failed, so it never
/// changes the result of an expression that was valid on its own terms —
/// it splits off exactly one trailing run of alphabetic characters,
/// evaluates it alone as the unit (e.g. `"Mm"` → its SI conversion factor),
/// evaluates everything before it as a plain dimensionless number, and
/// multiplies the two. Compound trailing units (e.g. `"m/s"`) aren't
/// split off, since only a contiguous alphabetic run is treated as the
/// unit.
fn evaluate_trailing_unit_expression(
    text: &str,
    dimension: fieldcad_core::Dimension,
) -> Option<f64> {
    let trimmed = text.trim();
    let mut split_at = trimmed.len();
    for (index, character) in trimmed.char_indices().rev() {
        if character.is_alphabetic() {
            split_at = index;
        } else {
            break;
        }
    }
    if split_at == 0 || split_at == trimmed.len() {
        return None;
    }
    let (head, unit_token) = trimmed.split_at(split_at);
    let head = head.trim_end();
    if head.is_empty() {
        return None;
    }
    let per_unit = evaluate_expression_text(unit_token, dimension)?;
    let factor = evaluate_expression_text(head, fieldcad_core::Dimension::DIMENSIONLESS)?;
    Some(factor * per_unit)
}

/// Shorthand for [`evaluate_literal_expression`] with a bare, unit-less
/// count/ratio field (arrow density, cell counts, scale factors, …), where
/// a plain typed number already carries [`fieldcad_core::Dimension::DIMENSIONLESS`].
pub(super) fn evaluate_dimensionless_expression(text: &str) -> Option<f64> {
    evaluate_literal_expression(text, fieldcad_core::Dimension::DIMENSIONLESS)
}

/// A `ValueProvider` for local one-shot/dry-run expression evaluation
/// outside the authoritative runtime: no distance probes exist to resolve,
/// so every reference misses.
pub(super) struct NoDistanceProvider;

impl fieldcad_expressions::ValueProvider for NoDistanceProvider {
    fn distance(&self, _probe: fieldcad_core::DistanceProbeId) -> Option<f64> {
        None
    }
}

pub(super) fn name_editor(
    ui: &mut egui::Ui,
    source: impl std::hash::Hash + std::fmt::Debug,
    name: &str,
) -> Option<String> {
    let id = ui.make_persistent_id(source);
    let mut draft = ui.data_mut(|data| {
        data.get_temp::<String>(id)
            .unwrap_or_else(|| name.to_owned())
    });
    let response = ui
        .horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut draft)
                    .id(id)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner;
    let cancel = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let accept = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

    if cancel {
        ui.data_mut(|data| data.remove::<String>(id));
        None
    } else if accept {
        ui.data_mut(|data| data.remove::<String>(id));
        (draft != name).then_some(draft)
    } else if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, draft));
        None
    } else {
        ui.data_mut(|data| data.remove::<String>(id));
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::ui::compute::format_engineering;
    use fieldcad_core::{
        ChannelId, Dimension, ObjectId, ObjectShape, ObjectSpec, Transform, Velocity, World,
        WorldCommand,
        quantities::{MassKg, kilogram},
    };
    use fieldcad_simulation::CommandPayload;
    use fieldcad_sources::{
        gravitational_mass_component_schema, independent_gravitational_mass_properties,
        inertial_mass_component_schema, linked_gravitational_mass_properties, mass_property_id,
    };
    use fieldcad_test_field::vector_channel_id;
    use glam::{DQuat, DVec3};

    use super::super::tests::{seeded_world, source};
    use super::{
        super::{ChannelLayerSettings, ComputeView, FrameContext, UiFrameOutput, UiModel},
        *,
    };
    use crate::camera::Projection;
    use crate::mcp::McpSession;
    use fieldcad_simulation::{DistanceHistory, MassAggregateHistory, ProbeHistory};

    // Import test-referenced functions from sibling modules
    use super::distance_probe_inspector::distance_probe_properties;
    use super::menu_bar::history_controls;
    use super::object_inspector::{
        format_vector, inertial_mass_kg, motion_summary, property_editor,
    };
    use super::probe_inspector::attachment_offset;
    use super::scene_tree::new_object_command;
    use super::shape_inspector::{plane_field_layers, plane_properties};
    use super::world_inspector::{field_system_controls, realtime_control, transport_sampling};

    #[test]
    fn coordinate_editor_arithmetic_expressions_evaluate_to_si_value() {
        assert_eq!(
            evaluate_literal_expression("3/2 mm", Dimension::LENGTH),
            Some(1.5e-3)
        );
        assert_eq!(
            evaluate_literal_expression("6400 / 2 * 1e3 km", Dimension::LENGTH),
            Some(3_200_000_000.0)
        );
        // Wrong dimension.
        assert_eq!(evaluate_literal_expression("1 kg", Dimension::LENGTH), None);
        // Malformed.
        assert_eq!(evaluate_literal_expression("1 /", Dimension::LENGTH), None);
        // A bare number, with no unit suffix, is dimensionless and evaluates
        // through the unit-less count/ratio shorthand used by arrow density,
        // cell counts, and similar fields.
        assert_eq!(evaluate_dimensionless_expression("3/2"), Some(1.5));
        assert_eq!(evaluate_dimensionless_expression("1 mm"), None);
        // Blank stays a miss, deferring to the caller's other parse fallbacks.
        assert_eq!(evaluate_literal_expression("  ", Dimension::LENGTH), None);
    }

    #[test]
    fn coordinate_editor_expressions_apply_a_trailing_unit_to_the_whole_expression() {
        // `fieldcad-expressions` binds a unit at multiplicative precedence,
        // so a unit not directly adjacent to the whole expression (past a
        // `+`/`-` outside parentheses) fails to compile as one self-
        // contained expression and needs `evaluate_trailing_unit_expression`'s
        // fallback. Every case below is checked against the same +-*/()
        // arithmetic evaluated in plain f64, in every order, whether or not
        // it happens to need the fallback — the two must always agree.
        let cases: &[(&str, f64)] = &[
            ("30/2 Mm", 30.0 / 2.0),                 // direct: unit adjacent to term
            ("120+15 Mm", 120.0 + 15.0),             // fallback: + before unit
            ("120+30/2 Mm", 120.0 + 30.0 / 2.0),     // fallback: + then /
            ("120-15 Mm", 120.0 - 15.0),             // fallback: - before unit
            ("120-30/2 Mm", 120.0 - 30.0 / 2.0),     // fallback: - then /
            ("120*2-15 Mm", 120.0 * 2.0 - 15.0),     // fallback: * before -
            ("120-15*2 Mm", 120.0 - 15.0 * 2.0),     // fallback: - before *, * binds tighter
            ("1+2+3 Mm", 1.0 + 2.0 + 3.0),           // fallback: left-associative + chain
            ("(120+15)/3 Mm", (120.0 + 15.0) / 3.0), // direct: parens make the sum atomic
            ("(120-15)*2 Mm", (120.0 - 15.0) * 2.0), // direct: parens, then *
            ("120/(2+3) Mm", 120.0 / (2.0 + 3.0)),   // direct: parens on the right of /
            ("(120-15)/(3+2) Mm", (120.0 - 15.0) / (3.0 + 2.0)), // direct: parens on both sides
        ];
        for (source, expected) in cases {
            let actual = evaluate_literal_expression(source, Dimension::LENGTH);
            let expected_si = expected * 1.0e6; // Mm -> m
            match actual {
                Some(actual) => assert!(
                    (actual - expected_si).abs() < 1.0e-6,
                    "{source}: expected {expected_si}, got {actual}"
                ),
                None => panic!("{source}: expected {expected_si}, got None"),
            }
        }
    }

    #[test]
    fn an_idle_name_editor_does_not_cache_the_authoritative_name() {
        let context = egui::Context::default();
        let source = ("name_editor_test", ObjectId::new(1));
        let mut id = None;
        let _ = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                id = Some(ui.make_persistent_id(source));
                assert_eq!(name_editor(ui, source, "old name"), None);
            });
        });

        assert!(
            context
                .data(|data| data.get_temp::<String>(id.unwrap()))
                .is_none(),
            "an untouched editor must follow a later authoritative world refresh"
        );
    }

    fn painted_text(shape: &egui::epaint::Shape, output: &mut String) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                output.push_str(&text.galley.job.text);
                output.push('\n');
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    painted_text(shape, output);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn field_system_details_are_collapsed_by_default_in_the_narrow_inspector() {
        let world = seeded_world();
        let source = source();
        let compute = ComputeView::build(&source, &world.snapshot(), None);
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(280.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            field_system_controls(
                ui,
                &mut UiModel::new(),
                &compute,
                false,
                &mut UiFrameOutput::default(),
            );
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }

        assert!(text.contains("Analytic test field"));
        assert!(
            !text.contains("Linear scalar"),
            "expanded channel details overcrowd the inspector: {text}"
        );
    }

    #[test]
    fn transport_sampling_does_not_submit_when_nothing_was_dragged() {
        let world = seeded_world();
        let source = source();
        let compute = ComputeView::build(&source, &world.snapshot(), None);
        let context = egui::Context::default();
        let mut output = UiFrameOutput::default();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            transport_sampling(ui, &compute, &mut output);
        });

        assert!(
            output.commands.is_empty(),
            "rendering the transport-density fields without touching any of \
             them must not submit a subscription change: {:?}",
            output.commands
        );
    }

    #[test]
    fn the_transport_bar_offers_undo_and_redo_for_what_the_source_recorded() {
        let mut compute = ComputeView::build(&source(), &seeded_world().snapshot(), None);
        compute.edit_history = fieldcad_simulation::EditHistoryStatus {
            undo: Some("Move object".to_owned()),
            redo: None,
            undo_depth: 1,
            redo_depth: 0,
        };

        let (commands, enabled) = drive_history_controls(&compute, false);
        assert!(enabled, "a paused, connected source can step back");
        assert_eq!(commands, vec![CommandPayload::Undo]);

        let (commands, enabled) = drive_history_controls(&compute, true);
        assert!(!enabled);
        assert!(commands.is_empty());

        compute.mode = fieldcad_core::SimulationMode::Running;
        let (commands, enabled) = drive_history_controls(&compute, false);
        assert!(!enabled);
        assert!(commands.is_empty());
        assert!(!compute.accepts_history_commands());
    }

    #[test]
    fn undo_is_offered_as_nothing_to_do_when_the_history_is_empty() {
        let compute = ComputeView::build(&source(), &seeded_world().snapshot(), None);
        assert!(!compute.edit_history.can_undo());

        let (commands, enabled) = drive_history_controls(&compute, false);

        assert!(!enabled);
        assert!(commands.is_empty());
    }

    fn drive_history_controls(
        compute: &ComputeView,
        edit_in_progress: bool,
    ) -> (Vec<CommandPayload>, bool) {
        let context = egui::Context::default();
        let world = seeded_world().snapshot();
        let history = ProbeHistory::default();
        let distance_history = DistanceHistory::default();
        let mass_aggregate_history = MassAggregateHistory::default();

        let run = |events: Vec<egui::Event>| {
            let mut output = UiFrameOutput::default();
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 120.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                ui.horizontal(|ui| {
                    history_controls(
                        ui,
                        &FrameContext {
                            compute,
                            catalog: &fieldcad_catalog::CatalogLoadReport::default(),
                            quick_add_hidden: &[],
                            world: &world,
                            probe_history: &history,
                            distance_history: &distance_history,
                            mass_aggregate_history: &mass_aggregate_history,
                            is_recording: false,
                            adapter_name: "Test adapter",
                            frame_time_ms: 16.0,
                            active_translation: None,
                            plane_normal_label: None,
                            plane_normal_active: false,
                            paused_for_edit: false,
                            edit_in_progress,
                            projection: Projection::default(),
                            camera_distance: 12.0,
                            camera_yaw: 0.0,
                            camera_pitch: 0.0,
                            mcp: &McpSession::Disabled,
                            frame_history: &[],
                            frame_min_ms: 0.0,
                            frame_max_ms: 0.0,
                            process_rss_kb: 0,
                            process_cpu_ms: 0.0,
                            mem_history: &[],
                            cpu_history: &[],
                            step_compute_history: &[],
                        },
                        &mut output,
                    );
                    rect = ui.min_rect();
                });
            });
            (output.commands, rect)
        };

        let (_, rect) = run(Vec::new());
        let centre = egui::pos2(rect.left() + 10.0, rect.center().y);
        run(vec![egui::Event::PointerMoved(centre)]);
        run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let (commands, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        let enabled = !commands.is_empty();
        (commands, enabled)
    }

    #[test]
    fn the_plane_inspector_hides_a_field_on_that_plane_and_not_everywhere() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                fieldcad_core::SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let mut compute = ComputeView::build(&source(), &snapshot, None);
        let channel = vector_channel_id();
        compute.vector_channels = vec![channel.clone()];

        let context = egui::Context::default();
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let mut layers: std::collections::BTreeMap<ChannelId, ChannelLayerSettings> =
            std::collections::BTreeMap::new();
        layers.insert(channel.clone(), ChannelLayerSettings::default());
        layers.get_mut(&channel).unwrap().visible = true;

        let run = |layers: &mut std::collections::BTreeMap<ChannelId, ChannelLayerSettings>,
                   events: Vec<egui::Event>| {
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(360.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                plane_field_layers(ui, plane, layers, &compute);
                rect = ui.min_rect();
            });
            rect
        };

        let rect = run(&mut layers, Vec::new());
        let header = rect.left_top() + egui::vec2(6.0, 8.0);
        for pressed in [true, false] {
            run(
                &mut layers,
                vec![
                    egui::Event::PointerMoved(header),
                    egui::Event::PointerButton {
                        pos: header,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let rect = run(&mut layers, Vec::new());
        assert!(
            rect.height() > 40.0,
            "the channel group did not open: {rect:?}"
        );

        let toggle = egui::pos2(rect.left() + 24.0, rect.top() + 28.0);
        for pressed in [true, false] {
            run(
                &mut layers,
                vec![
                    egui::Event::PointerMoved(toggle),
                    egui::Event::PointerButton {
                        pos: toggle,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }

        let layer = &layers[&channel];
        assert!(
            !layer.planes[&plane.id].visible,
            "clearing the plane's checkbox must hide the field on this plane"
        );
        assert!(
            layer.visible,
            "and must leave the layer itself visible everywhere else"
        );
    }

    /// Flow lines are offered as an independent control alongside arrows, not
    /// a replacement for them — see `scene::FlowLineDisplay`.
    #[test]
    fn the_plane_inspector_offers_flow_line_controls_alongside_arrows() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                fieldcad_core::SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let mut compute = ComputeView::build(&source(), &snapshot, None);
        let channel = vector_channel_id();
        compute.vector_channels = vec![channel.clone()];

        let context = egui::Context::default();
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let mut layers: std::collections::BTreeMap<ChannelId, ChannelLayerSettings> =
            std::collections::BTreeMap::new();
        layers.insert(channel.clone(), ChannelLayerSettings::default());
        layers.get_mut(&channel).unwrap().visible = true;

        let run = |layers: &mut std::collections::BTreeMap<ChannelId, ChannelLayerSettings>,
                   events: Vec<egui::Event>| {
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(360.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let full_output = context.run_ui(input, |ui| {
                plane_field_layers(ui, plane, layers, &compute);
                rect = ui.min_rect();
            });
            (rect, full_output)
        };

        let (rect, _) = run(&mut layers, Vec::new());
        let header = rect.left_top() + egui::vec2(6.0, 8.0);
        for pressed in [true, false] {
            run(
                &mut layers,
                vec![
                    egui::Event::PointerMoved(header),
                    egui::Event::PointerButton {
                        pos: header,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let (_, full_output) = run(&mut layers, Vec::new());
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }

        assert!(
            text.contains("Vector arrows") && text.contains("Flow lines"),
            "the plane inspector should offer both display styles: {text}"
        );
    }

    #[test]
    fn the_plane_inspector_offers_attachment_once_an_object_exists() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("anchor"))])
            .unwrap();
        world
            .commit([WorldCommand::CreatePlane(
                fieldcad_core::SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let compute = ComputeView::build(&source(), &snapshot, None);
        let mut layers = std::collections::BTreeMap::new();

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            plane_properties(
                ui,
                &snapshot,
                plane,
                &mut layers,
                &compute,
                &mut UiFrameOutput::default(),
            );
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }
        assert!(
            text.contains("Attach to"),
            "an unattached plane with an object in the world should offer attachment: {text}"
        );
    }

    #[test]
    fn an_attached_plane_reports_its_parent_and_offers_detaching() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("anchor"))])
            .unwrap();
        let object = created.created_objects[0];
        world
            .commit([WorldCommand::CreatePlane(
                fieldcad_core::SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z)
                    .unwrap()
                    .with_attached_to(object),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let compute = ComputeView::build(&source(), &snapshot, None);
        let mut layers = std::collections::BTreeMap::new();

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            plane_properties(
                ui,
                &snapshot,
                plane,
                &mut layers,
                &compute,
                &mut UiFrameOutput::default(),
            );
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }
        assert!(
            text.contains("Attached to anchor") && text.contains("Detach at current position"),
            "an attached plane should name its parent and offer detaching: {text}"
        );
    }

    #[test]
    fn the_distance_probe_inspector_shows_objects_and_the_live_reading() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(
                    ObjectSpec::new("near").with_transform(Transform::default()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("far")
                        .with_transform(Transform::at_finite(DVec3::new(3.0, 4.0, 0.0))),
                ),
            ])
            .unwrap();
        let created = world
            .commit([WorldCommand::CreateDistanceProbe(
                fieldcad_core::DistanceProbeSpec::new("gap", ObjectId::new(0), ObjectId::new(1)),
            )])
            .unwrap();
        let probe_id = created.created_distance_probes[0];
        let snapshot = world.snapshot();
        let probe = snapshot.distance_probe(probe_id).unwrap();
        let history = DistanceHistory::default();
        let mut model = UiModel::new();

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            distance_probe_properties(
                ui,
                &mut model,
                probe,
                &snapshot,
                &history,
                &mut UiFrameOutput::default(),
            );
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }
        assert!(
            text.contains("near")
                && text.contains("far")
                && text.contains("5.0000 m")
                && text.contains("Remove distance probe"),
            "the distance probe inspector should name both objects and the live distance: {text}"
        );
    }

    #[test]
    fn the_plane_inspector_separates_geometry_from_how_it_is_drawn() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                fieldcad_core::SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let compute = ComputeView::build(&source(), &snapshot, None);
        let mut layers = std::collections::BTreeMap::new();

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            plane_properties(
                ui,
                &snapshot,
                plane,
                &mut layers,
                &compute,
                &mut UiFrameOutput::default(),
            );
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }

        for heading in ["Geometry", "Field display"] {
            assert!(
                text.contains(heading),
                "the plane inspector is missing its {heading} section: {text}"
            );
        }
    }

    #[test]
    fn a_held_value_editor_reports_an_edit_in_progress_and_a_released_one_does_not() {
        let context = egui::Context::default();
        let mut value = 1.0;

        let mut run = |events: Vec<egui::Event>| {
            let mut editing = false;
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                coordinate_editor(ui, "x", &mut value, Dimension::LENGTH, &mut editing);
                rect = ui.min_rect();
            });
            (editing, rect.center())
        };

        let (editing, centre) = run(Vec::new());
        assert!(!editing, "an untouched editor is not an edit in progress");

        run(vec![egui::Event::PointerMoved(centre)]);
        let (editing, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(!editing, "a press alone has not started a drag");

        let (editing, _) = run(vec![egui::Event::PointerMoved(
            centre + egui::vec2(24.0, 0.0),
        )]);
        assert!(editing, "a drag in progress is an edit in progress");

        let (editing, _) = run(vec![egui::Event::PointerButton {
            pos: centre + egui::vec2(24.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(!editing, "releasing commits the edit");
    }

    #[test]
    fn realtime_update_is_offered_per_field_system_and_submitted_as_a_command() {
        let world = seeded_world();
        let source = source();
        let compute = ComputeView::build(&source, &world.snapshot(), None);
        let system = compute.field_systems[0].clone();
        assert!(system.realtime, "a scene starts fully live");

        let context = egui::Context::default();
        let run = |events: Vec<egui::Event>| {
            let mut output = UiFrameOutput::default();
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(280.0, 800.0),
                )),
                events,
                ..Default::default()
            };
            let full_output = context.run_ui(input, |ui| {
                realtime_control(ui, &system, &compute, &mut output);
                rect = ui.min_rect();
            });
            let mut text = String::new();
            for clipped in &full_output.shapes {
                painted_text(&clipped.shape, &mut text);
            }
            (output.commands, text, rect.center())
        };

        let (_, text, centre) = run(Vec::new());
        assert!(
            text.contains("Update while editing"),
            "the control is missing: {text}"
        );

        run(vec![egui::Event::PointerMoved(centre)]);
        let (commands, _, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(
            commands.is_empty(),
            "a press alone must not commit a choice"
        );

        let (commands, _, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(
            commands,
            vec![CommandPayload::SetFieldSystemRealtime {
                plugin: system.plugin.id.clone(),
                realtime: false,
            }]
        );
    }

    #[test]
    fn attachment_offset_preserves_probe_world_position_under_object_rotation() {
        let transform = Transform::new(
            DVec3::new(2.0, -1.0, 0.5),
            DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2),
        )
        .unwrap();
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("rotated").with_transform(transform),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let object = snapshot.objects().values().next().unwrap();
        let local_position = DVec3::new(0.25, 0.5, -0.75);
        let world_position = transform.apply(local_position);

        let recovered = attachment_offset(world_position, object);

        assert!((recovered - local_position).length() < 1.0e-12);
    }

    #[test]
    fn the_scene_panel_creates_an_object_with_no_physics_attached() {
        let world = World::new().snapshot();

        let CommandPayload::CommitWorld(commands) = new_object_command(&world) else {
            panic!("object authoring must issue a world transaction");
        };
        let WorldCommand::CreateObject(spec) = &commands[0] else {
            panic!("transaction must create an object");
        };

        assert!(spec.components.is_empty());
        assert!(!spec.pinned);
        assert!(matches!(spec.shape, Some(ObjectShape::Point { .. })));
    }

    #[test]
    fn an_object_becomes_movable_by_attaching_mass_alone() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(inertial_mass_component_schema()),
                WorldCommand::CreateObject(ObjectSpec::new("gizmo")),
            ])
            .unwrap();
        let object = ObjectId::new(0);

        let bare = world.snapshot();
        assert_eq!(
            motion_summary(bare.object(object).unwrap()),
            "no inertia — add Inertial mass to make it movable"
        );

        let schema = inertial_mass_component_schema();
        world
            .commit([WorldCommand::AttachComponent {
                object,
                component: schema.id.clone(),
                properties: schema.default_properties().unwrap(),
            }])
            .unwrap();

        let massive = world.snapshot();
        assert_eq!(
            motion_summary(massive.object(object).unwrap()),
            "moved by the forces acting on it"
        );
    }

    #[test]
    fn pinning_hands_motion_back_to_the_user() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("held").with_pinned(true),
            )])
            .unwrap();
        let object = ObjectId::new(0);
        let snapshot = world.snapshot();

        assert_eq!(
            motion_summary(snapshot.object(object).unwrap()),
            "held in place"
        );

        world
            .commit([WorldCommand::SetVelocity {
                object,
                velocity: Velocity::new(DVec3::X, DVec3::ZERO).unwrap(),
            }])
            .unwrap();
        let snapshot = world.snapshot();

        assert_eq!(
            motion_summary(snapshot.object(object).unwrap()),
            "carried at the velocity you set"
        );
    }

    #[test]
    fn every_registered_component_schema_can_be_attached_from_the_generic_menu() {
        for schema in [
            fieldcad_electromagnetic_sources::charge_component_schema(),
            fieldcad_sources::inertial_mass_component_schema(),
        ] {
            let properties = schema
                .default_properties()
                .unwrap_or_else(|error| panic!("{} has no defaults: {error}", schema.display_name));
            assert!(
                schema.validate(&properties).is_ok(),
                "{} defaults do not satisfy its own schema",
                schema.display_name
            );
        }
    }

    #[test]
    fn a_linked_gravitational_mass_cannot_be_edited_through_the_inspector() {
        let schema = gravitational_mass_component_schema();
        let mass = schema
            .properties
            .iter()
            .find(|property| property.id == mass_property_id())
            .unwrap();

        let row_is_interactive = |mut values: fieldcad_core::PropertyBag| {
            let context = egui::Context::default();
            let mut interactive = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                ui.add_enabled_ui(mass.is_relevant(&values), |ui| {
                    interactive = Some(ui.is_enabled());
                });
                property_editor(ui, ObjectId::new(0), mass, &mut values, &mut false);
            });
            interactive.expect("the row should have been laid out")
        };

        assert!(
            !row_is_interactive(linked_gravitational_mass_properties()),
            "a linked gravitational mass must render as non-interactive"
        );
        assert!(
            row_is_interactive(
                independent_gravitational_mass_properties(MassKg::new::<kilogram>(2.0)).unwrap()
            ),
            "unlinking must restore interaction"
        );
    }

    #[test]
    fn clearing_the_link_enables_the_value_within_one_frame() {
        let schema = gravitational_mass_component_schema();
        let mut values = linked_gravitational_mass_properties();
        let mass = schema
            .properties
            .iter()
            .find(|property| property.id == mass_property_id())
            .unwrap();

        assert!(!mass.is_relevant(&values));

        values.insert(
            fieldcad_sources::follows_inertial_property_id(),
            fieldcad_core::PropertyValue::Boolean(false),
        );

        assert!(
            mass.is_relevant(&values),
            "the value must be editable as soon as the switch is cleared"
        );
    }

    #[test]
    fn scalar_properties_render_across_the_range_physics_actually_uses() {
        assert_eq!(format_engineering(0.0), "0");
        assert_eq!(format_engineering(1.5), "1.5000");
        assert!(format_engineering(9.109e-31).contains("e-31"));
        assert!(format_engineering(6.02e23).contains("e23"));
    }

    #[test]
    fn inertial_mass_kg_reads_a_valid_attached_component_and_nothing_else() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(
                    fieldcad_sources::inertial_mass_component_schema(),
                ),
                WorldCommand::CreateObject(ObjectSpec::new("gizmo")),
            ])
            .unwrap();
        let object = ObjectId::new(0);

        let bare = world.snapshot();
        assert_eq!(inertial_mass_kg(bare.object(object).unwrap()), None);

        world
            .commit([WorldCommand::AttachComponent {
                object,
                component: fieldcad_sources::inertial_mass_component_id(),
                properties: fieldcad_sources::inertial_mass_properties(MassKg::new::<kilogram>(
                    3.5,
                ))
                .unwrap(),
            }])
            .unwrap();

        let massive = world.snapshot();
        assert_eq!(inertial_mass_kg(massive.object(object).unwrap()), Some(3.5));
    }

    #[test]
    fn format_vector_uses_engineering_notation_per_component() {
        let formatted = format_vector(DVec3::new(1.5, -2.0e8, 0.0), "N");
        assert!(formatted.starts_with("(1.5000, "));
        assert!(formatted.contains("e8"));
        assert!(formatted.ends_with(") N"));
    }
}
