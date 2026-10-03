use std::collections::HashMap;
use std::path::PathBuf;

use operow_core::{BusId, DbcRef, Timestamp, Topology};
use operow_dbc::{BusTarget, Database};
use operow_engine::{BusStats, Command, Engine, EngineEvent, EngineHandle, RunState};

use egui_flow::PulseStyle;

use crate::dbcs;
use crate::graph::{Graph, PulseDir, PulseSpec, pulses_for_events};
use crate::graph_window::{GraphWindow, YAxis};
use crate::icons;
use crate::inspector::Inspector;
use crate::project_tree;
use crate::settings::{self, AppSettings, WhenFull};
use crate::signal_dialog::{DialogOutcome, NewSignalDialog};
use crate::signals::SignalRef;
use crate::store::FrameStore;
use crate::theme::AppTheme;
use crate::trace::{NameLookup, Trace, TraceAction, TraceMode};
use crate::windows::WindowViewer;
use crate::workspace::{self, Dock, LayoutPreset, WindowId, WindowKind};

const MAX_EVENTS_PER_FRAME: usize = 256;
/// Minimum gap between pulses of one style on one wire.
const PULSE_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Per-bus live stats shown in the Statistics window and status bar.
#[derive(Default, Clone, Copy)]
pub struct LiveBusStats {
    prev: BusStats,
    pub load_pct: f64,
    pub frames_per_s: f64,
    pub total_frames: u64,
    pub error_frames: u64,
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
    /// `--open-settings`: open the settings dialog.
    pub open_settings: bool,
    /// `--layout-demo`: open extra windows (Trace 2, Graph 1, Generator 1).
    pub layout_demo: bool,
    /// `--demo-filters`: open "Body debug", a second trace with the bus and
    /// ID filters active, and focus it.
    pub demo_filters: bool,
    /// `--open-new-signal`: open the "New user signal" dialog.
    pub open_new_signal: bool,
    /// `--demo-graph`: open a Graph with EngineSpeed, Throttle (Y2) and
    /// Running and focus it.
    pub demo_graph: bool,
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
    dock: Dock,
    layout_preset: Option<LayoutPreset>,
    /// Per-instance state of every open Trace window.
    traces: HashMap<WindowId, Trace>,
    /// Per-instance state of every open Graph window.
    graphs: HashMap<WindowId, GraphWindow>,
    /// The Graph window that was focused last; "Add to graph" targets it.
    last_graph: Option<WindowId>,
    /// Every bus event, shared by all windows.
    store: FrameStore,
    settings: AppSettings,
    show_settings: bool,
    show_tree: bool,
    /// The "buffer full -> stop" action has already fired this run.
    buffer_stop_sent: bool,
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
    last_error: Option<String>,
    /// File the current topology was opened from / saved to; DBC paths are
    /// stored relative to its folder.
    project_path: Option<PathBuf>,
    import_dialog: Option<ImportDialog>,
    new_signal: Option<NewSignalDialog>,
    /// Flow-space position of the last right-click on the canvas, where
    /// "Add ECU"/"Add CAN Bus" place the new node.
    menu_pos: Option<egui::Pos2>,

    // --screenshot support
    screenshot_path: Option<PathBuf>,
    screenshot_start: Option<std::time::Instant>,
    screenshot_taken: bool,
    no_start: bool,
}

impl OperowApp {
    pub fn new(screenshot_path: Option<PathBuf>, settings: AppSettings) -> Self {
        let graph = Graph::default_demo();
        let mut names = NameLookup::default();
        names.rebuild(&graph.to_topology());

        let theme = if settings.dark_theme {
            AppTheme::Dark
        } else {
            AppTheme::Light
        };
        let mut app = OperowApp {
            graph,
            inspector: Inspector::default(),
            dock: workspace::default_layout(),
            layout_preset: Some(LayoutPreset::Default),
            traces: HashMap::new(),
            graphs: HashMap::new(),
            last_graph: None,
            store: FrameStore::new(settings.frame_buffer_size),
            settings,
            show_settings: false,
            show_tree: true,
            buffer_stop_sent: false,
            names,
            engine: Engine::spawn(),
            run_state: RunState::Stopped,
            speed: 1.0,
            sim_time: Timestamp::ZERO,
            prev_stats_time: Timestamp::ZERO,
            bus_stats: Default::default(),
            theme,
            animate_traffic: true,
            pulse_last: Default::default(),
            status_log: Vec::new(),
            last_error: None,
            project_path: None,
            import_dialog: None,
            new_signal: None,
            menu_pos: None,
            screenshot_path,
            screenshot_start: None,
            screenshot_taken: false,
            no_start: false,
        };
        app.sync_instances();
        app
    }

    /// Keep one `Trace` per open Trace window (dropping closed ones) and
    /// apply the buffer settings to the frame store.
    fn sync_instances(&mut self) {
        let open = workspace::open_windows(&self.dock);
        self.traces.retain(|id, _| open.contains(id));
        for id in open.iter().filter(|w| w.kind == WindowKind::Trace) {
            self.traces.entry(*id).or_default();
        }
        self.graphs.retain(|id, _| open.contains(id));
        for id in open.iter().filter(|w| w.kind == WindowKind::Graph) {
            self.graphs.entry(*id).or_default();
        }
        if self.last_graph.is_some_and(|g| !open.contains(&g)) {
            self.last_graph = None;
        }
        self.store.set_capacity(self.settings.frame_buffer_size);
        self.store
            .set_reject_when_full(self.settings.when_full == WhenFull::StopMeasurement);
    }

    fn open_window(&mut self, kind: WindowKind, force_new: bool) -> WindowId {
        let id = workspace::open_or_focus(&mut self.dock, kind, force_new);
        self.layout_preset = None;
        if kind == WindowKind::Graph {
            self.last_graph = Some(id);
        }
        id
    }

    fn set_layout(&mut self, preset: LayoutPreset) {
        self.dock = preset.build();
        self.layout_preset = Some(preset);
    }

    /// Clear what every Trace window shows; the shared store keeps its frames.
    fn clear_traces(&mut self) {
        for t in self.traces.values_mut() {
            t.clear(&self.store);
        }
    }

    /// The Graph window "Add to graph" targets: the one focused last, else
    /// the newest open one, else a new "Graph 1".
    fn target_graph(&mut self) -> WindowId {
        if let Some(id) = self.last_graph {
            return id;
        }
        let newest = workspace::open_windows(&self.dock)
            .into_iter()
            .filter(|w| w.kind == WindowKind::Graph)
            .max_by_key(|w| w.n);
        let id = match newest {
            Some(id) => id,
            None => self.open_window(WindowKind::Graph, true),
        };
        self.sync_instances();
        id
    }

    /// Put `sig` on a graph window and bring that window to the front.
    fn on_add_signal_to_graph(&mut self, sig: SignalRef) {
        let id = self.target_graph();
        let label = sig.label(&self.names, &self.graph.user_signals);
        if let Some(g) = self.graphs.get_mut(&id) {
            g.add_signal(sig, &self.names.dbcs, &self.graph.user_signals);
        }
        workspace::focus(&mut self.dock, id);
        self.last_graph = Some(id);
        self.log(format!("added {label} to {}", id.title()));
    }

    fn handle_trace_actions(&mut self, actions: Vec<TraceAction>) {
        for a in actions {
            match a {
                TraceAction::AddSignalToGraph(sig) => self.on_add_signal_to_graph(sig),
                TraceAction::CopyAsGenerator(frame) => self.log(format!(
                    "frame 0x{:X} copied as JSON (generator import comes later)",
                    frame.id
                )),
                TraceAction::Log(msg) => {
                    if msg.starts_with("error") {
                        self.log_error(msg);
                    } else {
                        self.log(msg);
                    }
                }
            }
        }
    }

    fn next_user_signal_id(&self) -> operow_core::UserSignalId {
        operow_core::UserSignalId(
            self.graph
                .user_signals
                .iter()
                .map(|u| u.id.0 + 1)
                .max()
                .unwrap_or(1),
        )
    }

    fn open_new_signal_dialog(&mut self) {
        let first = self.graph.to_topology().buses.first().map(|b| b.id);
        self.new_signal = Some(NewSignalDialog::new(first));
    }

    fn new_signal_dialog_ui(&mut self, ctx: &egui::Context) {
        if self.new_signal.is_none() {
            return;
        }
        let buses = self.graph.to_topology().buses;
        let next = self.next_user_signal_id();
        let Some(dlg) = &mut self.new_signal else {
            return;
        };
        match dlg.ui(ctx, &buses, &self.graph.user_signals, &self.store, next) {
            DialogOutcome::Open => {}
            DialogOutcome::Cancel => self.new_signal = None,
            DialogOutcome::Save(def) => {
                self.log(format!("added user signal {}", def.name));
                self.graph.user_signals.push(def);
                self.new_signal = None;
            }
        }
    }

    /// Startup options (mainly for headless screenshots).
    pub fn configure_startup(&mut self, opts: StartupOptions) {
        self.no_start = opts.no_start;
        self.show_settings = opts.open_settings;
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
        if opts.layout_demo {
            self.open_window(WindowKind::Trace, true);
            self.open_window(WindowKind::Graph, true);
            self.open_window(WindowKind::Generator, true);
        }
        let demo_trace = opts.demo_filters.then(|| {
            let id = workspace::open_or_focus(&mut self.dock, WindowKind::Trace, true);
            self.layout_preset = None;
            id
        });
        self.sync_instances();
        if let Some(id) = demo_trace {
            if let Some(t) = self.traces.get_mut(&id) {
                t.title = Some("Body debug".into());
                t.filters.bus.enabled = true;
                t.filters.bus.selected.insert("Body".into());
                t.filters.id.enabled = true;
                t.filters.id.text = "100-2FF".into();
            }
            workspace::focus(&mut self.dock, id);
        }
        if opts.open_new_signal {
            self.open_new_signal_dialog();
            if let Some(d) = &mut self.new_signal {
                d.name = "EngineSpeed".into();
                d.id_hex = "100".into();
                d.start_bit = 8;
                d.size = 16;
                d.factor = 0.25;
                d.unit = "rpm".into();
            }
        }
        if opts.demo_graph {
            self.demo_graph();
        }
        for t in self.traces.values_mut() {
            if opts.fixed_trace {
                t.mode = TraceMode::Fixed;
            }
            t.expand_all = opts.expand_signals;
        }
        if opts.show_log {
            workspace::focus(&mut self.dock, WindowId::new(WindowKind::Log, 1));
        }
        if let Some(path) = opts.import_dialog.as_deref() {
            self.begin_import(path);
        }
        if let Some(name) = opts.select.as_deref() {
            self.select_by_name(name);
        }
    }

    /// `--demo-graph`: EngineSpeed on Y1, Throttle on Y2 and Running as a
    /// step, on a new focused Graph window (needs `dbc_demo.operow.json`).
    fn demo_graph(&mut self) {
        let id = self.open_window(WindowKind::Graph, true);
        self.sync_instances();
        let bus = self
            .names
            .bus_names
            .iter()
            .find(|(_, n)| n.as_str() == "Powertrain")
            .map(|(b, _)| *b)
            .or_else(|| self.names.bus_names.keys().next().copied());
        let (Some(bus), Some(g)) = (bus, self.graphs.get_mut(&id)) else {
            return;
        };
        for name in ["EngineSpeed", "Throttle", "Running"] {
            g.add_signal(
                SignalRef::Dbc {
                    bus,
                    msg_id: 0x100,
                    extended: false,
                    signal_name: name.into(),
                },
                &self.names.dbcs,
                &self.graph.user_signals,
            );
        }
        g.window_s = 5;
        g.set_axis(1, YAxis::Y2);
        workspace::focus(&mut self.dock, id);
    }

    /// Replace the graph with `topo` loaded from `path` and load the
    /// databases it references.
    fn install_topology(&mut self, topo: &Topology, path: &std::path::Path) {
        self.graph = Graph::from_topology(topo);
        self.names.rebuild(topo);
        self.project_path = Some(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        // Restore the saved window layout; fall back to the default.
        let saved = topo
            .workspace
            .as_ref()
            .and_then(workspace::layout_from_json);
        self.layout_preset = if saved.is_some() {
            None
        } else {
            Some(LayoutPreset::Default)
        };
        self.dock = saved.unwrap_or_else(workspace::default_layout);
        // Graphs refer to the previous project's buses; start from scratch.
        self.graphs.clear();
        self.last_graph = None;
        self.sync_instances();
        if let Some(ws) = &topo.workspace {
            for (id, view) in workspace::traces_from_json(ws) {
                if let Some(t) = self.traces.get_mut(&id) {
                    t.apply_view(view);
                }
            }
            for (id, view) in workspace::graphs_from_json(ws) {
                if let Some(g) = self.graphs.get_mut(&id) {
                    g.apply_view(view);
                }
            }
        }
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
        self.store.clear();
        self.buffer_stop_sent = false;
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
        self.graphs.clear();
        self.last_graph = None;
        self.store.clear();
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
            let mut topo = self.graph.to_topology();
            let views = self.traces.iter().map(|(id, t)| (*id, t.view())).collect();
            let graphs = self.graphs.iter().map(|(id, g)| (*id, g.view())).collect();
            topo.workspace = workspace::layout_to_json_with(&self.dock, views, graphs);
            let json = topo.to_json();
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
                self.store.push_batch(&frames);
                if let Some(last) = frames.last() {
                    self.sim_time = last.time;
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
        egui::MenuBar::new().ui(ui, |ui| {
            let idle = self.run_state == RunState::Stopped;
            let running = self.run_state == RunState::Running;
            let paused = self.run_state == RunState::Paused;

            if ui
                .selectable_label(self.show_tree, "\u{2630}")
                .on_hover_text("Toggle project tree")
                .clicked()
            {
                self.show_tree = !self.show_tree;
            }
            ui.label(egui::RichText::new("Operow").strong());
            ui.separator();

            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    self.new_topology();
                    ui.close();
                }
                if ui.button("Open...").clicked() {
                    self.open_topology();
                    ui.close();
                }
                if ui.button("Save as...").clicked() {
                    self.save_topology();
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(idle, egui::Button::new("Import DBC..."))
                    .clicked()
                {
                    self.pick_dbc();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(idle, egui::Button::new("Add ECU")).clicked() {
                    self.graph.add_ecu(egui::pos2(40.0, 40.0), "NewEcu");
                    ui.close();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Add Gateway"))
                    .clicked()
                {
                    self.graph.add_gateway(egui::pos2(40.0, 120.0));
                    ui.close();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Add CAN Bus"))
                    .clicked()
                {
                    self.graph.add_bus(egui::pos2(40.0, 200.0));
                    ui.close();
                }
            });
            ui.menu_button("Window", |ui| {
                for kind in WindowKind::ALL {
                    if ui.button(kind.label()).clicked() {
                        self.open_window(kind, false);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.checkbox(&mut self.show_tree, "Project tree").clicked() {
                    ui.close();
                }
                if ui.button("Reset layout").clicked() {
                    self.set_layout(LayoutPreset::Default);
                    ui.close();
                }
            });
            ui.menu_button("Simulation", |ui| {
                if ui
                    .add_enabled(!running && !paused, egui::Button::new("Start"))
                    .clicked()
                {
                    self.start();
                    ui.close();
                }
                if ui
                    .add_enabled(running || paused, egui::Button::new("Stop"))
                    .clicked()
                {
                    self.stop();
                    ui.close();
                }
                let pr = if paused { "Resume" } else { "Pause" };
                if ui
                    .add_enabled(running || paused, egui::Button::new(pr))
                    .clicked()
                {
                    self.pause_resume();
                    ui.close();
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("Clear traces").clicked() {
                    self.clear_traces();
                    ui.close();
                }
                if ui.button("Clear log").clicked() {
                    self.status_log.clear();
                    ui.close();
                }
                if ui.button("Settings...").clicked() {
                    self.show_settings = true;
                    ui.close();
                }
            });
            ui.separator();

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
            ui.checkbox(&mut self.animate_traffic, "Animate");

            ui.separator();
            if ui.button("+ Trace").clicked() {
                self.open_window(WindowKind::Trace, true);
            }
            if ui.button("+ Graph").clicked() {
                self.open_window(WindowKind::Graph, true);
            }
            if ui.button("+ Generator").clicked() {
                self.open_window(WindowKind::Generator, true);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("\u{2699} Settings")
                    .on_hover_text("Settings")
                    .clicked()
                {
                    self.show_settings = !self.show_settings;
                }
                let current = self.layout_preset.map_or("Custom", |p| p.label());
                egui::ComboBox::from_id_salt("layout_combo")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for p in LayoutPreset::ALL {
                            if ui
                                .selectable_label(self.layout_preset == Some(p), p.label())
                                .clicked()
                            {
                                self.set_layout(p);
                            }
                        }
                    });
                ui.label("Layout:");
            });
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
            ui.label(format!("t = {:.3} s", self.sim_time.as_secs_f64()));
            ui.separator();
            let mut buses: Vec<(String, f64)> = self
                .bus_stats
                .iter()
                .map(|(b, s)| (self.names.bus_name(*b), s.load_pct))
                .collect();
            buses.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, load) in buses {
                ui.label(format!("{name}: {load:.1}%"));
                ui.separator();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!(
                    "Buffer: {} / {} frames",
                    self.store.len(),
                    self.store.capacity()
                ))
                .on_hover_text(format!(
                    "{} frames received in total (the oldest are dropped when full)",
                    self.store.total_pushed()
                ));
                ui.separator();
                let red = egui::Color32::from_rgb(0xd0, 0x30, 0x30);
                if let Some(err) = &self.last_error {
                    ui.colored_label(red, format!("last error: {err}"));
                } else if let Some(last) = self.status_log.last() {
                    if crate::script_editor::is_error_line(last) {
                        ui.colored_label(red, last);
                    } else {
                        ui.weak(last);
                    }
                }
            });
        });
    }

    fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let s = &mut self.settings;
                ui.heading("Measurement");
                ui.separator();
                egui::Grid::new("settings_measurement")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Frame buffer size:");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                for p in settings::BUFFER_PRESETS {
                                    if ui
                                        .selectable_label(
                                            s.frame_buffer_size == p,
                                            settings::format_frames(p),
                                        )
                                        .clicked()
                                    {
                                        s.frame_buffer_size = p;
                                        changed = true;
                                    }
                                }
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut s.frame_buffer_size)
                                            .range(1_000..=50_000_000)
                                            .speed(10_000.0)
                                            .suffix(" frames"),
                                    )
                                    .changed();
                            });
                            ui.weak(format!(
                                "Estimated RAM \u{2248} {} for the shared frame store ({} bytes/frame)",
                                settings::format_bytes(s.estimated_ram_bytes()),
                                settings::BYTES_PER_FRAME
                            ));
                        });
                        ui.end_row();

                        ui.label("When full:");
                        ui.horizontal(|ui| {
                            for w in [WhenFull::DropOldest, WhenFull::StopMeasurement] {
                                changed |= ui
                                    .selectable_value(&mut s.when_full, w, w.label())
                                    .changed();
                            }
                        });
                        ui.end_row();

                        ui.label("UI refresh:");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut s.ui_refresh_ms)
                                    .range(10..=1000)
                                    .suffix(" ms"),
                            )
                            .changed();
                        ui.end_row();
                    });
                ui.add_space(10.0);
                ui.heading("Appearance");
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    changed |= ui
                        .selectable_value(&mut s.dark_theme, true, "Dark")
                        .changed();
                    changed |= ui
                        .selectable_value(&mut s.dark_theme, false, "Light")
                        .changed();
                });
            });
        if !open {
            self.show_settings = false;
        }
        if changed {
            self.theme = if self.settings.dark_theme {
                AppTheme::Dark
            } else {
                AppTheme::Light
            };
            self.theme.apply(ctx);
            self.sync_instances();
        }
    }

    /// Stop the measurement once a trace buffer is full, when so configured.
    fn check_buffer_full(&mut self) {
        if self.settings.when_full != WhenFull::StopMeasurement
            || self.buffer_stop_sent
            || self.run_state == RunState::Stopped
            || !self.store.is_full()
        {
            return;
        }
        self.buffer_stop_sent = true;
        self.log(format!(
            "frame buffer full ({} frames): measurement stopped",
            self.settings.frame_buffer_size
        ));
        self.stop();
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
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.settings.save(storage);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();

        self.check_buffer_full();

        if self.run_state == RunState::Running {
            ctx.request_repaint_after(std::time::Duration::from_millis(
                self.settings.ui_refresh_ms as u64,
            ));
        }

        if self.screenshot_path.is_some() && self.screenshot_start.is_none() {
            // Kick off the demo run automatically for headless verification.
            self.speed = 1.0;
            if !self.no_start {
                self.start();
            }
            self.screenshot_start = Some(std::time::Instant::now());
        }

        self.sync_instances();
        let now = std::time::Instant::now();
        for t in self.traces.values_mut() {
            t.update(&self.store, &self.names, now);
        }
        let mut graphs_pending = false;
        for g in self.graphs.values_mut() {
            graphs_pending |= g.update(&self.store, &self.names.dbcs, &self.graph.user_signals);
        }
        if graphs_pending {
            ctx.request_repaint();
        }
        if let Some((_, tab)) = self.dock.find_active_focused()
            && tab.kind == WindowKind::Graph
        {
            self.last_graph = Some(*tab);
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            self.top_bar(ui);
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            self.status_bar(ui);
        });

        let mut tree = project_tree::TreeOutput::default();
        if self.show_tree {
            egui::SidePanel::left("project_tree")
                .resizable(true)
                .default_width(240.0)
                .show(ctx, |ui| {
                    ui.heading("Project");
                    ui.separator();
                    tree = project_tree::ui(ui, &self.graph, &self.names.dbcs);
                });
        }
        if let Some(id) = tree.picked {
            self.graph.select(id);
            self.open_window(WindowKind::Properties, false);
        }
        if tree.new_signal {
            self.open_new_signal_dialog();
        }
        if let Some(sig) = tree.add_signal_to_graph {
            self.on_add_signal_to_graph(sig);
        }
        if let Some(id) = tree.delete_signal {
            self.graph.user_signals.retain(|u| u.id != id);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let mut viewer = WindowViewer {
                    graph: &mut self.graph,
                    inspector: &mut self.inspector,
                    traces: &mut self.traces,
                    graphs: &mut self.graphs,
                    store: &self.store,
                    names: &self.names,
                    status_log: &mut self.status_log,
                    bus_stats: &self.bus_stats,
                    run_state: self.run_state,
                    theme: self.theme,
                    menu_pos: &mut self.menu_pos,
                    cmds: Vec::new(),
                    graph_changed: false,
                    trace_actions: Vec::new(),
                };
                let style = egui_dock::Style::from_egui(ui.style().as_ref());
                egui_dock::DockArea::new(&mut self.dock)
                    .style(style)
                    .show_add_buttons(false)
                    .show_close_buttons(true)
                    .show_inside(ui, &mut viewer);
                let (cmds, changed, actions) =
                    (viewer.cmds, viewer.graph_changed, viewer.trace_actions);
                self.handle_trace_actions(actions);
                for cmd in cmds {
                    let _ = self.engine.cmd.send(cmd);
                }
                if changed {
                    self.sync_dbcs();
                }
            });

        self.settings_ui(ctx);
        self.import_dialog_ui(ctx);
        self.new_signal_dialog_ui(ctx);
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
