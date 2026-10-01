//! The graph model owned by the application.

use egui::{Pos2, Rect, Vec2, vec2};

use crate::types::*;

/// Transient pointer interaction, kept between frames.
pub(crate) struct ConnectDrag {
    pub node: NodeId,
    pub handle: HandleId,
}

#[derive(Default)]
pub(crate) struct Interaction {
    pub node_drag: Option<NodeDrag>,
    pub box_select: Option<Pos2>,
    pub connecting: Option<ConnectDrag>,
}

pub(crate) struct NodeDrag {
    pub origins: Vec<(NodeId, Pos2)>,
    pub accum: Vec2,
}

/// Nodes, edges, viewport and selection. Mutate freely between frames; pass
/// to [`Flow::show`](crate::Flow::show) every frame.
pub struct FlowState<N, E> {
    pub nodes: Vec<Node<N>>,
    pub edges: Vec<Edge<E>>,
    pub viewport: Viewport,
    next_node: u64,
    next_edge: u64,
    pub(crate) fit_frames: u8,
    pub(crate) initialized: bool,
    pub(crate) interaction: Interaction,
}

impl<N, E> Default for FlowState<N, E> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            viewport: Viewport::default(),
            next_node: 1,
            next_edge: 1,
            fit_frames: 0,
            initialized: false,
            interaction: Interaction::default(),
        }
    }
}

impl<N, E> FlowState<N, E> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node with a fresh id.
    pub fn add_node(&mut self, position: Pos2, data: N) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node += 1;
        self.nodes.push(Node::new(id, position, data));
        id
    }

    /// Add a pre-built node, keeping its id. The id counter is advanced past it.
    /// Returns `false` (and drops the node) if the id is already taken.
    pub fn insert_node(&mut self, node: Node<N>) -> bool {
        if self.node(node.id).is_some() {
            return false;
        }
        self.next_node = self.next_node.max(node.id.0 + 1);
        self.nodes.push(node);
        true
    }

    pub fn node(&self, id: NodeId) -> Option<&Node<N>> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node<N>> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn edge(&self, id: EdgeId) -> Option<&Edge<E>> {
        self.edges.iter().find(|e| e.id == id)
    }

    pub fn edge_mut(&mut self, id: EdgeId) -> Option<&mut Edge<E>> {
        self.edges.iter_mut().find(|e| e.id == id)
    }

    /// Add an edge between two nodes' handles. Fails if either node is
    /// missing or an identical edge already exists. This does *not* check
    /// handle kinds; interactive connections are validated by the canvas.
    pub fn add_edge(&mut self, conn: Connection, data: E) -> Option<EdgeId> {
        if self.node(conn.source).is_none() || self.node(conn.target).is_none() {
            return None;
        }
        if self.edges.iter().any(|e| e.connection() == conn) {
            return None;
        }
        let id = EdgeId(self.next_edge);
        self.next_edge += 1;
        self.edges.push(Edge {
            id,
            source: conn.source,
            source_handle: conn.source_handle,
            target: conn.target,
            target_handle: conn.target_handle,
            data,
            kind: None,
            label: None,
            animated: false,
            arrow: false,
            selected: false,
            deletable: true,
        });
        Some(id)
    }

    /// Connect the default source handle of `from` to the default target
    /// handle of `to`.
    pub fn connect(&mut self, from: NodeId, to: NodeId, data: E) -> Option<EdgeId> {
        self.add_edge(
            Connection {
                source: from,
                source_handle: Handle::DEFAULT_SOURCE,
                target: to,
                target_handle: Handle::DEFAULT_TARGET,
            },
            data,
        )
    }

    /// Remove a node and every edge attached to it.
    pub fn remove_node(&mut self, id: NodeId) -> Option<(Node<N>, Vec<Edge<E>>)> {
        let idx = self.nodes.iter().position(|n| n.id == id)?;
        let node = self.nodes.remove(idx);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.edges)
            .into_iter()
            .partition(|e| e.source == id || e.target == id);
        self.edges = kept;
        Some((node, gone))
    }

    pub fn remove_edge(&mut self, id: EdgeId) -> Option<Edge<E>> {
        let idx = self.edges.iter().position(|e| e.id == id)?;
        Some(self.edges.remove(idx))
    }

    pub fn selected_nodes(&self) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|n| n.selected)
            .map(|n| n.id)
            .collect()
    }

    pub fn selected_edges(&self) -> Vec<EdgeId> {
        self.edges
            .iter()
            .filter(|e| e.selected)
            .map(|e| e.id)
            .collect()
    }

    pub fn clear_selection(&mut self) {
        self.nodes.iter_mut().for_each(|n| n.selected = false);
        self.edges.iter_mut().for_each(|e| e.selected = false);
    }

    pub fn select_all(&mut self) {
        self.nodes.iter_mut().for_each(|n| n.selected = true);
        self.edges.iter_mut().for_each(|e| e.selected = true);
    }

    /// Remove every selected, deletable node and edge (plus edges orphaned by
    /// node removal). Returns what was removed.
    pub fn delete_selected(&mut self) -> (Vec<Node<N>>, Vec<Edge<E>>) {
        let node_ids: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| n.selected && n.deletable)
            .map(|n| n.id)
            .collect();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for id in node_ids {
            if let Some((n, es)) = self.remove_node(id) {
                nodes.push(n);
                edges.extend(es);
            }
        }
        let edge_ids: Vec<_> = self
            .edges
            .iter()
            .filter(|e| e.selected && e.deletable)
            .map(|e| e.id)
            .collect();
        for id in edge_ids {
            edges.extend(self.remove_edge(id));
        }
        (nodes, edges)
    }

    /// Smallest rect containing every node, if there are any.
    pub fn bounds(&self) -> Option<Rect> {
        self.nodes.iter().map(Node::rect).reduce(|a, b| a.union(b))
    }

    /// Re-frame the viewport around all nodes on the next frames.
    pub fn fit_view(&mut self) {
        // Two frames: the first lays nodes out and measures them, the second
        // fits using the measured sizes.
        self.fit_frames = 2;
    }

    /// The viewport that frames `bounds` inside a canvas of `canvas_size`.
    pub fn viewport_for(
        bounds: Rect,
        canvas_size: Vec2,
        padding: f32,
        zoom_range: (f32, f32),
    ) -> Viewport {
        let w = (bounds.width() + 2.0 * padding).max(1.0);
        let h = (bounds.height() + 2.0 * padding).max(1.0);
        let zoom = (canvas_size.x / w)
            .min(canvas_size.y / h)
            .clamp(zoom_range.0, zoom_range.1);
        let center = bounds.center();
        Viewport {
            zoom,
            pan: canvas_size / 2.0 - vec2(center.x, center.y) * zoom,
        }
    }

    pub(crate) fn selection_snapshot(&self) -> (Vec<NodeId>, Vec<EdgeId>) {
        (self.selected_nodes(), self.selected_edges())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn two_nodes() -> (FlowState<(), ()>, NodeId, NodeId) {
        let mut s = FlowState::new();
        let a = s.add_node(pos2(0.0, 0.0), ());
        let b = s.add_node(pos2(200.0, 0.0), ());
        (s, a, b)
    }

    #[test]
    fn connect_rejects_duplicates_and_missing() {
        let (mut s, a, b) = two_nodes();
        assert!(s.connect(a, b, ()).is_some());
        assert!(s.connect(a, b, ()).is_none());
        assert!(s.connect(a, NodeId(99), ()).is_none());
    }

    #[test]
    fn removing_node_removes_its_edges() {
        let (mut s, a, b) = two_nodes();
        s.connect(a, b, ());
        let (_, gone) = s.remove_node(a).unwrap();
        assert_eq!(gone.len(), 1);
        assert!(s.edges.is_empty());
    }

    #[test]
    fn delete_selected_respects_deletable() {
        let (mut s, a, b) = two_nodes();
        s.connect(a, b, ());
        s.select_all();
        s.node_mut(b).unwrap().deletable = false;
        let (nodes, edges) = s.delete_selected();
        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 1);
        assert_eq!(s.nodes.len(), 1);
    }

    #[test]
    fn insert_node_advances_id_counter() {
        let mut s: FlowState<(), ()> = FlowState::new();
        assert!(s.insert_node(Node::new(NodeId(10), pos2(0.0, 0.0), ())));
        assert!(!s.insert_node(Node::new(NodeId(10), pos2(0.0, 0.0), ())));
        assert_eq!(s.add_node(pos2(0.0, 0.0), ()), NodeId(11));
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut vp = Viewport {
            pan: vec2(30.0, 10.0),
            zoom: 1.0,
        };
        let anchor = pos2(100.0, 80.0);
        let before = vp.to_flow(anchor);
        vp.zoom_at(anchor, 2.0, 0.1, 4.0);
        let after = vp.to_flow(anchor);
        assert!((before - after).length() < 1e-3);
        assert_eq!(vp.zoom, 2.0);
    }

    #[test]
    fn fit_viewport_centres_bounds() {
        let b = Rect::from_min_size(pos2(100.0, 100.0), vec2(200.0, 100.0));
        let vp = FlowState::<(), ()>::viewport_for(b, vec2(800.0, 600.0), 0.0, (0.1, 4.0));
        let c = vp.to_screen(b.center());
        assert!((c - pos2(400.0, 300.0)).length() < 1e-3);
    }
}
