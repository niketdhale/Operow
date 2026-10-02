use std::path::PathBuf;

use operow_core::{BusId, DbcRef, Timestamp, Topology};
use operow_dbc::{BusTarget, Database};
use operow_engine::{BusStats, Command, Engine, EngineEvent, EngineHandle, RunState};

use egui_flow::{Flow, FlowOptions, PulseStyle};

use crate::dbcs;
use crate::graph::{Graph, GraphNode, GraphViewer, PulseDir, PulseSpec, pulses_for_events};
use crate::icons;
use crate::inspector::Inspector;
use crate::theme::AppTheme;
use crate::trace::{NameLookup, Trace, TraceMode};

const MAX_EVENTS_PER_FRAME: usize = 256;
/// Minimum gap between pulses of one style on one wire.
const PULSE_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Per-bus live stats shown in the top bar.
#[derive(Default, Clone, Copy)]
struct LiveBusStats {
    prev: BusStats,
    load_pct: f64,
    frames_per_s: f64,
    total_frames: u64,
    error_frames: u64,
}

/// Options for headless runs / screenshots.
#[derive(Default)]
pub struct StartupOptions {
    pub fixed_trace: bool,
    pub expand_signals: bool,
    pub topology: Option<PathBuf>,
    pub select: Option<String>,
    pub no_start: bool,
    pub show_log: bool,
    /// `--dbc <path> --dbc-bus <name>`: import a DBC on startup.
    pub dbc: Option<PathBuf>,
    pub dbc_bus: Option<String>,
    /// `--open-import-dialog <dbc path>`: open the import dialog.
    pub import_dialog: Option<PathBuf>,
}

/// The "Import DBC" modal: a parsed file plus where to put it.
struct ImportDialog {
    path: PathBuf,
    db: Database,
    /// `None` creates a new bus.
    target: Option<BusId>,
    new_name: String,
    new_bitrate: u32,
    create_nodes: bool,
}

const BITRATES: [u32; 5] = [125_000, 250_000, 500_000, 800_000, 1_000_000];

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
    /// Show pulses on the wires for live traffic.
    animate_traffic: bool,
    /// When each pulse kind last fired on a wire, for throttling.
    pulse_last: std::collections::HashMap<PulseSpec, std::time::Instant>,
    status_log: Vec<String>,
    show_log: bool,
    last_error: Option<String>,
    /// File the current topology was opened from / saved to; DBC paths are
    /// stored relative to its folder.
    project_path: Option<PathBuf>,
    import_dialog: Option<ImportDialog>,
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
    no_start: bool,
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
            animate_traffic: true,
            pulse_last: Default::default(),
            status_log: Vec::new(),
            show_log: false,
            last_error: None,
            project_path: None,
            import_dialog: None,
            menu_pos: None,
            screenshot_path,
            screenshot_start: None,
            screenshot_taken: false,
            no_start: false,
        }
    }

    /// Startup options (mainly for headless screenshots).
    pub fn configure_startup(&mut self, opts: StartupOptions) {
        self.no_start = opts.no_start;
        self.show_log = opts.show_log;
        if opts.fixed_trace {
            self.trace.mode = TraceMode::Fixed;
        }
        self.trace.expand_all = opts.expand_signals;
        if let Some(path) = opts.topology.as_deref() {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|s| Topology::from_json(&s).map_err(|e| e.to_string()))
            {
                Ok(topo) => self.install_topology(&topo, path),
                Err(e) => self.last_error = Some(format!("load error: {e}")),
            }
        }
        if let Some(path) = opts.dbc.as_deref() {
            match dbcs::load_file(path) {
                Ok(db) => {
                    let name = opts.dbc_bus.clone().unwrap_or_else(|| "CAN1".into());
                    let topo = self.graph.to_topology();
                    let target = topo.buses.iter().find(|b| b.name == name).map(|b| b.id);
                    self.apply_import(path, &db, target, &name, 500_000, true);
                }
                Err(e) => self.log_error(format!("DBC {}: {e}", path.display())),
            }
        }
        if let Some(path) = opts.import_dialog.as_deref() {
            self.begin_import(path);
        }
        if let Some(name) = opts.select.as_deref() {
            self.select_by_name(name);
        }
    }

    /// Replace the graph with `topo` loaded from `path` and load the
    /// databases it references.
    fn install_topology(&mut self, topo: &Topology, path: &std::path::Path) {
        self.graph = Graph::from_topology(topo);
        self.names.rebuild(topo);
        self.project_path = Some(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        self.reload_dbcs();
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.project_path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
    }

    /// Load every database the graph references; failures are logged in red
    /// and skipped.
    fn reload_dbcs(&mut self) {
        let (store, errors) = dbcs::load_all(&self.graph.databases, self.project_dir().as_deref());
        self.names.dbcs = store;
        for e in errors {
            self.log_error(e);
        }
    }

    /// Drop loaded databases whose reference no longer exists (bus removed).
    fn sync_dbcs(&mut self) {
        let buses: Vec<BusId> = self.graph.databases.iter().map(|d| d.bus).collect();
        self.names.dbcs.by_bus.retain(|b, _| buses.contains(b));
    }

    fn log_error(&mut self, msg: String) {
        self.log(format!("error: {msg}"));
        self.last_error = Some(msg);
    }

    /// Ask for a DBC file, then open the import dialog for it.
    fn pick_dbc(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("DBC database", &["dbc"])
            .pick_file()
        {
            self.begin_import(&path);
        }
    }

    fn begin_import(&mut self, path: &std::path::Path) {
        match dbcs::load_file(path) {
            Ok(db) => {
                let first = self.graph.to_topology().buses.first().map(|b| b.id);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("CAN");
                self.import_dialog = Some(ImportDialog {
                    path: path.to_path_buf(),
                    db,
                    target: first,
                    new_name: stem.to_string(),
                    new_bitrate: 500_000,
                    create_nodes: true,
                });
            }
            Err(e) => self.log_error(format!("DBC {}: {e}", path.display())),
        }
    }

    /// Merge `db` into the graph (new bus when `target` is `None`), record
    /// the file reference and keep the parsed database for decoding.
    fn apply_import(
        &mut self,
        path: &std::path::Path,
        db: &Database,
        target: Option<BusId>,
        new_name: &str,
        new_bitrate: u32,
        create_nodes: bool,
    ) {
        let base = self.graph.to_topology();
        let target = match target {
            Some(id) => BusTarget::Existing(id),
            None => BusTarget::New {
                name: new_name.to_string(),
                bitrate: new_bitrate,
            },
        };
        match db.merge_into(&base, target, create_nodes) {
            Ok((mut merged, bus)) => {
                let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let stored = dbcs::stored_path(&abs, self.project_dir().as_deref());
                merged.databases.retain(|d| d.bus != bus);
                merged.databases.push(DbcRef { path: stored, bus });
                self.graph = Graph::from_topology_keeping(&merged, &self.graph);
                self.names.rebuild(&merged);
                self.names
                    .dbcs
                    .by_bus
                    .insert(bus, std::sync::Arc::new(db.clone()));
                self.log(format!(
                    "imported {} ({} messages)",
                    path.display(),
                    db.messages.len()
                ));
            }
            Err(e) => self.log_error(format!("DBC import: {e}")),
        }
    }

    fn import_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dlg) = &mut self.import_dialog else {
            return;
        };
        let buses = self.graph.to_topology().buses;
        let mut action = None;
        egui::Window::new("Import DBC")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(dlg.path.display().to_string()).monospace());
                ui.label(format!(
                    "{} nodes, {} messages",
                    dlg.db.nodes.len(),
                    dlg.db.messages.len()
                ));
                ui.add_space(6.0);
                egui::Grid::new("import_dbc_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Target bus:");
                        let current = match dlg.target {
                            Some(id) => buses
                                .iter()
                                .find(|b| b.id == id)
                                .map_or("?".to_string(), |b| b.name.clone()),
                            None => "New bus".to_string(),
                        };
                        egui::ComboBox::from_id_salt("import_target_bus")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for b in &buses {
                                    ui.selectable_value(&mut dlg.target, Some(b.id), &b.name);
                                }
                                ui.selectable_value(&mut dlg.target, None, "New bus");
                            });
                        ui.end_row();
                        if dlg.target.is_none() {
                            ui.label("Name:");
                            ui.text_edit_singleline(&mut dlg.new_name);
                            ui.end_row();
                            ui.label("Bitrate:");
                            egui::ComboBox::from_id_salt("import_bitrate")
                                .selected_text(crate::graph::format_bitrate(dlg.new_bitrate))
                                .show_ui(ui, |ui| {
                                    for r in BITRATES {
                                        ui.selectable_value(
                                            &mut dlg.new_bitrate,
                                            r,
                                            crate::graph::format_bitrate(r),
                                        );
                                    }
                                });
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
                ui.checkbox(&mut dlg.create_nodes, "Create nodes from DBC");
                ui.add_space(8.0);
                let running = self.run_state != RunState::Stopped;
                let name_ok = dlg.target.is_some() || !dlg.new_name.trim().is_empty();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!running && name_ok, egui::Button::new("Import"))
                        .on_disabled_hover_text(if running {
                            "Stop the measurement first"
                        } else {
                            "Enter a bus name"
                        })
                        .clicked()
                    {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                });
            });
        match action {
            Some(true) => {
                if let Some(d) = self.import_dialog.take() {
                    self.apply_import(
                        &d.path,
                        &d.db,
                        d.target,
                        d.new_name.trim(),
                        d.new_bitrate,
                        d.create_nodes,
                    );
                }
            }
            Some(false) => self.import_dialog = None,
            None => {}
        }
    }

    fn select_by_name(&mut self, name: &str) {
        let id = self
            .graph
            .state
            .nodes
            .iter()
            .find(|n| n.data.name() == name)
            .map(|n| n.id);
        if let Some(id) = id {
            self.graph.select(id);
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
            let msg = format!("invalid topology: {e}");
            self.log(format!("error: {msg}"));
            self.last_error = Some(msg);
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
        self.names.dbcs = Default::default();
        self.project_path = None;
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
                            self.install_topology(&topo, &path);
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
            // Keep DBC references valid when the project moves to a new folder.
            let old_dir = self.project_dir();
            let new_abs = path.canonicalize().unwrap_or_else(|_| path.clone());
            let new_dir = new_abs.parent().map(|p| p.to_path_buf());
            for d in &mut self.graph.databases {
                let abs = dbcs::resolve_path(old_dir.as_deref(), &d.path);
                let abs = abs.canonicalize().unwrap_or(abs);
                d.path = dbcs::stored_path(&abs, new_dir.as_deref());
            }
            let json = self.graph.to_topology().to_json();
            if let Err(e) = std::fs::write(&path, json) {
                self.last_error = Some(format!("write error: {e}"));
            } else {
                self.project_path = Some(new_abs);
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

    /// Animate each frame along its real route: the sender's wire onto the
    /// bus. Forwarded frames use the gateway accent. At most one pulse per
    /// wire and style per `PULSE_MIN_INTERVAL`.
    fn animate_frames(&mut self, frames: &[operow_core::BusEvent]) {
        let links = self.graph.to_topology().links;
        let now = std::time::Instant::now();
        self.pulse_last
            .retain(|_, t| now.duration_since(*t) < PULSE_MIN_INTERVAL * 20);
        for spec in pulses_for_events(frames, &links) {
            // egui-flow pulses only run source->target (ECU to bus), so the
            // bus-to-receiver legs cannot be drawn.
            if spec.dir != PulseDir::ToBus {
                continue;
            }
            if self
                .pulse_last
                .get(&spec)
                .is_some_and(|t| now.duration_since(*t) < PULSE_MIN_INTERVAL)
            {
                continue;
            }
            self.pulse_last.insert(spec, now);
            let color = if spec.kind.forwarded {
                self.theme.gateway_color()
            } else {
                self.theme.bus_color(if spec.kind.fd { 2 } else { 0 })
            };
            self.graph.pulse_link(
                spec.node,
                spec.bus,
                PulseStyle {
                    color: Some(color),
                    radius: 4.0,
                    duration: 0.6,
                },
            );
        }
    }

    fn handle_event(&mut self, ev: EngineEvent) {
        match ev {
            EngineEvent::Frames(frames) => {
                for f in &frames {
                    let bus_name = self.names.bus_name(f.bus);
                    let sender_name = self.names.node_name(f.sender);
                    let origin_name = self.names.node_name(f.origin);
                    let msg_name =
                        self.names
                            .msg_name(f.bus, f.origin, f.frame.id, f.frame.extended);
                    self.trace
                        .push(f, &bus_name, &sender_name, &origin_name, &msg_name);
                    self.sim_time = f.time;
                }
                if self.animate_traffic {
                    self.animate_frames(&frames);
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
            let idle = self.run_state == RunState::Stopped;
            if icons::icon_button_enabled(ui, idle, icons::import(), "Import DBC...").clicked() {
                self.pick_dbc();
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
            ui.checkbox(&mut self.animate_traffic, "Animate traffic");
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

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");
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
                for line in &self.status_log {
                    let text = egui::RichText::new(line).monospace();
                    if crate::script_editor::is_error_line(line) {
                        ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), text);
                    } else {
                        ui.label(text);
                    }
                }
            });
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
                if crate::script_editor::is_error_line(last) {
                    ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), last);
                } else {
                    ui.weak(last);
                }
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
                // `msg.bus` of None means all linked buses.
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
            if !self.no_start {
                self.start();
            }
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
                ui.horizontal(|ui| {
                    // The default selected-text colour is blue on the blue
                    // selection fill; use a high-contrast one in both themes.
                    ui.visuals_mut().selection.stroke.color = if ui.visuals().dark_mode {
                        egui::Color32::from_rgb(0x10, 0x14, 0x1c)
                    } else {
                        egui::Color32::WHITE
                    };
                    ui.selectable_value(&mut self.show_log, false, "Trace");
                    ui.selectable_value(&mut self.show_log, true, "Log");
                });
                if self.show_log {
                    self.log_ui(ui);
                } else {
                    self.trace.ui(ui, &self.names);
                }
            });

        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(660.0)
            .show(ctx, |ui| {
                let running = self.run_state != RunState::Stopped;
                for cmd in self
                    .inspector
                    .ui(ui, &mut self.graph, running, &self.names.dbcs)
                {
                    let _ = self.engine.cmd.send(cmd);
                }
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
                    self.sync_dbcs();
                }
            });

        self.import_dialog_ui(ctx);
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
