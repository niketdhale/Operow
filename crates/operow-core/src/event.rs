use serde::{Deserialize, Serialize};

use crate::frame::CanFrame;
use crate::ids::{BusId, NodeId, Timestamp};

/// Direction of a message relative to an ECU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Tx,
    Rx,
}

/// A single frame placed onto a bus. Receivers are derivable from the
/// topology (every other node linked to `bus`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BusEvent {
    pub time: Timestamp,
    pub bus: BusId,
    pub sender: NodeId,
    pub frame: CanFrame,
}
