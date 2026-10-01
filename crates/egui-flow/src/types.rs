//! Plain data types: ids, nodes, edges, handles and the viewport.

use egui::{Pos2, Rect, Vec2, pos2, vec2};

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident($inner:ty)) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        pub struct $name(pub $inner);
    };
}

id_type!(
    /// Identifies a node within one [`FlowState`](crate::FlowState).
    NodeId(u64)
);
id_type!(
    /// Identifies an edge within one [`FlowState`](crate::FlowState).
    EdgeId(u64)
);
id_type!(
    /// Identifies a handle (connection port) within one node.
    HandleId(u32)
);

/// Which side of a node a handle sits on. Also gives the direction an edge
/// leaves/enters the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// Outward unit direction from the node.
    pub fn dir(self) -> Vec2 {
        match self {
            Side::Left => vec2(-1.0, 0.0),
            Side::Right => vec2(1.0, 0.0),
            Side::Top => vec2(0.0, -1.0),
            Side::Bottom => vec2(0.0, 1.0),
        }
    }

    pub fn is_horizontal(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }
}

/// Whether a handle starts or ends a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HandleKind {
    Source,
    Target,
}

/// A connection port on a node.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Handle {
    pub id: HandleId,
    pub kind: HandleKind,
    pub side: Side,
    /// Position along `side`, `0.0..=1.0` (default `0.5`, centred).
    pub offset: f32,
}

impl Handle {
    /// Id of the default target handle (left side).
    pub const DEFAULT_TARGET: HandleId = HandleId(0);
    /// Id of the default source handle (right side).
    pub const DEFAULT_SOURCE: HandleId = HandleId(1);

    pub fn source(id: HandleId, side: Side) -> Self {
        Self {
            id,
            kind: HandleKind::Source,
            side,
            offset: 0.5,
        }
    }

    pub fn target(id: HandleId, side: Side) -> Self {
        Self {
            id,
            kind: HandleKind::Target,
            side,
            offset: 0.5,
        }
    }

    pub fn with_offset(mut self, offset: f32) -> Self {
        self.offset = offset.clamp(0.0, 1.0);
        self
    }

    /// Where this handle sits for a node occupying `rect`.
    pub fn position(&self, rect: Rect) -> Pos2 {
        let o = self.offset;
        match self.side {
            Side::Left => pos2(rect.min.x, rect.min.y + rect.height() * o),
            Side::Right => pos2(rect.max.x, rect.min.y + rect.height() * o),
            Side::Top => pos2(rect.min.x + rect.width() * o, rect.min.y),
            Side::Bottom => pos2(rect.min.x + rect.width() * o, rect.max.y),
        }
    }
}

/// How an edge is routed between its two handles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EdgeKind {
    /// Smooth cubic bezier (React Flow's default).
    #[default]
    Bezier,
    /// A direct line.
    Straight,
    /// Right-angled routing.
    Step,
    /// Right-angled routing with rounded corners.
    SmoothStep,
}

/// A node on the canvas. `data` is yours; the library only reads/writes the
/// other fields.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node<D> {
    pub id: NodeId,
    /// Top-left corner in flow coordinates.
    pub position: Pos2,
    /// Size in flow coordinates. Measured from the rendered content every
    /// frame, so this is only an initial estimate for the first frame.
    pub size: Vec2,
    pub data: D,
    pub selected: bool,
    pub draggable: bool,
    pub connectable: bool,
    pub deletable: bool,
}

impl<D> Node<D> {
    pub fn new(id: NodeId, position: Pos2, data: D) -> Self {
        Self {
            id,
            position,
            size: vec2(150.0, 40.0),
            data,
            selected: false,
            draggable: true,
            connectable: true,
            deletable: true,
        }
    }

    pub fn rect(&self) -> Rect {
        Rect::from_min_size(self.position, self.size)
    }
}

/// A connection between a source handle and a target handle.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Edge<D> {
    pub id: EdgeId,
    pub source: NodeId,
    pub source_handle: HandleId,
    pub target: NodeId,
    pub target_handle: HandleId,
    pub data: D,
    /// Overrides the canvas default routing.
    pub kind: Option<EdgeKind>,
    pub label: Option<String>,
    /// Draw marching dashes.
    pub animated: bool,
    /// Draw an arrowhead at the target.
    pub arrow: bool,
    pub selected: bool,
    pub deletable: bool,
}

impl<D> Edge<D> {
    pub fn connection(&self) -> Connection {
        Connection {
            source: self.source,
            source_handle: self.source_handle,
            target: self.target,
            target_handle: self.target_handle,
        }
    }
}

/// The endpoints of a (proposed or existing) edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Connection {
    pub source: NodeId,
    pub source_handle: HandleId,
    pub target: NodeId,
    pub target_handle: HandleId,
}

/// Pan and zoom. A flow-space point `p` is drawn at
/// `canvas.min + pan + p * zoom` on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Viewport {
    pub pan: Vec2,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl Viewport {
    /// Screen position (relative to the canvas top-left) of a flow point.
    pub fn to_screen(&self, p: Pos2) -> Pos2 {
        pos2(p.x * self.zoom + self.pan.x, p.y * self.zoom + self.pan.y)
    }

    /// Flow point under a canvas-relative screen position.
    pub fn to_flow(&self, p: Pos2) -> Pos2 {
        pos2(
            (p.x - self.pan.x) / self.zoom,
            (p.y - self.pan.y) / self.zoom,
        )
    }

    /// Zoom by `factor` keeping the canvas-relative point `anchor` fixed.
    pub fn zoom_at(&mut self, anchor: Pos2, factor: f32, min: f32, max: f32) {
        let new_zoom = (self.zoom * factor).clamp(min, max);
        let flow = self.to_flow(anchor);
        self.zoom = new_zoom;
        self.pan = anchor.to_vec2() - flow.to_vec2() * new_zoom;
    }
}
