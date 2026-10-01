//! Canvas configuration.

use crate::types::EdgeKind;

/// Pattern drawn behind the graph.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Background {
    None,
    #[default]
    Dots,
    Lines,
    Cross,
}

/// Behaviour switches for [`Flow`](crate::Flow). Every field has a sensible
/// default, so override with struct-update syntax:
/// `FlowOptions { minimap: true, ..Default::default() }`.
#[derive(Clone, Debug)]
pub struct FlowOptions {
    pub background: Background,
    /// Background pattern spacing in flow units.
    pub background_gap: f32,
    /// Snap dragged nodes to a grid of this size (flow units).
    pub snap_to_grid: Option<f32>,
    pub min_zoom: f32,
    pub max_zoom: f32,
    /// Mouse wheel zooms (like React Flow). When false the wheel pans and
    /// only ctrl+wheel / pinch zooms.
    pub zoom_on_scroll: bool,
    pub minimap: bool,
    pub controls: bool,
    pub default_edge_kind: EdgeKind,
    pub nodes_draggable: bool,
    pub nodes_connectable: bool,
    pub elements_selectable: bool,
    /// Delete/Backspace removes the selection.
    pub delete_key: bool,
    pub allow_self_loops: bool,
    /// Frame all nodes on the first frame.
    pub fit_view_on_init: bool,
    pub fit_view_padding: f32,
    /// How close (flow units) a dragged connection must get to a handle to snap to it.
    pub connection_radius: f32,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self {
            background: Background::Dots,
            background_gap: 20.0,
            snap_to_grid: None,
            min_zoom: 0.1,
            max_zoom: 4.0,
            zoom_on_scroll: true,
            minimap: false,
            controls: true,
            default_edge_kind: EdgeKind::Bezier,
            nodes_draggable: true,
            nodes_connectable: true,
            elements_selectable: true,
            delete_key: true,
            allow_self_loops: false,
            fit_view_on_init: false,
            fit_view_padding: 50.0,
            connection_radius: 20.0,
        }
    }
}
