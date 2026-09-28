//! Left side panel: node palette + properties inspector for the selected
//! ECU or CAN bus.

use egui_snarl::NodeId as SnarlId;
use operow_core::CanFrame;

use crate::graph::{Graph, GraphNode};

#[derive(Default)]
pub struct Inspector {
    /// Per-row hex-data scratch buffers, keyed by tx-message index, so users
    /// can type invalid-but-in-progress hex without losing their place.
    data_buf: std::collections::HashMap<usize, String>,
    id_buf: std::collections::HashMap<usize, String>,
    error: Option<String>,
    last_sel: Option<SnarlId>,
}

impl Inspector {
    pub fn ui(&mut self, ui: &mut egui::Ui, graph: &mut Graph, running: bool) {
        ui.heading("Palette");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("+ ECU"))
                .clicked()
            {
                graph.add_ecu(egui::pos2(40.0, 40.0), "NewEcu");
            }
            if ui
                .add_enabled(!running, egui::Button::new("+ Bus"))
                .clicked()
            {
                graph.add_bus(egui::pos2(40.0, 200.0));
            }
        });
        ui.separator();
        ui.heading("Properties");
        if running {
            ui.label(
                egui::RichText::new("Editing is disabled while the measurement is running.")
                    .weak()
                    .italics(),
            );
        }

        let Some(sel) = graph.selected else {
            ui.label("Select a node to edit its properties.");
            return;
        };
        if graph.snarl.get_node(sel).is_none() {
            graph.selected = None;
            return;
        }

        if self.last_sel != Some(sel) {
            self.last_sel = Some(sel);
            self.data_buf.clear();
            self.id_buf.clear();
            self.error = None;
        }

        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.node_ui(ui, graph, sel, running);
        });
    }

    fn node_ui(&mut self, ui: &mut egui::Ui, graph: &mut Graph, sel: SnarlId, running: bool) {
        let node = graph.snarl.get_node_mut(sel).expect("checked above");
        match node {
            GraphNode::Bus(bus) => {
                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut bus.name);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Bitrate (bit/s):");
                        ui.add(egui::DragValue::new(&mut bus.bitrate).range(1..=10_000_000));
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut bus.fd_enabled, "CAN FD");
                    });
                    ui.add_enabled_ui(bus.fd_enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Data bitrate (bit/s):");
                            egui::ComboBox::from_id_salt(("bus_data_bitrate", sel))
                                .selected_text(crate::graph::format_bitrate(bus.data_bitrate))
                                .show_ui(ui, |ui| {
                                    for rate in
                                        [1_000_000u32, 2_000_000, 4_000_000, 5_000_000, 8_000_000]
                                    {
                                        ui.selectable_value(
                                            &mut bus.data_bitrate,
                                            rate,
                                            crate::graph::format_bitrate(rate),
                                        );
                                    }
                                });
                            ui.add(
                                egui::DragValue::new(&mut bus.data_bitrate).range(1..=10_000_000),
                            );
                        });
                    });
                });
            }
            GraphNode::Ecu(ecu) => {
                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut ecu.name);
                    });
                    ui.separator();
                    ui.label("TX messages:");

                    let mut remove: Option<usize> = None;
                    egui::Grid::new(("tx_grid", sel))
                        .num_columns(12)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Name");
                            ui.strong("ID (hex)");
                            ui.strong("Ext");
                            ui.strong("FD");
                            ui.strong("BRS");
                            ui.strong("DLC");
                            ui.strong("Len");
                            ui.strong("Data (hex, up to 64 bytes)");
                            ui.strong("Period (ms)");
                            ui.strong("En");
                            ui.strong("");
                            ui.end_row();

                            for (i, msg) in ecu.tx.iter_mut().enumerate() {
                                ui.add(
                                    egui::TextEdit::singleline(&mut msg.name).desired_width(80.0),
                                );

                                let id_buf = self
                                    .id_buf
                                    .entry(i)
                                    .or_insert_with(|| format!("{:X}", msg.frame.id));
                                let id_resp =
                                    ui.add(egui::TextEdit::singleline(id_buf).desired_width(60.0));

                                let mut ext = msg.frame.extended;
                                ui.checkbox(&mut ext, "");

                                let mut fd = msg.frame.fd;
                                ui.checkbox(&mut fd, "");

                                let mut brs = msg.frame.brs;
                                ui.add_enabled_ui(fd, |ui| {
                                    ui.checkbox(&mut brs, "");
                                });
                                if !fd {
                                    brs = false;
                                }

                                ui.label(msg.frame.dlc_code().to_string());
                                ui.label(msg.frame.dlc.to_string());

                                let data_buf = self
                                    .data_buf
                                    .entry(i)
                                    .or_insert_with(|| hex_bytes(msg.frame.payload()));
                                let data_resp = ui
                                    .add(egui::TextEdit::singleline(data_buf).desired_width(220.0));

                                ui.add(egui::DragValue::new(&mut msg.period_ms).range(1..=60_000));
                                ui.checkbox(&mut msg.enabled, "");
                                if ui.small_button("🗑").clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();

                                // Apply edits after drawing the row so widget IDs stay stable.
                                let id_val =
                                    u32::from_str_radix(id_buf.trim().trim_start_matches("0x"), 16)
                                        .ok();
                                let data_val = parse_hex_bytes(data_buf);
                                if let (Some(id), Some(data)) = (id_val, data_val.as_ref()) {
                                    let result = if fd {
                                        CanFrame::new_fd(id, ext, brs, data)
                                    } else {
                                        CanFrame::new(id, ext, data)
                                    };
                                    match result {
                                        Ok(frame) => {
                                            msg.frame = frame;
                                            self.error = None;
                                        }
                                        Err(e) => self.error = Some(e.to_string()),
                                    }
                                } else if id_resp.changed() || data_resp.changed() {
                                    self.error = Some(
                                        "invalid ID or data hex (FD lengths allowed: 0-8, 12, \
                                         16, 20, 24, 32, 48, 64 bytes; classic: 0-8 bytes)"
                                            .to_string(),
                                    );
                                }
                            }
                        });

                    if ui.button("+ Add message").clicked() {
                        ecu.tx.push(operow_core::TxMessage {
                            name: format!("Msg{}", ecu.tx.len()),
                            frame: CanFrame::new(0x100, false, &[]).unwrap(),
                            period_ms: 100,
                            enabled: true,
                        });
                    }
                    if let Some(i) = remove {
                        ecu.tx.remove(i);
                        self.data_buf.remove(&i);
                        self.id_buf.remove(&i);
                    }
                });
            }
        }

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), err);
        }
    }
}

fn hex_bytes(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split_whitespace()
        .map(|tok| u8::from_str_radix(tok, 16).ok())
        .collect()
}
