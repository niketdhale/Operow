use serde::{Deserialize, Serialize};

use crate::frame::CanFrame;
use crate::ids::{BusId, NodeId, Timestamp};

/// Direction of a frame from the point of view of a bus. Exactly one event
/// is recorded per frame per bus, not one per receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// The frame was originated on this bus (`hop == 0`).
    Tx,
    /// The frame arrived on this bus via a gateway (`hop > 0`).
    Rx,
}

/// A single frame placed onto a bus. Receivers are derivable from the
/// topology (every other node linked to `bus`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BusEvent {
    pub time: Timestamp,
    pub bus: BusId,
    /// The node driving the wire (the gateway, for a forwarded frame).
    pub sender: NodeId,
    /// The ECU that first created the frame.
    pub origin: NodeId,
    pub dir: Direction,
    /// Shared by every copy of a frame across buses and gateway hops.
    pub frame_uid: u64,
    /// Number of gateways the frame has crossed.
    pub hop: u8,
    pub frame: CanFrame,
}
