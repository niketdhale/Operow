//! Things that happened during a frame.

use egui::{Pos2, Response};

use crate::types::*;

/// An interaction the application may want to react to.
#[derive(Clone, Debug)]
pub enum FlowEvent<N, E> {
    NodeClicked(NodeId),
    EdgeClicked(EdgeId),
    /// The empty canvas was clicked (flow coordinates).
    PaneClicked(Pos2),
    /// One or more nodes moved this frame.
    NodesDragged(Vec<NodeId>),
    NodesDragStopped(Vec<NodeId>),
    /// A new edge was created by dragging between handles.
    Connected(EdgeId),
    /// A connection drag was released without creating an edge. `pos` is the
    /// release point in flow coordinates; useful for "drop to add node".
    ConnectionDropped {
        node: NodeId,
        handle: HandleId,
        pos: Pos2,
    },
    /// Delete key removed these (already removed from the state; edges
    /// orphaned by node removal are included).
    Deleted {
        nodes: Vec<Node<N>>,
        edges: Vec<Edge<E>>,
    },
    SelectionChanged {
        nodes: Vec<NodeId>,
        edges: Vec<EdgeId>,
    },
    ViewportChanged(Viewport),
}

/// Result of [`Flow::show`](crate::Flow::show).
pub struct FlowResponse<N, E> {
    /// Response of the empty canvas, e.g. for `.context_menu(..)`.
    pub pane: Response,
    /// Per-node responses, e.g. for context menus or tooltips.
    pub nodes: Vec<(NodeId, Response)>,
    pub events: Vec<FlowEvent<N, E>>,
}

impl<N, E> FlowResponse<N, E> {
    pub fn node_response(&self, id: NodeId) -> Option<&Response> {
        self.nodes.iter().find(|(n, _)| *n == id).map(|(_, r)| r)
    }
}
