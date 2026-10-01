//! The trait applications implement to render node contents.

use egui::{Color32, CornerRadius, Frame, Margin, Ui};

use crate::types::{Connection, Handle, HandleId, Node, Side};

/// Customises how nodes look and which connections are allowed.
///
/// `N` is the node data type and `E` the edge data type.
pub trait FlowViewer<N, E> {
    /// Draw the node's contents. Any egui widget works, including text edits
    /// and buttons. Called inside [`node_frame`](Self::node_frame).
    fn node_ui(&mut self, ui: &mut Ui, node: &mut Node<N>);

    /// Connection ports for a node. Defaults to a target on the left and a
    /// source on the right.
    fn handles(&self, _node: &Node<N>) -> Vec<Handle> {
        vec![
            Handle::target(Handle::DEFAULT_TARGET, Side::Left),
            Handle::source(Handle::DEFAULT_SOURCE, Side::Right),
        ]
    }

    /// Frame drawn around [`node_ui`](Self::node_ui). The selection outline is
    /// added by the canvas, so this only needs fill/stroke/margin.
    fn node_frame(&self, ui: &Ui, _node: &Node<N>) -> Frame {
        let v = ui.visuals();
        Frame::new()
            .fill(v.window_fill)
            .stroke(v.widgets.noninteractive.bg_stroke)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(8))
    }

    /// Extra connection rule applied after the built-in checks (source →
    /// target, no duplicates, optional self-loop ban).
    fn can_connect(&self, _conn: &Connection) -> bool {
        true
    }

    /// Colour for this node in the minimap.
    fn minimap_color(&self, _node: &Node<N>) -> Option<Color32> {
        None
    }

    /// Label shown next to a handle; return `None` for no label.
    fn handle_label(&self, _node: &Node<N>, _handle: HandleId) -> Option<String> {
        None
    }
}
