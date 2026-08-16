//! Floating app-settings window — desktop-client-local preferences, not
//! part of any saved scene. See `crate::profile::UserProfile`.

use crate::profile::UserProfile;
use crate::ui::UiModel;

pub fn settings_window(context: &egui::Context, model: &mut UiModel, profile: &mut UserProfile) {
    if !model.settings_visible {
        return;
    }
    let mut open = model.settings_visible;
    egui::Window::new("Settings")
        .default_pos(egui::pos2(218.0, 48.0))
        .collapsible(true)
        .open(&mut open)
        .show(context, |ui| {
            let mut changed = false;
            changed |= ui
                .checkbox(&mut profile.show_help_on_startup, "Show help on startup")
                .changed();
            changed |= ui
                .checkbox(
                    &mut profile.show_diagnostics_on_startup,
                    "Show diagnostics on startup",
                )
                .changed();
            if changed {
                profile.save();
            }

            ui.separator();
            ui.heading("Reusable constants");
            match crate::user_constants::path() {
                Ok(path) => {
                    if ui
                        .link(path.display().to_string())
                        .on_hover_text(
                            "Documents embed an explicit dependency closure and never follow \
                             this file silently. Click to open its containing folder.",
                        )
                        .clicked()
                        && let Err(error) = crate::user_constants::reveal_containing_folder()
                    {
                        model.user_constants_status = Some(error.to_string());
                    }
                }
                Err(error) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 100, 90), error.to_string());
                }
            }
            super::expression_editor::user_constants_editor(ui, model);

            ui.separator();
            ui.label("Recent files");
            if profile.recent_files.is_empty() {
                ui.weak("None yet");
            } else {
                for path in &profile.recent_files {
                    ui.label(path.display().to_string());
                }
            }
        });
    model.settings_visible = open;
}
