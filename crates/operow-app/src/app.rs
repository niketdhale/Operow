use std::path::PathBuf;

use operow_core::{BusId, Timestamp, Topology};
use operow_engine::{BusStats, Command, Engine, EngineEvent, EngineHandle, RunState};

use egui_flow::{Flow, FlowOptions, PulseStyle};

use crate::graph::{Graph, GraphNode, GraphViewer};
use crate::icons;
use crate::inspector::Inspector;
use crate::theme::AppTheme;
use crate::trace::{NameLookup, Trace, TraceMode};

const MAX_EVENTS_PER_FRAME: usize = 256;

/// Per-bus live stats shown in the top bar.
#[derive(Default, Clone, Copy)]
struct LiveBusStats {
    prev: BusStats,
    load_pct: f64,
    frames_per_s: f64,
    total_frames: u64,
    error_frames: u64,
}

pub struct OperowApp {
    graph: Graph,
    inspector: Inspector,
    trace: Trace,
    names: NameLookup,

    engine: EngineHandle,
    run_state: RunState,
    speed: f64,
    sim_time: Timestamp,
    prev_stats_time: Timestamp,
    bus_stats: std::collections::HashMap<BusId, LiveBusStats>,

    theme: AppTheme,
    status_log: Vec<String>,
    last_error: Option<String>,
    /// Flow-space position of the last right-click on the canvas, where
    /// "Add ECU"/"Add CAN Bus" place the new node.
    menu_pos: Option<egui::Pos2>,

    // Interactive generator scratch state, stored per selected ECU via the
    // inspector node id is out of scope here; kept minimal: a floating
    // "send once" affordance lives in the top bar acting on the selected
    // node.

    // --screenshot support
    screenshot_path: Option<PathBuf>,
    screenshot_start: Option<std::time::Instant>,
    screenshot_taken: bool,
}

impl OperowApp {
    pub fn new(screenshot_path: Option<PathBuf>) -> Self {
        let graph = Graph::default_demo();
        let mut names = NameLookup::default();
        names.rebuild(&graph.to_topology());

        OperowApp {
            graph,
            inspector: Inspector::default(),
            trace: Trace::default(),
            names,
            engine: Engine::spawn(),
            run_state: RunState::Stopped,
            speed: 1.0,
            sim_time: Timestamp::ZERO,
            prev_stats_time: Timestamp::ZERO,
            bus_stats: Default::default(),
            theme: AppTheme::Light,
            status_log: Vec::new(),
            last_error: None,
            menu_pos: None,
            screenshot_path,
            screenshot_start: None,
            screenshot_taken: false,
        }
    }

    /// Startup options (mainly for headless screenshots): begin in
    /// fixed-position trace mode and/or load a topology file.
    pub fn configure_startup(&mut self, fixed_trace: bool, topology: Option<&std::path::Path>) {
        if fixed_trace {
            self.trace.mode = TraceMode::Fixed;
        }
        if let Some(path) = topology {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|s| Topology::from_json(&s).map_err(|e| e.to_string()))
            {
                Ok(topo) => {
                    self.graph = Graph::from_topology(&topo);
                    self.names.rebuild(&topo);
                }
                Err(e) => self.last_error = Some(format!("load error: {e}")),
            }
        }
    }

    fn log(&mut self, msg: impl Into<String>) {
        self.status_log.push(msg.into());
        if self.status_log.len() > 500 {
            self.status_log.remove(0);
        }
    }

    fn start(&mut self) {
        let topo = self.graph.to_topology();
        if let Err(e) = topo.validate() {
            self.last_error = Some(format!("invalid topology: {e}"));
            return;
        }
        self.names.rebuild(&topo);
        self.trace.clear();
        self.bus_stats.clear();
        self.sim_time = Timestamp::ZERO;
        self.prev_stats_time = Timestamp::ZERO;
        let _ = self.engine.cmd.send(Command::Load(topo));
        let _ = self.engine.cmd.send(Command::SetSpeed(self.speed));
        let _ = self.engine.cmd.send(Command::Start);
    }

    fn stop(&mut self) {
        let _ = self.engine.cmd.send(Command::Stop);
    }

    fn pause_resume(&mut self) {
        match self.run_state {
            RunState::Running => {
                let _ = self.engine.cmd.send(Command::Pause);
            }
            RunState::Paused => {
                let _ = self.engine.cmd.send(Command::Resume);
            }
            RunState::Stopped => {}
        }
    }

    fn new_topology(&mut self) {
        self.stop();
        self.graph = Graph::new();
        self.graph.add_bus(egui::pos2(80.0, 260.0));
        self.names.rebuild(&self.graph.to_topology());
        self.trace.clear();
    }

    fn open_topology(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Operow topology", &["operow.json", "json"])
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(s) => match Topology::from_json(&s) {
                    Ok(topo) => match topo.validate() {
                        Ok(()) => {
                            self.stop();
                            self.graph = Graph::from_topology(&topo);
                            self.names.rebuild(&topo);
                            self.log(format!("loaded {}", path.display()));
                        }
                        Err(e) => self.last_error = Some(format!("invalid topology: {e}")),
                    },
                    Err(e) => self.last_error = Some(format!("parse error: {e}")),
                },
                Err(e) => self.last_error = Some(format!("read error: {e}")),
            }
        }
    }

    fn save_topology(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("topology.operow.json")
            .add_filter("Operow topology", &["operow.json", "json"])
            .save_file()
        {
            let json = self.graph.to_topology().to_json();
            if let Err(e) = std::fs::write(&path, json) {
                self.last_error = Some(format!("write error: {e}"));
            } else {
                self.log(format!("saved {}", path.display()));
            }
        }
    }

    fn drain_events(&mut self) {
        let mut n = 0;
        while n < MAX_EVENTS_PER_FRAME {
            match self.engine.events.try_recv() {
                Ok(ev) => {
                    self.handle_event(ev);
                    n += 1;
                }
                Err(_) => break,
            }
        }
    }

    fn handle_event(&mut self, ev: EngineEvent) {
        match ev {
            EngineEvent::Frames(frames) => {
                for f in &frames {
                    let bus_name = self.names.bus_name(f.bus);
                    let sender_name = self.names.node_name(f.sender);
                    let origin_name = self.names.node_name(f.origin);
                    let msg_name = self.names.msg_name(f.origin, f.frame.id);
                    self.trace
                        .push(f, &bus_name, &sender_name, &origin_name, &msg_name);
                    self.sim_time = f.time;
                }
                // One particle per sending ECU per batch: shows live traffic on
                // the wires without a dot for each of the hundreds of frames/s.
                let mut senders: std::collections::HashMap<operow_core::NodeId, bool> =
                    std::collections::HashMap::new();
                for f in &frames {
                    *senders.entry(f.sender).or_default() |= f.frame.fd;
                }
                for (sender, fd) in senders {
                    let color = self.theme.bus_color(if fd { 2 } else { 0 });
                    self.graph.pulse_sender(
                        sender,
                        PulseStyle {
                            color: Some(color),
                            radius: 4.0,
                            duration: 0.6,
                        },
                    );
                }
            }
            EngineEvent::Stats { time, buses } => {
                let dt_ns = time.0.saturating_sub(self.prev_stats_time.0).max(1);
                for (bus, stats) in buses {
                    let entry = self.bus_stats.entry(bus).or_default();
                    let d_busy = stats.busy_ns.saturating_sub(entry.prev.busy_ns);
                    let d_frames = stats.frames.saturating_sub(entry.prev.frames);
                    entry.load_pct = d_busy as f64 / dt_ns as f64 * 100.0;
                    entry.frames_per_s = d_frames as f64 / (dt_ns as f64 / 1e9);
                    entry.total_frames = stats.frames;
                    entry.error_frames = stats.error_frames;
                    entry.prev = stats;
                }
                self.prev_stats_time = time;
                self.sim_time = time;
            }
            EngineEvent::State(s) => {
                self.run_state = s;
                self.log(format!("state -> {s:?}"));
            }
            EngineEvent::Log(msg) => self.log(msg),
            EngineEvent::Error(msg) => {
                self.last_error = Some(msg.clone());
                self.log(format!("error: {msg}"));
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if icons::icon_button(ui, icons::new(), "New topology").clicked() {
                self.new_topology();
            }
            if icons::icon_button(ui, icons::open(), "Open topology...").clicked() {
                self.open_topology();
            }
            if icons::icon_button(ui, icons::save(), "Save topology as...").clicked() {
                self.save_topology();
            }
            ui.separator();

            let running = self.run_state == RunState::Running;
            let paused = self.run_state == RunState::Paused;

            if icons::icon_button_enabled(ui, !running && !paused, icons::play(), "Start").clicked()
            {
                self.start();
            }
            if icons::icon_button_enabled(ui, running || paused, icons::stop(), "Stop").clicked() {
                self.stop();
            }
            let (pr_icon, pr_tip) = if paused {
                (icons::play(), "Resume")
            } else {
                (icons::pause(), "Pause")
            };
            if icons::icon_button_enabled(ui, running || paused, pr_icon, pr_tip).clicked() {
                self.pause_resume();
            }

            ui.separator();
            ui.label("Speed:");
            let mut speed_idx = if self.speed == 0.0 {
                3
            } else if self.speed >= 10.0 {
                2
            } else if self.speed >= 2.0 {
                1
            } else {
                0
            };
            let prev_idx = speed_idx;
            egui::ComboBox::from_id_salt("speed_combo")
                .selected_text(match speed_idx {
                    0 => "Real-time x1",
                    1 => "Real-time x2",
                    2 => "Real-time x10",
                    _ => "As fast as possible",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut speed_idx, 0, "Real-time x1");
                    ui.selectable_value(&mut speed_idx, 1, "Real-time x2");
                    ui.selectable_value(&mut speed_idx, 2, "Real-time x10");
                    ui.selectable_value(&mut speed_idx, 3, "As fast as possible");
                });
            if speed_idx != prev_idx {
                self.speed = match speed_idx {
                    0 => 1.0,
                    1 => 2.0,
                    2 => 10.0,
                    _ => 0.0,
                };
                let _ = self.engine.cmd.send(Command::SetSpeed(self.speed));
            }

            ui.separator();
            ui.label(format!("t = {:.3} s", self.sim_time.as_secs_f64()));

            ui.separator();
            let total_load: f64 = if self.bus_stats.is_empty() {
                0.0
            } else {
                self.bus_stats.values().map(|s| s.load_pct).sum::<f64>()
                    / self.bus_stats.len() as f64
            };
            ui.label(format!("avg load: {total_load:.1}%"));
            ui.label(format!("trace: {} rows", self.trace.len()));

            ui.separator();
            let theme_label = match self.theme {
                AppTheme::Light => "🌙 Dark",
                AppTheme::Dark => "☀ Light",
            };
            if ui.button(theme_label).clicked() {
                self.theme = self.theme.toggled();
                self.theme.apply(ui.ctx());
            }
        });

        if !self.bus_stats.is_empty() {
            ui.horizontal(|ui| {
                for (bus, stats) in &self.bus_stats {
                    let name = self.names.bus_name(*bus);
                    if stats.error_frames > 0 {
                        ui.label(format!(
                            "{name}: {:.1}% load, {:.0} fps, {} total, {} errors",
                            stats.load_pct,
                            stats.frames_per_s,
                            stats.total_frames,
                            stats.error_frames
                        ));
                    } else {
                        ui.label(format!(
                            "{name}: {:.1}% load, {:.0} fps, {} total",
                            stats.load_pct, stats.frames_per_s, stats.total_frames
                        ));
                    }
                    ui.separator();
                }
            });
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (color, label) = match self.run_state {
                RunState::Stopped => (egui::Color32::GRAY, "Stopped"),
                RunState::Running => (egui::Color32::from_rgb(0x1a, 0x9c, 0x3a), "Running"),
                RunState::Paused => (egui::Color32::from_rgb(0xd0, 0x90, 0x1a), "Paused"),
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, color);
            ui.label(label);
            ui.separator();
            ui.label(format!(
                "virtual time: {:.3} s",
                self.sim_time.as_secs_f64()
            ));
            ui.separator();
            if let Some(err) = &self.last_error {
                ui.colored_label(
                    egui::Color32::from_rgb(0xd0, 0x30, 0x30),
                    format!("last error: {err}"),
                );
            } else if let Some(last) = self.status_log.last() {
                ui.weak(last);
            }
        });
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
                let _ = self
                    .engine
                    .cmd
                    .send(Command::SendOnce(ecu_id, msg.bus, msg.frame));
            }
        }
    }

    fn take_screenshot_if_needed(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else {
            return;
        };
        if self.screenshot_taken {
            return;
        }
        let start = *self
            .screenshot_start
            .get_or_insert_with(std::time::Instant::now);
        if self.screenshot_start.is_none() {
            self.screenshot_start = Some(start);
        }
        if self.screenshot_start.unwrap().elapsed() < std::time::Duration::from_millis(2500) {
            ctx.request_repaint();
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        self.screenshot_taken = true;
        let _ = path; // consumed in the event handler below via ctx events
    }
}

impl eframe::App for OperowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();

        if self.run_state == RunState::Running {
            ctx.request_repaint();
        }

        if self.screenshot_path.is_some() && self.screenshot_start.is_none() {
            // Kick off the demo run automatically for headless verification.
            self.speed = 1.0;
            self.start();
            self.screenshot_start = Some(std::time::Instant::now());
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            self.top_bar(ui);
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            self.status_bar(ui);
        });

        egui::TopBottomPanel::bottom("trace_panel")
            .resizable(true)
            .default_height(260.0)
            .show(ctx, |ui| {
                self.trace.ui(ui);
            });

        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                let running = self.run_state != RunState::Stopped;
                self.inspector.ui(ui, &mut self.graph, running);
                self.send_once_ui(ui);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let running = self.run_state != RunState::Stopped;
                let opts = FlowOptions {
                    nodes_connectable: !running,
                    delete_key: !running,
                    ..Default::default()
                };
                let mut viewer = GraphViewer { theme: self.theme };
                let out =
                    Flow::new("graph")
                        .options(opts)
                        .show(ui, &mut self.graph.state, &mut viewer);

                if running {
                    return;
                }
                if out.pane.secondary_clicked() {
                    self.menu_pos = out.pane.interact_pointer_pos();
                }
                let pos = self.menu_pos.unwrap_or(egui::pos2(40.0, 40.0));
                out.pane.context_menu(|ui| {
                    ui.set_min_width(160.0);
                    if ui.button("Add ECU").clicked() {
                        let id = self.graph.add_ecu(pos, "NewEcu");
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
            });

        self.take_screenshot_if_needed(ctx);

        if let Some(path) = self.screenshot_path.clone() {
            ctx.input(|i| {
                for event in &i.raw.events {
                    if let egui::Event::Screenshot { image, .. } = event {
                        save_screenshot(&path, image);
                        std::process::exit(0);
                    }
                }
            });
        }
    }
}

fn save_screenshot(path: &std::path::Path, image: &egui::ColorImage) {
    let w = image.size[0] as u32;
    let h = image.size[1] as u32;
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    for px in &image.pixels {
        buf.extend_from_slice(&px.to_array());
    }
    if let Some(img) = image::RgbaImage::from_raw(w, h, buf) {
        let _ = img.save(path);
    }
}
