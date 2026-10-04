use std::collections::HashMap;
use std::path::PathBuf;

use operow_core::{BusId, CanErrorKind, DbcRef, Timestamp, Topology};
use operow_dbc::{BusTarget, Database};
use operow_engine::{
    BusStats, CanErrorCounts, Command, Engine, EngineEvent, EngineHandle, GeneratorId, InjectMode,
    InjectSpec, NodeErrorInfo, RunState,
};

use egui_flow::{EdgeId, PulseShape, PulseStyle};

use crate::dbcs;
use crate::generator_window::{AutoChange, DbcRow, GenRow, GeneratorWindow, SendMode, SigEdit};
use crate::graph::{Graph, GraphNode, PulseDir, pulses_for_events};
use crate::graph_window::{GraphWindow, YAxis};
use crate::icons;
use crate::inspector::Inspector;
use crate::logging::{
    CmpOp, Condition, LogContext, LogRuntime, LogState, LoggingConfig, SigCmp, StartTrigger,
};
use crate::logging_window::{self, LoggingInput};
use crate::network_view::NetworkView;
use crate::project_tree;
use crate::replay::{self, LogInfo, ReplaySource};
use crate::runtime::{FaultRule, FaultsState, RuntimeState};
use crate::settings::{self, AppSettings, WhenFull};
use crate::signal_dialog::{DialogOutcome, NewSignalDialog};
use crate::signals::{RawKind, SignalRef};
use crate::store::FrameStore;
use crate::theme::AppTheme;
use crate::trace::{NameLookup, Trace, TraceAction, TraceMode};
use crate::windows::WindowViewer;
use crate::workspace::{self, Dock, LayoutPreset, WindowId, WindowKind};

const MAX_EVENTS_PER_FRAME: usize = 256;
/// Light cap on pulses per wire and direction (20 per second), so
/// high-rate traffic does not saturate a wire. Beyond it, `egui-flow`'s own
/// per-edge limit replaces the oldest pulse.
const PULSE_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// Seconds one pulse takes along one wire; legs of a frame follow each other.
const PULSE_LEG_S: f32 = 0.45;

/// Per-bus live stats shown in the Statistics window and status bar.
#[derive(Default, Clone, Copy)]
pub struct LiveBusStats {
    prev: BusStats,
    pub load_pct: f64,
    pub frames_per_s: f64,
    pub total_frames: u64,
    pub error_frames: u64,
    pub can_errors: CanErrorCounts,
    pub dropped_bus_off: u64,
    pub dropped_offline: u64,
    pub dropped_msg_control: u64,
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
    /// `--demo-generator`: open Generator 1 with four rows next to a Trace
    /// and start its cyclic rows once the measurement runs.
    pub demo_generator: bool,
    /// `--network-view freeform|busline`: the Network window layout.
    pub network_view: Option<NetworkView>,
    /// `--demo-logging`: open the Logging window with a signal trigger and
    /// log into the temp folder while the measurement runs.
    pub demo_logging: bool,
    /// `--demo-errors`: on every start, corrupt the first 20 transmissions of
    /// the `Engine` node's 0x100 frame with CRC errors.
    pub demo_errors: bool,
    /// `--demo-faults`: open the Faults window with a 10% CRC rule on the
    /// first bus (any node) and take the `Body` node offline.
    pub demo_faults: bool,
    /// `--demo-busoff`: force the `Engine` node bus-off once running.
    pub demo_busoff: bool,
    /// `--open-log <path>`: open an offline session (channels mapped to the
    /// buses in order) and play it at x1.
    pub open_log: Option<PathBuf>,
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

/// The "Open log" modal: the channels of an ASC file and where to put them.
struct OpenLogDialog {
    path: PathBuf,
    info: LogInfo,
    /// Per channel: an existing bus, or `None` for a new one.
    targets: Vec<(u8, Option<BusId>)>,
}

/// Playback speeds of the replay bar; `None` is "as fast as possible".
const REPLAY_SPEEDS: [(Option<f64>, &str); 7] = [
    (Some(0.1), "\u{d7}0.1"),
    (Some(0.5), "\u{d7}0.5"),
    (Some(1.0), "\u{d7}1"),
    (Some(2.0), "\u{d7}2"),
    (Some(10.0), "\u{d7}10"),
    (Some(100.0), "\u{d7}100"),
    (None, "Max"),
];
/// Virtual time between the statistics updates of an offline session.
const OFFLINE_STATS_NS: u64 = 200_000_000;

/// An open offline session: a log streamed into the frame store while the
/// simulation stays stopped.
struct Offline {
    source: ReplaySource,
    /// File name for the status bar.
    file: String,
    /// Seek slider value (seconds) while it is being dragged.
    drag: Option<f64>,
    /// Text of the jump-to-time field.
    jump: String,
    /// Bus load counters built from the replayed frames.
    acc: HashMap<BusId, BusStats>,
    /// Nominal and data bitrate of each bus, for the load.
    rates: HashMap<BusId, (u32, u32)>,
    last_stats: Timestamp,
}

/// `--demo-errors`: 20 CRC errors on the `Engine` node's 0x100 frame.
fn demo_error_spec(topo: &Topology) -> Option<InjectSpec> {
    let node = topo.nodes.iter().find(|n| n.name == "Engine")?;
    let bus = topo.links.iter().find(|l| l.node == node.id)?.bus;
    Some(InjectSpec {
        bus,
        node: Some(node.id),
        id: Some((0x100, false)),
        kind: CanErrorKind::Crc,
        mode: InjectMode::Count(20),
        remaining: None,
    })
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
    /// Per-instance state of every open Generator window.
    generators: HashMap<WindowId, GeneratorWindow>,
    /// The Generator window that was focused last; "Copy as generator
    /// frame" targets it.
    last_generator: Option<WindowId>,
    /// `--demo-generator`: start this window's cyclic rows when running.
    demo_generator_pending: Option<WindowId>,
    /// Every bus event, shared by all windows.
    store: FrameStore,
    /// ASC logging settings (saved with the project) and its task.
    logging: LoggingConfig,
    log_rt: LogRuntime,
    show_logging: bool,
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
    /// Fault-confinement state of every node, from the live engine.
    node_states: Vec<NodeErrorInfo>,
    /// Run-time node/message controls sent to the engine (not saved).
    runtime: RuntimeState,
    /// Fault-injection rules and seed (not saved).
    faults: FaultsState,

    theme: AppTheme,
    /// Show pulses on the wires for live traffic.
    animate_traffic: bool,
    /// When a pulse last started on each wire and direction, for the rate cap.
    pulse_last: std::collections::HashMap<(EdgeId, PulseDir), std::time::Instant>,
    status_log: Vec<String>,
    last_error: Option<String>,
    /// File the current topology was opened from / saved to; DBC paths are
    /// stored relative to its folder.
    project_path: Option<PathBuf>,
    import_dialog: Option<ImportDialog>,
    open_log: Option<OpenLogDialog>,
    /// The open offline session, if any (the simulation is not run then).
    offline: Option<Offline>,
    new_signal: Option<NewSignalDialog>,
    /// Flow-space position of the last right-click on the canvas, where
    /// "Add ECU"/"Add CAN Bus" place the new node.
    menu_pos: Option<egui::Pos2>,

    // --screenshot support
    screenshot_path: Option<PathBuf>,
    screenshot_start: Option<std::time::Instant>,
    screenshot_taken: bool,
    no_start: bool,
    /// `--demo-errors`: inject CRC errors on every start.
    demo_errors: bool,
    /// `--demo-faults`: take `Body` offline on every start.
    demo_faults: bool,
    /// `--demo-busoff`: force `Engine` bus-off on every start.
    demo_busoff: bool,
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
            generators: HashMap::new(),
            last_generator: None,
            demo_generator_pending: None,
            store: FrameStore::new(settings.frame_buffer_size),
            logging: LoggingConfig::default(),
            log_rt: LogRuntime::default(),
            show_logging: false,
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
            node_states: Vec::new(),
            runtime: RuntimeState::default(),
            faults: FaultsState::default(),
            theme,
            animate_traffic: true,
            pulse_last: Default::default(),
            status_log: Vec::new(),
            last_error: None,
            project_path: None,
            import_dialog: None,
            open_log: None,
            offline: None,
            new_signal: None,
            menu_pos: None,
            screenshot_path,
            screenshot_start: None,
            screenshot_taken: false,
            no_start: false,
            demo_errors: false,
            demo_faults: false,
            demo_busoff: false,
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
        self.generators.retain(|id, _| open.contains(id));
        for id in open.iter().filter(|w| w.kind == WindowKind::Generator) {
            self.generators.entry(*id).or_default();
        }
        if self.last_generator.is_some_and(|g| !open.contains(&g)) {
            self.last_generator = None;
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
        if kind == WindowKind::Generator {
            self.last_generator = Some(id);
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

    /// The Generator window "Copy as generator frame" targets: the one
    /// focused last, else the newest open one, else a new "Generator 1".
    fn target_generator(&mut self) -> WindowId {
        if let Some(id) = self.last_generator {
            return id;
        }
        let newest = workspace::open_windows(&self.dock)
            .into_iter()
            .filter(|w| w.kind == WindowKind::Generator)
            .max_by_key(|w| w.n);
        let id = match newest {
            Some(id) => id,
            None => self.open_window(WindowKind::Generator, true),
        };
        self.sync_instances();
        id
    }

    /// Add a trace frame as a row of a Generator window and show it.
    fn on_copy_as_generator(&mut self, bus: BusId, frame: operow_core::CanFrame) {
        let id = self.target_generator();
        if let Some(g) = self.generators.get_mut(&id) {
            g.add_frame_row(bus, &frame);
        }
        workspace::focus(&mut self.dock, id);
        self.last_generator = Some(id);
        self.log(format!("copied frame 0x{:X} to {}", frame.id, id.title()));
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
                TraceAction::CopyAsGenerator(bus, frame) => self.on_copy_as_generator(bus, frame),
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
        self.demo_errors = opts.demo_errors;
        self.demo_faults = opts.demo_faults;
        self.demo_busoff = opts.demo_busoff;
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
        if let Some(view) = opts.network_view {
            self.graph.set_view(view);
        }
        if let Some(path) = opts.open_log.as_deref() {
            self.open_log_auto(path);
        }
        if opts.layout_demo {
            self.open_window(WindowKind::Trace, true);
            self.open_window(WindowKind::Graph, true);
            self.open_window(WindowKind::Generator, true);
        }
        if opts.demo_errors {
            self.dock = workspace::errors_demo_layout();
            self.layout_preset = None;
        }
        if opts.demo_faults || opts.demo_busoff {
            self.dock = workspace::faults_demo_layout();
            self.layout_preset = None;
        }
        if opts.demo_faults {
            let topo = self.graph.to_topology();
            self.faults.rules.push(FaultRule {
                bus: topo.buses.first().map(|b| b.id),
                kind: CanErrorKind::Crc,
                mode: crate::runtime::RuleMode::Probability,
                pct: 10.0,
                ..Default::default()
            });
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
        if opts.demo_generator {
            self.demo_generator();
        }
        if opts.demo_logging {
            self.demo_logging();
        }
        for t in self.traces.values_mut() {
            if opts.demo_errors {
                // The injected errors are at the very start of the run.
                t.autoscroll = false;
            }
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

    /// `--demo-logging`: start recording once the Body bus's DoorStatus
    /// (0x200) byte 0 equals 0, with 0.5 s of history and 1 s after the end.
    fn demo_logging(&mut self) {
        let bus = self
            .names
            .bus_names
            .iter()
            .find(|(_, n)| n.as_str() == "Body")
            .map(|(b, _)| *b)
            .or_else(|| self.names.bus_names.keys().min().copied());
        let Some(bus) = bus else { return };
        self.logging = LoggingConfig {
            enabled: true,
            folder: Some(std::env::temp_dir().join("operow-logs")),
            ..Default::default()
        };
        self.logging.trigger.start = StartTrigger::OnCondition(Condition::Signal {
            signal: SignalRef::Raw {
                bus,
                id: 0x200,
                extended: false,
                kind: RawKind::Byte(0),
            },
            cmp: SigCmp {
                op: CmpOp::Eq,
                value: 0.0,
            },
        });
        self.logging.trigger.pre_trigger_s = 0.5;
        self.logging.trigger.post_trigger_s = 1.0;
        self.show_logging = true;
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

    /// `--demo-generator`: Generator 1 with a cyclic raw row, a cyclic DBC
    /// row (EngineSpeed ramps), a Key row and a Once row, docked right of
    /// the Trace (needs `dbc_demo.operow.json`).
    fn demo_generator(&mut self) {
        // Make room: no project tree, no Properties.
        self.show_tree = false;
        let id = WindowId::new(WindowKind::Generator, 1);
        let trace = WindowId::new(WindowKind::Trace, 1);
        self.dock = workspace::generator_demo_layout(id);
        self.layout_preset = None;
        workspace::focus(&mut self.dock, trace);
        self.sync_instances();
        if let Some(t) = self.traces.get_mut(&trace) {
            use crate::trace::Col;
            t.hidden.extend([
                Col::Chn,
                Col::Dir,
                Col::Hop,
                Col::Type,
                Col::Count,
                Col::Dt,
                Col::Dlc,
                Col::Len,
            ]);
        }
        let bus = self
            .names
            .bus_names
            .iter()
            .find(|(_, n)| n.as_str() == "Powertrain")
            .map(|(b, _)| *b)
            .or_else(|| self.names.bus_names.keys().next().copied());
        let Some(g) = self.generators.get_mut(&id) else {
            return;
        };
        let signals = vec![
            SigEdit {
                name: "EngineSpeed".into(),
                value: 1500.0,
                auto: AutoChange::Ramp { period_s: 4.0 },
            },
            SigEdit {
                name: "CoolantTemp".into(),
                value: 90.0,
                auto: AutoChange::None,
            },
            SigEdit {
                name: "Throttle".into(),
                value: 35.0,
                auto: AutoChange::None,
            },
            SigEdit {
                name: "Running".into(),
                value: 1.0,
                auto: AutoChange::None,
            },
        ];
        g.rows = vec![
            GenRow {
                uid: 1,
                bus,
                id_text: "3A0".into(),
                data_text: "DE AD BE EF 01 02 03 04".into(),
                mode: SendMode::Cyclic,
                period_ms: 100,
                ..Default::default()
            },
            GenRow {
                uid: 2,
                bus,
                mode: SendMode::Cyclic,
                period_ms: 50,
                dbc: Some(DbcRow {
                    msg: "EngineData".into(),
                    signals,
                }),
                ..Default::default()
            },
            GenRow {
                uid: 3,
                bus,
                id_text: "3B0".into(),
                data_text: "01 00 00 00 00 00 00 00".into(),
                mode: SendMode::Key,
                key: Some("F5".into()),
                ..Default::default()
            },
            GenRow {
                uid: 4,
                bus: None,
                id_text: "18FF1234".into(),
                extended: true,
                dlc: 4,
                data_text: "CA FE 00 01".into(),
                mode: SendMode::Once,
                ..Default::default()
            },
        ];
        let view = g.view();
        g.apply_view(view);
        self.demo_generator_pending = Some(id);
        self.last_generator = Some(id);
    }

    /// Replace the graph with `topo` loaded from `path` and load the
    /// databases it references.
    fn install_topology(&mut self, topo: &Topology, path: &std::path::Path) {
        self.graph = Graph::from_topology(topo);
        // Projects saved before the Network views existed have no layout
        // and open in the default bus-line view.
        if let Some(layout) = topo
            .workspace
            .as_ref()
            .and_then(workspace::network_from_json)
        {
            self.graph.apply_layout(&layout);
        }
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
        self.generators.clear();
        self.last_generator = None;
        self.logging = topo
            .workspace
            .as_ref()
            .and_then(workspace::logging_from_json)
            .unwrap_or_default();
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
            for (id, view) in workspace::generators_from_json(ws) {
                if let Some(g) = self.generators.get_mut(&id) {
                    g.apply_view(view);
                }
            }
        }
        self.reload_dbcs();
    }

    /// Project file name without `.operow.json`, for log file names.
    fn project_name(&self) -> String {
        self.project_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .map(|n| {
                let n = n.strip_suffix(".json").unwrap_or(&n);
                n.strip_suffix(".operow").unwrap_or(n).to_string()
            })
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "operow".into())
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

    // ---- offline replay --------------------------------------------------

    /// Ask for an ASC or BLF file, then open the channel mapping dialog for it.
    fn pick_log(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Vector logs", &["asc", "blf"])
            .pick_file()
        {
            self.begin_open_log(&path);
        }
    }

    /// Default channel mapping: channels in order onto the existing buses,
    /// then new buses.
    fn default_targets(&self, info: &LogInfo) -> Vec<(u8, Option<BusId>)> {
        let buses = self.graph.to_topology().buses;
        info.channels
            .iter()
            .enumerate()
            .map(|(i, ch)| (*ch, buses.get(i).map(|b| b.id)))
            .collect()
    }

    fn begin_open_log(&mut self, path: &std::path::Path) {
        match replay::probe(path) {
            Ok(info) => {
                let targets = self.default_targets(&info);
                self.open_log = Some(OpenLogDialog {
                    path: path.to_path_buf(),
                    info,
                    targets,
                });
            }
            Err(e) => self.log_error(format!("log {}: {e}", path.display())),
        }
    }

    /// `--open-log`: map the channels in order and play at x1.
    fn open_log_auto(&mut self, path: &std::path::Path) {
        match replay::probe(path) {
            Ok(info) => {
                let targets = self.default_targets(&info);
                self.start_offline(path, info, &targets);
                if let Some(off) = &mut self.offline {
                    off.source.playing = true;
                }
            }
            Err(e) => self.log_error(format!("log {}: {e}", path.display())),
        }
    }

    /// Open an offline session: create the buses for channels mapped to a
    /// new one, stop the simulation and start reading `path` (paused).
    fn start_offline(
        &mut self,
        path: &std::path::Path,
        info: LogInfo,
        targets: &[(u8, Option<BusId>)],
    ) {
        self.stop();
        let mut map = HashMap::new();
        let mut created = 0;
        for &(ch, target) in targets {
            let bus = match target {
                Some(b) => b,
                None => {
                    let flow = self
                        .graph
                        .add_bus(egui::pos2(80.0, 260.0 + 80.0 * created as f32));
                    created += 1;
                    match self.graph.node_mut(flow) {
                        Some(GraphNode::Bus(b)) => {
                            b.name = format!("ch{ch}");
                            b.id
                        }
                        _ => continue,
                    }
                }
            };
            map.insert(ch, bus);
        }
        let topo = self.graph.to_topology();
        if created > 0 && self.graph.view() == NetworkView::BusLine {
            self.graph.auto_arrange();
        }
        self.names.rebuild(&topo);
        let rates = topo
            .buses
            .iter()
            .map(|b| (b.id, (b.bitrate, b.data_bitrate)))
            .collect();
        self.store.clear();
        self.bus_stats.clear();
        self.node_states.clear();
        self.buffer_stop_sent = false;
        self.sim_time = info.first;
        self.prev_stats_time = info.first;
        let file = path.file_name().map_or_else(
            || path.display().to_string(),
            |f| f.to_string_lossy().into(),
        );
        self.log(format!(
            "opened log {} ({:.3} s - {:.3} s)",
            path.display(),
            info.first.as_secs_f64(),
            info.last.as_secs_f64()
        ));
        self.offline = Some(Offline {
            source: ReplaySource::new(path, &info, map),
            file,
            drag: None,
            jump: String::new(),
            acc: HashMap::new(),
            rates,
            last_stats: info.first,
        });
    }

    /// Leave the offline session: back to simulation mode.
    fn close_log(&mut self) {
        if self.offline.take().is_some() {
            self.store.clear();
            self.bus_stats.clear();
            self.node_states.clear();
            self.sim_time = Timestamp::ZERO;
            self.prev_stats_time = Timestamp::ZERO;
            self.log("closed offline log");
        }
    }

    /// Clear everything shown and restart at `time` (a seek).
    fn offline_seek(&mut self, time: Timestamp) {
        let Some(off) = &mut self.offline else {
            return;
        };
        off.source.seek(time);
        off.acc.clear();
        off.last_stats = off.source.position();
        let pos = off.source.position();
        self.store.clear();
        self.bus_stats.clear();
        self.node_states.clear();
        self.prev_stats_time = pos;
        self.sim_time = pos;
    }

    /// Feed the due frames of the offline source into the store and keep
    /// the per-bus statistics.
    fn update_offline(&mut self, ctx: &egui::Context) {
        let Some(off) = &mut self.offline else {
            return;
        };
        let adv = off.source.tick(std::time::Instant::now());
        let error = off.source.error.take();
        for ev in &adv.events {
            if let Some(&(nominal, data)) = off.rates.get(&ev.bus) {
                let s = off.acc.entry(ev.bus).or_default();
                if let Some(kind) = ev.error_kind() {
                    // Logs do not say how long an error frame was.
                    match kind {
                        CanErrorKind::Bit => s.can_errors.bit += 1,
                        CanErrorKind::Stuff => s.can_errors.stuff += 1,
                        CanErrorKind::Crc => s.can_errors.crc += 1,
                        CanErrorKind::Form => s.can_errors.form += 1,
                        CanErrorKind::Ack => s.can_errors.ack += 1,
                    }
                    continue;
                }
                s.frames += 1;
                s.busy_ns += operow_engine::frame_duration_ns_any(&ev.frame, nominal, data);
            }
        }
        let pos = off.source.position();
        let playing = off.source.playing;
        let stats_due = pos.0.saturating_sub(off.last_stats.0) >= OFFLINE_STATS_NS;
        let stats = stats_due.then(|| {
            off.last_stats = pos;
            off.acc.iter().map(|(b, s)| (*b, *s)).collect::<Vec<_>>()
        });
        self.store.push_batch(&adv.events);
        if adv.restarted {
            self.store.clear();
            self.bus_stats.clear();
            self.node_states.clear();
            self.prev_stats_time = pos;
            if let Some(off) = &mut self.offline {
                off.acc.clear();
                off.last_stats = pos;
            }
        } else if let Some(buses) = stats {
            self.handle_event(EngineEvent::Stats { time: pos, buses });
        }
        self.sim_time = pos;
        if let Some(e) = error {
            self.log_error(format!("log: {e}"));
        }
        if playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(
                self.settings.ui_refresh_ms as u64,
            ));
        }
    }

    /// Play/pause, speed, seek, jump, loop and close for the open log.
    fn replay_bar(&mut self, ui: &mut egui::Ui) {
        let Some(off) = &mut self.offline else {
            return;
        };
        let mut seek: Option<Timestamp> = None;
        let mut close = false;
        let src = &mut off.source;
        let finished = src.position() >= src.last && !src.looped;
        let (first, last) = (src.first.as_secs_f64(), src.last.as_secs_f64());
        ui.horizontal(|ui| {
            let (icon, tip) = if src.playing {
                (icons::pause(), "Pause")
            } else {
                (icons::play(), "Play")
            };
            if icons::icon_button(ui, icon, tip).clicked() {
                if src.playing {
                    src.playing = false;
                } else {
                    if finished {
                        seek = Some(src.first);
                    }
                    src.playing = true;
                }
            }
            let current = REPLAY_SPEEDS
                .iter()
                .find(|(s, _)| *s == src.speed)
                .map_or("\u{d7}1", |(_, l)| l);
            ui.label("Speed:");
            egui::ComboBox::from_id_salt("replay_speed")
                .width(70.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (s, label) in REPLAY_SPEEDS {
                        ui.selectable_value(&mut src.speed, s, label);
                    }
                });
            ui.separator();

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close log").clicked() {
                    close = true;
                }
                ui.checkbox(&mut src.looped, "Loop");
                let go = ui.button("Go").on_hover_text("Jump to the time (seconds)");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut off.jump)
                        .hint_text("jump to s")
                        .desired_width(70.0),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if go.clicked() || enter {
                    let text = off.jump.trim().trim_end_matches('s').trim();
                    if let Ok(t) = text.parse::<f64>()
                        && t.is_finite()
                    {
                        seek = Some(Timestamp((t.max(0.0) * 1e9) as u64));
                    }
                }
                ui.separator();
                let shown = off.drag.unwrap_or_else(|| src.position().as_secs_f64());
                ui.monospace(format!("{shown:8.3} s / {last:.3} s"));
                // The slider takes what is left.
                ui.spacing_mut().slider_width = (ui.available_width() - 24.0).max(60.0);
                let mut value = shown;
                let resp = ui.add_enabled(
                    last > first,
                    egui::Slider::new(&mut value, first..=last.max(first + 1e-9))
                        .show_value(false)
                        .trailing_fill(true),
                );
                if resp.dragged() {
                    off.drag = Some(value);
                }
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    off.drag = None;
                    seek = Some(Timestamp((value * 1e9) as u64));
                }
            });
        });
        if let Some(t) = seek {
            self.offline_seek(t);
        }
        if close {
            self.close_log();
        }
    }

    fn open_log_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dlg) = &mut self.open_log else {
            return;
        };
        let buses = self.graph.to_topology().buses;
        let mut action = None;
        egui::Window::new("Open log")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(dlg.path.display().to_string()).monospace());
                ui.label(format!(
                    "{:.3} s - {:.3} s, {} channel(s)",
                    dlg.info.first.as_secs_f64(),
                    dlg.info.last.as_secs_f64(),
                    dlg.info.channels.len()
                ));
                ui.add_space(6.0);
                egui::Grid::new("open_log_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        for (ch, target) in &mut dlg.targets {
                            ui.label(format!("Channel {ch}:"));
                            let text = match target {
                                Some(id) => buses
                                    .iter()
                                    .find(|b| b.id == *id)
                                    .map_or("?".to_string(), |b| b.name.clone()),
                                None => format!("New bus ch{ch}"),
                            };
                            egui::ComboBox::from_id_salt(("open_log_ch", *ch))
                                .selected_text(text)
                                .show_ui(ui, |ui| {
                                    for b in &buses {
                                        ui.selectable_value(target, Some(b.id), &b.name);
                                    }
                                    ui.selectable_value(
                                        target,
                                        None,
                                        format!("New bus ch{ch} (500k)"),
                                    );
                                });
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                });
            });
        match action {
            Some(true) => {
                if let Some(d) = self.open_log.take() {
                    self.start_offline(&d.path, d.info, &d.targets);
                }
            }
            Some(false) => self.open_log = None,
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
        if self.offline.is_some() {
            self.log("close the offline log to run the simulation");
            return;
        }
        let mut topo = self.graph.to_topology();
        // Replay nodes read their log from the engine thread; hand it
        // paths that do not depend on the working directory.
        let dir = self.project_dir();
        for n in &mut topo.nodes {
            if let operow_core::NodeKind::Replay { path, .. } = &mut n.kind
                && !path.trim().is_empty()
            {
                *path = dbcs::resolve_path(dir.as_deref(), path)
                    .to_string_lossy()
                    .into_owned();
            }
        }
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
        self.node_states.clear();
        self.sim_time = Timestamp::ZERO;
        self.prev_stats_time = Timestamp::ZERO;
        let demo_errors = self.demo_errors.then(|| demo_error_spec(&topo)).flatten();
        self.runtime.reset();
        let by_name = |name: &str| topo.nodes.iter().find(|n| n.name == name).map(|n| n.id);
        let demo_offline = self.demo_faults.then(|| by_name("Body")).flatten();
        let demo_bus_off = self.demo_busoff.then(|| by_name("Engine")).flatten();
        let buses_of = |node| crate::runtime::buses_of(&topo.links, node);
        let demo_offline = demo_offline.map(|n| (n, buses_of(n)));
        let demo_bus_off = demo_bus_off.map(|n| (n, buses_of(n)));
        let _ = self.engine.cmd.send(Command::Load(topo));
        let _ = self.engine.cmd.send(Command::SetSpeed(self.speed));
        if let Some(spec) = demo_errors {
            let _ = self.engine.cmd.send(Command::InjectErrors(spec));
        }
        for cmd in self.faults.sync_commands(false) {
            let _ = self.engine.cmd.send(cmd);
        }
        if let Some((node, buses)) = demo_offline {
            let cmd = self.runtime.set_online(node, None, &buses, false);
            let _ = self.engine.cmd.send(cmd);
        }
        if let Some((node, buses)) = demo_bus_off {
            for bus in buses {
                let _ = self.engine.cmd.send(Command::ForceBusOff(node, bus));
            }
        }
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
        self.offline = None;
        self.graph = Graph::new();
        self.graph.add_bus(egui::pos2(80.0, 260.0));
        self.names.rebuild(&self.graph.to_topology());
        self.names.dbcs = Default::default();
        self.project_path = None;
        self.logging = LoggingConfig::default();
        self.graphs.clear();
        self.last_graph = None;
        self.generators.clear();
        self.last_generator = None;
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
                            self.offline = None;
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
            let generators = self
                .generators
                .iter()
                .map(|(id, g)| (*id, g.view()))
                .collect();
            topo.workspace = workspace::with_logging(
                workspace::layout_to_json_with(
                    &self.dock,
                    views,
                    graphs,
                    generators,
                    Some(self.graph.network_layout()),
                ),
                &self.logging,
            );
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
    /// bus, then the bus's wire to every receiver one leg later. A frame a
    /// gateway forwards is its own event and chains on after that. Pulses
    /// are blue for originated frames, gateway-coloured when forwarded and
    /// pink diamonds for Generator windows; hovering one shows
    /// `0x<id> <message>`.
    fn animate_frames(&mut self, frames: &[operow_core::BusEvent]) {
        let links = self.graph.links();
        let wires = self.graph.wire_map();
        let now = std::time::Instant::now();
        self.pulse_last
            .retain(|_, t| now.duration_since(*t) < PULSE_MIN_INTERVAL * 20);
        for spec in pulses_for_events(frames, &links) {
            // A Generator window's virtual sender has no wire.
            let Some(&edge) = wires.get(&(spec.node, spec.bus)) else {
                continue;
            };
            let key = (edge, spec.dir);
            if self
                .pulse_last
                .get(&key)
                .is_some_and(|t| now.duration_since(*t) < PULSE_MIN_INTERVAL)
            {
                continue;
            }
            self.pulse_last.insert(key, now);
            let (color, shape, radius) = if spec.kind.error {
                (self.theme.error_color(), PulseShape::Diamond, 5.0)
            } else if spec.kind.generator {
                (self.theme.generator_color(), PulseShape::Diamond, 5.0)
            } else if spec.kind.forwarded {
                (self.theme.gateway_color(), PulseShape::Circle, 4.0)
            } else {
                let c = self.theme.bus_color(if spec.kind.fd { 2 } else { 0 });
                (c, PulseShape::Circle, 4.0)
            };
            let name = self
                .names
                .msg_name(spec.bus, spec.origin, spec.id, spec.extended);
            let label = if spec.kind.error {
                format!("Error frame (0x{:X})", spec.id)
            } else if name.is_empty() {
                format!("0x{:X}", spec.id)
            } else {
                format!("0x{:X} {name}", spec.id)
            };
            self.graph.pulse_wire(
                edge,
                spec.dir,
                PulseStyle {
                    color: Some(color),
                    radius,
                    duration: PULSE_LEG_S,
                    delay: PULSE_LEG_S * f32::from(spec.legs),
                    label: Some(label),
                    shape,
                    ..Default::default()
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
                    entry.can_errors = stats.can_errors;
                    entry.dropped_bus_off = stats.dropped_bus_off;
                    entry.dropped_offline = stats.dropped_offline;
                    entry.dropped_msg_control = stats.dropped_msg_control;
                    entry.prev = stats;
                }
                self.prev_stats_time = time;
                self.sim_time = time;
            }
            EngineEvent::NodeStates { nodes, .. } => {
                self.runtime.observe(&nodes, std::time::Instant::now());
                self.node_states = nodes;
            }
            EngineEvent::State(s) => {
                let was_stopped = self.run_state == RunState::Stopped;
                self.run_state = s;
                if s == RunState::Stopped {
                    // The engine forgot its node and message controls (a
                    // Load also reports Stopped, but then none exist yet).
                    if !was_stopped {
                        self.runtime.reset();
                    }
                    // The engine dropped its generator timers.
                    for g in self.generators.values_mut() {
                        g.stop_local();
                    }
                }
                self.log(format!("state -> {s:?}"));
            }
            EngineEvent::Log(msg) => self.log(msg),
            EngineEvent::DiagResponse { req, resp, .. } => {
                // The diagnostic console (next job) consumes these; log for now.
                let line = match resp {
                    Ok(r) => operow_uds::describe(&r, false),
                    Err(e) => format!("no response ({e})"),
                };
                self.log(format!(
                    "diag {} -> {line}",
                    operow_uds::describe(&req, true)
                ));
            }
            EngineEvent::Error(msg) => {
                self.last_error = Some(msg.clone());
                self.log(format!("error: {msg}"));
            }
        }
    }

    /// Feed the logging task: pressed keys, new frames, measurement state.
    fn run_logging(&mut self, ctx: &egui::Context) {
        let keys: Vec<char> = if ctx.wants_keyboard_input() {
            Vec::new()
        } else {
            ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) => t.chars().next(),
                        _ => None,
                    })
                    .collect()
            })
        };
        let mut buses: Vec<BusId> = self.names.bus_names.keys().copied().collect();
        buses.sort();
        let project = self.project_name();
        let dir = self.project_dir();
        let lctx = LogContext {
            store: &self.store,
            buses: &buses,
            project: &project,
            project_dir: dir.as_deref(),
            dbcs: &self.names.dbcs,
            users: &self.graph.user_signals,
        };
        let running = self.run_state != RunState::Stopped;
        let msgs = self.log_rt.update(&self.logging, running, &keys, &lctx);
        for m in msgs {
            match m.strip_prefix("error: ") {
                Some(e) => self.log_error(e.to_string()),
                None => self.log(m),
            }
        }
    }

    fn logging_window_ui(&mut self, ctx: &egui::Context) {
        if !self.show_logging {
            return;
        }
        let mut buses: Vec<(BusId, String)> = self
            .graph
            .to_topology()
            .buses
            .iter()
            .map(|b| (b.id, b.name.clone()))
            .collect();
        buses.sort_by_key(|(id, _)| *id);
        let project = self.project_name();
        let dir = self.project_dir();
        let input = LoggingInput {
            buses: &buses,
            dbcs: &self.names.dbcs,
            users: &self.graph.user_signals,
            names: &self.names,
            status: self.log_rt.status(),
            files: &self.log_rt.files,
            project: &project,
            project_dir: dir.as_deref(),
        };
        let mut open = true;
        logging_window::show(ctx, &mut open, &mut self.logging, &input);
        self.show_logging = open;
    }

    /// Sender names, demo start, key presses, auto-change and engine sync
    /// of every Generator window.
    fn update_generators(&mut self, ctx: &egui::Context) {
        self.names.generator_names.clear();
        for (id, g) in &self.generators {
            self.names
                .generator_names
                .insert(GeneratorId(id.n).node(), g.sender_name(*id));
        }
        let running = self.run_state == RunState::Running;
        if running
            && let Some(id) = self.demo_generator_pending.take()
            && let Some(g) = self.generators.get_mut(&id)
        {
            for i in 0..g.rows.len() {
                if g.rows[i].mode == SendMode::Cyclic {
                    g.start_row(i);
                }
            }
        }
        let mut cmds = Vec::new();
        for (id, g) in self.generators.iter_mut() {
            cmds.extend(g.update(ctx, *id, &self.names, running));
        }
        for cmd in cmds {
            let _ = self.engine.cmd.send(cmd);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            let idle = self.run_state == RunState::Stopped;
            let running = self.run_state == RunState::Running;
            let paused = self.run_state == RunState::Paused;
            let offline = self.offline.is_some();

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
                if ui
                    .add_enabled(idle, egui::Button::new("Open log..."))
                    .on_hover_text("Analyze an ASC log offline, without simulating")
                    .clicked()
                {
                    self.pick_log();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                let item =
                    |text: &str, shortcut: &str| egui::Button::new(text).shortcut_text(shortcut);
                if ui
                    .add_enabled(idle && self.graph.can_undo(), item("Undo", "Ctrl+Z"))
                    .clicked()
                {
                    self.graph.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(idle && self.graph.can_redo(), item("Redo", "Ctrl+Y"))
                    .clicked()
                {
                    self.graph.redo();
                    ui.close();
                }
                ui.separator();
                let selected = idle && self.graph.has_selection();
                if ui.add_enabled(selected, item("Copy", "Ctrl+C")).clicked() {
                    self.graph.copy();
                    ui.close();
                }
                if ui.add_enabled(idle, item("Paste", "Ctrl+V")).clicked() {
                    self.graph.paste();
                    ui.close();
                }
                if ui
                    .add_enabled(selected, item("Duplicate", "Ctrl+D"))
                    .clicked()
                {
                    self.graph.duplicate();
                    ui.close();
                }
                ui.separator();
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
                    .add_enabled(idle, egui::Button::new("Add Replay node"))
                    .clicked()
                {
                    self.graph.add_replay(egui::pos2(40.0, 160.0));
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
                if ui.button("Logging...").clicked() {
                    self.show_logging = true;
                    ui.close();
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
                    .add_enabled(!running && !paused && !offline, egui::Button::new("Start"))
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

            if icons::icon_button_enabled(
                ui,
                !running && !paused && !offline,
                icons::play(),
                if offline {
                    "Close the offline log to run the simulation"
                } else {
                    "Start"
                },
            )
            .clicked()
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
            self.log_button(ui);

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

    /// The "Log" toggle: a dot that is red while recording and amber while
    /// armed. Right-click or long-press opens the Logging window.
    fn log_button(&mut self, ui: &mut egui::Ui) {
        let state = self.log_rt.status().state;
        let dot = if self.logging.enabled && state == LogState::Idle {
            egui::Color32::from_rgb(0xa0, 0x50, 0x50)
        } else {
            logging_window::state_color(state)
        };
        let mut button = egui::Button::new("       Log");
        if self.logging.enabled {
            button = button.fill(if self.theme == AppTheme::Dark {
                egui::Color32::from_rgb(0x4a, 0x2a, 0x2a)
            } else {
                egui::Color32::from_rgb(0xf4, 0xd4, 0xd4)
            });
        }
        let resp = ui.add(button).on_hover_text(
            "Log bus traffic to an ASC file while the measurement runs\n\
             Right-click or long-press for the logging settings",
        );
        let c = egui::pos2(resp.rect.left() + 13.0, resp.rect.center().y);
        ui.painter().circle_filled(c, 5.0, dot);
        if resp.clicked() {
            self.logging.enabled = !self.logging.enabled;
        }
        if resp.secondary_clicked() || resp.long_touched() {
            self.show_logging = true;
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (color, label) = match self.run_state {
                RunState::Stopped => (egui::Color32::GRAY, "Stopped"),
                RunState::Running => (egui::Color32::from_rgb(0x1a, 0x9c, 0x3a), "Running"),
                RunState::Paused => (egui::Color32::from_rgb(0xd0, 0x90, 0x1a), "Paused"),
            };
            if let Some(off) = &self.offline {
                let src = &off.source;
                let amber = egui::Color32::from_rgb(0xd0, 0x90, 0x1a);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.0, amber);
                ui.label(
                    egui::RichText::new(format!(
                        "OFFLINE: {} \u{b7} {:.3} s / {:.3} s",
                        off.file,
                        src.position().as_secs_f64(),
                        src.last.as_secs_f64()
                    ))
                    .strong(),
                );
            } else {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.0, color);
                ui.label(label);
                ui.separator();
                ui.label(format!("t = {:.3} s", self.sim_time.as_secs_f64()));
            }
            ui.separator();
            let log = self.log_rt.status();
            if log.state != LogState::Idle {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    5.0,
                    logging_window::state_color(log.state),
                );
                if log.state == LogState::Armed {
                    ui.label("Log armed");
                } else {
                    ui.label(format!(
                        "REC {} \u{b7} {}",
                        logging_window::format_elapsed(log.elapsed_s),
                        logging_window::format_size(log.bytes)
                    ));
                }
                ui.separator();
            }
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

        self.update_offline(ctx);
        self.sync_instances();
        self.run_logging(ctx);
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
        if let Some((_, tab)) = self.dock.find_active_focused() {
            match tab.kind {
                WindowKind::Graph => self.last_graph = Some(*tab),
                WindowKind::Generator => self.last_generator = Some(*tab),
                _ => {}
            }
        }
        self.update_generators(ctx);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            self.top_bar(ui);
        });

        if self.offline.is_some() {
            egui::TopBottomPanel::top("replay_bar").show(ctx, |ui| {
                self.replay_bar(ui);
            });
        }

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
                    generators: &mut self.generators,
                    store: &self.store,
                    names: &self.names,
                    status_log: &mut self.status_log,
                    bus_stats: &self.bus_stats,
                    node_states: &self.node_states,
                    run_state: self.run_state,
                    faults: &mut self.faults,
                    runtime: &mut self.runtime,
                    theme: self.theme,
                    menu_pos: &mut self.menu_pos,
                    project_dir: self
                        .project_path
                        .as_deref()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf()),
                    cmds: Vec::new(),
                    trace_actions: Vec::new(),
                    open_request: None,
                };
                let style = egui_dock::Style::from_egui(ui.style().as_ref());
                egui_dock::DockArea::new(&mut self.dock)
                    .style(style)
                    .show_add_buttons(false)
                    .show_close_buttons(true)
                    .show_inside(ui, &mut viewer);
                let (cmds, actions) = (viewer.cmds, viewer.trace_actions);
                if let Some(kind) = viewer.open_request {
                    self.open_window(kind, false);
                }
                self.handle_trace_actions(actions);
                for cmd in cmds {
                    let _ = self.engine.cmd.send(cmd);
                }
                // Buses can disappear through undo, cut or the Delete key.
                self.sync_dbcs();
            });

        self.settings_ui(ctx);
        self.logging_window_ui(ctx);
        self.import_dialog_ui(ctx);
        self.open_log_dialog_ui(ctx);
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
