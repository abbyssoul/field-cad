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
