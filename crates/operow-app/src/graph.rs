//! The node-graph editor: ECU and CAN-bus nodes wired together with
//! `egui-flow`, convertible to/from an `operow_core::Topology`.

use std::collections::{HashMap, HashSet};

use egui::{Color32, CornerRadius, Frame, Margin, Pos2, Stroke, pos2, vec2};
use egui_flow::{
    ArrowStyle, Connection, Edge, EdgeId, EdgeKind, EdgeLabelStyle, Editor, FlowEvent, FlowState,
    FlowViewer, Handle, LayoutDirection, LayoutOptions, LineStyle, Node, NodeId as FlowId,
    PulseOverflow, PulseStyle, Side,
};

use operow_core::{
    BusEvent, BusId, CanBusConfig, DbcRef, Domain, EcuConfig, Link, NodeId, NodeKind, Topology,
    TxMessage, UserSignalDef, WireArrow, WireKind, WireLine, WireOverride, WireStyle,
};
use operow_engine::GENERATOR_NODE_BASE;

use crate::icons;
use crate::network_view::{
    self, BUS_DEFAULT_WIDTH, BUS_HEIGHT, BUS_MAX_WIDTH, BUS_MIN_WIDTH, EdgeLook, HandlePlan,
    NetworkLayout, NetworkView, NodeKey, Place,
};
use crate::runtime::NodeBadge;
use crate::theme::AppTheme;

/// Most pulses in flight on one wire; the oldest is replaced beyond this.
const MAX_PULSES_PER_EDGE: usize = 16;
/// Padding and header height of a domain group around its members.
const DOMAIN_PADDING: f32 = 16.0;
const DOMAIN_HEADER: f32 = 30.0;
/// Size of a domain group until it is fitted to its members.
const DOMAIN_SIZE: egui::Vec2 = vec2(220.0, 120.0);
/// Seconds the free-form auto layout glides.
const LAYOUT_SECONDS: f32 = 0.4;
/// Smallest node size, as `egui-flow` defaults it.
const MIN_NODE_SIZE: egui::Vec2 = vec2(60.0, 30.0);

/// A node placed on the canvas: either a simulated ECU or a CAN bus.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // few nodes; boxing would touch every match
pub enum GraphNode {
    Ecu(EcuConfig),
    Bus(CanBusConfig),
    /// A network domain: an `egui-flow` group. Its members are the nodes
    /// whose flow parent it is.
    Domain(DomainNode),
}

/// Canvas data of a network domain.
#[derive(Debug, Clone)]
pub struct DomainNode {
    pub id: u32,
    pub name: String,
    pub color: Option<[u8; 3]>,
}

impl GraphNode {
    pub fn name(&self) -> &str {
        match self {
            GraphNode::Ecu(e) => &e.name,
            GraphNode::Bus(b) => &b.name,
            GraphNode::Domain(d) => &d.name,
        }
    }

    /// The simulation id, which layouts are saved under.
    pub fn key(&self) -> NodeKey {
        match self {
            GraphNode::Ecu(e) => NodeKey::Ecu(e.id.0),
            GraphNode::Bus(b) => NodeKey::Bus(b.id.0),
            GraphNode::Domain(d) => NodeKey::Domain(d.id),
        }
    }
}

/// What a dragged wire end did to the topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconnect {
    /// The link moved; `stale` counts routes and messages of the node that
    /// still name the bus it left (flagged by the route validation).
    Moved {
        node: String,
        from: String,
        to: String,
        stale: usize,
    },
    /// Only the attachment point changed, the link is the same.
    Unchanged,
    /// The new connection is not allowed; the wire went back.
    Reverted(String),
}

impl Reconnect {
    /// Status line for the log, if there is something to say.
    pub fn message(&self) -> Option<String> {
        match self {
            Reconnect::Moved {
                node,
                from,
                to,
                stale,
            } => {
                let mut m = format!("{node}: link moved from {from} to {to}");
                if *stale > 0 {
                    m.push_str(&format!(
                        "; {stale} route(s)/message(s) still use {from} (see Properties)"
                    ));
                }
                Some(m)
            }
            Reconnect::Unchanged => None,
            Reconnect::Reverted(why) => Some(why.clone()),
        }
    }
}

/// Owns the flow graph plus the id counters needed to allocate fresh
/// `NodeId`/`BusId` values for new nodes. The selection lives in the flow
/// state, so the properties inspector reads it from there.
pub struct Graph {
    pub state: FlowState<GraphNode, ()>,
    next_node_id: u32,
    next_bus_id: u32,
    /// DBC files referenced by the project; round-tripped through the
    /// topology but not shown on the canvas.
    pub databases: Vec<DbcRef>,
    /// User-defined signals; round-tripped through the topology.
    pub user_signals: Vec<UserSignalDef>,
    /// Test modules of the project; round-tripped through the topology.
    pub tests: Vec<String>,
    /// Which layout the canvas currently shows.
    view: NetworkView,
    /// Saved positions of the free-form layout. While that view is active
    /// the canvas holds the live positions and this is refreshed on demand.
    free: HashMap<NodeKey, Place>,
    /// Saved positions and bar sizes of the bus-line layout.
    line: HashMap<NodeKey, Place>,
    next_domain_id: u32,
    /// Buses that belong to a domain while the bus-line view shows (bars
    /// stay outside groups there); the canvas holds them in free-form.
    bus_domain: Vec<(FlowId, FlowId)>,
    /// Style of every wire without an override of its own.
    pub wire_default: Option<WireStyle>,
    /// Per-wire overrides, by the simulation ids of the link.
    pub wire_styles: HashMap<(NodeId, BusId), WireStyle>,
    /// Undo/redo history and clipboard of the canvas.
    pub editor: Editor<GraphNode, ()>,
}

impl Graph {
    pub fn new() -> Self {
        let mut state = FlowState::new();
        state.max_pulses_per_edge = MAX_PULSES_PER_EDGE;
        state.pulse_overflow = PulseOverflow::ReplaceOldest;
        Graph {
            editor: Editor::new(&state),
            state,
            next_node_id: 1,
            next_bus_id: 1,
            databases: Vec::new(),
            user_signals: Vec::new(),
            tests: Vec::new(),
            view: NetworkView::default(),
            free: HashMap::new(),
            line: HashMap::new(),
            next_domain_id: 1,
            bus_domain: Vec::new(),
            wire_default: None,
            wire_styles: HashMap::new(),
        }
    }

    /// Build the default preloaded topology: one CAN FD bus (500k/2M) with
    /// three ECUs.
    pub fn default_demo() -> Self {
        let mut g = Graph::new();
        // Designed with free-form positions; bus-line is laid out below.
        g.view = NetworkView::FreeForm;
        let bus = g.add_bus(Pos2::new(160.0, 260.0));
        if let Some(GraphNode::Bus(b)) = g.node_mut(bus) {
            b.fd_enabled = true;
            b.data_bitrate = 2_000_000;
        }

        let engine = g.add_ecu(Pos2::new(60.0, 60.0), "Engine");
        if let Some(GraphNode::Ecu(e)) = g.node_mut(engine) {
            e.tx.push(tx(
                "EngineRPM",
                0x100,
                10,
                &[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0],
            ));
            e.tx.push(tx("EngineTemp", 0x101, 100, &[0x5A, 0x01]));
        }

        let brake = g.add_ecu(Pos2::new(280.0, 60.0), "Brake");
        if let Some(GraphNode::Ecu(e)) = g.node_mut(brake) {
            e.tx.push(tx("BrakeStatus", 0x200, 20, &[0x01, 0x02, 0x03, 0x04]));
        }

        let gateway = g.add_ecu(Pos2::new(500.0, 60.0), "Gateway");
        if let Some(GraphNode::Ecu(e)) = g.node_mut(gateway) {
            e.tx.push(tx("GatewayHeartbeat", 0x300, 50, &[0xAA]));
            let fd_data: Vec<u8> = (0..64).collect();
            e.tx.push(operow_core::TxMessage {
                name: "GwDiagFD".to_string(),
                frame: operow_core::CanFrame::new_fd(0x400, false, true, &fd_data).unwrap(),
                period_ms: 100,
                enabled: true,
                bus: None,
                send_type: Default::default(),
            });
        }

        for ecu in [engine, brake, gateway] {
            g.state.connect(ecu, bus, ());
        }
        g.capture_active();
        g.view = NetworkView::BusLine;
        g.apply_active(Vec::new());
        g.state.fit_view();
        g.reset_history();
        g
    }

    pub fn add_ecu(&mut self, pos: Pos2, name: &str) -> FlowId {
        let flow = self.insert_ecu(pos, name);
        self.editor.commit(&self.state);
        flow
    }

    fn insert_ecu(&mut self, pos: Pos2, name: &str) -> FlowId {
        let id = NodeId(self.next_node_id);
        self.next_node_id += 1;
        self.state.add_node(
            pos,
            GraphNode::Ecu(EcuConfig {
                id,
                name: name.to_string(),
                tx: Vec::new(),
                kind: Default::default(),
                pos: (pos.x, pos.y),
                script: None,
                diag: None,
            }),
        )
    }

    /// Add a gateway node (an ECU-like node with an empty route table).
    pub fn add_gateway(&mut self, pos: Pos2) -> FlowId {
        let name = format!("Gateway {}", self.next_node_id);
        let id = self.insert_ecu(pos, &name);
        if let Some(GraphNode::Ecu(e)) = self.node_mut(id) {
            e.kind = NodeKind::Gateway { routes: vec![] };
        }
        self.editor.commit(&self.state);
        id
    }

    /// Add a Replay node (no log file and no channel mapping yet).
    pub fn add_replay(&mut self, pos: Pos2) -> FlowId {
        let name = format!("Replay {}", self.next_node_id);
        let id = self.insert_ecu(pos, &name);
        if let Some(GraphNode::Ecu(e)) = self.node_mut(id) {
            e.kind = NodeKind::new_replay();
        }
        self.editor.commit(&self.state);
        id
    }

    /// Make the wires of Replay node `id` match its channel mapping: one
    /// wire to every mapped bus, none to other buses.
    pub fn sync_replay_links(&mut self, id: FlowId) {
        let Some(GraphNode::Ecu(e)) = self.node(id) else {
            return;
        };
        let NodeKind::Replay { channel_map, .. } = &e.kind else {
            return;
        };
        let wanted: HashSet<BusId> = channel_map.iter().map(|(_, b)| *b).collect();
        let bus_flow = |g: &Graph, bus: BusId| {
            g.state
                .nodes
                .iter()
                .find(|n| matches!(&n.data, GraphNode::Bus(b) if b.id == bus))
                .map(|n| n.id)
        };
        let mut have = HashSet::new();
        let mut drop_edges = Vec::new();
        for edge in &self.state.edges {
            if edge.source != id {
                continue;
            }
            match self.node(edge.target) {
                Some(GraphNode::Bus(b)) if wanted.contains(&b.id) && have.insert(b.id) => {}
                _ => drop_edges.push(edge.id),
            }
        }
        let mut changed = !drop_edges.is_empty();
        for edge in drop_edges {
            self.state.remove_edge(edge);
        }
        for bus in wanted.difference(&have) {
            if let Some(target) = bus_flow(self, *bus) {
                self.state.connect(id, target, ());
                changed = true;
            }
        }
        if changed {
            self.editor.commit(&self.state);
        }
    }

    pub fn add_bus(&mut self, pos: Pos2) -> FlowId {
        let id = BusId(self.next_bus_id);
        self.next_bus_id += 1;
        let name = format!("CAN{}", self.next_bus_id - 1);
        let flow = self.state.add_node(
            pos,
            GraphNode::Bus(CanBusConfig {
                id,
                name,
                bitrate: 500_000,
                fd_enabled: false,
                data_bitrate: 2_000_000,
                simulate_ack: false,
                hardware: None,
            }),
        );
        self.editor.commit(&self.state);
        flow
    }

    pub fn node(&self, id: FlowId) -> Option<&GraphNode> {
        self.state.node(id).map(|n| &n.data)
    }

    pub fn node_mut(&mut self, id: FlowId) -> Option<&mut GraphNode> {
        self.state.node_mut(id).map(|n| &mut n.data)
    }

    /// Remove a node and the wires attached to it.
    pub fn remove(&mut self, id: FlowId) {
        if let Some(GraphNode::Bus(b)) = self.node(id) {
            let bus = b.id;
            self.databases.retain(|d| d.bus != bus);
        }
        self.state.remove_node(id);
        self.editor.commit(&self.state);
    }

    /// Make `id` the only selected node.
    pub fn select(&mut self, id: FlowId) {
        self.state.clear_selection();
        if let Some(n) = self.state.node_mut(id) {
            n.selected = true;
        }
    }

    /// The selected node shown in the properties inspector, if any.
    pub fn selected(&self) -> Option<FlowId> {
        self.state.nodes.iter().find(|n| n.selected).map(|n| n.id)
    }

    /// The wire (edge) between each ECU and bus, by simulation ids.
    pub fn wire_map(&self) -> HashMap<(NodeId, BusId), EdgeId> {
        let mut map = HashMap::new();
        for e in &self.state.edges {
            if let (Some(GraphNode::Ecu(n)), Some(GraphNode::Bus(b))) =
                (self.node(e.source), self.node(e.target))
            {
                map.insert((n.id, b.id), e.id);
            }
        }
        map
    }

    /// The links of the current wiring (cheaper than a full topology).
    pub fn links(&self) -> Vec<Link> {
        let mut out: Vec<Link> = self
            .wire_map()
            .keys()
            .map(|&(node, bus)| Link { node, bus })
            .collect();
        out.sort_by_key(|l| (l.bus, l.node));
        out
    }

    /// Send a particle along a wire: ECU to bus, or bus to ECU.
    pub fn pulse_wire(&mut self, edge: EdgeId, dir: PulseDir, style: PulseStyle) {
        match dir {
            PulseDir::ToBus => self.state.pulse_edge(edge, style),
            PulseDir::FromBus => self.state.pulse_edge_reverse(edge, style),
        };
    }

    /// Convert the graph into a `Topology` for the simulation engine.
    pub fn to_topology(&self) -> Topology {
        let mut nodes = Vec::new();
        let mut buses = Vec::new();
        let mut links = Vec::new();

        for node in &self.state.nodes {
            match &node.data {
                GraphNode::Ecu(e) => {
                    let mut cfg = e.clone();
                    // The topology keeps the free-form position, whatever
                    // view is showing.
                    let pos = self.free_position(node);
                    cfg.pos = (pos.x, pos.y);
                    nodes.push(cfg);
                }
                GraphNode::Bus(b) => buses.push(b.clone()),
                GraphNode::Domain(_) => {}
            }
        }

        for edge in &self.state.edges {
            if let (Some(GraphNode::Ecu(e)), Some(GraphNode::Bus(b))) =
                (self.node(edge.source), self.node(edge.target))
            {
                links.push(Link {
                    node: e.id,
                    bus: b.id,
                });
            }
        }

        let mut wires: Vec<WireOverride> = self
            .wire_styles
            .iter()
            .filter(|(k, s)| !s.is_empty() && links.iter().any(|l| (l.node, l.bus) == **k))
            .map(|(&(node, bus), style)| WireOverride {
                node,
                bus,
                style: style.clone(),
            })
            .collect();
        wires.sort_by_key(|w| (w.bus, w.node));

        Topology {
            nodes,
            buses,
            links,
            databases: self.databases.clone(),
            user_signals: self.user_signals.clone(),
            tests: self.tests.clone(),
            workspace: None,
            domains: self.topology_domains(),
            wire_default: self.wire_default.clone().filter(|s| !s.is_empty()),
            wires,
        }
    }

    /// The domains of the canvas, with their members, for the topology.
    fn topology_domains(&self) -> Vec<Domain> {
        let members = self.memberships();
        let mut out = Vec::new();
        for n in &self.state.nodes {
            let GraphNode::Domain(d) = &n.data else {
                continue;
            };
            let mut dom = Domain {
                id: d.id,
                name: d.name.clone(),
                color: d.color,
                members: Vec::new(),
                bus_members: Vec::new(),
                collapsed: n.collapsed,
                parent: n.parent.and_then(|p| self.domain_id(p)),
            };
            for &(child, group) in &members {
                if group != n.id {
                    continue;
                }
                match self.node(child) {
                    Some(GraphNode::Ecu(e)) => dom.members.push(e.id),
                    Some(GraphNode::Bus(b)) => dom.bus_members.push(b.id),
                    _ => {}
                }
            }
            out.push(dom);
        }
        out
    }

    /// Rebuild the graph from a loaded `Topology`. Positions come from the
    /// ECUs' saved free-form positions; use [`Graph::apply_layout`] to restore
    /// what the workspace saved.
    pub fn from_topology(topo: &Topology) -> Self {
        Self::from_topology_keeping(topo, &Graph::new())
    }

    /// Like [`Graph::from_topology`], but nodes that also exist in `old`
    /// keep their place in both layouts (the topology does not store bus
    /// positions) and the view stays as it was.
    pub fn from_topology_keeping(topo: &Topology, old: &Graph) -> Self {
        let mut g = Graph::new();
        g.databases = topo.databases.clone();
        g.user_signals = topo.user_signals.clone();
        g.tests = topo.tests.clone();
        g.wire_default = topo.wire_default.clone();
        g.wire_styles = topo
            .wires
            .iter()
            .map(|w| ((w.node, w.bus), w.style.clone()))
            .collect();
        let mut ecu_map = std::collections::HashMap::new();
        let mut bus_map = std::collections::HashMap::new();

        for ecu in &topo.nodes {
            let pos = Pos2::new(ecu.pos.0, ecu.pos.1);
            let fid = g.state.add_node(pos, GraphNode::Ecu(ecu.clone()));
            g.next_node_id = g.next_node_id.max(ecu.id.0 + 1);
            ecu_map.insert(ecu.id, fid);
        }
        for (i, bus) in topo.buses.iter().enumerate() {
            let fid = g
                .state
                .add_node(default_free_bus_pos(i), GraphNode::Bus(bus.clone()));
            g.next_bus_id = g.next_bus_id.max(bus.id.0 + 1);
            bus_map.insert(bus.id, fid);
        }
        for link in &topo.links {
            if let (Some(&ecu), Some(&bus)) = (ecu_map.get(&link.node), bus_map.get(&link.bus)) {
                g.state.connect(ecu, bus, ());
            }
        }
        let mut dom_map = HashMap::new();
        for d in &topo.domains {
            let node = GraphNode::Domain(DomainNode {
                id: d.id,
                name: d.name.clone(),
                color: d.color,
            });
            dom_map.insert(d.id, g.state.add_group(Pos2::ZERO, DOMAIN_SIZE, node));
            g.next_domain_id = g.next_domain_id.max(d.id + 1);
        }
        for d in &topo.domains {
            let group = dom_map[&d.id];
            for m in &d.members {
                if let Some(&c) = ecu_map.get(m) {
                    g.state.set_parent(c, Some(group));
                }
            }
            // The graph starts in the bus-line view, where buses wait aside.
            for b in &d.bus_members {
                if let Some(&c) = bus_map.get(b) {
                    g.bus_domain.push((c, group));
                }
            }
            if let Some(&p) = d.parent.and_then(|p| dom_map.get(&p)) {
                g.state.set_parent(group, Some(p));
            }
        }
        for d in &topo.domains {
            g.state.set_collapsed(dom_map[&d.id], d.collapsed);
        }
        g.apply_layout(&old.network_layout());
        g
    }

    // ---- network views -------------------------------------------------

    /// The layout the canvas currently shows.
    pub fn view(&self) -> NetworkView {
        self.view
    }

    /// Where the free-form layout puts `node`: the live position while that
    /// view is showing, else the saved one, else the ECU's stored position.
    fn free_position(&self, node: &Node<GraphNode>) -> Pos2 {
        if self.view == NetworkView::FreeForm {
            return self.state.abs_position(node.id).unwrap_or(node.position);
        }
        match self.free.get(&node.data.key()) {
            Some(p) => p.pos(),
            None => match &node.data {
                GraphNode::Ecu(e) => pos2(e.pos.0, e.pos.1),
                _ => node.position,
            },
        }
    }

    /// Where `node` is in flow space, whatever group it sits in.
    fn place_of(&self, node: &Node<GraphNode>) -> Place {
        let pos = self.state.abs_position(node.id).unwrap_or(node.position);
        Place {
            x: pos.x,
            y: pos.y,
            w: node.fixed_size.map(|s| s.x),
            h: node.fixed_size.map(|s| s.y),
        }
    }

    fn live_places(&self) -> HashMap<NodeKey, Place> {
        self.state
            .nodes
            .iter()
            .map(|n| (n.data.key(), self.place_of(n)))
            .collect()
    }

    /// Store the live positions under the active view.
    fn capture_active(&mut self) {
        let live = self.live_places();
        let (active, other) = match self.view {
            NetworkView::FreeForm => (&mut self.free, &mut self.line),
            NetworkView::BusLine => (&mut self.line, &mut self.free),
        };
        other.retain(|k, _| live.contains_key(k));
        *active = live;
    }

    /// Put the nodes where the active view's saved layout says. Nodes it
    /// does not know get a free-form fallback, or are laid out for bus-line.
    /// `members` is the (child, group) membership to restore afterwards;
    /// the layout itself works on flow-space positions, outside any group.
    fn apply_active(&mut self, members: Vec<(FlowId, FlowId)>) {
        self.flatten();
        let view = self.view;
        let saved = match view {
            NetworkView::FreeForm => &self.free,
            NetworkView::BusLine => &self.line,
        };
        let mut missing = Vec::new();
        // Domains without a saved place are fitted to their members below.
        let mut unplaced = Vec::new();
        let mut bus_index = 0;
        for n in &mut self.state.nodes {
            let is_bus = matches!(n.data, GraphNode::Bus(_));
            let index = bus_index;
            bus_index += usize::from(is_bus);
            match saved.get(&n.data.key()) {
                Some(p) => {
                    n.position = p.pos();
                    n.fixed_size = p.size();
                }
                None if matches!(n.data, GraphNode::Domain(_)) => unplaced.push(n.id),
                None if view == NetworkView::FreeForm => {
                    n.position = match &n.data {
                        GraphNode::Ecu(e) => pos2(e.pos.0, e.pos.1),
                        _ => default_free_bus_pos(index),
                    };
                    n.fixed_size = None;
                }
                None => {
                    n.fixed_size = None;
                    missing.push(n.id);
                }
            }
            if let Some(f) = n.fixed_size {
                n.size = f;
            }
        }
        let placeable = self
            .state
            .nodes
            .iter()
            .filter(|n| !matches!(n.data, GraphNode::Domain(_)))
            .count();
        if view == NetworkView::BusLine && !missing.is_empty() {
            if missing.len() == placeable {
                network_view::auto_arrange(&mut self.state);
            } else {
                network_view::place_missing(&mut self.state, &missing);
            }
        }
        self.regroup(&members);
        self.fit_domains(&unplaced);
    }

    /// Every (child, group) pair: from the canvas, plus the buses set aside
    /// while the bus-line view shows.
    fn memberships(&self) -> Vec<(FlowId, FlowId)> {
        let mut v: Vec<_> = self
            .state
            .nodes
            .iter()
            .filter_map(|n| n.parent.map(|p| (n.id, p)))
            .collect();
        if self.view == NetworkView::BusLine {
            v.extend(
                self.bus_domain.iter().filter(|(c, g)| {
                    self.state.node(*c).is_some() && self.state.node(*g).is_some()
                }),
            );
        }
        v
    }

    /// Take every node out of its group, keeping it where it is on screen.
    fn flatten(&mut self) {
        let ids: Vec<FlowId> = self.state.nodes.iter().map(|n| n.id).collect();
        for id in ids {
            self.state.set_parent(id, None);
        }
    }

    /// Put `members` back into their groups. Bus bars stay outside groups in
    /// the bus-line view (a group around a bar would wreck its row layout);
    /// they are remembered for the free-form view.
    fn regroup(&mut self, members: &[(FlowId, FlowId)]) {
        self.bus_domain.clear();
        for &(child, group) in members {
            let is_bus = matches!(self.node(child), Some(GraphNode::Bus(_)));
            if is_bus && self.view == NetworkView::BusLine {
                self.bus_domain.push((child, group));
            } else {
                self.state.set_parent(child, Some(group));
            }
        }
    }

    /// Fit groups to their members, innermost first.
    fn fit_domains(&mut self, groups: &[FlowId]) {
        let mut groups = groups.to_vec();
        groups.sort_by_key(|g| std::cmp::Reverse(self.state.depth(*g)));
        for g in groups {
            self.state.fit_group(g, DOMAIN_PADDING, DOMAIN_HEADER);
        }
    }

    fn domain_nodes(&self) -> Vec<FlowId> {
        self.state
            .nodes
            .iter()
            .filter(|n| matches!(n.data, GraphNode::Domain(_)))
            .map(|n| n.id)
            .collect()
    }

    pub fn domain_name(&self, flow: FlowId) -> Option<&str> {
        match self.node(flow)? {
            GraphNode::Domain(d) => Some(&d.name),
            _ => None,
        }
    }

    fn domain_id(&self, flow: FlowId) -> Option<u32> {
        match self.node(flow)? {
            GraphNode::Domain(d) => Some(d.id),
            _ => None,
        }
    }

    /// Switch layouts. Each keeps its own positions and bar sizes, so going
    /// back and forth never scrambles either. Clears the undo history, whose
    /// steps would otherwise restore the other layout's positions.
    pub fn set_view(&mut self, view: NetworkView) {
        if view == self.view {
            return;
        }
        self.capture_active();
        let members = self.memberships();
        self.view = view;
        self.apply_active(members);
        self.state.fit_view();
        self.reset_history();
    }

    /// Both layouts and the active view, for the workspace file.
    pub fn network_layout(&self) -> NetworkLayout {
        let live = self.live_places();
        let (free, line) = match self.view {
            NetworkView::FreeForm => (live.clone(), self.line.clone()),
            NetworkView::BusLine => (self.free.clone(), live.clone()),
        };
        let sorted = |m: HashMap<NodeKey, Place>| {
            let mut v: Vec<(NodeKey, Place)> = m
                .into_iter()
                .filter(|(k, _)| live.contains_key(k))
                .collect();
            v.sort_by_key(|(k, _)| *k);
            v
        };
        NetworkLayout {
            mode: self.view,
            free: sorted(free),
            line: sorted(line),
        }
    }

    /// Restore a saved layout: both position sets and the active view.
    pub fn apply_layout(&mut self, layout: &NetworkLayout) {
        let members = self.memberships();
        self.free = layout.free.iter().copied().collect();
        self.line = layout.line.iter().copied().collect();
        self.view = layout.mode;
        self.apply_active(members);
        self.state.fit_view();
        self.reset_history();
    }

    /// Lay the bus-line view out: buses stacked, ECUs above their bus,
    /// gateways between the buses they bridge.
    pub fn auto_arrange(&mut self) {
        let members = self.memberships();
        self.flatten();
        network_view::auto_arrange(&mut self.state);
        self.regroup(&members);
        let groups = self.domain_nodes();
        self.fit_domains(&groups);
        self.state.fit_view();
        self.editor.commit(&self.state);
    }

    /// Free-form view: arrange the nodes top to bottom along their wires,
    /// gliding there. The canvas sends `LayoutFinished` when it arrives.
    pub fn auto_layout(&mut self, animate: bool) {
        let options = LayoutOptions {
            direction: LayoutDirection::TopToBottom,
            ..Default::default()
        };
        if animate {
            self.state.auto_layout_animated(&options, LAYOUT_SECONDS);
        } else {
            self.state.auto_layout(&options);
            self.state.fit_view();
        }
    }

    /// Forget undo history (after the canvas content was replaced).
    pub fn reset_history(&mut self) {
        self.editor = Editor::new(&self.state);
    }

    // ---- domains ---------------------------------------------------------

    /// Every domain on the canvas, with the number of nodes it holds directly.
    pub fn domains(&self) -> Vec<(FlowId, &DomainNode)> {
        self.state
            .nodes
            .iter()
            .filter_map(|n| match &n.data {
                GraphNode::Domain(d) => Some((n.id, d)),
                _ => None,
            })
            .collect()
    }

    /// Direct members of a domain (nodes and nested domains), in canvas order.
    pub fn domain_members(&self, group: FlowId) -> Vec<FlowId> {
        self.memberships()
            .into_iter()
            .filter(|m| m.1 == group)
            .map(|m| m.0)
            .collect()
    }

    fn new_domain(&mut self, name: &str) -> DomainNode {
        let id = self.next_domain_id;
        self.next_domain_id += 1;
        DomainNode {
            id,
            name: name.to_string(),
            color: None,
        }
    }

    /// Wrap the selected nodes in a new domain. Bus bars are left out in the
    /// bus-line view. `None` when nothing suitable is selected.
    pub fn group_selection(&mut self, name: &str) -> Option<FlowId> {
        if self.view == NetworkView::BusLine {
            for n in &mut self.state.nodes {
                n.selected &= !matches!(n.data, GraphNode::Bus(_));
            }
        }
        let data = self.new_domain(name);
        let group =
            self.state
                .group_selected(GraphNode::Domain(data), DOMAIN_PADDING, DOMAIN_HEADER);
        if group.is_some() {
            self.editor.commit(&self.state);
        } else {
            self.next_domain_id -= 1;
        }
        group
    }

    /// Take `id` out of its domain, or dissolve it if it is one (its
    /// members stay where they are).
    pub fn ungroup(&mut self, id: FlowId) {
        let done = if self.domain_id(id).is_some() {
            self.state.ungroup(id)
        } else {
            self.state.node(id).is_some_and(|n| n.parent.is_some())
                && self.state.set_parent(id, None)
        };
        if done {
            self.editor.commit(&self.state);
        }
    }

    pub fn ungroup_selection(&mut self) {
        let selected = self.state.selected_nodes();
        for id in selected {
            self.ungroup(id);
        }
    }

    pub fn rename_domain(&mut self, group: FlowId, name: &str) {
        if let Some(GraphNode::Domain(d)) = self.node_mut(group) {
            d.name = name.to_string();
            self.editor.commit(&self.state);
        }
    }

    /// Nodes attached to exactly one bus, per bus, that are not in a domain
    /// yet: `(bus name, members)`. Gateways stay out.
    fn per_bus_groups(&self) -> Vec<(String, Vec<FlowId>)> {
        let mut out = Vec::new();
        for bus in &self.state.nodes {
            let GraphNode::Bus(b) = &bus.data else {
                continue;
            };
            let members: Vec<FlowId> = self
                .state
                .nodes
                .iter()
                .filter(|n| {
                    n.parent.is_none()
                        && matches!(&n.data, GraphNode::Ecu(e)
                            if !matches!(e.kind, NodeKind::Gateway { .. }))
                })
                .filter(|n| {
                    let mut targets = self.state.edges.iter().filter(|e| e.source == n.id);
                    targets.next().is_some_and(|e| e.target == bus.id)
                        && targets.all(|e| e.target == bus.id)
                })
                .map(|n| n.id)
                .collect();
            if !members.is_empty() {
                out.push((b.name.clone(), members));
            }
        }
        out
    }

    /// One domain per bus holding the nodes attached only to that bus.
    /// Returns how many were made.
    pub fn create_domains_per_bus(&mut self) -> usize {
        let groups = self.per_bus_groups();
        for (name, members) in &groups {
            let data = self.new_domain(name);
            let group = self
                .state
                .add_group(Pos2::ZERO, DOMAIN_SIZE, GraphNode::Domain(data));
            for m in members {
                self.state.set_parent(*m, Some(group));
            }
            self.state.fit_group(group, DOMAIN_PADDING, DOMAIN_HEADER);
        }
        if !groups.is_empty() {
            self.editor.commit(&self.state);
        }
        groups.len()
    }

    // ---- wire styles -----------------------------------------------------

    /// The link an edge stands for.
    pub fn wire_key(&self, edge: EdgeId) -> Option<(NodeId, BusId)> {
        let e = self.state.edge(edge)?;
        match (self.node(e.source)?, self.node(e.target)?) {
            (GraphNode::Ecu(n), GraphNode::Bus(b)) => Some((n.id, b.id)),
            _ => None,
        }
    }

    /// Store `style` as the override of a wire (an empty one removes it).
    pub fn set_wire_style(&mut self, key: (NodeId, BusId), style: WireStyle) {
        if style.is_empty() {
            self.wire_styles.remove(&key);
        } else {
            self.wire_styles.insert(key, style);
        }
    }

    // ---- per-frame upkeep ----------------------------------------------

    /// Get the canvas ready for a frame: unique ids after a paste, view
    /// constraints on buses, the handle of every wire end and the look of
    /// every wire. Returns the handles for the viewer, and whether a
    /// database reference had to be dropped (a bus went away).
    pub fn prepare(&mut self, theme: AppTheme) -> (HandlePlan, bool) {
        self.fix_duplicate_ids();
        let buses: HashSet<BusId> = self
            .state
            .nodes
            .iter()
            .filter_map(|n| match &n.data {
                GraphNode::Bus(b) => Some(b.id),
                _ => None,
            })
            .collect();
        let before = self.databases.len();
        self.databases.retain(|d| buses.contains(&d.bus));
        let pruned = self.databases.len() != before;

        let view = self.view;
        for n in &mut self.state.nodes {
            if !matches!(n.data, GraphNode::Bus(_)) {
                continue;
            }
            match view {
                NetworkView::BusLine => {
                    n.min_size = vec2(BUS_MIN_WIDTH, BUS_HEIGHT);
                    n.max_size = Some(vec2(BUS_MAX_WIDTH, BUS_HEIGHT));
                    let f = n
                        .fixed_size
                        .get_or_insert(vec2(BUS_DEFAULT_WIDTH, BUS_HEIGHT));
                    *f = vec2(f.x.clamp(BUS_MIN_WIDTH, BUS_MAX_WIDTH), BUS_HEIGHT);
                }
                NetworkView::FreeForm => {
                    n.min_size = MIN_NODE_SIZE;
                    n.max_size = None;
                }
            }
        }

        let plan = match view {
            NetworkView::BusLine => network_view::plan_bus_line(&self.state),
            NetworkView::FreeForm => HandlePlan::default(),
        };
        let looks: Vec<(EdgeId, EdgeLook, (NodeId, BusId))> = self
            .state
            .edges
            .iter()
            .filter_map(|e| {
                let (GraphNode::Ecu(ecu), GraphNode::Bus(bus)) =
                    (self.node(e.source)?, self.node(e.target)?)
                else {
                    return None;
                };
                Some((e.id, network_view::edge_look(ecu, bus.id), (ecu.id, bus.id)))
            })
            .collect();
        for (id, look, key) in looks {
            let handles = plan
                .edges
                .get(&id)
                .copied()
                .unwrap_or((Handle::DEFAULT_SOURCE, Handle::DEFAULT_TARGET));
            let Some(e) = self.state.edge_mut(id) else {
                continue;
            };
            e.source_handle = handles.0;
            e.target_handle = handles.1;
            e.kind = (view == NetworkView::BusLine).then_some(EdgeKind::Step);
            e.line_style = if look.dashed {
                LineStyle::Dashed
            } else {
                LineStyle::Solid
            };
            e.arrow = look.arrow_to_bus;
            e.arrow_at_source = look.arrow_to_ecu;
            e.arrow_style = ArrowStyle::Triangle;
            e.color = look.gateway.then(|| theme.gateway_color());
            e.width = look.gateway.then_some(2.0);
            if e.label != look.label {
                e.label = look.label;
            }
            e.label_style = EdgeLabelStyle {
                position: 0.5,
                size: 10.0,
                color: Some(theme.gateway_color()),
                background: None,
            };
            if let Some(style) =
                resolved_wire_style(self.wire_styles.get(&key), self.wire_default.as_ref())
            {
                apply_wire_style(e, &style);
            }
        }
        (plan, pruned)
    }

    /// Give pasted copies fresh ids. A node whose NodeId/BusId an earlier
    /// node already has is a copy: it gets new ids, the name "X (copy)", and
    /// where a copied gateway or message names a copied bus it linked to,
    /// that bus's new id. Returns whether anything changed.
    pub fn fix_duplicate_ids(&mut self) -> bool {
        let mut seen_ecu = HashSet::new();
        let mut seen_bus = HashSet::new();
        let mut seen_domain = HashSet::new();
        let mut domain_dups = Vec::new();
        let mut ecu_dups = Vec::new();
        let mut bus_dups = Vec::new();
        for (i, n) in self.state.nodes.iter().enumerate() {
            match &n.data {
                GraphNode::Ecu(e) if !seen_ecu.insert(e.id) => ecu_dups.push(i),
                GraphNode::Bus(b) if !seen_bus.insert(b.id) => bus_dups.push(i),
                GraphNode::Domain(d) if !seen_domain.insert(d.id) => domain_dups.push(i),
                _ => {}
            }
        }
        if ecu_dups.is_empty() && bus_dups.is_empty() && domain_dups.is_empty() {
            return false;
        }
        let mut names: HashSet<String> = self
            .state
            .nodes
            .iter()
            .map(|n| n.data.name().to_string())
            .collect();
        for i in domain_dups {
            let new = self.next_domain_id;
            self.next_domain_id += 1;
            if let GraphNode::Domain(d) = &mut self.state.nodes[i].data {
                d.id = new;
                d.name = copy_name(&d.name, &names);
                names.insert(d.name.clone());
            }
        }
        // New bus node -> (id it was copied with, id it has now).
        let mut bus_map: HashMap<FlowId, (BusId, BusId)> = HashMap::new();
        for i in bus_dups {
            let new = BusId(self.next_bus_id);
            self.next_bus_id += 1;
            let flow = self.state.nodes[i].id;
            if let GraphNode::Bus(b) = &mut self.state.nodes[i].data {
                let old = b.id;
                b.id = new;
                b.name = copy_name(&b.name, &names);
                names.insert(b.name.clone());
                bus_map.insert(flow, (old, new));
            }
        }
        for i in ecu_dups {
            let new = NodeId(self.next_node_id);
            self.next_node_id += 1;
            let flow = self.state.nodes[i].id;
            let remap: HashMap<BusId, BusId> = self
                .state
                .edges
                .iter()
                .filter(|e| e.source == flow)
                .filter_map(|e| bus_map.get(&e.target).copied())
                .collect();
            if let GraphNode::Ecu(cfg) = &mut self.state.nodes[i].data {
                cfg.id = new;
                cfg.name = copy_name(&cfg.name, &names);
                names.insert(cfg.name.clone());
                remap_buses(cfg, &remap);
            }
        }
        true
    }

    // ---- editing -------------------------------------------------------

    fn snapshot_data(&self) -> HashMap<FlowId, GraphNode> {
        self.state
            .nodes
            .iter()
            .map(|n| (n.id, n.data.clone()))
            .collect()
    }

    /// Undo and redo restore node data from the last history step, which
    /// would revert Properties edits made since; keep the current data of
    /// every node that survives.
    fn restore_data(&mut self, kept: HashMap<FlowId, GraphNode>) {
        for n in &mut self.state.nodes {
            if let Some(data) = kept.get(&n.id) {
                n.data = data.clone();
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        self.editor.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.editor.can_redo()
    }

    pub fn undo(&mut self) -> bool {
        let kept = self.snapshot_data();
        let done = self.editor.undo(&mut self.state);
        self.restore_data(kept);
        self.fix_duplicate_ids();
        done
    }

    pub fn redo(&mut self) -> bool {
        let kept = self.snapshot_data();
        let done = self.editor.redo(&mut self.state);
        self.restore_data(kept);
        self.fix_duplicate_ids();
        done
    }

    /// Whether any node is selected (so copy and duplicate have something to do).
    pub fn has_selection(&self) -> bool {
        self.state.nodes.iter().any(|n| n.selected)
    }

    pub fn copy(&mut self) -> bool {
        self.editor.copy(&self.state)
    }

    /// Paste the clipboard as new nodes with fresh ids.
    pub fn paste(&mut self) -> bool {
        let done = self.editor.paste(&mut self.state);
        self.fix_duplicate_ids();
        done
    }

    /// Copy the selection and paste it right away.
    pub fn duplicate(&mut self) -> bool {
        let copies = self.state.duplicate_selected(self.editor.paste_offset);
        if copies.is_empty() {
            return false;
        }
        self.fix_duplicate_ids();
        self.editor.commit(&self.state);
        true
    }

    /// The simulation ids of ECU `a` and bus `b` when `edge` runs ECU -> bus.
    fn ecu_bus_of(&self, source: FlowId, target: FlowId) -> Option<(&EcuConfig, &CanBusConfig)> {
        match (self.node(source)?, self.node(target)?) {
            (GraphNode::Ecu(e), GraphNode::Bus(b)) => Some((e, b)),
            _ => None,
        }
    }

    /// Another wire already joins this ECU and bus.
    fn already_linked(&self, edge: EdgeId, source: FlowId, target: FlowId) -> bool {
        self.state
            .edges
            .iter()
            .any(|e| e.id != edge && e.source == source && e.target == target)
    }

    /// A wire was drawn: drop it again if the ECU is already on that bus.
    /// Returns whether it stays.
    pub fn accept_connection(&mut self, edge: EdgeId) -> bool {
        let Some(e) = self.state.edge(edge) else {
            return false;
        };
        if self.already_linked(edge, e.source, e.target) {
            self.state.remove_edge(edge);
            return false;
        }
        true
    }

    /// A wire end was dragged to another handle; the edge already carries
    /// the new ends and the topology links follow from the edges. Reverts
    /// the move if the result is not an ECU -> bus link, or it duplicates
    /// one. Routes that name the bus the node left are left alone, so the
    /// route validation flags them.
    pub fn on_reconnected(&mut self, edge: EdgeId, old: Connection, new: Connection) -> Reconnect {
        let revert = |g: &mut Graph, why: String| {
            if let Some(e) = g.state.edge_mut(edge) {
                e.source = old.source;
                e.source_handle = old.source_handle;
                e.target = old.target;
                e.target_handle = old.target_handle;
            }
            Reconnect::Reverted(why)
        };
        let Some((new_ecu, new_bus)) = self.ecu_bus_of(new.source, new.target) else {
            return revert(self, "a wire must join an ECU to a bus".into());
        };
        let (new_ecu_name, new_bus_name) = (new_ecu.name.clone(), new_bus.name.clone());
        if self.already_linked(edge, new.source, new.target) {
            return revert(
                self,
                format!("{new_ecu_name} is already linked to {new_bus_name}"),
            );
        }
        if (old.source, old.target) == (new.source, new.target) {
            return Reconnect::Unchanged;
        }
        let Some((old_ecu, old_bus)) = self.ecu_bus_of(old.source, old.target) else {
            return Reconnect::Unchanged;
        };
        let (old_bus_name, old_bus_id) = (old_bus.name.clone(), old_bus.id);
        let stale = stale_refs(old_ecu, old_bus_id);
        Reconnect::Moved {
            node: old_ecu.name.clone(),
            from: old_bus_name,
            to: new_bus_name,
            stale,
        }
    }

    /// Apply a frame's canvas events: keep the topology consistent with
    /// wires the user drew or moved, then record history and run the
    /// undo/redo/copy/paste shortcuts. Returns log lines to show.
    pub fn process_events(&mut self, events: &[FlowEvent<GraphNode, ()>]) -> Vec<String> {
        let mut log = Vec::new();
        let mut kept = Vec::with_capacity(events.len());
        for ev in events {
            match ev {
                FlowEvent::Connected(id) => {
                    if !self.accept_connection(*id) {
                        log.push("already linked to that bus".to_string());
                        continue;
                    }
                }
                FlowEvent::Reconnected { edge, old, new } => {
                    let outcome = self.on_reconnected(*edge, *old, *new);
                    log.extend(outcome.message());
                    if matches!(outcome, Reconnect::Reverted(_) | Reconnect::Unchanged) {
                        continue;
                    }
                }
                FlowEvent::ParentChanged {
                    node,
                    parent: Some(_),
                } if self.view == NetworkView::BusLine
                    && matches!(self.node(*node), Some(GraphNode::Bus(_))) =>
                {
                    // Bars stay outside domains in the bus-line view.
                    self.state.set_parent(*node, None);
                    log.push("bus bars cannot join a domain in the bus-line view".to_string());
                    continue;
                }
                FlowEvent::LayoutFinished => self.capture_active(),
                _ => {}
            }
            kept.push(ev.clone());
        }
        let history = kept
            .iter()
            .any(|e| matches!(e, FlowEvent::UndoRequested | FlowEvent::RedoRequested));
        let data = history.then(|| self.snapshot_data());
        self.editor.process(&mut self.state, &kept);
        if let Some(data) = data {
            self.restore_data(data);
        }
        log
    }
}

/// The style a wire gets from its override and the project default, `None`
/// when neither sets anything (the automatic look stays).
pub fn resolved_wire_style(
    own: Option<&WireStyle>,
    default: Option<&WireStyle>,
) -> Option<WireStyle> {
    let style = match (own, default) {
        (Some(o), Some(d)) => o.over(d),
        (Some(s), None) | (None, Some(s)) => s.clone(),
        (None, None) => return None,
    };
    (!style.is_empty()).then_some(style)
}

/// Write the fields `style` sets into `edge`, leaving the rest as they are.
pub fn apply_wire_style(edge: &mut Edge<()>, style: &WireStyle) {
    if let Some(k) = style.kind {
        edge.kind = Some(match k {
            WireKind::Bezier => EdgeKind::Bezier,
            WireKind::Straight => EdgeKind::Straight,
            WireKind::Step => EdgeKind::Step,
            WireKind::SmoothStep => EdgeKind::SmoothStep,
        });
    }
    if let Some(l) = style.line {
        edge.line_style = match l {
            WireLine::Solid => LineStyle::Solid,
            WireLine::Dashed => LineStyle::Dashed,
            WireLine::Dotted => LineStyle::Dotted,
        };
    }
    if let Some([r, g, b]) = style.color {
        edge.color = Some(Color32::from_rgb(r, g, b));
    }
    if let Some(w) = style.width {
        edge.width = Some(w);
    }
    if let Some(a) = style.arrow {
        edge.arrow = a != WireArrow::None;
        edge.arrow_style = match a {
            WireArrow::Open => ArrowStyle::Open,
            WireArrow::Circle => ArrowStyle::Circle,
            WireArrow::Diamond => ArrowStyle::Diamond,
            WireArrow::None | WireArrow::Triangle => ArrowStyle::Triangle,
        };
    }
    if let Some(a) = style.arrow_at_source {
        edge.arrow_at_source = a;
    }
    if let Some(a) = style.animated {
        edge.animated = a;
    }
    if let Some(l) = &style.label {
        edge.label = Some(l.clone()).filter(|l| !l.is_empty());
    }
}

/// Where the free-form view puts the `i`-th bus when nothing is saved.
fn default_free_bus_pos(i: usize) -> Pos2 {
    Pos2::new(160.0, 260.0 + 160.0 * i as f32)
}

/// `Engine` -> `Engine (copy)`, then `Engine (copy 2)` and so on, skipping
/// names in `taken`. A name that already ends in a copy suffix is not
/// extended further.
pub fn copy_name(name: &str, taken: &HashSet<String>) -> String {
    let base = match name.rfind(" (copy") {
        Some(i) if name.ends_with(')') => &name[..i],
        _ => name,
    };
    let first = format!("{base} (copy)");
    if !taken.contains(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{base} (copy {n})"))
        .find(|c| !taken.contains(c))
        .unwrap_or(first)
}

/// Replace bus ids in a node's messages and routes.
fn remap_buses(cfg: &mut EcuConfig, remap: &HashMap<BusId, BusId>) {
    if remap.is_empty() {
        return;
    }
    let map = |b: BusId| remap.get(&b).copied().unwrap_or(b);
    for m in &mut cfg.tx {
        m.bus = m.bus.map(map);
    }
    match &mut cfg.kind {
        NodeKind::Gateway { routes } => {
            for r in routes {
                r.from_bus = map(r.from_bus);
                for t in &mut r.to_buses {
                    *t = map(*t);
                }
            }
        }
        NodeKind::Replay { channel_map, .. } => {
            for (_, b) in channel_map {
                *b = map(*b);
            }
        }
        NodeKind::Ecu => {}
    }
}

/// Routes and messages of `ecu` that name `bus`.
fn stale_refs(ecu: &EcuConfig, bus: BusId) -> usize {
    let msgs = ecu.tx.iter().filter(|m| m.bus == Some(bus)).count();
    let routes = match &ecu.kind {
        NodeKind::Gateway { routes } => routes
            .iter()
            .filter(|r| r.from_bus == bus || r.to_buses.contains(&bus))
            .count(),
        NodeKind::Replay { channel_map, .. } => {
            channel_map.iter().filter(|(_, b)| *b == bus).count()
        }
        NodeKind::Ecu => 0,
    };
    msgs + routes
}

fn tx(name: &str, id: u32, period_ms: u32, data: &[u8]) -> TxMessage {
    TxMessage {
        name: name.to_string(),
        frame: operow_core::CanFrame::new(id, false, data).unwrap(),
        period_ms,
        enabled: true,
        bus: None,
        send_type: Default::default(),
    }
}

/// Renders ECU and bus nodes. In free-form view every node has one handle
/// (ECUs on the bottom edge, buses on the top) and the only wires that can
/// be drawn are ECU -> bus; in bus-line view the handles come from the
/// [`HandlePlan`] and buses are bars with the load inside.
pub struct GraphViewer {
    pub theme: AppTheme,
    view: NetworkView,
    plan: HandlePlan,
    /// Bus load in percent, shown inside the bars.
    loads: HashMap<BusId, f64>,
    /// ECU/bus pairs that already have a wire.
    wired: HashSet<(FlowId, FlowId)>,
    /// Live state badge of every node (empty when not running).
    badges: HashMap<NodeId, NodeBadge>,
    /// CAN error frames seen per bus, shown in the bus label when above 0.
    bus_errors: HashMap<BusId, u64>,
}

impl GraphViewer {
    pub fn new(
        theme: AppTheme,
        graph: &Graph,
        plan: HandlePlan,
        loads: HashMap<BusId, f64>,
    ) -> Self {
        GraphViewer {
            theme,
            view: graph.view(),
            plan,
            loads,
            wired: graph
                .state
                .edges
                .iter()
                .map(|e| (e.source, e.target))
                .collect(),
            badges: HashMap::new(),
            bus_errors: HashMap::new(),
        }
    }

    /// Show node state badges and per-bus error counts.
    pub fn with_runtime(
        mut self,
        badges: HashMap<NodeId, NodeBadge>,
        bus_errors: HashMap<BusId, u64>,
    ) -> Self {
        self.badges = badges;
        self.bus_errors = bus_errors;
        self
    }

    fn badge_of(&self, data: &GraphNode) -> Option<NodeBadge> {
        match data {
            GraphNode::Ecu(e) => self.badges.get(&e.id).copied(),
            GraphNode::Bus(_) | GraphNode::Domain(_) => None,
        }
    }
}

/// Text inside a bus bar: `CAN1 · 500k · 12.3%`, with ` · HW can0` (or
/// ` · HW can0 (listen)`) after the name for a bus bound to hardware and ` · 20 err` appended when `errors > 0`.
pub fn bus_bar_label(b: &CanBusConfig, load: Option<f64>, errors: u64) -> String {
    let rate = if b.fd_enabled {
        format!(
            "FD {}/{}",
            format_bitrate(b.bitrate),
            format_bitrate(b.data_bitrate)
        )
    } else {
        format_bitrate(b.bitrate)
    };
    let load = load.map_or("0%".to_string(), |l| format!("{l:.1}%"));
    let err = if errors > 0 {
        format!(" \u{b7} {errors} err")
    } else {
        String::new()
    };
    let hw = b
        .hardware
        .as_ref()
        .map_or(String::new(), crate::hw_ui::bar_suffix);
    format!("{}{hw} \u{b7} {rate} \u{b7} {load}{err}", b.name)
}

impl FlowViewer<GraphNode, ()> for GraphViewer {
    fn node_ui(&mut self, ui: &mut egui::Ui, node: &mut Node<GraphNode>) {
        if let GraphNode::Domain(d) = &node.data {
            // Room on the right for the collapse toggle the canvas draws.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&d.name)
                        .strong()
                        .color(self.accent(&node.data)),
                );
                ui.add_space(28.0);
            });
            return;
        }
        if let (NetworkView::BusLine, GraphNode::Bus(b)) = (self.view, &node.data) {
            ui.horizontal(|ui| {
                ui.add(icons::icon_image(ui, icons::bus()));
                let load = self.loads.get(&b.id).copied();
                let errors = self.bus_errors.get(&b.id).copied().unwrap_or(0);
                ui.label(egui::RichText::new(bus_bar_label(b, load, errors)).strong());
                if let Some(hw) = &b.hardware {
                    crate::hw_ui::chip_ui(ui, hw);
                }
            });
            return;
        }
        let (icon, title) = match &node.data {
            GraphNode::Ecu(e) if matches!(e.kind, NodeKind::Gateway { .. }) => {
                (icons::gateway(), node.data.name().to_string())
            }
            GraphNode::Ecu(e) if matches!(e.kind, NodeKind::Replay { .. }) => {
                (icons::replay(), node.data.name().to_string())
            }
            GraphNode::Ecu(_) => (icons::ecu(), node.data.name().to_string()),
            GraphNode::Domain(_) => unreachable!("handled above"),
            GraphNode::Bus(b) if b.fd_enabled => (
                icons::bus(),
                format!(
                    "{} (CAN FD {}/{}){}",
                    node.data.name(),
                    format_bitrate(b.bitrate),
                    format_bitrate(b.data_bitrate),
                    self.err_suffix(b.id)
                ),
            ),
            GraphNode::Bus(b) => (
                icons::bus(),
                format!(
                    "{} ({}){}",
                    node.data.name(),
                    format_bitrate(b.bitrate),
                    self.err_suffix(b.id)
                ),
            ),
        };
        let badge = self.badge_of(&node.data);
        if badge == Some(NodeBadge::Offline) {
            ui.set_opacity(0.45);
        }
        ui.horizontal(|ui| {
            ui.add(icons::icon_image(ui, icon));
            ui.label(egui::RichText::new(title).strong());
            if let GraphNode::Bus(b) = &node.data
                && let Some(hw) = &b.hardware
            {
                crate::hw_ui::chip_ui(ui, hw);
            }
            if let GraphNode::Ecu(e) = &node.data
                && e.script.is_some()
            {
                ui.add(icons::icon_image(ui, icons::script()));
            }
            if let Some(b) = badge {
                // Keep the badge readable on a dimmed (offline) node.
                ui.set_opacity(1.0);
                badge_ui(ui, b);
            }
        });
        if let GraphNode::Ecu(e) = &node.data
            && let Some(sub) = subtitle(e)
        {
            ui.label(egui::RichText::new(sub).small().weak());
        }
    }

    fn handles(&self, node: &Node<GraphNode>) -> Vec<Handle> {
        if let Some(h) = self.plan.nodes.get(&node.id) {
            return h.clone();
        }
        match node.data {
            GraphNode::Ecu(_) => vec![Handle::source(Handle::DEFAULT_SOURCE, Side::Bottom)],
            GraphNode::Bus(_) => vec![Handle::target(Handle::DEFAULT_TARGET, Side::Top)],
            GraphNode::Domain(_) => Vec::new(),
        }
    }

    fn can_connect(&self, conn: &Connection) -> bool {
        !self.wired.contains(&(conn.source, conn.target))
    }

    fn node_frame(&self, ui: &egui::Ui, node: &Node<GraphNode>) -> Frame {
        let accent = self.accent(&node.data);
        if let GraphNode::Domain(_) = &node.data {
            let [r, g, b, _] = accent.to_array();
            return Frame::new()
                .fill(Color32::from_rgba_unmultiplied(r, g, b, 30))
                .stroke(Stroke::new(1.5_f32, accent))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(12, 6));
        }
        if let (NetworkView::BusLine, GraphNode::Bus(_)) = (self.view, &node.data) {
            return Frame::new()
                .fill(ui.visuals().window_fill.lerp_to_gamma(accent, 0.2))
                .stroke(Stroke::new(2.0_f32, accent))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::symmetric(8, 3));
        }
        let stroke = match self.badge_of(&node.data) {
            Some(b @ (NodeBadge::Passive | NodeBadge::BusOff | NodeBadge::Offline)) => {
                Stroke::new(2.5_f32, b.color())
            }
            _ => Stroke::new(1.5_f32, accent),
        };
        Frame::new()
            .fill(ui.visuals().window_fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(8))
    }

    fn minimap_color(&self, node: &Node<GraphNode>) -> Option<egui::Color32> {
        Some(self.accent(&node.data))
    }
}

/// Small state badge: a dot for an active node, a coloured word otherwise.
fn badge_ui(ui: &mut egui::Ui, badge: NodeBadge) {
    let color = badge.color();
    if badge == NodeBadge::Active {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        resp.on_hover_text("Error active");
    } else {
        ui.label(
            egui::RichText::new(badge.label())
                .size(11.0)
                .strong()
                .color(color),
        );
    }
}

impl GraphViewer {
    fn err_suffix(&self, bus: BusId) -> String {
        match self.bus_errors.get(&bus) {
            Some(n) if *n > 0 => format!(" \u{b7} {n} err"),
            _ => String::new(),
        }
    }

    fn accent(&self, data: &GraphNode) -> egui::Color32 {
        match data {
            GraphNode::Ecu(e) if matches!(e.kind, NodeKind::Gateway { .. }) => {
                self.theme.gateway_color()
            }
            GraphNode::Ecu(e) if matches!(e.kind, NodeKind::Replay { .. }) => {
                self.theme.replay_color()
            }
            GraphNode::Ecu(_) => self.theme.bus_color(1),
            GraphNode::Bus(_) => self.theme.bus_color(0),
            GraphNode::Domain(d) => d.color.map_or_else(
                || self.theme.bus_color(d.id as usize + 2),
                |[r, g, b]| Color32::from_rgb(r, g, b),
            ),
        }
    }
}

/// Node subtitle, e.g. `3 route(s) · 2 msg(s)`; `None` when there is nothing to show.
pub fn subtitle(e: &EcuConfig) -> Option<String> {
    let mut parts = Vec::new();
    match &e.kind {
        NodeKind::Gateway { routes } => parts.push(format!("{} route(s)", routes.len())),
        NodeKind::Replay {
            path, channel_map, ..
        } => {
            let file = std::path::Path::new(path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .filter(|f| !f.is_empty());
            parts.push(file.unwrap_or_else(|| "no log file".into()));
            parts.push(format!("{} ch", channel_map.len()));
        }
        NodeKind::Ecu => {}
    }
    if !e.tx.is_empty() {
        parts.push(format!("{} msg(s)", e.tx.len()));
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// Formats a bit/s value compactly, e.g. `500k`, `2M`.
pub fn format_bitrate(bps: u32) -> String {
    if bps >= 1_000_000 && bps.is_multiple_of(1_000_000) {
        format!("{}M", bps / 1_000_000)
    } else if bps >= 1_000 && bps.is_multiple_of(1_000) {
        format!("{}k", bps / 1_000)
    } else {
        format!("{bps}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn databases_round_trip_and_bus_positions_kept() {
        let mut g = Graph::new();
        let bus = g.add_bus(Pos2::new(500.0, 400.0));
        g.databases.push(DbcRef {
            path: "a.dbc".into(),
            bus: BusId(1),
        });
        let _ = bus;
        let topo = g.to_topology();
        assert_eq!(topo.databases, g.databases);
        let g2 = Graph::from_topology_keeping(&topo, &g);
        assert_eq!(g2.databases, g.databases);
        let pos = g2.state.nodes[0].position;
        assert_eq!(pos, Pos2::new(500.0, 400.0));
        // Removing the bus drops its database reference.
        let mut g3 = g2;
        let id = g3.state.nodes[0].id;
        g3.remove(id);
        assert!(g3.databases.is_empty());
    }

    fn wired_graph() -> (Graph, FlowId, FlowId, FlowId, FlowId) {
        let mut g = Graph::new();
        let b1 = g.add_bus(Pos2::ZERO);
        let b2 = g.add_bus(Pos2::new(0.0, 300.0));
        let ecu = g.add_ecu(Pos2::new(0.0, -100.0), "Engine");
        if let Some(GraphNode::Ecu(e)) = g.node_mut(ecu) {
            e.tx.push(tx("A", 1, 10, &[1]));
            e.script = Some("x".into());
        }
        g.state.connect(ecu, b1, ()).unwrap();
        (g, ecu, b1, b2, ecu)
    }

    #[test]
    fn paste_allocates_fresh_ids_and_remaps_copied_buses() {
        let mut g = Graph::new();
        let bus = g.add_bus(Pos2::ZERO);
        let gw = g.add_gateway(Pos2::new(0.0, -100.0));
        let ecu = g.add_ecu(Pos2::new(200.0, -100.0), "Engine");
        let bus_id = match g.node(bus) {
            Some(GraphNode::Bus(b)) => b.id,
            _ => unreachable!(),
        };
        if let Some(GraphNode::Ecu(e)) = g.node_mut(ecu) {
            e.tx.push(tx("RPM", 0x100, 10, &[1, 2]));
            e.script = Some("let a = 1;".into());
        }
        if let Some(GraphNode::Ecu(e)) = g.node_mut(gw) {
            e.kind = NodeKind::Gateway {
                routes: vec![operow_core::RouteRule {
                    from_bus: bus_id,
                    to_buses: vec![],
                    filter: operow_core::IdFilter::Any,
                    remap_id: None,
                    delay_us: 0,
                }],
            };
        }
        g.state.connect(ecu, bus, ()).unwrap();
        g.state.connect(gw, bus, ()).unwrap();
        g.state.select_all();
        assert!(g.copy());
        assert!(g.paste());
        assert_eq!(g.state.nodes.len(), 6);
        let ecus: Vec<_> = g
            .state
            .nodes
            .iter()
            .filter_map(|n| match &n.data {
                GraphNode::Ecu(e) => Some(e.clone()),
                _ => None,
            })
            .collect();
        let ids: HashSet<_> = ecus.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 4, "every ECU has its own NodeId");
        let buses: HashSet<_> = g
            .state
            .nodes
            .iter()
            .filter_map(|n| match &n.data {
                GraphNode::Bus(b) => Some(b.id),
                _ => None,
            })
            .collect();
        assert_eq!(buses.len(), 2, "the copied bus has a fresh BusId");
        let copy = ecus.iter().find(|e| e.name == "Engine (copy)").unwrap();
        assert_eq!(copy.tx.len(), 1);
        assert_eq!(copy.script.as_deref(), Some("let a = 1;"));
        // The copied gateway's route follows the copied bus.
        let new_bus = *buses.iter().find(|b| **b != bus_id).unwrap();
        let gwc = ecus
            .iter()
            .find(|e| e.name.ends_with("(copy)") && matches!(e.kind, NodeKind::Gateway { .. }))
            .unwrap();
        match &gwc.kind {
            NodeKind::Gateway { routes } => assert_eq!(routes[0].from_bus, new_bus),
            _ => unreachable!(),
        }
        // Copies are wired to the copied bus, so the topology still validates.
        assert_eq!(g.to_topology().validate(), Ok(()));
        assert_eq!(g.to_topology().links.len(), 4);
        // A second duplicate names the copy "(copy 2)".
        assert!(!g.fix_duplicate_ids());
        let taken: HashSet<String> = ["Engine".into(), "Engine (copy)".into()].into();
        assert_eq!(copy_name("Engine (copy)", &taken), "Engine (copy 2)");
    }

    #[test]
    fn reconnect_moves_the_link_and_flags_stale_routes() {
        let (mut g, ecu, b1, b2, _) = wired_graph();
        let edge = g.state.edges[0].id;
        let old = g.state.edge(edge).unwrap().connection();
        // Drag the bus end onto bus 2.
        let e = g.state.edge_mut(edge).unwrap();
        e.target = b2;
        let new = e.connection();
        if let Some(GraphNode::Ecu(c)) = g.node_mut(ecu) {
            c.tx[0].bus = Some(BusId(1));
        }
        let r = g.on_reconnected(edge, old, new);
        assert!(
            matches!(&r, Reconnect::Moved { from, to, stale: 1, .. } if from == "CAN1" && to == "CAN2"),
            "{r:?}"
        );
        let topo = g.to_topology();
        assert_eq!(
            topo.links,
            vec![Link {
                node: NodeId(1),
                bus: BusId(2)
            }]
        );
        // The message still names CAN1: validation flags it, nothing was rewritten.
        assert!(topo.validate().is_err());
        assert_eq!(topo.nodes[0].tx[0].bus, Some(BusId(1)));
        let _ = b1;
    }

    #[test]
    fn reconnect_to_a_non_bus_or_duplicate_is_reverted() {
        let (mut g, ecu, b1, b2, _) = wired_graph();
        g.state.connect(ecu, b2, ()).unwrap();
        let edge = g.state.edges[0].id;
        let old = g.state.edge(edge).unwrap().connection();
        g.state.edge_mut(edge).unwrap().target = b2;
        let new = g.state.edge(edge).unwrap().connection();
        let r = g.on_reconnected(edge, old, new);
        assert!(matches!(r, Reconnect::Reverted(_)));
        assert_eq!(g.state.edge(edge).unwrap().target, b1);
        assert_eq!(g.to_topology().links.len(), 2);
    }

    #[test]
    fn switching_views_keeps_each_layout() {
        let mut g = Graph::default_demo();
        assert_eq!(g.view(), NetworkView::BusLine);
        let line: Vec<Pos2> = g.state.nodes.iter().map(|n| n.position).collect();
        g.state.nodes[0].position = Pos2::new(11.0, 22.0);
        g.set_view(NetworkView::FreeForm);
        // Free-form starts from the ECUs' stored positions.
        assert_eq!(g.state.nodes[1].position, Pos2::new(60.0, 60.0));
        g.state.nodes[1].position = Pos2::new(5.0, 6.0);
        g.set_view(NetworkView::BusLine);
        assert_eq!(g.state.nodes[0].position, Pos2::new(11.0, 22.0));
        assert_ne!(g.state.nodes[0].position, line[0]);
        g.set_view(NetworkView::FreeForm);
        assert_eq!(g.state.nodes[1].position, Pos2::new(5.0, 6.0));
        // The topology stores the free-form position in either view.
        g.set_view(NetworkView::BusLine);
        let pos = g.to_topology().nodes[0].pos;
        assert_eq!(pos, (5.0, 6.0));
    }

    /// The demo graph in free-form, with Engine and Brake in a domain.
    fn grouped() -> (Graph, FlowId, FlowId, FlowId) {
        let mut g = Graph::default_demo();
        g.set_view(NetworkView::FreeForm);
        let ids: Vec<FlowId> = g.state.nodes.iter().map(|n| n.id).collect();
        let (bus, engine, brake) = (ids[0], ids[1], ids[2]);
        g.state.clear_selection();
        g.state.node_mut(engine).unwrap().selected = true;
        g.state.node_mut(brake).unwrap().selected = true;
        let group = g.group_selection("Drive").unwrap();
        (g, group, engine, bus)
    }

    #[test]
    fn domains_round_trip_through_the_topology() {
        let (mut g, group, engine, bus) = grouped();
        g.state.set_parent(bus, Some(group));
        g.state.set_collapsed(group, true);
        let topo = g.to_topology();
        assert_eq!(topo.validate(), Ok(()));
        let d = &topo.domains[0];
        assert_eq!(
            (d.name.as_str(), d.collapsed, d.parent),
            ("Drive", true, None)
        );
        assert_eq!((d.members.len(), d.bus_members.len()), (2, 1));
        let g2 = Graph::from_topology(&topo);
        let again = g2.to_topology();
        assert_eq!(again.domains, topo.domains);
        // Collapse and membership follow the canvas (ParentChanged / GroupToggled).
        let mut g3 = Graph::from_topology(&topo);
        let (group3, _) = g3.domains().first().map(|(i, d)| (*i, d.id)).unwrap();
        let e3 = g3
            .state
            .nodes
            .iter()
            .find(|n| n.parent == Some(group3))
            .unwrap()
            .id;
        g3.state.set_parent(e3, None);
        g3.state.set_collapsed(group3, false);
        let d3 = &g3.to_topology().domains[0];
        assert!(!d3.collapsed && d3.members.len() + d3.bus_members.len() == 2);
        let _ = engine;
    }

    #[test]
    fn positions_survive_groups_in_both_views() {
        let (mut g, group, engine, _) = grouped();
        let abs = g.state.abs_position(engine).unwrap();
        g.state.node_mut(group).unwrap().position += vec2(100.0, 50.0);
        let moved = g.state.abs_position(engine).unwrap();
        assert_eq!(moved, abs + vec2(100.0, 50.0));
        // Relative position differs from the absolute one, but the topology
        // and the saved layout store flow-space positions.
        assert_ne!(g.state.node(engine).unwrap().position, moved);
        assert_eq!(g.to_topology().nodes[0].pos, (moved.x, moved.y));
        g.set_view(NetworkView::BusLine);
        g.set_view(NetworkView::FreeForm);
        assert_eq!(g.state.abs_position(engine), Some(moved));
        assert_eq!(g.state.node(engine).unwrap().parent, Some(group));
        let layout = g.network_layout();
        let g2 = {
            let mut g2 = Graph::from_topology(&g.to_topology());
            g2.apply_layout(&layout);
            g2
        };
        let e2 = g2
            .state
            .nodes
            .iter()
            .find(|n| n.data.name() == "Engine")
            .unwrap();
        assert_eq!(g2.state.abs_position(e2.id), Some(moved));
    }

    #[test]
    fn buses_stay_out_of_domains_in_bus_line_but_keep_membership() {
        let (mut g, group, _, bus) = grouped();
        g.state.set_parent(bus, Some(group));
        g.set_view(NetworkView::BusLine);
        assert_eq!(g.state.node(bus).unwrap().parent, None);
        assert_eq!(g.to_topology().domains[0].bus_members.len(), 1);
        // Dropping a bar into a group is reverted.
        g.process_events(&[FlowEvent::ParentChanged {
            node: bus,
            parent: Some(group),
        }]);
        assert_eq!(g.state.node(bus).unwrap().parent, None);
        g.set_view(NetworkView::FreeForm);
        assert_eq!(g.state.node(bus).unwrap().parent, Some(group));
    }

    #[test]
    fn domains_per_bus_leave_gateways_and_shared_nodes_out() {
        let mut g = Graph::new();
        let b1 = g.add_bus(Pos2::ZERO);
        let b2 = g.add_bus(Pos2::new(0.0, 200.0));
        let a = g.add_ecu(Pos2::new(0.0, -100.0), "A");
        let shared = g.add_ecu(Pos2::new(100.0, -100.0), "Shared");
        let gw = g.add_gateway(Pos2::new(200.0, -100.0));
        let loose = g.add_ecu(Pos2::new(300.0, -100.0), "Loose");
        g.state.connect(a, b1, ()).unwrap();
        g.state.connect(shared, b1, ()).unwrap();
        g.state.connect(shared, b2, ()).unwrap();
        g.state.connect(gw, b1, ()).unwrap();
        assert_eq!(g.per_bus_groups(), vec![("CAN1".to_string(), vec![a])]);
        assert_eq!(g.create_domains_per_bus(), 1);
        assert!(g.state.node(a).unwrap().parent.is_some());
        for n in [shared, gw, loose] {
            assert_eq!(g.state.node(n).unwrap().parent, None);
        }
        // Running it again finds nothing new.
        assert_eq!(g.create_domains_per_bus(), 0);
    }

    #[test]
    fn pasted_domains_get_fresh_ids() {
        let (mut g, group, _, _) = grouped();
        let copy = g
            .state
            .add_group(Pos2::ZERO, DOMAIN_SIZE, g.node(group).unwrap().clone());
        assert!(g.fix_duplicate_ids());
        let ids: HashSet<u32> = g.domains().iter().map(|(_, d)| d.id).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(g.domain_name(copy), Some("Drive (copy)"));
    }

    #[test]
    fn wire_style_resolves_link_then_default_then_automatic() {
        let mut edge = Graph::default_demo().state.edges[0].clone();
        edge.width = Some(9.0);
        let default = WireStyle {
            width: Some(2.0),
            line: Some(WireLine::Dotted),
            ..Default::default()
        };
        let own = WireStyle {
            line: Some(WireLine::Dashed),
            arrow: Some(WireArrow::Diamond),
            color: Some([1, 2, 3]),
            animated: Some(true),
            label: Some("x".into()),
            kind: Some(WireKind::SmoothStep),
            ..Default::default()
        };
        assert_eq!(resolved_wire_style(None, None), None);
        let r = resolved_wire_style(Some(&own), Some(&default)).unwrap();
        assert_eq!((r.line, r.width), (Some(WireLine::Dashed), Some(2.0)));
        apply_wire_style(&mut edge, &r);
        assert_eq!(edge.line_style, LineStyle::Dashed);
        assert_eq!(edge.width, Some(2.0));
        assert_eq!(edge.kind, Some(EdgeKind::SmoothStep));
        assert_eq!(edge.color, Some(Color32::from_rgb(1, 2, 3)));
        assert!(edge.arrow && edge.animated);
        assert_eq!(edge.arrow_style, ArrowStyle::Diamond);
        assert_eq!(edge.label.as_deref(), Some("x"));
        // Unset fields keep the automatic look; "no arrow" hides it.
        let mut edge = Graph::default_demo().state.edges[0].clone();
        edge.width = Some(9.0);
        let none = WireStyle {
            arrow: Some(WireArrow::None),
            ..Default::default()
        };
        apply_wire_style(&mut edge, &none);
        assert_eq!((edge.width, edge.arrow), (Some(9.0), false));
    }

    #[test]
    fn wire_styles_round_trip_and_drop_with_their_link() {
        let mut g = Graph::default_demo();
        let link = g.links()[0];
        let style = WireStyle {
            width: Some(4.0),
            ..Default::default()
        };
        g.set_wire_style((link.node, link.bus), style.clone());
        g.wire_default = Some(WireStyle {
            line: Some(WireLine::Dotted),
            ..Default::default()
        });
        g.set_wire_style((NodeId(99), BusId(99)), style.clone());
        let topo = g.to_topology();
        assert_eq!(topo.wires.len(), 1, "style of a missing link is dropped");
        let g2 = Graph::from_topology(&topo);
        assert_eq!(g2.wire_styles.get(&(link.node, link.bus)), Some(&style));
        assert_eq!(g2.wire_default, g.wire_default);
        g.set_wire_style((link.node, link.bus), WireStyle::default());
        assert!(g.to_topology().wires.is_empty());
    }

    #[test]
    fn bus_bar_label_shows_name_rate_and_load() {
        let b = CanBusConfig {
            id: BusId(1),
            name: "CAN1".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            hardware: None,
        };
        assert_eq!(
            bus_bar_label(&b, Some(12.34), 0),
            "CAN1 \u{b7} 500k \u{b7} 12.3%"
        );
        let fd = CanBusConfig {
            fd_enabled: true,
            ..b.clone()
        };
        assert_eq!(
            bus_bar_label(&fd, None, 0),
            "CAN1 \u{b7} FD 500k/2M \u{b7} 0%"
        );
        let hw = CanBusConfig {
            hardware: Some(operow_core::HwBinding::new("socketcan:can0")),
            ..b.clone()
        };
        assert_eq!(
            bus_bar_label(&hw, None, 0),
            "CAN1 \u{b7} HW can0 (listen) \u{b7} 500k \u{b7} 0%"
        );
        assert_eq!(
            bus_bar_label(&fd, Some(1.0), 20),
            "CAN1 \u{b7} FD 500k/2M \u{b7} 1.0% \u{b7} 20 err"
        );
        assert_eq!(
            bus_bar_label(&fd, None, 0),
            bus_bar_label(&fd, None, 0),
            "no suffix without errors"
        );
    }

    #[test]
    fn replay_node_links_follow_its_channel_mapping() {
        let mut g = Graph::new();
        let a = g.add_bus(Pos2::ZERO);
        let b = g.add_bus(Pos2::new(0.0, 100.0));
        let r = g.add_replay(Pos2::new(100.0, 0.0));
        let bus_id = |g: &Graph, f| match g.node(f) {
            Some(GraphNode::Bus(b)) => b.id,
            _ => unreachable!(),
        };
        let (ida, idb) = (bus_id(&g, a), bus_id(&g, b));
        let set = |g: &mut Graph, map: Vec<(u8, BusId)>| {
            if let Some(GraphNode::Ecu(e)) = g.node_mut(r)
                && let NodeKind::Replay { channel_map, .. } = &mut e.kind
            {
                *channel_map = map;
            }
            g.sync_replay_links(r);
        };
        set(&mut g, vec![(1, ida), (2, idb)]);
        let linked = |g: &Graph| -> Vec<BusId> {
            let mut v: Vec<_> = g.links().iter().map(|l| l.bus).collect();
            v.sort();
            v
        };
        assert_eq!(linked(&g), [ida, idb]);
        // Two channels on one bus make one wire; dropping one removes it.
        set(&mut g, vec![(1, ida), (2, ida)]);
        assert_eq!(linked(&g), [ida]);
        set(&mut g, vec![]);
        assert!(linked(&g).is_empty());
        assert_eq!(g.to_topology().validate(), Ok(()));
        let sub = subtitle(match g.node(r) {
            Some(GraphNode::Ecu(e)) => e,
            _ => unreachable!(),
        });
        assert_eq!(sub.as_deref(), Some("no log file \u{b7} 0 ch"));
    }

    #[test]
    fn gateway_creation_and_subtitle() {
        let mut g = Graph::new();
        let id = g.add_gateway(Pos2::ZERO);
        let Some(GraphNode::Ecu(e)) = g.node(id) else {
            panic!()
        };
        assert_eq!(e.name, "Gateway 1");
        assert_eq!(subtitle(e).as_deref(), Some("0 route(s)"));
        let mut e = e.clone();
        e.tx.push(tx("A", 1, 10, &[]));
        assert_eq!(subtitle(&e).as_deref(), Some("0 route(s) \u{b7} 1 msg(s)"));
        e.kind = NodeKind::Ecu;
        e.tx.clear();
        assert_eq!(subtitle(&e), None);
    }
}

/// Which way a pulse travels along an ECU-bus wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PulseDir {
    /// ECU drives the frame onto the bus.
    ToBus,
    /// Bus delivers the frame to a receiving ECU.
    FromBus,
}

/// Visual class of a pulse: originated (`Tx`), gateway-forwarded
/// (`Forwarded`) or injected by a Generator window, each optionally CAN FD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PulseKind {
    pub forwarded: bool,
    pub fd: bool,
    /// Sent by a Generator window (a virtual sender with no wire).
    pub generator: bool,
    /// A CAN error frame: only the sender's wire pulses, in red.
    pub error: bool,
}

/// One animated hop of a frame along an ECU-bus wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PulseSpec {
    pub node: NodeId,
    pub bus: BusId,
    pub dir: PulseDir,
    pub kind: PulseKind,
    /// CAN id and format of the frame, for the hover label.
    pub id: u32,
    pub extended: bool,
    /// The ECU that created the frame (to look up its message name).
    pub origin: NodeId,
    /// How many wire legs come before this one, so a frame forwarded by a
    /// gateway starts when the previous hop's pulse has arrived.
    pub legs: u8,
}

/// Gateway hops whose delay is still modelled; later hops start together.
const MAX_DELAYED_HOPS: u8 = 3;

/// Map a batch of bus events onto a deduplicated list of wire pulses: the
/// sender's wire to the bus, then the bus's wire to every other linked node
/// (a reverse leg, one leg later). Order is first-seen, so the sender leg
/// precedes the receiver legs. A forwarded frame is its own event from the
/// gateway, so it chains on by itself, `2 * hop` legs late. Generator
/// frames have no sender wire, so their receiver legs start at once.
pub fn pulses_for_events(events: &[BusEvent], links: &[Link]) -> Vec<PulseSpec> {
    let mut out: Vec<PulseSpec> = Vec::new();
    let mut push = |spec: PulseSpec| {
        if !out.contains(&spec) {
            out.push(spec);
        }
    };
    for ev in events {
        let generator = ev.sender.0 >= GENERATOR_NODE_BASE;
        let kind = PulseKind {
            forwarded: ev.hop > 0,
            fd: ev.frame.fd,
            generator,
            error: ev.is_error(),
        };
        let base = 2 * ev.hop.min(MAX_DELAYED_HOPS);
        let spec = |node, dir, legs| PulseSpec {
            node,
            bus: ev.bus,
            dir,
            kind,
            id: ev.frame.id,
            extended: ev.frame.extended,
            origin: ev.origin,
            legs,
        };
        push(spec(
            ev.sender,
            PulseDir::ToBus,
            if generator { 0 } else { base },
        ));
        if ev.is_error() {
            continue;
        }
        for l in links
            .iter()
            .filter(|l| l.bus == ev.bus && l.node != ev.sender)
        {
            let legs = if generator { 0 } else { base + 1 };
            push(spec(l.node, PulseDir::FromBus, legs));
        }
    }
    out
}

#[cfg(test)]
mod pulse_tests {
    use super::*;
    use operow_core::{CanFrame, Direction, Timestamp};

    fn ev(bus: u32, sender: u32, hop: u8, fd: bool) -> BusEvent {
        let frame = if fd {
            CanFrame::new_fd(0x100, false, false, &[0; 12]).unwrap()
        } else {
            CanFrame::new(0x100, false, &[0; 8]).unwrap()
        };
        BusEvent {
            time: Timestamp(0),
            bus: BusId(bus),
            sender: NodeId(sender),
            origin: NodeId(1),
            dir: if hop > 0 {
                Direction::Rx
            } else {
                Direction::Tx
            },
            frame_uid: 1,
            hop,
            frame,
            kind: Default::default(),
        }
    }

    fn links() -> Vec<Link> {
        [(1, 1), (9, 1), (9, 2), (2, 2), (3, 2)]
            .iter()
            .map(|&(n, b)| Link {
                node: NodeId(n),
                bus: BusId(b),
            })
            .collect()
    }

    fn kind(forwarded: bool, fd: bool, generator: bool) -> PulseKind {
        PulseKind {
            forwarded,
            fd,
            generator,
            error: false,
        }
    }

    #[test]
    fn forwarded_event_pulses_gateway_to_bus2() {
        let p = pulses_for_events(&[ev(2, 9, 1, false)], &links());
        assert_eq!(
            p[0],
            PulseSpec {
                node: NodeId(9),
                bus: BusId(2),
                dir: PulseDir::ToBus,
                kind: kind(true, false, false),
                id: 0x100,
                extended: false,
                origin: NodeId(1),
                legs: 2,
            }
        );
    }

    #[test]
    fn sender_leg_then_receiver_legs_one_leg_later() {
        // ECU 1 transmits on bus 1: its wire in, then the wire out to node 9.
        let p = pulses_for_events(&[ev(1, 1, 0, false)], &links());
        assert_eq!(p.len(), 2);
        assert_eq!(
            (p[0].node, p[0].dir, p[0].legs),
            (NodeId(1), PulseDir::ToBus, 0)
        );
        assert_eq!(
            (p[1].node, p[1].dir, p[1].legs),
            (NodeId(9), PulseDir::FromBus, 1)
        );
        assert!(p.iter().all(|s| s.bus == BusId(1) && !s.kind.generator));
    }

    #[test]
    fn receivers_included_sender_excluded() {
        let p = pulses_for_events(&[ev(2, 9, 1, false)], &links());
        let from: Vec<_> = p
            .iter()
            .filter(|s| s.dir == PulseDir::FromBus)
            .map(|s| s.node)
            .collect();
        assert_eq!(from, vec![NodeId(2), NodeId(3)]);
        assert_eq!(p.len(), 3);
        // The receiver legs of a forwarded frame follow its sender leg.
        assert!(
            p.iter()
                .filter(|s| s.dir == PulseDir::FromBus)
                .all(|s| s.legs == 3)
        );
    }

    #[test]
    fn gateway_chain_hops_are_spaced_two_legs_apart() {
        // CAN1 tx, then the gateway's forwarded copy on CAN2.
        let p = pulses_for_events(&[ev(1, 1, 0, false), ev(2, 9, 1, false)], &links());
        let arrive_at_gateway = p
            .iter()
            .find(|s| s.node == NodeId(9) && s.dir == PulseDir::FromBus)
            .unwrap();
        let leave_gateway = p
            .iter()
            .find(|s| s.node == NodeId(9) && s.dir == PulseDir::ToBus)
            .unwrap();
        assert_eq!(leave_gateway.legs, arrive_at_gateway.legs + 1);
        // Deep hops stop adding delay.
        let deep = pulses_for_events(&[ev(2, 9, 7, false)], &links());
        assert_eq!(deep[0].legs, 2 * MAX_DELAYED_HOPS);
    }

    #[test]
    fn generator_frames_use_the_generator_kind_and_start_at_the_bus() {
        let gen_sender = GENERATOR_NODE_BASE + 1;
        let p = pulses_for_events(&[ev(1, gen_sender, 0, false)], &links());
        assert!(p.iter().all(|s| s.kind == kind(false, false, true)));
        // Sender leg has no wire (the app skips it); receivers start at once.
        assert_eq!((p[0].node, p[0].dir), (NodeId(gen_sender), PulseDir::ToBus));
        let rx: Vec<_> = p.iter().filter(|s| s.dir == PulseDir::FromBus).collect();
        assert_eq!(rx.len(), 2);
        assert!(rx.iter().all(|s| s.legs == 0));
    }

    #[test]
    fn duplicates_are_merged_but_styles_kept() {
        let evs = [ev(1, 1, 0, false), ev(1, 1, 0, false), ev(1, 1, 0, true)];
        let p = pulses_for_events(&evs, &links());
        // 1 ToBus + 1 FromBus (node 9) per style.
        assert_eq!(p.len(), 4);
    }
}
