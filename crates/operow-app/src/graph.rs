//! The node-graph editor: ECU and CAN-bus nodes wired together with
//! `egui-snarl`, convertible to/from an `operow_core::Topology`.

use egui::{Color32, Pos2, Stroke};
use egui_snarl::ui::{PinInfo, SnarlPin, SnarlStyle, SnarlViewer};
use egui_snarl::{InPin, InPinId, NodeId as SnarlId, OutPin, OutPinId, Snarl};

use operow_core::{BusId, CanBusConfig, EcuConfig, Link, NodeId, Topology, TxMessage};

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

/// Owns the snarl graph plus the id counters needed to allocate fresh
/// `NodeId`/`BusId` values for new nodes.
pub struct Graph {
    pub snarl: Snarl<GraphNode>,
    next_node_id: u32,
    next_bus_id: u32,
    /// Selected node, kept for the properties inspector.
    pub selected: Option<SnarlId>,
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            snarl: Snarl::new(),
            next_node_id: 1,
            next_bus_id: 1,
            selected: None,
        }
    }

    /// Build the default preloaded topology: one 500k bus with three ECUs.
    pub fn default_demo() -> Self {
        let mut g = Graph::new();
        let bus = g.add_bus(Pos2::new(80.0, 260.0));
        let bus_id = match &g.snarl[bus] {
            GraphNode::Bus(b) => b.id,
            GraphNode::Ecu(_) => unreachable!(),
        };

        let engine = g.add_ecu(Pos2::new(60.0, 60.0), "Engine");
        if let GraphNode::Ecu(e) = &mut g.snarl[engine] {
            e.tx.push(tx("EngineRPM", 0x100, 10));
            e.tx.push(tx("EngineTemp", 0x101, 100));
        }

        let brake = g.add_ecu(Pos2::new(280.0, 60.0), "Brake");
        if let GraphNode::Ecu(e) = &mut g.snarl[brake] {
            e.tx.push(tx("BrakeStatus", 0x200, 20));
        }

        let gateway = g.add_ecu(Pos2::new(500.0, 60.0), "Gateway");
        if let GraphNode::Ecu(e) = &mut g.snarl[gateway] {
            e.tx.push(tx("GatewayHeartbeat", 0x300, 50));
        }

        for ecu in [engine, brake, gateway] {
            g.snarl.connect(
                OutPinId {
                    node: ecu,
                    output: 0,
                },
                InPinId {
                    node: bus,
                    input: 0,
                },
            );
            let _ = bus_id;
        }
        g
    }

    pub fn add_ecu(&mut self, pos: Pos2, name: &str) -> SnarlId {
        let id = NodeId(self.next_node_id);
        self.next_node_id += 1;
        self.snarl.insert_node(
            pos,
            GraphNode::Ecu(EcuConfig {
                id,
                name: name.to_string(),
                tx: Vec::new(),
                pos: (pos.x, pos.y),
            }),
        )
    }

    pub fn add_bus(&mut self, pos: Pos2) -> SnarlId {
        let id = BusId(self.next_bus_id);
        self.next_bus_id += 1;
        let name = format!("CAN{}", self.next_bus_id - 1);
        self.snarl.insert_node(
            pos,
            GraphNode::Bus(CanBusConfig {
                id,
                name,
                bitrate: 500_000,
            }),
        )
    }

    /// Convert the graph into a `Topology` for the simulation engine.
    pub fn to_topology(&self) -> Topology {
        let mut nodes = Vec::new();
        let mut buses = Vec::new();
        let mut links = Vec::new();

        for (id, node) in self.snarl.nodes_ids_data() {
            match &node.value {
                GraphNode::Ecu(e) => {
                    let mut cfg = e.clone();
                    cfg.pos = (node.pos.x, node.pos.y);
                    nodes.push(cfg);
                }
                GraphNode::Bus(b) => buses.push(b.clone()),
            }
            let _ = id;
        }

        for (out_pin, in_pin) in self.snarl.wires() {
            let (ecu_side, bus_side) = (out_pin.node, in_pin.node);
            if let (Some(GraphNode::Ecu(e)), Some(GraphNode::Bus(b))) =
                (self.snarl.get_node(ecu_side), self.snarl.get_node(bus_side))
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
            let sid = g.snarl.insert_node(pos, GraphNode::Ecu(ecu.clone()));
            g.next_node_id = g.next_node_id.max(ecu.id.0 + 1);
            ecu_map.insert(ecu.id, sid);
        }
        for (i, bus) in topo.buses.iter().enumerate() {
            let pos = Pos2::new(80.0, 260.0 + 160.0 * i as f32);
            let sid = g.snarl.insert_node(pos, GraphNode::Bus(bus.clone()));
            g.next_bus_id = g.next_bus_id.max(bus.id.0 + 1);
            bus_map.insert(bus.id, sid);
        }
        for link in &topo.links {
            if let (Some(&ecu_sid), Some(&bus_sid)) =
                (ecu_map.get(&link.node), bus_map.get(&link.bus))
            {
                g.snarl.connect(
                    OutPinId {
                        node: ecu_sid,
                        output: 0,
                    },
                    InPinId {
                        node: bus_sid,
                        input: 0,
                    },
                );
            }
        }
        g
    }
}

fn tx(name: &str, id: u32, period_ms: u32) -> TxMessage {
    TxMessage {
        name: name.to_string(),
        frame: operow_core::CanFrame::new(id, false, &[0; 8]).unwrap(),
        period_ms,
        enabled: true,
    }
}

/// Actions requested from within the graph viewer that the app needs to
/// apply outside of the immediate `show` borrow (e.g. opening menus).
#[derive(Default)]
pub struct GraphActions {
    pub select: Option<SnarlId>,
    pub delete: Option<SnarlId>,
}

pub struct GraphViewer<'a> {
    pub theme: AppTheme,
    pub running: bool,
    pub actions: &'a mut GraphActions,
    pub pending_add_ecu: &'a mut Option<Pos2>,
    pub pending_add_bus: &'a mut Option<Pos2>,
}

impl<'a> SnarlViewer<GraphNode> for GraphViewer<'a> {
    fn title(&mut self, node: &GraphNode) -> String {
        match node {
            GraphNode::Ecu(_) => format!("🖳 {}", node.name()),
            GraphNode::Bus(_) => format!("▬ {}", node.name()),
        }
    }

    fn inputs(&mut self, node: &GraphNode) -> usize {
        match node {
            GraphNode::Ecu(_) => 0,
            GraphNode::Bus(_) => 1,
        }
    }

    fn outputs(&mut self, node: &GraphNode) -> usize {
        match node {
            GraphNode::Ecu(_) => 1,
            GraphNode::Bus(_) => 0,
        }
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<GraphNode>,
    ) -> impl SnarlPin + 'static {
        let _ = (ui, snarl, pin);
        PinInfo::circle()
            .with_fill(self.theme.bus_color(0))
            .with_stroke(Stroke::new(1.0, Color32::BLACK))
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<GraphNode>,
    ) -> impl SnarlPin + 'static {
        let _ = (ui, snarl, pin);
        PinInfo::circle()
            .with_fill(self.theme.bus_color(1))
            .with_stroke(Stroke::new(1.0, Color32::BLACK))
    }

    fn has_body(&mut self, node: &GraphNode) -> bool {
        matches!(node, GraphNode::Ecu(e) if !e.tx.is_empty())
    }

    fn show_body(
        &mut self,
        node: SnarlId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<GraphNode>,
    ) {
        if let Some(GraphNode::Ecu(e)) = snarl.get_node(node) {
            ui.label(
                egui::RichText::new(format!("{} msg(s)", e.tx.len()))
                    .small()
                    .weak(),
            );
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<GraphNode>) {
        if self.running {
            return;
        }
        // Only allow ECU output -> Bus input connections (enforced by
        // pin counts already, since only ECUs have outputs and only
        // buses have inputs).
        snarl.connect(from.id, to.id);
    }

    fn has_graph_menu(&mut self, _pos: Pos2, _snarl: &mut Snarl<GraphNode>) -> bool {
        !self.running
    }

    fn show_graph_menu(&mut self, pos: Pos2, ui: &mut egui::Ui, _snarl: &mut Snarl<GraphNode>) {
        ui.set_min_width(160.0);
        if ui.button("Add ECU").clicked() {
            *self.pending_add_ecu = Some(pos);
            ui.close();
        }
        if ui.button("Add CAN Bus").clicked() {
            *self.pending_add_bus = Some(pos);
            ui.close();
        }
    }

    fn has_node_menu(&mut self, _node: &GraphNode) -> bool {
        !self.running
    }

    fn show_node_menu(
        &mut self,
        node: SnarlId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        _snarl: &mut Snarl<GraphNode>,
    ) {
        ui.set_min_width(120.0);
        if ui.button("Properties").clicked() {
            self.actions.select = Some(node);
            ui.close();
        }
        if ui.button("Delete").clicked() {
            self.actions.delete = Some(node);
            ui.close();
        }
    }

    fn draw_background(
        &mut self,
        background: Option<&egui_snarl::ui::BackgroundPattern>,
        viewport: &egui::Rect,
        snarl_style: &SnarlStyle,
        style: &egui::Style,
        painter: &egui::Painter,
        snarl: &Snarl<GraphNode>,
    ) {
        let _ = snarl;
        if let Some(bg) = background {
            bg.draw(viewport, snarl_style, style, painter);
        }
    }
}

/// A dark/light-aware snarl style.
pub fn snarl_style(theme: AppTheme) -> SnarlStyle {
    let mut style = SnarlStyle::new();
    style.bg_pattern_stroke = Some(match theme {
        AppTheme::Light => Stroke::new(1.0, Color32::from_gray(210)),
        AppTheme::Dark => Stroke::new(1.0, Color32::from_gray(60)),
    });
    style
}
