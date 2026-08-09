//! Queue panel: inspect pending mutations, pause/resume, and cancel commands.

use fieldcad_simulation::{CommandLifecycle, CommandPayload, CommandRecord};

use crate::ui::{FrameContext, UiFrameOutput};

pub fn queue_window(context: &egui::Context, frame: &FrameContext<'_>, output: &mut UiFrameOutput) {
    let queue = &frame.compute.queue;
    egui::Window::new("Queue")
        .default_pos(egui::pos2(218.0, 48.0))
        .default_size(egui::vec2(360.0, 320.0))
        .resizable(true)
        .collapsible(true)
        .show(context, |ui| {
            ui.horizontal(|ui| {
                if queue.paused {
                    ui.colored_label(egui::Color32::from_rgb(235, 190, 75), "⏸ paused");
                    if ui
                        .button("Resume queue")
                        .on_hover_text(
                            "Held mutations apply at the next eligible tick boundary, in \
                             submission order",
                        )
                        .clicked()
                    {
                        output.submit(CommandPayload::ResumeQueue);
                    }
                } else {
                    ui.colored_label(egui::Color32::from_rgb(95, 200, 110), "● running");
                    if ui
                        .button("Pause queue")
                        .on_hover_text(
                            "Hold queued scene/domain mutations at their tick boundary; \
                             simulation ticks continue and new mutations are still accepted",
                        )
                        .clicked()
                    {
                        output.submit(CommandPayload::PauseQueue);
                    }
                }
            });

            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if queue.pending.is_empty() {
                        ui.label("Nothing pending.");
                    } else {
                        for record in &queue.pending {
                            queue_record_row(ui, record, output);
                        }
                    }

                    if !queue.history.is_empty() {
                        ui.separator();
                        ui.collapsing("History", |ui| {
                            for record in queue.history.iter().rev() {
                                queue_record_row(ui, record, output);
                            }
                        });
                    }
                });
        });
}

fn lifecycle_label(state: CommandLifecycle) -> &'static str {
    match state {
        CommandLifecycle::Submitted => "Submitted",
        CommandLifecycle::Queued => "Queued",
        CommandLifecycle::Applied => "Applied",
        CommandLifecycle::Rejected => "Rejected",
        CommandLifecycle::Cancelled => "Cancelled",
    }
}

fn queue_record_row(ui: &mut egui::Ui, record: &CommandRecord, output: &mut UiFrameOutput) {
    ui.horizontal(|ui| {
        ui.monospace(format!("#{}", record.command.get()));
        ui.label(record.kind.label());
        ui.label(lifecycle_label(record.state));
        if record.state == CommandLifecycle::Rejected
            && let Some(error) = &record.error
        {
            ui.colored_label(egui::Color32::from_rgb(240, 105, 95), "⚠")
                .on_hover_text(error);
        }
        if record.state == CommandLifecycle::Queued && ui.button("Cancel").clicked() {
            output.submit(CommandPayload::CancelQueuedCommand(record.command));
        }
    });
}
