//! Content of each docked window and the `TabViewer` dispatching to them.

use std::collections::HashMap;

use egui_dock::TabViewer;
use egui_flow::{Flow, FlowOptions};
use operow_core::BusId;
use operow_engine::{Command, RunState};

use crate::app::LiveBusStats;
use crate::graph::{Graph, GraphNode, GraphViewer};
use crate::graph_window::GraphWindow;
use crate::icons;
use crate::inspector::Inspector;
use crate::store::FrameStore;
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
    pub store: &'a FrameStore,
    pub names: &'a NameLookup,
    pub status_log: &'a mut Vec<String>,
    pub bus_stats: &'a HashMap<BusId, LiveBusStats>,
    pub run_state: RunState,
    pub theme: AppTheme,
    pub menu_pos: &'a mut Option<egui::Pos2>,
    /// Engine commands issued by windows this frame.
    pub cmds: Vec<Command>,
    /// A node was deleted, so loaded databases need pruning.
    pub graph_changed: bool,
    /// Requests from trace windows (graph wiring, generator, log lines).
    pub trace_actions: Vec<TraceAction>,
}

impl WindowViewer<'_> {
    fn running(&self) -> bool {
        self.run_state != RunState::Stopped
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
                if icons::icon_text_button(ui, icons::bus(), "Bus").clicked() {
                    self.graph.add_bus(egui::pos2(40.0, 200.0));
                }
            });
            if running {
                ui.weak("Editing is disabled while the measurement is running.");
            }
        });
        ui.separator();

        let opts = FlowOptions {
            nodes_connectable: !running,
            delete_key: !running,
            ..Default::default()
        };
        let mut viewer = GraphViewer { theme: self.theme };
        let out = Flow::new("graph")
            .options(opts)
            .show(ui, &mut self.graph.state, &mut viewer);

        if running {
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
            self.graph_changed = true;
        }
    }

    fn properties_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        let cmds = self.inspector.ui(ui, self.graph, running, &self.names.dbcs);
        self.cmds.extend(cmds);
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
                    .num_columns(5)
                    .spacing([16.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["Bus", "Load", "Frames/s", "Total", "Errors"] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for (name, s) in rows {
                            ui.label(name);
                            ui.add(
                                egui::ProgressBar::new((s.load_pct / 100.0).clamp(0.0, 1.0) as f32)
                                    .desired_width(180.0)
                                    .text(format!("{:.1}%", s.load_pct)),
                            );
                            ui.label(format!("{:.0}", s.frames_per_s));
                            ui.label(s.total_frames.to_string());
                            if s.error_frames > 0 {
                                ui.colored_label(ERROR_RED, s.error_frames.to_string());
                            } else {
                                ui.label("0");
                            }
                            ui.end_row();
                        }
                    });
            });
    }
}

impl TabViewer for WindowViewer<'_> {
    type Tab = WindowId;

    fn title(&mut self, tab: &mut WindowId) -> egui::WidgetText {
        let title = match tab.kind {
            WindowKind::Trace => self.traces.get(tab).and_then(|t| t.title.as_deref()),
            WindowKind::Graph => self.graphs.get(tab).and_then(|g| g.title.as_deref()),
            _ => None,
        };
        match title {
            Some(t) => format!("{} \u{b7} {t}", tab.title()).into(),
            None => tab.title().into(),
        }
    }

    fn on_tab_button(&mut self, tab: &mut WindowId, response: &egui::Response) {
        // Double-click a Trace or Graph tab to rename it.
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
            WindowKind::Graph => {
                if let Some(g) = self.graphs.get_mut(&id) {
                    g.ui(ui, id, self.store, self.names, &self.graph.user_signals);
                }
            }
            WindowKind::Generator => {
                ui.add_space(8.0);
                ui.heading(id.title());
                ui.weak("Coming in a later step.");
            }
        }
    }

    fn scroll_bars(&self, _tab: &WindowId) -> [bool; 2] {
        // Windows manage their own scrolling.
        [false, false]
    }
}
