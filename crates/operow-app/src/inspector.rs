//! Properties tab: inspector for the selected
//! ECU or CAN bus.

use egui_flow::NodeId as FlowId;
use operow_core::{BusId, CanFrame, IdFilter, NodeId, NodeKind, RouteRule, SendType, TxMessage};
use operow_engine::{Command, EcuCommand};

use crate::dbcs::{self, DbcStore};
use crate::graph::{Graph, GraphNode};
use crate::icons;

#[derive(Default)]
pub struct Inspector {
    /// Per-row hex-data scratch buffers, keyed by tx-message index, so users
    /// can type invalid-but-in-progress hex without losing their place.
    data_buf: std::collections::HashMap<usize, String>,
    id_buf: std::collections::HashMap<usize, String>,
    /// Route-table hex scratch buffers keyed by (route index, field).
    route_buf: std::collections::HashMap<(usize, u8), String>,
    error: Option<String>,
    last_sel: Option<FlowId>,
    /// Live (running-only) UI state, keyed by (ECU, tx-message index).
    live_active: std::collections::HashMap<(NodeId, usize), bool>,
    live_payload: std::collections::HashMap<(NodeId, usize), String>,
    live_error: std::collections::HashMap<(NodeId, usize), String>,
    was_running: bool,
    script_check: crate::script_editor::ScriptCheck,
    script_window: bool,
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
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut Graph,
        running: bool,
        dbcs: &DbcStore,
    ) -> Vec<Command> {
        let mut cmds = Vec::new();
        if running != self.was_running {
            self.was_running = running;
            self.live_active.clear();
            self.live_payload.clear();
            self.live_error.clear();
        }
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
            self.route_buf.clear();
            self.error = None;
            self.script_window = false;
        }

        ui.add_space(4.0);
        egui::ScrollArea::both().show(ui, |ui| {
            self.node_ui(ui, graph, sel, running, dbcs, &mut cmds);
        });
        cmds
    }

    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut Graph,
        sel: FlowId,
        running: bool,
        dbcs: &DbcStore,
        cmds: &mut Vec<Command>,
    ) {
        let linked = linked_buses(graph, sel);
        if matches!(graph.node(sel), Some(GraphNode::Ecu(e)) if matches!(e.kind, NodeKind::Gateway { .. }))
        {
            ui.strong("Gateway");
        }
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
                    ui.horizontal(|ui| {
                        ui.label("Node type:");
                        let is_gw = matches!(ecu.kind, NodeKind::Gateway { .. });
                        egui::ComboBox::from_id_salt(("node_type", sel))
                            .selected_text(if is_gw { "Gateway" } else { "ECU" })
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(!is_gw, "ECU (drops routes)").clicked()
                                    && is_gw
                                {
                                    ecu.kind = NodeKind::Ecu;
                                    self.route_buf.clear();
                                }
                                if ui.selectable_label(is_gw, "Gateway").clicked() && !is_gw {
                                    ecu.kind = NodeKind::Gateway { routes: vec![] };
                                }
                            });
                    });
                    if let NodeKind::Gateway { routes } = &mut ecu.kind {
                        ui.separator();
                        Self::routes_ui(ui, &mut self.route_buf, sel, routes, &linked);
                    }
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
                                    egui::TextEdit::singleline(&mut msg.name).desired_width(110.0),
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
                        .icon(crate::icons::disclosure)
                        .id_salt(("tx_msg", sel, i))
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(!running, |ui| {
                                Self::edit_behavior_ui(ui, sel, i, msg, &linked);
                            });
                            if let Some(def) = dbcs.message_for_tx(msg, &linked) {
                                Self::dbc_ui(ui, sel, i, def);
                            }
                            if running {
                                self.live_ui(ui, ecu_id, i, msg, cmds);
                            }
                        });
                }

                ui.separator();
                self.script_ui(ui, sel, &mut ecu.script, running);
            }
        }

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), err);
        }
    }
}

impl Inspector {
    /// Collapsible Rhai script section for ECU-like nodes.
    fn script_ui(
        &mut self,
        ui: &mut egui::Ui,
        sel: FlowId,
        script: &mut Option<String>,
        running: bool,
    ) {
        use crate::script_editor::{self as se, TEMPLATE};
        let id = ui.make_persistent_id(("script_section", sel));
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id,
            script.is_some(),
        );
        state
            .show_header(ui, |ui| se::header_ui(ui, script.as_deref()))
            .body(|ui| {
                ui.horizontal(|ui| {
                    if script.is_none() {
                        if ui
                            .add_enabled(!running, egui::Button::new("Add script"))
                            .clicked()
                        {
                            *script = Some(TEMPLATE.to_string());
                        }
                    } else {
                        if ui
                            .add_enabled(!running, egui::Button::new("Remove script"))
                            .clicked()
                        {
                            *script = None;
                            self.script_window = false;
                        }
                        if ui.button("Open in window").clicked() {
                            self.script_window = true;
                        }
                    }
                });
                if let Some(src) = script.as_mut() {
                    se::editor_ui(
                        ui,
                        egui::Id::new(("script_edit", sel)),
                        src,
                        !running,
                        &mut self.script_check,
                        14,
                    );
                }
            });
        if self.script_window
            && let Some(src) = script.as_mut()
        {
            let mut open = true;
            egui::Window::new("Script")
                .id(egui::Id::new(("script_window", sel)))
                .open(&mut open)
                .resizable(true)
                .default_size([640.0, 480.0])
                .show(ui.ctx(), |ui| {
                    se::editor_ui(
                        ui,
                        egui::Id::new(("script_edit_win", sel)),
                        src,
                        !running,
                        &mut self.script_check,
                        28,
                    );
                });
            self.script_window = open;
        }
    }

    fn routes_ui(
        ui: &mut egui::Ui,
        bufs: &mut std::collections::HashMap<(usize, u8), String>,
        sel: FlowId,
        routes: &mut Vec<RouteRule>,
        linked: &[(BusId, String)],
    ) {
        ui.label("Routes:");
        if linked.len() < 2 {
            ui.label(
                egui::RichText::new("Connect this gateway to at least two buses")
                    .weak()
                    .italics(),
            );
        }
        let mut remove: Option<usize> = None;
        if !routes.is_empty() {
            egui::Grid::new(("route_grid", sel))
                .num_columns(7)
                .striped(true)
                .show(ui, |ui| {
                    for h in [
                        "",
                        "From",
                        "To",
                        "Filter",
                        "Remap ID (hex)",
                        "Delay (\u{b5}s)",
                        "",
                    ] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for (i, r) in routes.iter_mut().enumerate() {
                        if let Some(issue) = route_issue(r, linked) {
                            ui.label(egui::RichText::new("\u{26a0}").color(RED))
                                .on_hover_text(issue);
                        } else {
                            ui.label("");
                        }
                        egui::ComboBox::from_id_salt(("route_from", sel, i))
                            .selected_text(bus_label(Some(r.from_bus), linked))
                            .show_ui(ui, |ui| {
                                for (id, name) in linked {
                                    ui.selectable_value(&mut r.from_bus, *id, name);
                                }
                            });
                        let to_text = if r.to_buses.is_empty() {
                            "\u{2014}".to_string()
                        } else {
                            r.to_buses
                                .iter()
                                .map(|b| bus_label(Some(*b), linked))
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        ui.menu_button(to_text, |ui| {
                            for (id, name) in linked.iter().filter(|(id, _)| *id != r.from_bus) {
                                let mut on = r.to_buses.contains(id);
                                if ui.checkbox(&mut on, name).changed() {
                                    if on {
                                        r.to_buses.push(*id);
                                    } else {
                                        r.to_buses.retain(|b| b != id);
                                    }
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            let cur = FilterKind::of(r.filter);
                            egui::ComboBox::from_id_salt(("route_filter", sel, i))
                                .width(70.0)
                                .selected_text(cur.label())
                                .show_ui(ui, |ui| {
                                    for k in FilterKind::ALL {
                                        if ui.selectable_label(cur == k, k.label()).clicked()
                                            && cur != k
                                        {
                                            r.filter = k.to_filter(r.filter);
                                            bufs.retain(|(row, f), _| *row != i || *f == 10);
                                        }
                                    }
                                });
                            match &mut r.filter {
                                IdFilter::Any => {}
                                IdFilter::Exact { id, extended } => {
                                    hex_edit(ui, bufs, (i, 0), id, 52.0);
                                    ui.checkbox(extended, "Ext");
                                }
                                IdFilter::Range { lo, hi } => {
                                    hex_edit(ui, bufs, (i, 0), lo, 52.0);
                                    ui.label("..");
                                    hex_edit(ui, bufs, (i, 1), hi, 52.0);
                                }
                                IdFilter::Mask { id, mask } => {
                                    hex_edit(ui, bufs, (i, 0), id, 52.0);
                                    ui.label("&");
                                    hex_edit(ui, bufs, (i, 1), mask, 52.0);
                                }
                            }
                        });
                        let buf = bufs.entry((i, 10)).or_insert_with(|| {
                            r.remap_id.map(|v| format!("{v:X}")).unwrap_or_default()
                        });
                        let parsed = parse_optional_hex(buf);
                        let mut te = egui::TextEdit::singleline(buf).desired_width(52.0);
                        if parsed.is_none() {
                            te = te.text_color(RED);
                        }
                        ui.add(te);
                        if let Some(v) = parsed {
                            r.remap_id = v;
                        }
                        ui.add(egui::DragValue::new(&mut r.delay_us).range(0..=10_000_000));
                        if icons::icon_button(ui, icons::clear(), "Remove route").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
        }
        if ui.button("+ Add route").clicked()
            && let Some(r) = default_route(linked)
        {
            routes.push(r);
        }
        if let Some(i) = remove {
            routes.remove(i);
            bufs.clear();
        }
    }

    /// Read-only DBC view of a transmit message: its name and signal table.
    fn dbc_ui(ui: &mut egui::Ui, sel: FlowId, i: usize, def: &operow_dbc::MessageDef) {
        ui.horizontal(|ui| {
            ui.label("DBC message:");
            ui.strong(&def.name);
        });
        egui::CollapsingHeader::new(format!("Signals ({})", def.signals.len()))
            .id_salt(("dbc_signals", sel, i))
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new(("dbc_grid", sel, i))
                    .striped(true)
                    .num_columns(4)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        for h in ["Signal", "Bits", "Factor / offset", "Unit"] {
                            ui.label(egui::RichText::new(h).small().weak());
                        }
                        ui.end_row();
                        for s in &def.signals {
                            ui.label(&s.name);
                            ui.monospace(dbcs::bit_layout(s));
                            ui.monospace(format!(
                                "{} / {}",
                                dbcs::format_value(s.factor, 0.5),
                                dbcs::format_value(s.offset, 0.5)
                            ));
                            ui.label(&s.unit);
                            ui.end_row();
                        }
                    });
            });
    }

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

const RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x30, 0x30);

/// Filter discriminant without its parameters, for the combo box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterKind {
    Any,
    Exact,
    Range,
    Mask,
}

impl FilterKind {
    pub const ALL: [FilterKind; 4] = [
        FilterKind::Any,
        FilterKind::Exact,
        FilterKind::Range,
        FilterKind::Mask,
    ];

    pub fn of(f: IdFilter) -> Self {
        match f {
            IdFilter::Any => FilterKind::Any,
            IdFilter::Exact { .. } => FilterKind::Exact,
            IdFilter::Range { .. } => FilterKind::Range,
            IdFilter::Mask { .. } => FilterKind::Mask,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FilterKind::Any => "Any",
            FilterKind::Exact => "Exact",
            FilterKind::Range => "Range",
            FilterKind::Mask => "Mask",
        }
    }

    /// Convert `current` to this kind, carrying over its first id value.
    pub fn to_filter(self, current: IdFilter) -> IdFilter {
        if FilterKind::of(current) == self {
            return current;
        }
        let id = match current {
            IdFilter::Any => 0,
            IdFilter::Exact { id, .. } | IdFilter::Mask { id, .. } => id,
            IdFilter::Range { lo, .. } => lo,
        };
        match self {
            FilterKind::Any => IdFilter::Any,
            FilterKind::Exact => IdFilter::Exact {
                id,
                extended: false,
            },
            FilterKind::Range => IdFilter::Range { lo: id, hi: id },
            FilterKind::Mask => IdFilter::Mask { id, mask: 0x7FF },
        }
    }
}

/// A new route: first linked bus to all other linked buses, any id.
pub fn default_route(linked: &[(BusId, String)]) -> Option<RouteRule> {
    let (from, _) = linked.first()?;
    Some(RouteRule {
        from_bus: *from,
        to_buses: linked.iter().skip(1).map(|(b, _)| *b).collect(),
        filter: IdFilter::Any,
        remap_id: None,
        delay_us: 0,
    })
}

/// Per-row problem with a route, mirroring `Topology::validate`.
pub fn route_issue(r: &RouteRule, linked: &[(BusId, String)]) -> Option<String> {
    let is_linked = |b: &BusId| linked.iter().any(|(id, _)| id == b);
    if !is_linked(&r.from_bus) {
        return Some("From bus is not linked to this gateway".into());
    }
    if r.to_buses.is_empty() {
        return Some("No destination buses selected".into());
    }
    if r.to_buses.iter().any(|b| !is_linked(b)) {
        return Some("A destination bus is not linked to this gateway".into());
    }
    if r.to_buses.contains(&r.from_bus) {
        return Some("From bus is also a destination".into());
    }
    None
}

fn parse_hex_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(s, 16).ok()
}

/// `Some(None)` for empty input, `Some(Some(v))` for a valid hex value,
/// `None` when invalid.
pub fn parse_optional_hex(s: &str) -> Option<Option<u32>> {
    if s.trim().is_empty() {
        Some(None)
    } else {
        parse_hex_u32(s).map(Some)
    }
}

/// Hex text field bound to `value`; invalid text is shown red and not applied.
fn hex_edit(
    ui: &mut egui::Ui,
    bufs: &mut std::collections::HashMap<(usize, u8), String>,
    key: (usize, u8),
    value: &mut u32,
    width: f32,
) {
    let buf = bufs.entry(key).or_insert_with(|| format!("{:X}", *value));
    let parsed = parse_hex_u32(buf);
    let mut te = egui::TextEdit::singleline(buf).desired_width(width);
    if parsed.is_none() {
        te = te.text_color(RED);
    }
    ui.add(te);
    if let Some(v) = parsed {
        *value = v;
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

    fn linked2() -> Vec<(BusId, String)> {
        vec![(BusId(1), "A".into()), (BusId(2), "B".into())]
    }

    #[test]
    fn filter_kind_conversions() {
        for k in FilterKind::ALL {
            assert_eq!(FilterKind::of(k.to_filter(IdFilter::Any)), k);
        }
        let f = FilterKind::Range.to_filter(IdFilter::Exact {
            id: 0x123,
            extended: true,
        });
        assert_eq!(
            f,
            IdFilter::Range {
                lo: 0x123,
                hi: 0x123
            }
        );
        let ex = IdFilter::Exact {
            id: 5,
            extended: true,
        };
        assert_eq!(FilterKind::Exact.to_filter(ex), ex);
    }

    #[test]
    fn default_route_and_issues() {
        assert!(default_route(&[]).is_none());
        let r = default_route(&linked2()).unwrap();
        assert_eq!(r.from_bus, BusId(1));
        assert_eq!(r.to_buses, vec![BusId(2)]);
        assert_eq!(route_issue(&r, &linked2()), None);

        let mut bad = r.clone();
        bad.to_buses.clear();
        assert!(route_issue(&bad, &linked2()).is_some());
        bad.to_buses = vec![BusId(1)];
        assert!(route_issue(&bad, &linked2()).unwrap().contains("also"));
        bad.from_bus = BusId(9);
        assert!(route_issue(&bad, &linked2()).unwrap().contains("From"));
        bad.from_bus = BusId(1);
        bad.to_buses = vec![BusId(7)];
        assert!(
            route_issue(&bad, &linked2())
                .unwrap()
                .contains("destination")
        );
    }

    #[test]
    fn optional_hex() {
        assert_eq!(parse_optional_hex(""), Some(None));
        assert_eq!(parse_optional_hex("  "), Some(None));
        assert_eq!(parse_optional_hex("1F"), Some(Some(0x1F)));
        assert_eq!(parse_optional_hex("0x200"), Some(Some(0x200)));
        assert_eq!(parse_optional_hex("xyz"), None);
    }

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex_bytes("01 ff"), Some(vec![1, 255]));
        assert_eq!(parse_hex_bytes("zz"), None);
    }
}
