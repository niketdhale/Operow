//! The docked workspace: window kinds, instance ids, default layouts and
//! (de)serialization into the project file.

use egui_dock::{DockState, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::diag_window::DiagView;
use crate::generator_window::GeneratorView;
use crate::graph_window::GraphView;
use crate::logging::LoggingConfig;
use crate::network_view::NetworkLayout;
use crate::trace::TraceView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WindowKind {
    Network,
    Properties,
    Trace,
    Log,
    Statistics,
    Graph,
    Generator,
    Faults,
    Diag,
}

impl WindowKind {
    pub const ALL: [WindowKind; 9] = [
        WindowKind::Network,
        WindowKind::Properties,
        WindowKind::Trace,
        WindowKind::Log,
        WindowKind::Statistics,
        WindowKind::Graph,
        WindowKind::Generator,
        WindowKind::Faults,
        WindowKind::Diag,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WindowKind::Network => "Network",
            WindowKind::Properties => "Properties",
            WindowKind::Trace => "Trace",
            WindowKind::Log => "Log",
            WindowKind::Statistics => "Statistics",
            WindowKind::Graph => "Graph",
            WindowKind::Generator => "Generator",
            WindowKind::Faults => "Faults",
            WindowKind::Diag => "Diagnostics",
        }
    }

    /// Whether several instances of this kind may be open at once.
    pub fn multi(self) -> bool {
        matches!(
            self,
            WindowKind::Trace | WindowKind::Graph | WindowKind::Generator | WindowKind::Diag
        )
    }
}

/// One open window: a kind plus an instance number (starting at 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowId {
    pub kind: WindowKind,
    pub n: u32,
}

impl WindowId {
    pub fn new(kind: WindowKind, n: u32) -> Self {
        WindowId { kind, n }
    }

    /// Tab title: "Trace", "Trace 2", "Graph 1".
    pub fn title(&self) -> String {
        match self.kind {
            WindowKind::Graph | WindowKind::Generator => {
                format!("{} {}", self.kind.label(), self.n)
            }
            _ if self.n <= 1 => self.kind.label().to_string(),
            _ => format!("{} {}", self.kind.label(), self.n),
        }
    }
}

pub type Dock = DockState<WindowId>;

fn w(kind: WindowKind) -> WindowId {
    WindowId::new(kind, 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPreset {
    Default,
    TraceFocus,
    NetworkOnly,
}

impl LayoutPreset {
    pub const ALL: [LayoutPreset; 3] = [
        LayoutPreset::Default,
        LayoutPreset::TraceFocus,
        LayoutPreset::NetworkOnly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LayoutPreset::Default => "Default",
            LayoutPreset::TraceFocus => "Trace focus",
            LayoutPreset::NetworkOnly => "Network only",
        }
    }

    pub fn build(self) -> Dock {
        match self {
            LayoutPreset::Default => preset(0.78, 0.62),
            LayoutPreset::TraceFocus => preset(0.78, 0.35),
            LayoutPreset::NetworkOnly => {
                let mut dock = DockState::new(vec![w(WindowKind::Network)]);
                dock.main_surface_mut().split_right(
                    NodeIndex::root(),
                    0.78,
                    vec![w(WindowKind::Properties)],
                );
                dock
            }
        }
    }
}

/// Network centre-top, Properties right, Trace/Log/Statistics tabbed below.
fn preset(right: f32, top: f32) -> Dock {
    let mut dock = DockState::new(vec![w(WindowKind::Network)]);
    let tree = dock.main_surface_mut();
    let [network, _props] =
        tree.split_right(NodeIndex::root(), right, vec![w(WindowKind::Properties)]);
    tree.split_below(
        network,
        top,
        vec![
            w(WindowKind::Trace),
            w(WindowKind::Log),
            w(WindowKind::Statistics),
        ],
    );
    dock
}

/// Network on top; Trace/Log/Statistics below with `generator` docked to
/// the right of them (`--demo-generator`).
pub fn generator_demo_layout(generator: WindowId) -> Dock {
    let mut dock = DockState::new(vec![w(WindowKind::Network)]);
    let tree = dock.main_surface_mut();
    let [_, bottom] = tree.split_below(
        NodeIndex::root(),
        0.35,
        vec![
            w(WindowKind::Trace),
            w(WindowKind::Log),
            w(WindowKind::Statistics),
        ],
    );
    tree.split_right(bottom, 0.56, vec![generator]);
    dock
}

/// Network on top; the Diagnostics window and a Trace side by side below
/// (`--demo-diag`).
pub fn diag_demo_layout(diag: WindowId, trace: WindowId) -> Dock {
    let mut dock = DockState::new(vec![w(WindowKind::Network)]);
    let tree = dock.main_surface_mut();
    let [_, bottom] = tree.split_below(NodeIndex::root(), 0.22, vec![diag]);
    tree.split_right(bottom, 0.42, vec![trace]);
    dock
}

/// Network on top; Trace and Statistics side by side below (`--demo-errors`).
pub fn errors_demo_layout() -> Dock {
    let mut dock = DockState::new(vec![w(WindowKind::Network)]);
    let tree = dock.main_surface_mut();
    let [_, bottom] = tree.split_below(
        NodeIndex::root(),
        0.35,
        vec![w(WindowKind::Trace), w(WindowKind::Log)],
    );
    tree.split_right(bottom, 0.55, vec![w(WindowKind::Statistics)]);
    dock
}

/// Network on top; Trace and Log below with Faults to the right of them
/// (`--demo-faults`, `--demo-busoff`).
pub fn faults_demo_layout() -> Dock {
    let mut dock = DockState::new(vec![w(WindowKind::Network)]);
    let tree = dock.main_surface_mut();
    let [left, _props] = tree.split_right(NodeIndex::root(), 0.7, vec![w(WindowKind::Properties)]);
    let [_, bottom] = tree.split_below(
        left,
        0.5,
        vec![
            w(WindowKind::Statistics),
            w(WindowKind::Trace),
            w(WindowKind::Log),
        ],
    );
    tree.split_right(bottom, 0.56, vec![w(WindowKind::Faults)]);
    dock
}

pub fn default_layout() -> Dock {
    LayoutPreset::Default.build()
}

/// All open windows.
pub fn open_windows(dock: &Dock) -> Vec<WindowId> {
    dock.iter_all_tabs().map(|(_, t)| *t).collect()
}

/// Instance number for a new window of `kind`: one past the highest open.
pub fn next_instance(dock: &Dock, kind: WindowKind) -> u32 {
    open_windows(dock)
        .iter()
        .filter(|t| t.kind == kind)
        .map(|t| t.n)
        .max()
        .map_or(1, |n| n + 1)
}

/// Open a window of `kind`, or focus it when it is a singleton that is
/// already open. `force_new` always creates a new instance (multi kinds).
/// Returns the window shown.
pub fn open_or_focus(dock: &mut Dock, kind: WindowKind, force_new: bool) -> WindowId {
    if (!kind.multi() || !force_new)
        && let Some(id) = open_windows(dock).into_iter().find(|t| t.kind == kind)
    {
        focus(dock, id);
        return id;
    }
    let id = WindowId::new(kind, next_instance(dock, kind));
    // Prefer the leaf that already holds windows of this kind.
    let open = open_windows(dock);
    let mut sibling = open.iter().copied().find(|t| t.kind == kind);
    if sibling.is_none() && matches!(kind, WindowKind::Faults | WindowKind::Diag) {
        // Next to the bottom windows, not on top of the Network.
        sibling = open.iter().copied().find(|t| {
            matches!(
                t.kind,
                WindowKind::Statistics | WindowKind::Log | WindowKind::Trace
            )
        });
    }
    match sibling.and_then(|s| dock.find_tab(&s)) {
        Some((surface, node, _)) => {
            dock.set_focused_node_and_surface((surface, node));
            dock.push_to_focused_leaf(id);
        }
        None => dock.push_to_focused_leaf(id),
    }
    id
}

pub fn focus(dock: &mut Dock, id: WindowId) {
    if let Some((surface, node, tab)) = dock.find_tab(&id) {
        dock.set_active_tab((surface, node, tab));
        dock.set_focused_node_and_surface((surface, node));
    }
}

const LAYOUT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct SavedLayout {
    version: u32,
    dock: Dock,
    /// Per-window settings of the Trace windows (title, filters, ...).
    #[serde(default)]
    traces: Vec<(WindowId, TraceView)>,
    /// Per-window settings of the Graph windows (signals, mode, ...).
    #[serde(default)]
    graphs: Vec<(WindowId, GraphView)>,
    /// Per-window settings of the Generator windows (rows, modes, ...).
    #[serde(default)]
    generators: Vec<(WindowId, GeneratorView)>,
    /// Per-window settings of the Diagnostics windows (target, saved
    /// requests, ...).
    #[serde(default)]
    diags: Vec<(WindowId, DiagView)>,
    /// The Network window's view and per-view positions. Projects saved
    /// before it existed have none and open in the default bus-line view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<NetworkLayout>,
    /// ASC logging settings. Projects saved before logging existed have
    /// none and get the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    logging: Option<LoggingConfig>,
}

#[cfg(test)]
pub fn layout_to_json(dock: &Dock) -> Option<serde_json::Value> {
    layout_to_json_with(dock, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None)
}

/// Like [`layout_to_json`], also saving the settings of Trace, Graph,
/// Generator and Diagnostics windows and the Network window's view and
/// positions.
pub fn layout_to_json_with(
    dock: &Dock,
    mut traces: Vec<(WindowId, TraceView)>,
    mut graphs: Vec<(WindowId, GraphView)>,
    mut generators: Vec<(WindowId, GeneratorView)>,
    mut diags: Vec<(WindowId, DiagView)>,
    network: Option<NetworkLayout>,
) -> Option<serde_json::Value> {
    traces.sort_by_key(|(id, _)| (id.kind, id.n));
    graphs.sort_by_key(|(id, _)| (id.kind, id.n));
    generators.sort_by_key(|(id, _)| (id.kind, id.n));
    diags.sort_by_key(|(id, _)| (id.kind, id.n));
    // Unlaid-out rects are infinite, which JSON cannot represent. Rects are
    // recomputed on the next frame, so store zeros.
    let mut dock = dock.clone();
    for (_, node) in dock.iter_all_nodes_mut() {
        node.set_rect(egui::Rect::ZERO);
    }
    for (_, leaf) in dock.iter_leaves_mut() {
        leaf.viewport = egui::Rect::ZERO;
    }
    serde_json::to_value(SavedLayout {
        version: LAYOUT_VERSION,
        dock,
        traces,
        graphs,
        generators,
        diags,
        network,
        logging: None,
    })
    .ok()
}

/// Add the logging settings to a layout produced by [`layout_to_json_with`].
pub fn with_logging(
    layout: Option<serde_json::Value>,
    logging: &LoggingConfig,
) -> Option<serde_json::Value> {
    let mut layout = layout?;
    layout
        .as_object_mut()?
        .insert("logging".into(), serde_json::to_value(logging).ok()?);
    Some(layout)
}

/// Saved logging settings; `None` when absent or invalid.
pub fn logging_from_json(value: &serde_json::Value) -> Option<LoggingConfig> {
    serde_json::from_value::<SavedLayout>(value.clone())
        .ok()
        .and_then(|s| s.logging)
}

/// Saved Network view and positions; `None` when absent or invalid.
pub fn network_from_json(value: &serde_json::Value) -> Option<NetworkLayout> {
    serde_json::from_value::<SavedLayout>(value.clone())
        .ok()
        .and_then(|s| s.network)
}

/// Saved Trace window settings; empty when absent or invalid.
pub fn traces_from_json(value: &serde_json::Value) -> Vec<(WindowId, TraceView)> {
    serde_json::from_value::<SavedLayout>(value.clone())
        .map(|s| s.traces)
        .unwrap_or_default()
}

/// Saved Graph window settings; empty when absent or invalid.
pub fn graphs_from_json(value: &serde_json::Value) -> Vec<(WindowId, GraphView)> {
    serde_json::from_value::<SavedLayout>(value.clone())
        .map(|s| s.graphs)
        .unwrap_or_default()
}

/// Saved Generator window settings; empty when absent or invalid.
pub fn generators_from_json(value: &serde_json::Value) -> Vec<(WindowId, GeneratorView)> {
    serde_json::from_value::<SavedLayout>(value.clone())
        .map(|s| s.generators)
        .unwrap_or_default()
}

/// Saved Diagnostics window settings; empty when absent or invalid.
pub fn diags_from_json(value: &serde_json::Value) -> Vec<(WindowId, DiagView)> {
    serde_json::from_value::<SavedLayout>(value.clone())
        .map(|s| s.diags)
        .unwrap_or_default()
}

/// Parse a saved layout; `None` when invalid, an unknown version or empty.
pub fn layout_from_json(value: &serde_json::Value) -> Option<Dock> {
    let saved: SavedLayout = serde_json::from_value(value.clone()).ok()?;
    if saved.version != LAYOUT_VERSION || open_windows(&saved.dock).is_empty() {
        return None;
    }
    Some(saved.dock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::Topology;

    fn kinds(dock: &Dock) -> Vec<WindowKind> {
        open_windows(dock).iter().map(|t| t.kind).collect()
    }

    #[test]
    fn default_layout_has_expected_tabs() {
        let dock = default_layout();
        let k = kinds(&dock);
        for kind in [
            WindowKind::Network,
            WindowKind::Properties,
            WindowKind::Trace,
            WindowKind::Log,
            WindowKind::Statistics,
        ] {
            assert!(k.contains(&kind), "missing {kind:?}");
        }
        assert_eq!(k.len(), 5);
    }

    #[test]
    fn instance_naming() {
        assert_eq!(WindowId::new(WindowKind::Trace, 1).title(), "Trace");
        assert_eq!(WindowId::new(WindowKind::Trace, 2).title(), "Trace 2");
        assert_eq!(WindowId::new(WindowKind::Graph, 1).title(), "Graph 1");
        assert_eq!(WindowId::new(WindowKind::Network, 1).title(), "Network");
    }

    #[test]
    fn open_new_trace_gets_next_number() {
        let mut dock = default_layout();
        let id = open_or_focus(&mut dock, WindowKind::Trace, true);
        assert_eq!(id.title(), "Trace 2");
        assert_eq!(
            open_or_focus(&mut dock, WindowKind::Trace, true).title(),
            "Trace 3"
        );
        // Singletons are focused, not duplicated.
        open_or_focus(&mut dock, WindowKind::Log, true);
        assert_eq!(
            kinds(&dock)
                .iter()
                .filter(|k| **k == WindowKind::Log)
                .count(),
            1
        );
        // Non-forced open of a multi kind focuses the existing one.
        let n = open_windows(&dock).len();
        open_or_focus(&mut dock, WindowKind::Trace, false);
        assert_eq!(open_windows(&dock).len(), n);
    }

    #[test]
    fn closed_window_can_be_reopened() {
        let mut dock = default_layout();
        dock.retain_tabs(|t| t.kind != WindowKind::Properties);
        assert!(!kinds(&dock).contains(&WindowKind::Properties));
        open_or_focus(&mut dock, WindowKind::Properties, false);
        assert!(kinds(&dock).contains(&WindowKind::Properties));
    }

    #[test]
    fn workspace_round_trips_through_topology() {
        let mut dock = default_layout();
        open_or_focus(&mut dock, WindowKind::Trace, true);
        let topo = Topology {
            workspace: layout_to_json(&dock),
            ..Default::default()
        };
        let back = Topology::from_json(&topo.to_json()).unwrap();
        let restored = layout_from_json(back.workspace.as_ref().unwrap()).unwrap();
        assert_eq!(open_windows(&restored), open_windows(&dock));
    }

    #[test]
    fn trace_settings_persist_in_workspace_json() {
        let mut dock = default_layout();
        let id = open_or_focus(&mut dock, WindowKind::Trace, true);
        let mut view = TraceView {
            title: Some("Body debug".into()),
            ..Default::default()
        };
        view.filters.id.enabled = true;
        view.filters.id.text = "100-2FF".into();
        let json = layout_to_json_with(
            &dock,
            vec![(id, view)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
        )
        .unwrap();
        let back = traces_from_json(&json);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].0, id);
        assert_eq!(back[0].1.title.as_deref(), Some("Body debug"));
        assert_eq!(back[0].1.filters.id.text, "100-2FF");
        assert!(layout_from_json(&json).is_some());
        // Layouts saved before trace settings existed still load.
        let old = layout_to_json(&dock).unwrap();
        assert!(traces_from_json(&old).is_empty());
        assert!(traces_from_json(&serde_json::json!("x")).is_empty());
    }

    #[test]
    fn graph_config_persists_in_workspace_json() {
        use crate::graph_window::{GraphLayout, GraphMode, PlotSignal, SeriesStyle, YAxis};
        use crate::signals::{RawKind, SignalRef};
        let mut dock = default_layout();
        let id = open_or_focus(&mut dock, WindowKind::Graph, true);
        let view = GraphView {
            title: Some("Engine".into()),
            signals: vec![
                PlotSignal {
                    sig: SignalRef::Dbc {
                        bus: operow_core::BusId(1),
                        msg_id: 0x100,
                        extended: false,
                        signal_name: "EngineSpeed".into(),
                    },
                    color: [10, 20, 30],
                    axis: YAxis::Y2,
                    style: SeriesStyle::Step,
                    visible: false,
                },
                PlotSignal {
                    sig: SignalRef::Raw {
                        bus: operow_core::BusId(1),
                        id: 0x200,
                        extended: true,
                        kind: RawKind::Byte(3),
                    },
                    color: [1, 2, 3],
                    axis: YAxis::Y1,
                    style: SeriesStyle::Points,
                    visible: true,
                },
            ],
            mode: GraphMode::Paused,
            window_s: 30,
            layout: GraphLayout::Stacked,
        };
        let topo = Topology {
            workspace: layout_to_json_with(
                &dock,
                Vec::new(),
                vec![(id, view.clone())],
                Vec::new(),
                Vec::new(),
                None,
            ),
            ..Default::default()
        };
        let back = Topology::from_json(&topo.to_json()).unwrap();
        let ws = back.workspace.as_ref().unwrap();
        let graphs = graphs_from_json(ws);
        assert_eq!(graphs, vec![(id, view)]);
        assert!(layout_from_json(ws).is_some());
        // Layouts saved before graph settings existed still load.
        let old = layout_to_json(&dock).unwrap();
        assert!(graphs_from_json(&old).is_empty());
        assert!(graphs_from_json(&serde_json::json!("x")).is_empty());
    }

    #[test]
    fn generator_config_persists_in_workspace_json() {
        use crate::generator_window::{
            AutoChange, DbcRow, GenRow, GeneratorView, SendMode, SigEdit,
        };
        let mut dock = default_layout();
        let id = open_or_focus(&mut dock, WindowKind::Generator, true);
        let view = GeneratorView {
            title: Some("Bench".into()),
            rows: vec![
                GenRow {
                    uid: 1,
                    bus: Some(operow_core::BusId(2)),
                    id_text: "1A0".into(),
                    mode: SendMode::Cyclic,
                    period_ms: 25,
                    ..Default::default()
                },
                GenRow {
                    uid: 2,
                    mode: SendMode::Key,
                    key: Some("F3".into()),
                    dbc: Some(DbcRow {
                        msg: "EngineData".into(),
                        signals: vec![SigEdit {
                            name: "Throttle".into(),
                            value: 42.5,
                            auto: AutoChange::Counter { step: 0.5 },
                        }],
                    }),
                    ..Default::default()
                },
            ],
        };
        let topo = Topology {
            workspace: layout_to_json_with(
                &dock,
                Vec::new(),
                Vec::new(),
                vec![(id, view.clone())],
                Vec::new(),
                None,
            ),
            ..Default::default()
        };
        let back = Topology::from_json(&topo.to_json()).unwrap();
        let ws = back.workspace.as_ref().unwrap();
        assert_eq!(generators_from_json(ws), vec![(id, view)]);
        assert!(layout_from_json(ws).is_some());
        // Layouts saved before generator settings existed still load.
        let old = layout_to_json(&dock).unwrap();
        assert!(generators_from_json(&old).is_empty());
        assert!(generators_from_json(&serde_json::json!("x")).is_empty());
    }

    #[test]
    fn diagnostics_windows_persist_in_workspace_json() {
        use crate::diag_window::{DiagView, SavedRequest, TargetIds};
        let mut dock = default_layout();
        let id = open_or_focus(&mut dock, WindowKind::Diag, true);
        assert_eq!(id.title(), "Diagnostics");
        assert_eq!(
            open_or_focus(&mut dock, WindowKind::Diag, true).title(),
            "Diagnostics 2"
        );
        let view = DiagView {
            title: Some("Engine ECU".into()),
            target: Some("Engine".into()),
            ids: TargetIds {
                bus: Some(operow_core::BusId(1)),
                req_id: 0x7E0,
                resp_id: 0x7E8,
                functional_id: 0x7DF,
                extended: false,
                fd: true,
            },
            functional: true,
            tester_present: true,
            tester_present_ms: 1000,
            saved: vec![SavedRequest {
                name: "VIN".into(),
                hex: "22 F1 90".into(),
            }],
        };
        let topo = Topology {
            workspace: layout_to_json_with(
                &dock,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![(id, view.clone())],
                None,
            ),
            ..Default::default()
        };
        let back = Topology::from_json(&topo.to_json()).unwrap();
        let ws = back.workspace.as_ref().unwrap();
        assert_eq!(diags_from_json(ws), vec![(id, view)]);
        assert!(layout_from_json(ws).is_some());
        // Layouts saved before the Diagnostics window existed still load.
        let old = layout_to_json(&dock).unwrap();
        assert!(diags_from_json(&old).is_empty());
        assert!(diags_from_json(&serde_json::json!("x")).is_empty());
    }

    #[test]
    fn logging_config_persists_in_workspace_json() {
        use crate::logging::{Condition, StartTrigger, StopTrigger};
        let dock = default_layout();
        let mut cfg = LoggingConfig {
            enabled: true,
            split_minutes: Some(5),
            ..Default::default()
        };
        cfg.trigger.start = StartTrigger::OnCondition(Condition::Key('x'));
        cfg.trigger.stop = StopTrigger::AfterSeconds(3.0);
        let json = with_logging(layout_to_json(&dock), &cfg).unwrap();
        let topo = Topology {
            workspace: Some(json),
            ..Default::default()
        };
        let back = Topology::from_json(&topo.to_json()).unwrap();
        let ws = back.workspace.as_ref().unwrap();
        assert_eq!(logging_from_json(ws), Some(cfg));
        assert!(layout_from_json(ws).is_some());
        // Projects saved before logging existed have none.
        let old = layout_to_json(&dock).unwrap();
        assert_eq!(logging_from_json(&old), None);
        assert_eq!(logging_from_json(&serde_json::json!("x")), None);
    }

    #[test]
    fn network_view_and_positions_persist_per_mode() {
        use crate::network_view::{NetworkLayout, NetworkView, NodeKey, Place};
        let dock = default_layout();
        let layout = NetworkLayout {
            mode: NetworkView::FreeForm,
            free: vec![(
                NodeKey::Ecu(1),
                Place {
                    x: 1.0,
                    y: 2.0,
                    w: None,
                    h: None,
                },
            )],
            line: vec![
                (
                    NodeKey::Ecu(1),
                    Place {
                        x: 40.0,
                        y: 0.0,
                        w: None,
                        h: None,
                    },
                ),
                (
                    NodeKey::Bus(1),
                    Place {
                        x: 0.0,
                        y: 120.0,
                        w: Some(700.0),
                        h: Some(28.0),
                    },
                ),
            ],
        };
        let topo = Topology {
            workspace: layout_to_json_with(
                &dock,
                vec![],
                vec![],
                vec![],
                vec![],
                Some(layout.clone()),
            ),
            ..Default::default()
        };
        let back = Topology::from_json(&topo.to_json()).unwrap();
        let ws = back.workspace.as_ref().unwrap();
        assert_eq!(network_from_json(ws), Some(layout));
        assert!(layout_from_json(ws).is_some());
        // Projects saved without the setting have none, so they open in the
        // default bus-line view.
        let old = layout_to_json(&dock).unwrap();
        assert_eq!(network_from_json(&old), None);
        assert_eq!(NetworkLayout::default().mode, NetworkView::BusLine);
        let partial: NetworkLayout = serde_json::from_str("{}").unwrap();
        assert_eq!(partial.mode, NetworkView::BusLine);
    }

    #[test]
    fn graph_layout_round_trips_through_the_workspace() {
        use crate::graph::Graph;
        use crate::network_view::NetworkView;
        let mut g = Graph::default_demo();
        g.state.nodes[0].position = egui::pos2(11.0, 22.0);
        g.set_view(NetworkView::FreeForm);
        g.state.nodes[1].position = egui::pos2(5.0, 6.0);
        let json = layout_to_json_with(
            &default_layout(),
            vec![],
            vec![],
            vec![],
            vec![],
            Some(g.network_layout()),
        );
        let layout = network_from_json(json.as_ref().unwrap()).unwrap();
        let mut g2 = Graph::from_topology(&g.to_topology());
        g2.apply_layout(&layout);
        assert_eq!(g2.view(), NetworkView::FreeForm);
        let at = |g: &Graph, name: &str| {
            g.state
                .nodes
                .iter()
                .find(|n| n.data.name() == name)
                .unwrap()
                .position
        };
        assert_eq!(at(&g2, "Engine"), egui::pos2(5.0, 6.0));
        g2.set_view(NetworkView::BusLine);
        assert_eq!(at(&g2, "CAN1"), egui::pos2(11.0, 22.0));
    }

    #[test]
    fn invalid_layout_falls_back() {
        assert!(layout_from_json(&serde_json::json!({"nope": 1})).is_none());
        assert!(layout_from_json(&serde_json::json!("x")).is_none());
        assert!(layout_from_json(&serde_json::json!({"version": 99, "dock": null})).is_none());
        let empty = layout_to_json(&DockState::new(vec![])).unwrap();
        assert!(layout_from_json(&empty).is_none());
    }

    #[test]
    fn missing_workspace_is_none() {
        let topo = Topology::from_json("{}").unwrap();
        assert!(topo.workspace.is_none());
    }
}
