//! Left side panel: node palette + properties inspector for the selected
//! ECU or CAN bus.

use egui_flow::NodeId as FlowId;
use operow_core::{BusId, CanFrame, NodeId, SendType, TxMessage};
use operow_engine::{Command, EcuCommand};

use crate::graph::{Graph, GraphNode};
use crate::icons;

#[derive(Default)]
pub struct Inspector {
    /// Per-row hex-data scratch buffers, keyed by tx-message index, so users
    /// can type invalid-but-in-progress hex without losing their place.
    data_buf: std::collections::HashMap<usize, String>,
    id_buf: std::collections::HashMap<usize, String>,
    error: Option<String>,
    last_sel: Option<FlowId>,
    /// Live (running-only) UI state, keyed by (ECU, tx-message index).
    live_active: std::collections::HashMap<(NodeId, usize), bool>,
    live_payload: std::collections::HashMap<(NodeId, usize), String>,
    live_error: std::collections::HashMap<(NodeId, usize), String>,
    was_running: bool,
}

/// Send-type discriminant without its parameters, for the combo box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendKind {
    Cyclic,
    Event,
    OnChange,
    CyclicIfActive,
    CyclicAndEvent,
}

const DEFAULT_MIN_GAP_MS: u32 = 100;

impl SendKind {
    pub const ALL: [SendKind; 5] = [
        SendKind::Cyclic,
        SendKind::Event,
        SendKind::OnChange,
        SendKind::CyclicIfActive,
        SendKind::CyclicAndEvent,
    ];

    pub fn of(st: SendType) -> Self {
        match st {
            SendType::Cyclic => SendKind::Cyclic,
            SendType::Event => SendKind::Event,
            SendType::OnChange { .. } => SendKind::OnChange,
            SendType::CyclicIfActive => SendKind::CyclicIfActive,
            SendType::CyclicAndEvent => SendKind::CyclicAndEvent,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SendKind::Cyclic => "Cyclic",
            SendKind::Event => "Event",
            SendKind::OnChange => "OnChange",
            SendKind::CyclicIfActive => "CyclicIfActive",
            SendKind::CyclicAndEvent => "CyclicAndEvent",
        }
    }

    /// The send type for this kind; `OnChange` keeps `current`'s gap if it
    /// already is one, otherwise uses the default.
    pub fn to_send_type(self, current: SendType) -> SendType {
        match self {
            SendKind::Cyclic => SendType::Cyclic,
            SendKind::Event => SendType::Event,
            SendKind::OnChange => match current {
                SendType::OnChange { min_gap_ms } => SendType::OnChange { min_gap_ms },
                _ => SendType::OnChange {
                    min_gap_ms: DEFAULT_MIN_GAP_MS,
                },
            },
            SendKind::CyclicIfActive => SendType::CyclicIfActive,
            SendKind::CyclicAndEvent => SendType::CyclicAndEvent,
        }
    }
}

/// Whether `period_ms` is used by this send type.
pub fn uses_period(st: SendType) -> bool {
    !matches!(st, SendType::Event | SendType::OnChange { .. })
}

/// Whether a manual "Trigger" is meaningful for this send type.
pub fn can_trigger(st: SendType) -> bool {
    matches!(
        st,
        SendType::Event | SendType::OnChange { .. } | SendType::CyclicAndEvent
    )
}

/// Buses the ECU node `sel` is wired to, in edge order.
pub fn linked_buses(graph: &Graph, sel: FlowId) -> Vec<(BusId, String)> {
    let mut out: Vec<(BusId, String)> = Vec::new();
    for edge in &graph.state.edges {
        if edge.source != sel {
            continue;
        }
        if let Some(GraphNode::Bus(b)) = graph.node(edge.target)
            && !out.iter().any(|(id, _)| *id == b.id)
        {
            out.push((b.id, b.name.clone()));
        }
    }
    out
}

/// Display text for a message's bus selection.
pub fn bus_label(bus: Option<BusId>, linked: &[(BusId, String)]) -> String {
    match bus {
        None => "All buses".to_string(),
        Some(id) => linked
            .iter()
            .find(|(b, _)| *b == id)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| "\u{26a0} unlinked".to_string()),
    }
}

/// One-line summary, e.g. `0x100 · Cyclic 10ms · All buses`.
pub fn msg_summary(msg: &TxMessage, linked: &[(BusId, String)]) -> String {
    let kind = SendKind::of(msg.send_type);
    let timing = match msg.send_type {
        SendType::OnChange { min_gap_ms } => format!("OnChange gap {min_gap_ms}ms"),
        SendType::Event => "Event".to_string(),
        _ => format!("{} {}ms", kind.label(), msg.period_ms),
    };
    format!(
        "0x{:X} \u{b7} {} \u{b7} {}",
        msg.frame.id,
        timing,
        bus_label(msg.bus, linked)
    )
}

impl Inspector {
    /// Draws the inspector; returns engine commands issued by live controls.
    pub fn ui(&mut self, ui: &mut egui::Ui, graph: &mut Graph, running: bool) -> Vec<Command> {
        let mut cmds = Vec::new();
        if running != self.was_running {
            self.was_running = running;
            self.live_active.clear();
            self.live_payload.clear();
            self.live_error.clear();
        }
        ui.heading("Palette");
        ui.horizontal(|ui| {
            if icons::icon_button_enabled(ui, !running, icons::ecu(), "Add ECU").clicked() {
                graph.add_ecu(egui::pos2(40.0, 40.0), "NewEcu");
            }
            if icons::icon_button_enabled(ui, !running, icons::bus(), "Add CAN bus").clicked() {
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

        let Some(sel) = graph.selected() else {
            ui.label("Select a node to edit its properties.");
            return cmds;
        };

        if self.last_sel != Some(sel) {
            self.last_sel = Some(sel);
            self.data_buf.clear();
            self.id_buf.clear();
            self.error = None;
        }

        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.node_ui(ui, graph, sel, running, &mut cmds);
        });
        cmds
    }

    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut Graph,
        sel: FlowId,
        running: bool,
        cmds: &mut Vec<Command>,
    ) {
        let linked = linked_buses(graph, sel);
        let node = graph.node_mut(sel).expect("selected node exists");
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

                                ui.add_enabled(
                                    uses_period(msg.send_type),
                                    egui::DragValue::new(&mut msg.period_ms).range(1..=60_000),
                                );
                                ui.checkbox(&mut msg.enabled, "");
                                if icons::icon_button(ui, icons::clear(), "Remove message")
                                    .clicked()
                                {
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
                            bus: None,
                            send_type: Default::default(),
                        });
                    }
                    if let Some(i) = remove {
                        ecu.tx.remove(i);
                        self.data_buf.remove(&i);
                        self.id_buf.remove(&i);
                    }
                });

                if !ecu.tx.is_empty() {
                    ui.separator();
                    ui.label("Message behavior:");
                }
                let ecu_id = ecu.id;
                for (i, msg) in ecu.tx.iter_mut().enumerate() {
                    let header = msg_summary(msg, &linked);
                    egui::CollapsingHeader::new(format!("{} \u{2013} {header}", msg.name))
                        .id_salt(("tx_msg", sel, i))
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(!running, |ui| {
                                Self::edit_behavior_ui(ui, sel, i, msg, &linked);
                            });
                            if running {
                                self.live_ui(ui, ecu_id, i, msg, cmds);
                            }
                        });
                }
            }
        }

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), err);
        }
    }
}

impl Inspector {
    fn edit_behavior_ui(
        ui: &mut egui::Ui,
        sel: FlowId,
        i: usize,
        msg: &mut TxMessage,
        linked: &[(BusId, String)],
    ) {
        ui.horizontal(|ui| {
            ui.label("Send type:");
            let cur = SendKind::of(msg.send_type);
            egui::ComboBox::from_id_salt(("send_type", sel, i))
                .selected_text(cur.label())
                .show_ui(ui, |ui| {
                    for k in SendKind::ALL {
                        if ui.selectable_label(cur == k, k.label()).clicked() && cur != k {
                            msg.send_type = k.to_send_type(msg.send_type);
                        }
                    }
                });
            if let SendType::OnChange { min_gap_ms } = &mut msg.send_type {
                ui.label("Min gap (ms):");
                ui.add(egui::DragValue::new(min_gap_ms).range(0..=60_000));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Bus:");
            egui::ComboBox::from_id_salt(("tx_bus", sel, i))
                .selected_text(bus_label(msg.bus, linked))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut msg.bus, None, "All buses");
                    for (id, name) in linked {
                        ui.selectable_value(&mut msg.bus, Some(*id), name);
                    }
                });
            if msg
                .bus
                .is_some_and(|b| !linked.iter().any(|(id, _)| *id == b))
                && ui.small_button("Reset").clicked()
            {
                msg.bus = None;
            }
        });
    }

    fn live_ui(
        &mut self,
        ui: &mut egui::Ui,
        ecu: NodeId,
        i: usize,
        msg: &TxMessage,
        cmds: &mut Vec<Command>,
    ) {
        let key = (ecu, i);
        ui.separator();
        ui.strong("Live");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_trigger(msg.send_type),
                    egui::Button::image_and_text(
                        egui::Image::new(icons::send())
                            .fit_to_exact_size(egui::vec2(16.0, 16.0))
                            .tint(ui.visuals().text_color()),
                        "Trigger",
                    ),
                )
                .on_hover_text("Send this message now (Event, OnChange, CyclicAndEvent)")
                .clicked()
            {
                cmds.push(Command::Ecu(ecu, EcuCommand::Trigger { msg: i }));
            }
            let active = self.live_active.entry(key).or_insert(false);
            let is_cia = msg.send_type == SendType::CyclicIfActive;
            if ui
                .add_enabled(is_cia, egui::Checkbox::new(active, "Active"))
                .changed()
            {
                cmds.push(Command::Ecu(
                    ecu,
                    EcuCommand::SetActive {
                        msg: i,
                        active: *active,
                    },
                ));
            }
        });
        let buf = self
            .live_payload
            .entry(key)
            .or_insert_with(|| hex_bytes(msg.frame.payload()));
        let mut set = false;
        ui.horizontal(|ui| {
            ui.label("Payload:");
            let r = ui.add(egui::TextEdit::singleline(buf).desired_width(160.0));
            set |= r.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
            set |= ui.button("Set").clicked();
        });
        if set {
            let f = &msg.frame;
            let res = parse_hex_bytes(buf)
                .ok_or_else(|| "invalid hex bytes".to_string())
                .and_then(|data| {
                    let r = if f.fd {
                        CanFrame::new_fd(f.id, f.extended, f.brs, &data)
                    } else {
                        CanFrame::new(f.id, f.extended, &data)
                    };
                    r.map(|_| data).map_err(|e| e.to_string())
                });
            match res {
                Ok(data) => {
                    self.live_error.remove(&key);
                    cmds.push(Command::Ecu(ecu, EcuCommand::SetPayload { msg: i, data }));
                }
                Err(e) => {
                    self.live_error.insert(key, e);
                }
            }
        }
        if let Some(e) = self.live_error.get(&key) {
            ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), e);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(st: SendType, bus: Option<BusId>) -> TxMessage {
        TxMessage {
            name: "M".into(),
            frame: CanFrame::new(0x100, false, &[1]).unwrap(),
            period_ms: 10,
            enabled: true,
            bus,
            send_type: st,
        }
    }

    #[test]
    fn kind_roundtrip_and_defaults() {
        for k in SendKind::ALL {
            assert_eq!(SendKind::of(k.to_send_type(SendType::Cyclic)), k);
        }
        assert_eq!(
            SendKind::OnChange.to_send_type(SendType::Cyclic),
            SendType::OnChange { min_gap_ms: 100 }
        );
        assert_eq!(
            SendKind::OnChange.to_send_type(SendType::OnChange { min_gap_ms: 7 }),
            SendType::OnChange { min_gap_ms: 7 }
        );
    }

    #[test]
    fn period_and_trigger_rules() {
        assert!(uses_period(SendType::Cyclic));
        assert!(!uses_period(SendType::Event));
        assert!(!uses_period(SendType::OnChange { min_gap_ms: 1 }));
        assert!(can_trigger(SendType::CyclicAndEvent));
        assert!(!can_trigger(SendType::CyclicIfActive));
    }

    #[test]
    fn summary_and_bus_label() {
        let linked = vec![(BusId(1), "Pt".to_string())];
        assert_eq!(
            msg_summary(&msg(SendType::Cyclic, None), &linked),
            "0x100 \u{b7} Cyclic 10ms \u{b7} All buses"
        );
        assert_eq!(
            msg_summary(
                &msg(SendType::OnChange { min_gap_ms: 5 }, Some(BusId(1))),
                &linked
            ),
            "0x100 \u{b7} OnChange gap 5ms \u{b7} Pt"
        );
        assert_eq!(bus_label(Some(BusId(9)), &linked), "\u{26a0} unlinked");
    }

    #[test]
    fn linked_buses_from_edges() {
        let g = Graph::default_demo();
        let ecu = g
            .state
            .nodes
            .iter()
            .find(|n| matches!(n.data, GraphNode::Ecu(_)))
            .unwrap()
            .id;
        assert_eq!(linked_buses(&g, ecu).len(), 1);
    }

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex_bytes("01 ff"), Some(vec![1, 255]));
        assert_eq!(parse_hex_bytes("zz"), None);
    }
}
