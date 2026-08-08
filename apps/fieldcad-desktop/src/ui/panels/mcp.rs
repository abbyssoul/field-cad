//! MCP (Model Context Protocol) panel: enable/disable the embedded server
//! and display connection info.

use crate::mcp::{McpAction, McpSession};

pub fn mcp_window(context: &egui::Context, mcp: &McpSession) -> Option<McpAction> {
    let mut action = None;
    egui::Window::new("MCP")
        .default_pos(egui::pos2(218.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .show(context, |ui| match mcp {
            McpSession::Disabled => {
                ui.label(
                    "Let an external agent (or another client) drive this exact session over MCP.",
                );
                if ui.button("Enable MCP").clicked() {
                    action = Some(McpAction::Enable);
                }
            }
            McpSession::Running(running) => {
                ui.horizontal(|ui| match crate::mcp::connection_count(running) {
                    Some(0) => {
                        ui.colored_label(egui::Color32::GRAY, "●");
                        ui.label("No client connected");
                    }
                    Some(count) => {
                        ui.colored_label(egui::Color32::from_rgb(95, 200, 110), "●");
                        ui.label(format!(
                            "{count} client{} connected",
                            if count == 1 { "" } else { "s" }
                        ));
                    }
                    None => {
                        ui.colored_label(egui::Color32::GRAY, "●");
                        ui.label("Checking…");
                    }
                })
                .response
                .on_hover_text(
                    "A session persists until a client explicitly disconnects, so this can lag \
                     behind a client that vanished uncleanly (e.g. was killed).",
                );
                ui.label("Pass this token and URL to your agent's MCP client config:");
                egui::Grid::new("mcp_running")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Token");
                        ui.horizontal(|ui| {
                            let mut token = running.token.clone();
                            ui.add(
                                egui::TextEdit::singleline(&mut token)
                                    .password(true)
                                    .desired_width(220.0),
                            );
                            if ui.button("Copy").clicked() {
                                context.copy_text(running.token.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("URL");
                        ui.monospace(format!("http://{}/mcp", running.addr));
                        ui.end_row();
                    });
                ui.separator();
                if ui.button("Disable MCP").clicked() {
                    action = Some(McpAction::Disable);
                }
            }
            McpSession::Failed(error) => {
                ui.colored_label(
                    egui::Color32::from_rgb(240, 105, 95),
                    format!("MCP server failed: {error}"),
                );
                if ui.button("Enable MCP").clicked() {
                    action = Some(McpAction::Enable);
                }
            }
        });
    action
}
