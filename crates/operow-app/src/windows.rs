//! Content of each docked window and the `TabViewer` dispatching to them.

use std::collections::HashMap;

use egui_dock::TabViewer;
use egui_flow::{Flow, FlowOptions};
use operow_core::{BusId, CanErrorKind, NodeErrorState};
use operow_engine::{Command, NodeErrorInfo, RunState};

use crate::app::LiveBusStats;
use crate::diag_window::DiagWindow;
use crate::generator_window::GeneratorWindow;
use crate::graph::{Graph, GraphNode, GraphViewer};
use crate::graph_window::GraphWindow;
use crate::icons;
use crate::inspector::Inspector;
use crate::network_view::NetworkView;
use crate::runtime::{self, FaultsState, RuntimeState};
use crate::store::FrameStore;
use crate::test_editor::TestEditor;
use crate::tests_window::{TestsAction, TestsState};
use crate::theme::AppTheme;
use crate::trace::{NameLookup, Trace, TraceAction};
use crate::workspace::{WindowId, WindowKind};

const ERROR_RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x30, 0x30);

/// Borrows of the app state the windows draw and edit for one frame.
pub struct WindowViewer<'a> {
    pub graph: &'a mut Graph,
    pub inspector: &'a mut Inspector,
    pub traces: &'a mut HashMap<WindowId, Trace>,
    pub graphs: &'a mut HashMap<WindowId, GraphWindow>,
    pub generators: &'a mut HashMap<WindowId, GeneratorWindow>,
    pub diags: &'a mut HashMap<WindowId, DiagWindow>,
    pub tests: &'a mut TestsState,
    pub test_editors: &'a mut HashMap<WindowId, TestEditor>,
    /// Simulation time in seconds, for request timestamps.
    pub sim_time_s: f64,
    pub store: &'a FrameStore,
    pub names: &'a NameLookup,
    pub status_log: &'a mut Vec<String>,
    pub bus_stats: &'a HashMap<BusId, LiveBusStats>,
    pub node_states: &'a [NodeErrorInfo],
    pub hw_status: &'a [operow_engine::HwBusStatus],
    pub run_state: RunState,
    pub faults: &'a mut FaultsState,
    pub runtime: &'a mut RuntimeState,
    pub theme: AppTheme,
    pub menu_pos: &'a mut Option<egui::Pos2>,
    /// Folder of the project file, for relative log paths.
    pub project_dir: Option<std::path::PathBuf>,
    /// Engine commands issued by windows this frame.
    pub cmds: Vec<Command>,
    /// Requests from trace windows (graph wiring, generator, log lines).
    pub trace_actions: Vec<TraceAction>,
    /// Requests from the Tests window and the test editors.
    pub test_actions: Vec<TestsAction>,
    /// A window a button asked to open this frame.
    pub open_request: Option<WindowKind>,
}

impl WindowViewer<'_> {
    fn running(&self) -> bool {
        self.run_state != RunState::Stopped
    }

    /// Generator frames may only be sent while the measurement is running.
    fn can_send(&self) -> bool {
        self.run_state == RunState::Running
    }

    fn network_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!running, |ui| {
                if icons::icon_text_button(ui, icons::ecu(), "ECU").clicked() {
                    self.graph.add_ecu(egui::pos2(40.0, 40.0), "NewEcu");
                }
                if icons::icon_text_button(ui, icons::gateway(), "Gateway").clicked() {
                    self.graph.add_gateway(egui::pos2(40.0, 120.0));
                }
                if icons::icon_text_button(ui, icons::replay(), "Replay").clicked() {
                    self.graph.add_replay(egui::pos2(40.0, 160.0));
                }
                if icons::icon_text_button(ui, icons::bus(), "Bus").clicked() {
                    self.graph.add_bus(egui::pos2(40.0, 200.0));
                }
            });
            ui.separator();
            ui.label("View:");
            let mut view = self.graph.view();
            for v in NetworkView::ALL {
                ui.selectable_value(&mut view, v, v.label());
            }
            if view != self.graph.view() {
                self.graph.set_view(view);
            }
            let bus_line = view == NetworkView::BusLine;
            if ui
                .add_enabled(bus_line, egui::Button::new("Auto-arrange"))
                .on_hover_text("Stack the buses, put ECUs above their bus and gateways between")
                .on_disabled_hover_text("Available in the bus-line view")
                .clicked()
            {
                self.graph.auto_arrange();
            }
            if running {
                if ui
                    .button("Faults")
                    .on_hover_text("Open the fault-injection window")
                    .clicked()
                {
                    self.open_request = Some(WindowKind::Faults);
                }
                ui.weak("Editing is disabled while the measurement is running.");
            }
        });
        ui.separator();

        let view = self.graph.view();
        let (plan, _) = self.graph.prepare(self.theme);
        let loads = self
            .bus_stats
            .iter()
            .map(|(b, s)| (*b, s.load_pct))
            .collect();
        let opts = FlowOptions {
            nodes_connectable: !running,
            delete_key: !running,
            edges_reconnectable: !running,
            keyboard_shortcuts: !running,
            alignment_guides: true,
            keyboard_nudge: true,
            highlight_connected: true,
            connection_radius: if view == NetworkView::BusLine {
                36.0
            } else {
                20.0
            },
            ..Default::default()
        };
        let mut viewer = GraphViewer::new(self.theme, self.graph, plan, loads);
        if running {
            let links = self.graph.links();
            let bus_errors = self
                .bus_stats
                .iter()
                .map(|(b, s)| (*b, s.can_errors.total()))
                .collect();
            viewer = viewer.with_runtime(
                runtime::badges(
                    &links,
                    &self
                        .runtime
                        .held_states(self.node_states, std::time::Instant::now()),
                    self.runtime.offline_set(),
                ),
                bus_errors,
            );
        }
        let out = Flow::new("graph")
            .options(opts)
            .show(ui, &mut self.graph.state, &mut viewer);
        self.status_log
            .extend(self.graph.process_events(&out.events));

        if running {
            self.runtime_node_menus(&out);
            return;
        }
        if out.pane.secondary_clicked() {
            *self.menu_pos = out.pane.interact_pointer_pos();
        }
        let pos = self.menu_pos.unwrap_or(egui::pos2(40.0, 40.0));
        out.pane.context_menu(|ui| {
            ui.set_min_width(160.0);
            if ui.button("Add ECU").clicked() {
                let id = self.graph.add_ecu(pos, "NewEcu");
                self.graph.select(id);
                ui.close();
            }
            if ui.button("Add Gateway").clicked() {
                let id = self.graph.add_gateway(pos);
                self.graph.select(id);
                ui.close();
            }
            if ui.button("Add Replay node").clicked() {
                let id = self.graph.add_replay(pos);
                self.graph.select(id);
                ui.close();
            }
            if ui.button("Add CAN Bus").clicked() {
                let id = self.graph.add_bus(pos);
                self.graph.select(id);
                ui.close();
            }
        });
        let mut delete = None;
        for (id, resp) in &out.nodes {
            resp.context_menu(|ui| {
                ui.set_min_width(120.0);
                if ui.button("Properties").clicked() {
                    self.graph.select(*id);
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    delete = Some(*id);
                    ui.close();
                }
            });
        }
        if let Some(id) = delete {
            self.graph.remove(id);
        }
    }

    /// Right-click menu of a node while running: online/offline and
    /// force bus-off.
    fn runtime_node_menus(&mut self, out: &egui_flow::FlowResponse<GraphNode, ()>) {
        let links = self.graph.links();
        let mut cmds = Vec::new();
        for (id, resp) in &out.nodes {
            let Some(GraphNode::Ecu(ecu)) = self.graph.node(*id) else {
                continue;
            };
            let node = ecu.id;
            let buses = runtime::buses_of(&links, node);
            let rt = &mut *self.runtime;
            let names = self.names;
            resp.context_menu(|ui| {
                ui.set_min_width(150.0);
                let online = buses.iter().any(|b| !rt.is_offline(node, *b));
                let label = if online { "Go offline" } else { "Go online" };
                if ui.button(label).clicked() {
                    cmds.push(rt.set_online(node, None, &buses, !online));
                    ui.close();
                }
                ui.menu_button("Force bus-off", |ui| {
                    for bus in &buses {
                        if ui.button(names.bus_name(*bus)).clicked() {
                            cmds.push(Command::ForceBusOff(node, *bus));
                            ui.close();
                        }
                    }
                });
            });
        }
        self.cmds.extend(cmds);
    }

    fn properties_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        let links = self.graph.links();
        let mut rt_cmds = Vec::new();
        let (rt, states, names) = (&mut *self.runtime, self.node_states, self.names);
        let mut extra = |ui: &mut egui::Ui, graph: &Graph, sel: egui_flow::NodeId| {
            if let (true, Some(GraphNode::Ecu(ecu))) = (running, graph.node(sel)) {
                rt_cmds.extend(runtime::runtime_section(ui, ecu, &links, rt, states, names));
            }
        };
        let cmds = self.inspector.ui(
            ui,
            self.graph,
            running,
            &self.names.dbcs,
            self.project_dir.as_deref(),
            &mut extra,
        );
        self.cmds.extend(cmds);
        self.cmds.extend(rt_cmds);
        self.send_once_ui(ui);
    }

    fn send_once_ui(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.graph.selected() else {
            return;
        };
        let Some(GraphNode::Ecu(ecu)) = self.graph.node(sel) else {
            return;
        };
        if ecu.tx.is_empty() {
            return;
        }
        ui.separator();
        ui.label(format!("Interactive generator ({}):", ecu.name));
        let ecu_id = ecu.id;
        for msg in ecu.tx.clone() {
            if ui.button(format!("Send {}", msg.name)).clicked() {
                // `msg.bus` of None means all linked buses.
                self.cmds
                    .push(Command::SendOnce(ecu_id, msg.bus, msg.frame));
            }
        }
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if icons::icon_button(ui, icons::clear(), "Clear log").clicked() {
                self.status_log.clear();
            }
            ui.label(format!("{} lines", self.status_log.len()));
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.status_log.iter() {
                    let text = egui::RichText::new(line).monospace();
                    if crate::script_editor::is_error_line(line) {
                        ui.colored_label(ERROR_RED, text);
                    } else {
                        ui.label(text);
                    }
                }
            });
    }

    fn statistics_ui(&self, ui: &mut egui::Ui) {
        if self.bus_stats.is_empty() {
            ui.weak("No statistics yet. Start the simulation.");
            return;
        }
        let mut rows: Vec<(String, &LiveBusStats)> = self
            .bus_stats
            .iter()
            .map(|(b, s)| (self.names.bus_name(*b), s))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Grid::new("bus_stats_grid")
                    .num_columns(6)
                    .spacing([16.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Bus");
                        ui.strong("Load");
                        ui.strong("Frames/s");
                        ui.strong("Total");
                        ui.strong("CAN errors")
                            .on_hover_text("Error frames on the bus; hover a value for the kinds");
                        ui.strong("Dropped").on_hover_text(
                            "Frames that were never sent: CAN FD on a classic bus, from a bus-off or offline node, or dropped by a message control",
                        );
                        ui.end_row();
                        for (name, s) in rows {
                            ui.label(name);
                            load_bar(ui, s.load_pct);
                            ui.label(format!("{:.0}", s.frames_per_s));
                            ui.label(s.total_frames.to_string());
                            let errors = s.can_errors.total();
                            if errors > 0 {
                                let kinds: Vec<String> = CanErrorKind::ALL
                                    .iter()
                                    .filter(|k| s.can_errors.get(**k) > 0)
                                    .map(|k| format!("{}: {}", k.label(), s.can_errors.get(*k)))
                                    .collect();
                                ui.colored_label(ERROR_RED, errors.to_string())
                                    .on_hover_text(kinds.join("\n"));
                            } else {
                                ui.label("0");
                            }
                            let dropped = s.error_frames
                                + s.dropped_bus_off
                                + s.dropped_offline
                                + s.dropped_msg_control;
                            if dropped > 0 {
                                ui.colored_label(ERROR_RED, dropped.to_string()).on_hover_text(
                                    format!(
                                        "Unsupported (FD on classic bus): {}\nBus-off sender: {}\nOffline sender: {}\nMessage control: {}",
                                        s.error_frames,
                                        s.dropped_bus_off,
                                        s.dropped_offline,
                                        s.dropped_msg_control
                                    ),
                                );
                            } else {
                                ui.label("0");
                            }
                            ui.end_row();
                        }
                    });
                if !self.hw_status.is_empty() {
                    ui.add_space(12.0);
                    ui.strong("Hardware buses");
                    self.hw_status_ui(ui);
                }
                if !self.node_states.is_empty() {
                    ui.add_space(12.0);
                    ui.strong("Nodes");
                    self.node_states_ui(ui);
                }
            });
    }

    /// Per hardware bus: interface, link and controller state, TEC/REC,
    /// frames received and sent, and frames held back by listen-only.
    fn hw_status_ui(&self, ui: &mut egui::Ui) {
        let mut rows: Vec<(String, &operow_engine::HwBusStatus)> = self
            .hw_status
            .iter()
            .map(|s| (self.names.bus_name(s.bus), s))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        egui::Grid::new("hw_status_grid")
            .num_columns(8)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for h in [
                    "Bus",
                    "Interface",
                    "State",
                    "TEC/REC",
                    "Rx",
                    "Tx",
                    "Listen-only drops",
                ] {
                    ui.strong(h);
                }
                ui.end_row();
                for (name, s) in rows {
                    ui.label(&name);
                    ui.label(&s.interface);
                    let (text, ok) = crate::hw_ui::status_chip(s);
                    let state = match &s.link {
                        operow_engine::HwLink::Error(e) => format!("error: {e}"),
                        operow_engine::HwLink::Closed => "closed".to_string(),
                        operow_engine::HwLink::Open => s.controller.state.label().to_string(),
                    };
                    if ok {
                        ui.label(state);
                    } else {
                        ui.colored_label(ERROR_RED, state).on_hover_text(text);
                    }
                    ui.label(format!("{}/{}", s.controller.tec, s.controller.rec));
                    ui.label(s.rx_frames.to_string());
                    ui.label(s.tx_frames.to_string());
                    let dropped = self
                        .bus_stats
                        .get(&s.bus)
                        .map_or(0, |b| b.dropped_listen_only);
                    ui.label(dropped.to_string());
                    ui.end_row();
                }
            });
    }

    /// Per-node fault-confinement table: state and TEC/REC on every bus.
    fn node_states_ui(&self, ui: &mut egui::Ui) {
        let mut rows: Vec<(String, String, &NodeErrorInfo)> = self
            .node_states
            .iter()
            .map(|n| (self.names.node_name(n.node), self.names.bus_name(n.bus), n))
            .collect();
        rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        egui::Grid::new("node_state_grid")
            .num_columns(5)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for h in ["Node", "Bus", "State", "TEC", "REC"] {
                    ui.strong(h);
                }
                ui.end_row();
                for (node, bus, n) in rows {
                    ui.label(node);
                    ui.label(bus);
                    let text = n.state.label();
                    match n.state {
                        NodeErrorState::ErrorActive => ui.label(text),
                        NodeErrorState::ErrorPassive => {
                            ui.colored_label(egui::Color32::from_rgb(0xd0, 0x90, 0x10), text)
                        }
                        NodeErrorState::BusOff => ui.colored_label(ERROR_RED, text),
                    };
                    ui.label(n.tec.to_string());
                    ui.label(n.rec.to_string());
                    ui.end_row();
                }
            });
    }
}

/// Bus load bar with the percentage centred in it. Unlike `ProgressBar` it
/// draws no filled cap at 0%.
fn load_bar(ui: &mut egui::Ui, pct: f64) {
    let size = egui::vec2(110.0, ui.spacing().interact_size.y.min(18.0));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = egui::CornerRadius::same(3);
    let visuals = ui.visuals();
    let painter = ui.painter();
    painter.rect_filled(rect, radius, visuals.extreme_bg_color);
    let frac = (pct / 100.0).clamp(0.0, 1.0) as f32;
    if frac > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * frac).max(2.0));
        painter.rect_filled(fill, radius, visuals.selection.bg_fill);
    }
    painter.rect_stroke(
        rect,
        radius,
        visuals.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{pct:.1}%"),
        egui::TextStyle::Body.resolve(ui.style()),
        visuals.text_color(),
    );
}

impl TabViewer for WindowViewer<'_> {
    type Tab = WindowId;

    fn title(&mut self, tab: &mut WindowId) -> egui::WidgetText {
        let title = match tab.kind {
            WindowKind::Trace => self.traces.get(tab).and_then(|t| t.title.as_deref()),
            WindowKind::Graph => self.graphs.get(tab).and_then(|g| g.title.as_deref()),
            WindowKind::Generator => self.generators.get(tab).and_then(|g| g.title.as_deref()),
            WindowKind::Diag => self.diags.get(tab).and_then(|g| g.title.as_deref()),
            _ => None,
        };
        if tab.kind == WindowKind::TestEditor {
            let name = self.test_editors.get(tab).map(|e| e.title());
            return format!("Test \u{b7} {}", name.unwrap_or_default()).into();
        }
        match title {
            Some(t) => format!("{} \u{b7} {t}", tab.title()).into(),
            None => tab.title().into(),
        }
    }

    fn on_tab_button(&mut self, tab: &mut WindowId, response: &egui::Response) {
        // Double-click a Trace, Graph or Generator tab to rename it.
        if !response.double_clicked() {
            return;
        }
        match tab.kind {
            WindowKind::Trace => {
                if let Some(trace) = self.traces.get_mut(tab) {
                    trace.begin_rename();
                }
            }
            WindowKind::Graph => {
                if let Some(g) = self.graphs.get_mut(tab) {
                    g.begin_rename();
                }
            }
            WindowKind::Generator => {
                if let Some(g) = self.generators.get_mut(tab) {
                    g.begin_rename();
                }
            }
            WindowKind::Diag => {
                if let Some(g) = self.diags.get_mut(tab) {
                    g.begin_rename();
                }
            }
            _ => {}
        }
    }

    fn id(&mut self, tab: &mut WindowId) -> egui::Id {
        egui::Id::new(("window", *tab))
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut WindowId) {
        let id = *tab;
        // The default selected-text colour is blue on the blue selection
        // fill; use a high-contrast one in both themes.
        ui.visuals_mut().selection.stroke.color = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(0x10, 0x14, 0x1c)
        } else {
            egui::Color32::WHITE
        };
        match id.kind {
            WindowKind::Network => self.network_ui(ui),
            WindowKind::Properties => self.properties_ui(ui),
            WindowKind::Trace => {
                if let Some(trace) = self.traces.get_mut(&id) {
                    let actions = trace.ui(ui, self.store, self.names);
                    self.trace_actions.extend(actions);
                }
            }
            WindowKind::Log => self.log_ui(ui),
            WindowKind::Statistics => self.statistics_ui(ui),
            WindowKind::Faults => {
                let running = self.running();
                let cmds = self.faults.ui(ui, self.names, running);
                self.cmds.extend(cmds);
            }
            WindowKind::Graph => {
                if let Some(g) = self.graphs.get_mut(&id) {
                    g.ui(ui, id, self.store, self.names, &self.graph.user_signals);
                }
            }
            WindowKind::Generator => {
                let can_send = self.can_send();
                if let Some(g) = self.generators.get_mut(&id) {
                    let cmds = g.ui(ui, id, self.names, can_send);
                    self.cmds.extend(cmds);
                }
            }
            WindowKind::Diag => {
                let can_send = self.can_send();
                if let Some(d) = self.diags.get_mut(&id) {
                    let cmds = d.ui(ui, id, self.names, can_send, self.sim_time_s);
                    self.cmds.extend(cmds);
                }
            }
            WindowKind::Tests => {
                let actions = self.tests.ui(ui, egui::Id::new(("tests", id)));
                self.test_actions.extend(actions);
            }
            WindowKind::TestEditor => {
                let running = self.tests.running();
                if let Some(e) = self.test_editors.get_mut(&id) {
                    let actions = e.ui(ui, egui::Id::new(("test_editor", id)), running);
                    self.test_actions.extend(actions);
                }
            }
        }
    }

    fn scroll_bars(&self, _tab: &WindowId) -> [bool; 2] {
        // Windows manage their own scrolling.
        [false, false]
    }
}
