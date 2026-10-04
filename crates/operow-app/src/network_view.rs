//! The two layouts of the Network window.
//!
//! * Free-form: nodes sit anywhere and every node has one handle.
//! * Bus-line: buses are wide horizontal bars and every attached node gets
//!   its own handle on the bar, so wires are short vertical drops.
//!
//! The pure parts (handle placement, auto-arrange, edge looks, the
//! persisted layout) live here so they can be tested without a UI.

use std::collections::HashMap;

use egui::{Pos2, Vec2, pos2, vec2};
use egui_flow::{EdgeId, FlowState, Handle, HandleId, NodeId as FlowId, Side};
use operow_core::{BusId, EcuConfig, IdFilter, NodeKind, RouteRule};
use serde::{Deserialize, Serialize};

use crate::graph::GraphNode;

/// Height of a bus bar in bus-line mode.
pub const BUS_HEIGHT: f32 = 28.0;
/// Narrowest a bus bar can be resized to.
pub const BUS_MIN_WIDTH: f32 = 160.0;
/// Widest a bus bar can be resized to.
pub const BUS_MAX_WIDTH: f32 = 8000.0;
/// Width of a bus bar that has no saved size.
pub const BUS_DEFAULT_WIDTH: f32 = 640.0;
/// Vertical gap between a bar and the row of nodes next to it.
const ROW_GAP: f32 = 80.0;
/// Horizontal gap between nodes of one row.
const NODE_GAP: f32 = 36.0;
/// Space between a bar's end and its outermost node.
const MARGIN_X: f32 = 40.0;
/// Fallback size of a node that has not been measured yet.
const FALLBACK_NODE: Vec2 = vec2(150.0, 40.0);

/// How the Network window arranges its nodes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkView {
    FreeForm,
    #[default]
    BusLine,
}

impl NetworkView {
    pub const ALL: [NetworkView; 2] = [NetworkView::FreeForm, NetworkView::BusLine];

    pub fn label(self) -> &'static str {
        match self {
            NetworkView::FreeForm => "Free-form",
            NetworkView::BusLine => "Bus-line",
        }
    }

    /// Parses `freeform` / `free-form` / `busline` / `bus-line` (any case).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "freeform" | "free" => Some(NetworkView::FreeForm),
            "busline" | "bus" | "line" => Some(NetworkView::BusLine),
            _ => None,
        }
    }
}

/// Identifies a canvas node by its simulation id, so a layout survives the
/// canvas ids being reallocated on load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NodeKey {
    Ecu(u32),
    Bus(u32),
}

/// Saved position (and size, when resized) of one node.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub x: f32,
    pub y: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f32>,
}

impl Place {
    pub fn pos(&self) -> Pos2 {
        pos2(self.x, self.y)
    }

    /// The saved size, when both sides were stored.
    pub fn size(&self) -> Option<Vec2> {
        Some(vec2(self.w?, self.h?))
    }
}

/// What the workspace file stores for the Network window: the active view
/// plus one set of positions per view, so toggling never scrambles either.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkLayout {
    #[serde(default)]
    pub mode: NetworkView,
    #[serde(default)]
    pub free: Vec<(NodeKey, Place)>,
    #[serde(default)]
    pub line: Vec<(NodeKey, Place)>,
}

/// Handle id of the wire between an ECU and a bus, as seen from the other
/// end: the ECU's handle for bus `n`, the bus's handle for ECU `n`.
pub fn link_handle(n: u32) -> HandleId {
    HandleId(0x1000u32.saturating_add(n))
}

/// Spare target handle on a bus's bottom edge (the top one is
/// `Handle::DEFAULT_TARGET`), for wires that do not exist yet.
pub const BUS_SPARE_BOTTOM: HandleId = HandleId(2);

/// Where along a bar (`0.0..=1.0`) the handle sits that is dropped from flow
/// x `anchor_x`, so the wire from a node above or below is vertical.
pub fn bus_handle_offset(bar_x: f32, bar_w: f32, anchor_x: f32) -> f32 {
    if bar_w <= 0.0 {
        return 0.5;
    }
    ((anchor_x - bar_x) / bar_w).clamp(0.0, 1.0)
}

/// Offsets for `n` handles sharing one side of a node: `1/(n+1) ..= n/(n+1)`.
pub fn spread_offsets(n: usize) -> Vec<f32> {
    (1..=n).map(|i| i as f32 / (n + 1) as f32).collect()
}

/// Handles of every node plus the handle ids each edge must use.
#[derive(Debug, Default, Clone)]
pub struct HandlePlan {
    pub nodes: HashMap<FlowId, Vec<Handle>>,
    pub edges: HashMap<EdgeId, (HandleId, HandleId)>,
}

struct Wire {
    edge: EdgeId,
    ecu: FlowId,
    bus: FlowId,
    ecu_id: u32,
    bus_id: u32,
    /// The ECU sits above the bar.
    above: bool,
}

/// Which wires of `state` run ECU -> bus, in a stable order.
fn wires(state: &FlowState<GraphNode, ()>) -> Vec<Wire> {
    let mut out = Vec::new();
    for e in &state.edges {
        let (Some(a), Some(b)) = (state.node(e.source), state.node(e.target)) else {
            continue;
        };
        let (GraphNode::Ecu(ecu), GraphNode::Bus(bus)) = (&a.data, &b.data) else {
            continue;
        };
        out.push(Wire {
            edge: e.id,
            ecu: a.id,
            bus: b.id,
            ecu_id: ecu.id.0,
            bus_id: bus.id.0,
            above: a.rect().center().y < b.rect().center().y,
        });
    }
    out.sort_by_key(|w| (w.ecu, w.bus_id));
    out
}

/// Handles for bus-line mode: one handle on a bar per attached node (top
/// side for nodes above, bottom for below), offset to the node's x; one
/// handle per bus on each ECU, spread along the side facing that bus.
pub fn plan_bus_line(state: &FlowState<GraphNode, ()>) -> HandlePlan {
    let mut plan = HandlePlan::default();
    let wires = wires(state);
    let mut bus_handles: HashMap<FlowId, Vec<Handle>> = HashMap::new();
    let mut ecu_handles: HashMap<FlowId, Vec<Handle>> = HashMap::new();

    // Group by ECU and side to spread the handles of a gateway.
    let mut groups: HashMap<(FlowId, bool), Vec<&Wire>> = HashMap::new();
    for w in &wires {
        groups.entry((w.ecu, w.above)).or_default().push(w);
    }
    for ((_, above), group) in &groups {
        let offsets = spread_offsets(group.len());
        for (w, off) in group.iter().zip(offsets) {
            let (Some(ecu), Some(bus)) = (state.node(w.ecu), state.node(w.bus)) else {
                continue;
            };
            let ecu_side = if *above { Side::Bottom } else { Side::Top };
            let bus_side = if *above { Side::Top } else { Side::Bottom };
            let src = link_handle(w.bus_id);
            let tgt = link_handle(w.ecu_id);
            ecu_handles
                .entry(w.ecu)
                .or_default()
                .push(Handle::source(src, ecu_side).with_offset(off));
            let anchor = ecu.position.x + ecu.size.x * off;
            let boff = bus_handle_offset(bus.position.x, bus.size.x, anchor);
            bus_handles
                .entry(w.bus)
                .or_default()
                .push(Handle::target(tgt, bus_side).with_offset(boff));
            plan.edges.insert(w.edge, (src, tgt));
        }
    }

    let lowest_bus = state
        .nodes
        .iter()
        .filter(|n| matches!(n.data, GraphNode::Bus(_)))
        .map(|n| n.rect().center().y)
        .fold(f32::NEG_INFINITY, f32::max);
    for n in &state.nodes {
        match &n.data {
            GraphNode::Ecu(_) => {
                let mut hs = ecu_handles.remove(&n.id).unwrap_or_default();
                hs.sort_by_key(|h| h.id);
                if hs.is_empty() {
                    // Not wired yet: one handle facing the nearest bar.
                    let below_all = n.rect().center().y > lowest_bus;
                    let side = if below_all { Side::Top } else { Side::Bottom };
                    hs.push(Handle::source(Handle::DEFAULT_SOURCE, side));
                }
                plan.nodes.insert(n.id, hs);
            }
            GraphNode::Bus(_) => {
                let mut hs = bus_handles.remove(&n.id).unwrap_or_default();
                hs.sort_by_key(|h| h.id);
                // Spares so a node that is not attached yet has something to drop on.
                hs.push(Handle::target(Handle::DEFAULT_TARGET, Side::Top).with_offset(0.02));
                hs.push(Handle::target(BUS_SPARE_BOTTOM, Side::Bottom).with_offset(0.02));
                plan.nodes.insert(n.id, hs);
            }
        }
    }
    plan
}

/// Lays the nodes out for bus-line mode: buses stacked vertically (in
/// canvas order), each ECU in a row above its first bus, gateways in the
/// row between the buses they bridge, unconnected ECUs below the last bus.
pub fn auto_arrange(state: &mut FlowState<GraphNode, ()>) {
    let buses: Vec<(FlowId, u32)> = state
        .nodes
        .iter()
        .filter_map(|n| match &n.data {
            GraphNode::Bus(b) => Some((n.id, b.id.0)),
            _ => None,
        })
        .collect();
    let bus_index = |flow: FlowId| buses.iter().position(|(f, _)| *f == flow);

    // Band `i` is the row above bus `i`; `loose` holds unconnected ECUs.
    let mut bands: Vec<Vec<FlowId>> = vec![Vec::new(); buses.len()];
    let mut gateways: Vec<Vec<FlowId>> = vec![Vec::new(); buses.len()];
    let mut loose: Vec<FlowId> = Vec::new();
    for n in &state.nodes {
        let GraphNode::Ecu(cfg) = &n.data else {
            continue;
        };
        let mut linked: Vec<usize> = state
            .edges
            .iter()
            .filter(|e| e.source == n.id)
            .filter_map(|e| bus_index(e.target))
            .collect();
        linked.sort_unstable();
        linked.dedup();
        let is_gateway = matches!(cfg.kind, NodeKind::Gateway { .. });
        match (linked.first(), linked.last()) {
            (Some(_), Some(&last)) if is_gateway && linked.len() >= 2 => {
                gateways[last].push(n.id);
            }
            (Some(&first), _) => bands[first].push(n.id),
            _ => loose.push(n.id),
        }
    }
    for (band, gws) in bands.iter_mut().zip(gateways) {
        band.extend(gws);
    }

    let size_of = |state: &FlowState<GraphNode, ()>, id: FlowId| {
        state.node(id).map_or(FALLBACK_NODE, |n| n.size)
    };
    let row_width = |state: &FlowState<GraphNode, ()>, row: &[FlowId]| {
        row.iter().map(|id| size_of(state, *id).x).sum::<f32>()
            + NODE_GAP * row.len().saturating_sub(1) as f32
    };
    let widest = bands
        .iter()
        .chain(std::iter::once(&loose))
        .map(|r| row_width(state, r))
        .fold(0.0, f32::max);
    let bar_w = (widest + 2.0 * MARGIN_X).clamp(BUS_DEFAULT_WIDTH, BUS_MAX_WIDTH);

    let mut y = 0.0;
    let place_row = |state: &mut FlowState<GraphNode, ()>, row: &[FlowId], y: f32| -> f32 {
        let h = row
            .iter()
            .map(|id| size_of(state, *id).y)
            .fold(0.0, f32::max);
        let mut x = MARGIN_X;
        for id in row {
            let size = size_of(state, *id);
            if let Some(n) = state.node_mut(*id) {
                // Bottoms line up so every wire into the bar is equally long.
                n.position = pos2(x, y + h - size.y);
            }
            x += size.x + NODE_GAP;
        }
        h
    };
    for (i, (flow, _)) in buses.iter().enumerate() {
        if !bands[i].is_empty() {
            let h = place_row(state, &bands[i], y);
            y += h + ROW_GAP;
        }
        if let Some(n) = state.node_mut(*flow) {
            n.position = pos2(0.0, y);
            n.fixed_size = Some(vec2(bar_w, BUS_HEIGHT));
            n.size = vec2(bar_w, BUS_HEIGHT);
        }
        y += BUS_HEIGHT + ROW_GAP;
    }
    place_row(state, &loose, y);
}

/// Gives nodes without a saved bus-line position a sensible spot without
/// moving the placed ones: a new bus below everything, a new ECU beside the
/// others above its first bus, an unconnected ECU below everything.
pub fn place_missing(state: &mut FlowState<GraphNode, ()>, missing: &[FlowId]) {
    let is_missing = |id: FlowId| missing.contains(&id);
    for &id in missing {
        let Some(node) = state.node(id) else {
            continue;
        };
        let size = node.size;
        let is_bus = matches!(node.data, GraphNode::Bus(_));
        let placed = |s: &FlowState<GraphNode, ()>| {
            s.nodes
                .iter()
                .filter(|n| n.id != id && !is_missing(n.id))
                .map(|n| n.rect())
                .reduce(|a, b| a.union(b))
        };
        let bounds = placed(state);
        let bottom = bounds.map_or(0.0, |b| b.max.y + ROW_GAP);
        let left = bounds.map_or(0.0, |b| b.min.x);
        if is_bus {
            let w = BUS_DEFAULT_WIDTH;
            if let Some(n) = state.node_mut(id) {
                n.position = pos2(left, bottom);
                n.fixed_size = Some(vec2(w, BUS_HEIGHT));
                n.size = vec2(w, BUS_HEIGHT);
            }
            continue;
        }
        // First wired bus that has a place.
        let bus = state
            .edges
            .iter()
            .filter(|e| e.source == id)
            .filter_map(|e| state.node(e.target))
            .find(|b| !is_missing(b.id))
            .map(|b| b.rect());
        let pos = match bus {
            Some(bar) => {
                let right = state
                    .nodes
                    .iter()
                    .filter(|n| n.id != id && !is_missing(n.id))
                    .filter(|n| {
                        let r = n.rect();
                        r.max.y <= bar.min.y && r.max.y >= bar.min.y - 2.0 * ROW_GAP - r.height()
                    })
                    .map(|n| n.rect().max.x)
                    .fold(bar.min.x + MARGIN_X - NODE_GAP, f32::max);
                pos2(right + NODE_GAP, bar.min.y - ROW_GAP - size.y)
            }
            None => pos2(left + MARGIN_X, bottom),
        };
        if let Some(n) = state.node_mut(id) {
            n.position = pos;
        }
    }
}

/// How one wire is drawn.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EdgeLook {
    /// The node is inactive, so the wire is dashed.
    pub dashed: bool,
    /// Arrowhead at the bus end: some route of this gateway sends here.
    pub arrow_to_bus: bool,
    /// Arrowhead at the gateway end: some route receives from this bus.
    pub arrow_to_ecu: bool,
    /// Route summary such as `100-1FF →`.
    pub label: Option<String>,
    pub gateway: bool,
}

/// A node that sends nothing and forwards nothing: no enabled message, no
/// script and, for a gateway, no routes (typically a DBC-only node); a
/// Replay node is inactive until it has a file and a channel mapping.
pub fn is_inactive(ecu: &EcuConfig) -> bool {
    let active_kind = match &ecu.kind {
        NodeKind::Gateway { routes } => !routes.is_empty(),
        NodeKind::Replay {
            path, channel_map, ..
        } => !path.trim().is_empty() && !channel_map.is_empty(),
        NodeKind::Ecu => false,
    };
    !active_kind && ecu.script.is_none() && !ecu.tx.iter().any(|m| m.enabled)
}

/// Short text for a route's id filter: `100-1FF`, `1A0`, `100/7F0`, `all`.
pub fn filter_label(f: &IdFilter) -> String {
    match *f {
        IdFilter::Any => "all".to_string(),
        IdFilter::Exact { id, extended } => {
            format!("{id:X}{}", if extended { "x" } else { "" })
        }
        IdFilter::Range { lo, hi } => format!("{lo:X}-{hi:X}"),
        IdFilter::Mask { id, mask } => format!("{id:X}/{mask:X}"),
    }
}

/// Filters of the routes sending onto `bus` and of those receiving from it,
/// each as a short list (`a, b +2`).
pub fn route_labels(routes: &[RouteRule], bus: BusId) -> (Option<String>, Option<String>) {
    let join = |filters: Vec<String>| -> Option<String> {
        if filters.is_empty() {
            return None;
        }
        let mut shown: Vec<String> = filters.iter().take(2).cloned().collect();
        if filters.len() > 2 {
            shown.push(format!("+{}", filters.len() - 2));
        }
        Some(shown.join(", "))
    };
    let out = routes
        .iter()
        .filter(|r| r.to_buses.contains(&bus))
        .map(|r| filter_label(&r.filter))
        .collect();
    let inc = routes
        .iter()
        .filter(|r| r.from_bus == bus)
        .map(|r| filter_label(&r.filter))
        .collect();
    (join(out), join(inc))
}

/// How the wire between `ecu` and `bus` looks.
pub fn edge_look(ecu: &EcuConfig, bus: BusId) -> EdgeLook {
    let mut look = EdgeLook {
        dashed: is_inactive(ecu),
        ..Default::default()
    };
    if let NodeKind::Gateway { routes } = &ecu.kind {
        look.gateway = true;
        let (out, inc) = route_labels(routes, bus);
        look.arrow_to_bus = out.is_some();
        look.arrow_to_ecu = inc.is_some();
        look.label = match (inc, out) {
            (None, None) => None,
            (Some(i), None) => Some(format!("\u{2190} {i}")),
            (None, Some(o)) => Some(format!("{o} \u{2192}")),
            (Some(i), Some(o)) => Some(format!("\u{2190} {i}  |  {o} \u{2192}")),
        };
    }
    look
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use operow_core::{CanFrame, TxMessage};

    fn rect_of(g: &Graph, id: FlowId) -> egui::Rect {
        g.state.node(id).unwrap().rect()
    }

    /// Two buses, two ECUs on the first, one on the second, a gateway on both.
    fn sample() -> (Graph, Vec<FlowId>, FlowId, FlowId, FlowId) {
        let mut g = Graph::new();
        let b1 = g.add_bus(Pos2::ZERO);
        let b2 = g.add_bus(Pos2::ZERO);
        let e1 = g.add_ecu(Pos2::ZERO, "Engine");
        let e2 = g.add_ecu(Pos2::ZERO, "Brake");
        let e3 = g.add_ecu(Pos2::ZERO, "Body");
        let gw = g.add_gateway(Pos2::ZERO);
        let lone = g.add_ecu(Pos2::ZERO, "Lone");
        for (a, b) in [(e1, b1), (e2, b1), (e3, b2), (gw, b1), (gw, b2)] {
            g.state.connect(a, b, ()).unwrap();
        }
        (g, vec![e1, e2, e3, lone], gw, b1, b2)
    }

    #[test]
    fn offsets_spread_and_clamp() {
        assert_eq!(spread_offsets(1), vec![0.5]);
        assert_eq!(spread_offsets(2), vec![1.0 / 3.0, 2.0 / 3.0]);
        assert!(spread_offsets(0).is_empty());
        assert_eq!(bus_handle_offset(100.0, 400.0, 200.0), 0.25);
        assert_eq!(bus_handle_offset(100.0, 400.0, 50.0), 0.0);
        assert_eq!(bus_handle_offset(100.0, 400.0, 900.0), 1.0);
        assert_eq!(bus_handle_offset(0.0, 0.0, 5.0), 0.5);
    }

    #[test]
    fn handles_drop_vertically_onto_the_bar() {
        let (mut g, ecus, gw, b1, b2) = sample();
        // Bar b1 at x 0..400, y 200; Engine above at x 100 (150 wide).
        g.state.node_mut(b1).unwrap().position = pos2(0.0, 200.0);
        g.state.node_mut(b1).unwrap().size = vec2(400.0, BUS_HEIGHT);
        g.state.node_mut(b2).unwrap().position = pos2(0.0, 500.0);
        g.state.node_mut(b2).unwrap().size = vec2(400.0, BUS_HEIGHT);
        g.state.node_mut(ecus[0]).unwrap().position = pos2(100.0, 40.0);
        g.state.node_mut(gw).unwrap().position = pos2(100.0, 340.0);
        let plan = plan_bus_line(&g.state);

        // Engine: one bottom handle at the centre; the bar's handle for it
        // sits straight above (x = 100 + 75 = 175 -> offset 175/400).
        let eh = &plan.nodes[&ecus[0]];
        assert_eq!(eh.len(), 1);
        assert_eq!(eh[0].side, Side::Bottom);
        assert_eq!(eh[0].offset, 0.5);
        let engine_id = match g.node(ecus[0]).unwrap() {
            GraphNode::Ecu(e) => e.id.0,
            _ => unreachable!(),
        };
        let bar = &plan.nodes[&b1];
        let on_bar = bar
            .iter()
            .find(|h| h.id == link_handle(engine_id))
            .expect("bar handle for Engine");
        assert_eq!(on_bar.side, Side::Top);
        assert!((on_bar.offset - 175.0 / 400.0).abs() < 1e-6);

        // The gateway sits between the bars: its wire to b1 leaves the top,
        // the one to b2 the bottom, each from the middle of its side.
        let gh = &plan.nodes[&gw];
        assert_eq!(gh.len(), 2);
        assert!(gh.iter().any(|h| h.side == Side::Top && h.offset == 0.5));
        assert!(gh.iter().any(|h| h.side == Side::Bottom && h.offset == 0.5));
        let gw_id = match g.node(gw).unwrap() {
            GraphNode::Ecu(e) => e.id.0,
            _ => unreachable!(),
        };
        let on_b2 = plan.nodes[&b2]
            .iter()
            .find(|h| h.id == link_handle(gw_id))
            .unwrap();
        assert_eq!(on_b2.side, Side::Top, "the gateway is above b2");
        // Every edge got matching handle ids.
        assert_eq!(plan.edges.len(), g.state.edges.len());
    }

    #[test]
    fn gateway_handles_to_two_buses_on_one_side_are_distinct() {
        let mut g = Graph::new();
        let b1 = g.add_bus(Pos2::ZERO);
        let b2 = g.add_bus(Pos2::ZERO);
        let gw = g.add_gateway(Pos2::ZERO);
        g.state.connect(gw, b1, ()).unwrap();
        g.state.connect(gw, b2, ()).unwrap();
        // Both bars above the gateway.
        g.state.node_mut(b1).unwrap().position = pos2(0.0, 0.0);
        g.state.node_mut(b2).unwrap().position = pos2(0.0, 100.0);
        g.state.node_mut(gw).unwrap().position = pos2(0.0, 300.0);
        let plan = plan_bus_line(&g.state);
        let hs = &plan.nodes[&gw];
        assert_eq!(hs.len(), 2);
        assert!(hs.iter().all(|h| h.side == Side::Top));
        assert_ne!(hs[0].offset, hs[1].offset);
        assert_ne!(hs[0].id, hs[1].id);
        let mut offs: Vec<f32> = hs.iter().map(|h| h.offset).collect();
        offs.sort_by(f32::total_cmp);
        assert_eq!(offs, spread_offsets(2));
    }

    #[test]
    fn unwired_ecu_gets_a_default_handle_facing_the_bars() {
        let mut g = Graph::new();
        let bus = g.add_bus(Pos2::ZERO);
        let above = g.add_ecu(Pos2::ZERO, "A");
        let below = g.add_ecu(Pos2::ZERO, "B");
        g.state.node_mut(bus).unwrap().position = pos2(0.0, 200.0);
        g.state.node_mut(above).unwrap().position = pos2(0.0, 0.0);
        g.state.node_mut(below).unwrap().position = pos2(0.0, 400.0);
        let plan = plan_bus_line(&g.state);
        assert_eq!(plan.nodes[&above][0].side, Side::Bottom);
        assert_eq!(plan.nodes[&below][0].side, Side::Top);
        assert_eq!(plan.nodes[&above][0].id, Handle::DEFAULT_SOURCE);
    }

    #[test]
    fn auto_arrange_has_no_overlaps_and_gateways_between_buses() {
        let (mut g, ecus, gw, b1, b2) = sample();
        auto_arrange(&mut g.state);
        let ids: Vec<FlowId> = g.state.nodes.iter().map(|n| n.id).collect();
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert!(
                    !rect_of(&g, *a).intersects(rect_of(&g, *b)),
                    "{a:?} overlaps {b:?}"
                );
            }
        }
        // Buses are stacked bars of the standard height.
        let (r1, r2) = (rect_of(&g, b1), rect_of(&g, b2));
        assert_eq!(r1.height(), BUS_HEIGHT);
        assert!(r2.min.y > r1.max.y);
        // ECUs of bus 1 are above it, the ECU of bus 2 above that one.
        assert!(rect_of(&g, ecus[0]).max.y < r1.min.y);
        assert!(rect_of(&g, ecus[1]).max.y < r1.min.y);
        assert!(rect_of(&g, ecus[2]).max.y < r2.min.y);
        assert!(rect_of(&g, ecus[2]).min.y > r1.max.y);
        // The gateway sits between the two bars.
        let gy = rect_of(&g, gw).center().y;
        assert!(gy > r1.center().y && gy < r2.center().y);
        // The unconnected ECU is below the last bar.
        assert!(rect_of(&g, ecus[3]).min.y > r2.max.y);
        // Running it again changes nothing.
        let before: Vec<Pos2> = g.state.nodes.iter().map(|n| n.position).collect();
        auto_arrange(&mut g.state);
        let after: Vec<Pos2> = g.state.nodes.iter().map(|n| n.position).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn missing_nodes_are_placed_without_moving_the_others() {
        let (mut g, ecus, _gw, b1, _b2) = sample();
        auto_arrange(&mut g.state);
        let before = rect_of(&g, ecus[0]);
        let extra = g.add_ecu(Pos2::ZERO, "Extra");
        g.state.connect(extra, b1, ()).unwrap();
        let bus3 = g.add_bus(Pos2::ZERO);
        place_missing(&mut g.state, &[extra, bus3]);
        assert_eq!(rect_of(&g, ecus[0]), before);
        let bar = rect_of(&g, b1);
        assert!(rect_of(&g, extra).max.y < bar.min.y);
        for n in &g.state.nodes {
            if n.id != extra {
                assert!(!n.rect().intersects(rect_of(&g, extra)), "{:?}", n.id);
            }
        }
        assert!(rect_of(&g, bus3).min.y > rect_of(&g, b1).max.y);
    }

    #[test]
    fn view_names_parse() {
        assert_eq!(NetworkView::parse("freeform"), Some(NetworkView::FreeForm));
        assert_eq!(NetworkView::parse("Free-form"), Some(NetworkView::FreeForm));
        assert_eq!(NetworkView::parse("bus-line"), Some(NetworkView::BusLine));
        assert_eq!(NetworkView::parse("nope"), None);
        assert_eq!(NetworkView::default(), NetworkView::BusLine);
    }

    fn ecu_with(msgs: &[(bool, u32)]) -> EcuConfig {
        EcuConfig {
            id: operow_core::NodeId(1),
            name: "E".into(),
            tx: msgs
                .iter()
                .map(|&(enabled, id)| TxMessage {
                    name: "M".into(),
                    frame: CanFrame::new(id, false, &[0]).unwrap(),
                    period_ms: 10,
                    enabled,
                    bus: None,
                    send_type: Default::default(),
                })
                .collect(),
            kind: NodeKind::Ecu,
            pos: (0.0, 0.0),
            script: None,
            diag: None,
        }
    }

    #[test]
    fn inactive_nodes_have_dashed_wires() {
        assert!(edge_look(&ecu_with(&[]), BusId(1)).dashed);
        assert!(edge_look(&ecu_with(&[(false, 1)]), BusId(1)).dashed);
        assert!(!edge_look(&ecu_with(&[(false, 1), (true, 2)]), BusId(1)).dashed);
        let mut scripted = ecu_with(&[]);
        scripted.script = Some("fn x() {}".into());
        assert!(!edge_look(&scripted, BusId(1)).dashed);
    }

    #[test]
    fn gateway_wires_show_route_summary_and_arrowheads() {
        let mut gw = ecu_with(&[]);
        gw.kind = NodeKind::Gateway {
            routes: vec![
                RouteRule {
                    from_bus: BusId(1),
                    to_buses: vec![BusId(2)],
                    filter: IdFilter::Range {
                        lo: 0x100,
                        hi: 0x1FF,
                    },
                    remap_id: None,
                    delay_us: 0,
                },
                RouteRule {
                    from_bus: BusId(2),
                    to_buses: vec![BusId(1)],
                    filter: IdFilter::Exact {
                        id: 0x200,
                        extended: false,
                    },
                    remap_id: None,
                    delay_us: 0,
                },
            ],
        };
        // Wire to bus 2: the first route sends there.
        let to2 = edge_look(&gw, BusId(2));
        assert!(to2.gateway && to2.arrow_to_bus && to2.arrow_to_ecu);
        assert_eq!(
            to2.label.as_deref(),
            Some("\u{2190} 200  |  100-1FF \u{2192}")
        );
        assert!(!to2.dashed, "a gateway with routes is active");
        let (out, inc) = route_labels(
            match &gw.kind {
                NodeKind::Gateway { routes } => routes,
                _ => unreachable!(),
            },
            BusId(1),
        );
        assert_eq!(out.as_deref(), Some("200"));
        assert_eq!(inc.as_deref(), Some("100-1FF"));
        // No routes: no label, no arrowheads, but dashed.
        let empty = EcuConfig {
            kind: NodeKind::Gateway { routes: vec![] },
            ..ecu_with(&[])
        };
        let look = edge_look(&empty, BusId(1));
        assert!(look.label.is_none() && !look.arrow_to_bus && look.dashed);
        // Plain ECUs never get a label.
        assert!(edge_look(&ecu_with(&[(true, 1)]), BusId(1)).label.is_none());
    }

    #[test]
    fn filter_labels() {
        assert_eq!(filter_label(&IdFilter::Any), "all");
        assert_eq!(
            filter_label(&IdFilter::Exact {
                id: 0x1A0,
                extended: true
            }),
            "1A0x"
        );
        assert_eq!(
            filter_label(&IdFilter::Mask {
                id: 0x100,
                mask: 0x7F0
            }),
            "100/7F0"
        );
        let many: Vec<RouteRule> = (0..4)
            .map(|i| RouteRule {
                from_bus: BusId(1),
                to_buses: vec![BusId(2)],
                filter: IdFilter::Exact {
                    id: i,
                    extended: false,
                },
                remap_id: None,
                delay_us: 0,
            })
            .collect();
        assert_eq!(route_labels(&many, BusId(2)).0.as_deref(), Some("0, 1, +2"));
    }
}
