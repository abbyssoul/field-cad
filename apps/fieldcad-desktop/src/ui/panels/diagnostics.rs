//! Floating diagnostics window.

use crate::ui::plot::history_plot;
use crate::ui::{FrameContext, UiModel};

const FRAME_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(100, 155, 245);
const MEM_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(95, 210, 120);
const CPU_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(245, 205, 75);
const STEP_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(200, 120, 220);

pub fn diagnostics_window(
    context: &egui::Context,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    command_error: Option<&str>,
) {
    let config = &mut model.diagnostics_config;

    egui::Window::new("Diagnostics")
        .default_pos(egui::pos2(218.0, 48.0))
        .collapsible(true)
        .show(context, |ui| {
            ui.collapsing("⚙ Settings", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Update every:");
                    ui.add(
                        egui::Slider::new(&mut config.update_interval_ms, 16..=5000)
                            .suffix("ms")
                            .logarithmic(true),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut config.show_frame_time, "Frame timing");
                    ui.checkbox(&mut config.show_memory, "Memory");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut config.show_cpu, "CPU");
                    ui.checkbox(&mut config.show_scene_info, "Scene info");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut config.show_solver_step, "Solver step time");
                    ui.checkbox(&mut config.show_solver_diagnostics, "Solver diagnostics");
                });
            });

            if config.show_frame_time {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Frame");
                    ui.monospace(format!("{:.2} ms", frame.frame_time_ms));
                    if let Some(fps) = fps_over_last_second(frame.frame_history) {
                        ui.label(format!("{fps:.0} FPS")).on_hover_text(
                            "Rendered frames per second, averaged over the most recent \
                             second of history — not the simulation's own clock.",
                        );
                    }
                    if !frame.frame_history.is_empty() && frame.frame_min_ms.is_finite() {
                        ui.label(format!(
                            "(min {:.1} / max {:.1})",
                            frame.frame_min_ms, frame.frame_max_ms
                        ));
                    }
                });
                metric_history_dropdown(
                    ui,
                    "frame_plot",
                    frame.frame_history,
                    FRAME_TRACE_COLOR,
                    "ms",
                );
            }

            if config.show_memory {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Mem");
                    if frame.process_rss_kb > 0 {
                        ui.monospace(format!("{:.1} MiB", frame.process_rss_kb as f64 / 1024.0));
                    } else {
                        ui.monospace("—");
                    }
                });
                metric_history_dropdown(ui, "mem_plot", frame.mem_history, MEM_TRACE_COLOR, "MiB");
            }

            if config.show_cpu {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("CPU");
                    if frame.process_cpu_ms > 0.0 {
                        ui.monospace(format!("{:.1}s total", frame.process_cpu_ms / 1000.0));
                    } else {
                        ui.monospace("—");
                    }
                });
                metric_history_dropdown(ui, "cpu_plot", frame.cpu_history, CPU_TRACE_COLOR, "s");
            }

            if config.show_solver_step {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Step");
                    let compute_ms = frame.compute.step_compute_ms;
                    if compute_ms > 0.0 {
                        ui.monospace(format!("{compute_ms:.2} ms")).on_hover_text(
                            "Wall-clock time the compute thread took to finish the most \
                                 recent simulation tick: force collection, every time-stepped \
                                 solver's own advance, dynamics integration, and the snapshot \
                                 it publishes.",
                        );
                        let dt_ms = frame.compute.time_step_seconds * 1_000.0;
                        if dt_ms > 0.0 {
                            let factor = dt_ms / compute_ms as f64;
                            let color = if factor >= 1.0 {
                                egui::Color32::from_rgb(95, 210, 120)
                            } else {
                                egui::Color32::from_rgb(235, 105, 90)
                            };
                            ui.colored_label(color, format!("({factor:.2}× real-time)"))
                                .on_hover_text(
                                    "Simulated dt ÷ time to compute one step. Below 1× means \
                                     this machine cannot compute steps as fast as the \
                                     simulation clock advances at the current dt — running \
                                     will fall behind wall-clock time.",
                                );
                        }
                    } else {
                        ui.monospace("—");
                    }
                });
                metric_history_dropdown(
                    ui,
                    "step_plot",
                    frame.step_compute_history,
                    STEP_TRACE_COLOR,
                    "ms",
                );
            }

            if config.show_scene_info {
                ui.separator();
                ui.strong("Scene");
                egui::Grid::new("scene_info")
                    .num_columns(2)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        ui.label("GPU");
                        ui.monospace(frame.adapter_name);
                        ui.end_row();
                        ui.label("Objects");
                        ui.monospace(
                            frame
                                .world
                                .objects()
                                .values()
                                .filter(|object| !object.derived)
                                .count()
                                .to_string(),
                        );
                        ui.end_row();
                        ui.label("Samples");
                        ui.monospace(format_count(frame.compute.total_samples));
                        ui.end_row();
                    });
                ui.label(format!("Compute: {}", frame.compute.description));
            }

            if config.show_solver_diagnostics && !frame.compute.diagnostics.is_empty() {
                ui.separator();
                ui.strong("Solver diagnostics");
                for line in &frame.compute.diagnostics {
                    ui.small(line);
                }
            }

            if let Some(error) = command_error {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(240, 105, 95),
                    format!("Last command rejected: {error}"),
                );
            }
        });
}

fn metric_history_dropdown(
    ui: &mut egui::Ui,
    id: &str,
    history: &[f32],
    color: egui::Color32,
    unit: &str,
) {
    if history.len() < 2 {
        return;
    }
    egui::CollapsingHeader::new("Plot")
        .id_salt(id)
        .default_open(false)
        .show(ui, |ui| {
            history_plot(ui, history, color, unit);
        });
}

/// Rendered frames per second, averaged over the most recent second of
/// `history` (each entry is one render's wall-clock duration in
/// milliseconds, oldest first). Falls back to whatever history is
/// available when there isn't yet a full second of it, so the reading
/// isn't `None` for the first second after the window opens.
fn fps_over_last_second(history: &[f32]) -> Option<f32> {
    let mut elapsed_ms = 0.0_f32;
    let mut count = 0_u32;
    for &frame_ms in history.iter().rev() {
        elapsed_ms += frame_ms;
        count += 1;
        if elapsed_ms >= 1_000.0 {
            break;
        }
    }
    (elapsed_ms > 0.0).then(|| count as f32 * 1_000.0 / elapsed_ms)
}

fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        let s = n.to_string();
        let mut result = String::with_capacity(s.len() + 2);
        for (i, c) in s.chars().enumerate() {
            if (s.len() - i).is_multiple_of(3) && i > 0 {
                result.push('\u{202f}');
            }
            result.push(c);
        }
        result
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_over_last_second_averages_a_steady_frame_rate() {
        // 60 fps ~= 16.667 ms/frame; 60 of them span exactly one second.
        let history = vec![1_000.0 / 60.0; 60];
        let fps = fps_over_last_second(&history).unwrap();
        assert!((fps - 60.0).abs() < 0.5, "expected ~60 FPS, got {fps}");
    }

    #[test]
    fn fps_over_last_second_uses_only_the_most_recent_second() {
        // A long run of slow frames followed by one second of fast ones:
        // the slow prefix must not drag the reading down.
        let mut history = vec![100.0; 20]; // 10 fps, oldest
        history.extend(vec![10.0; 100]); // 100 fps, most recent
        let fps = fps_over_last_second(&history).unwrap();
        assert!((fps - 100.0).abs() < 1.0, "expected ~100 FPS, got {fps}");
    }

    #[test]
    fn fps_over_last_second_is_none_for_empty_history() {
        assert_eq!(fps_over_last_second(&[]), None);
    }

    #[test]
    fn fps_over_last_second_falls_back_to_partial_history() {
        // Fewer than a second's worth of frames: still report something
        // rather than waiting for a full second of history to exist.
        let history = vec![50.0; 3]; // 150 ms total, 3 frames
        let fps = fps_over_last_second(&history).unwrap();
        assert!((fps - 20.0).abs() < 0.1, "expected ~20 FPS, got {fps}");
    }
}
