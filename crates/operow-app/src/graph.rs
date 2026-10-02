//! The node-graph editor: ECU and CAN-bus nodes wired together with
//! `egui-flow`, convertible to/from an `operow_core::Topology`.

use egui::{CornerRadius, Frame, Margin, Pos2, Stroke};
use egui_flow::{FlowState, FlowViewer, Handle, Node, NodeId as FlowId, PulseStyle, Side};

use operow_core::{BusId, CanBusConfig, EcuConfig, Link, NodeId, NodeKind, Topology, TxMessage};

use crate::icons;
use crate::theme::AppTheme;

/// A node placed on the canvas: either a simulated ECU or a CAN bus.
#[derive(Debug, Clone)]
pub enum GraphNode {
    Ecu(EcuConfig),
    Bus(CanBusConfig),
}

impl GraphNode {
    pub fn name(&self) -> &str {
        match self {
            GraphNode::Ecu(e) => &e.name,
            GraphNode::Bus(b) => &b.name,
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
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            state: FlowState::new(),
            next_node_id: 1,
            next_bus_id: 1,
        }
    }

    /// Build the default preloaded topology: one CAN FD bus (500k/2M) with
    /// three ECUs.
    pub fn default_demo() -> Self {
        let mut g = Graph::new();
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
        g.state.fit_view();
        g
    }

    pub fn add_ecu(&mut self, pos: Pos2, name: &str) -> FlowId {
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
            }),
        )
    }

    /// Add a gateway node (an ECU-like node with an empty route table).
    pub fn add_gateway(&mut self, pos: Pos2) -> FlowId {
        let name = format!("Gateway {}", self.next_node_id);
        let id = self.add_ecu(pos, &name);
        if let Some(GraphNode::Ecu(e)) = self.node_mut(id) {
            e.kind = NodeKind::Gateway { routes: vec![] };
        }
        id
    }

    pub fn add_bus(&mut self, pos: Pos2) -> FlowId {
        let id = BusId(self.next_bus_id);
        self.next_bus_id += 1;
        let name = format!("CAN{}", self.next_bus_id - 1);
        self.state.add_node(
            pos,
            GraphNode::Bus(CanBusConfig {
                id,
                name,
                bitrate: 500_000,
                fd_enabled: false,
                data_bitrate: 2_000_000,
            }),
        )
    }

    pub fn node(&self, id: FlowId) -> Option<&GraphNode> {
        self.state.node(id).map(|n| &n.data)
    }

    pub fn node_mut(&mut self, id: FlowId) -> Option<&mut GraphNode> {
        self.state.node_mut(id).map(|n| &mut n.data)
    }

    /// Remove a node and the wires attached to it.
    pub fn remove(&mut self, id: FlowId) {
        self.state.remove_node(id);
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

    /// Send a particle down every wire leaving the ECU `sender`, to show it
    /// transmitting a frame. No-op if the ECU isn't on the canvas.
    pub fn pulse_sender(&mut self, sender: NodeId, style: PulseStyle) {
        let Some(node) = self
            .state
            .nodes
            .iter()
            .find(|n| matches!(&n.data, GraphNode::Ecu(e) if e.id == sender))
            .map(|n| n.id)
        else {
            return;
        };
        let edges: Vec<_> = self
            .state
            .edges
            .iter()
            .filter(|e| e.source == node)
            .map(|e| e.id)
            .collect();
        for edge in edges {
            self.state.pulse_edge(edge, style);
        }
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
                    cfg.pos = (node.position.x, node.position.y);
                    nodes.push(cfg);
                }
                GraphNode::Bus(b) => buses.push(b.clone()),
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

        Topology {
            nodes,
            buses,
            links,
        }
    }

    /// Rebuild the graph from a loaded `Topology`, keeping saved positions.
    pub fn from_topology(topo: &Topology) -> Self {
        let mut g = Graph::new();
        let mut ecu_map = std::collections::HashMap::new();
        let mut bus_map = std::collections::HashMap::new();

        for ecu in &topo.nodes {
            let pos = Pos2::new(ecu.pos.0, ecu.pos.1);
            let fid = g.state.add_node(pos, GraphNode::Ecu(ecu.clone()));
            g.next_node_id = g.next_node_id.max(ecu.id.0 + 1);
            ecu_map.insert(ecu.id, fid);
        }
        for (i, bus) in topo.buses.iter().enumerate() {
            let pos = Pos2::new(160.0, 260.0 + 160.0 * i as f32);
            let fid = g.state.add_node(pos, GraphNode::Bus(bus.clone()));
            g.next_bus_id = g.next_bus_id.max(bus.id.0 + 1);
            bus_map.insert(bus.id, fid);
        }
        for link in &topo.links {
            if let (Some(&ecu), Some(&bus)) = (ecu_map.get(&link.node), bus_map.get(&link.bus)) {
                g.state.connect(ecu, bus, ());
            }
        }
        g.state.fit_view();
        g
    }
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

/// Renders ECU and bus nodes. ECUs have a single output on the bottom edge
/// and buses a single input on the top edge, so the only wires that can be
/// drawn are ECU -> bus.
pub struct GraphViewer {
    pub theme: AppTheme,
}

impl FlowViewer<GraphNode, ()> for GraphViewer {
    fn node_ui(&mut self, ui: &mut egui::Ui, node: &mut Node<GraphNode>) {
        let (icon, title) = match &node.data {
            GraphNode::Ecu(e) if matches!(e.kind, NodeKind::Gateway { .. }) => {
                (icons::gateway(), node.data.name().to_string())
            }
            GraphNode::Ecu(_) => (icons::ecu(), node.data.name().to_string()),
            GraphNode::Bus(b) if b.fd_enabled => (
                icons::bus(),
                format!(
                    "{} (CAN FD {}/{})",
                    node.data.name(),
                    format_bitrate(b.bitrate),
                    format_bitrate(b.data_bitrate)
                ),
            ),
            GraphNode::Bus(b) => (
                icons::bus(),
                format!("{} ({})", node.data.name(), format_bitrate(b.bitrate)),
            ),
        };
        ui.horizontal(|ui| {
            ui.add(icons::icon_image(ui, icon));
            ui.label(egui::RichText::new(title).strong());
            if let GraphNode::Ecu(e) = &node.data
                && e.script.is_some()
            {
                ui.add(icons::icon_image(ui, icons::script()));
            }
        });
        if let GraphNode::Ecu(e) = &node.data
            && let Some(sub) = subtitle(e)
        {
            ui.label(egui::RichText::new(sub).small().weak());
        }
    }

    fn handles(&self, node: &Node<GraphNode>) -> Vec<Handle> {
        match node.data {
            GraphNode::Ecu(_) => vec![Handle::source(Handle::DEFAULT_SOURCE, Side::Bottom)],
            GraphNode::Bus(_) => vec![Handle::target(Handle::DEFAULT_TARGET, Side::Top)],
        }
    }

    fn node_frame(&self, ui: &egui::Ui, node: &Node<GraphNode>) -> Frame {
        let accent = self.accent(&node.data);
        Frame::new()
            .fill(ui.visuals().window_fill)
            .stroke(Stroke::new(1.5_f32, accent))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(8))
    }

    fn minimap_color(&self, node: &Node<GraphNode>) -> Option<egui::Color32> {
        Some(self.accent(&node.data))
    }
}

impl GraphViewer {
    fn accent(&self, data: &GraphNode) -> egui::Color32 {
        match data {
            GraphNode::Ecu(e) if matches!(e.kind, NodeKind::Gateway { .. }) => {
                self.theme.gateway_color()
            }
            GraphNode::Ecu(_) => self.theme.bus_color(1),
            GraphNode::Bus(_) => self.theme.bus_color(0),
        }
    }
}

/// Node subtitle, e.g. `3 route(s) · 2 msg(s)`; `None` when there is nothing to show.
pub fn subtitle(e: &EcuConfig) -> Option<String> {
    let mut parts = Vec::new();
    if let NodeKind::Gateway { routes } = &e.kind {
        parts.push(format!("{} route(s)", routes.len()));
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
