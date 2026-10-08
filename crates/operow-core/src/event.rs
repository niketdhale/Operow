use serde::{Deserialize, Serialize};

use crate::frame::Frame;
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

/// The CAN error types an error frame can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CanErrorKind {
    Bit,
    Stuff,
    Crc,
    Form,
    Ack,
}

impl CanErrorKind {
    /// Every kind, in display order.
    pub const ALL: [CanErrorKind; 5] = [
        CanErrorKind::Bit,
        CanErrorKind::Stuff,
        CanErrorKind::Crc,
        CanErrorKind::Form,
        CanErrorKind::Ack,
    ];

    /// Short human-readable name, e.g. `"CRC"`.
    pub fn label(self) -> &'static str {
        match self {
            CanErrorKind::Bit => "Bit",
            CanErrorKind::Stuff => "Stuff",
            CanErrorKind::Crc => "CRC",
            CanErrorKind::Form => "Form",
            CanErrorKind::Ack => "ACK",
        }
    }
}

/// CAN fault-confinement state of a node on one bus (ISO 11898-1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeErrorState {
    #[default]
    ErrorActive,
    ErrorPassive,
    BusOff,
}

impl NodeErrorState {
    pub fn label(self) -> &'static str {
        match self {
            NodeErrorState::ErrorActive => "Error Active",
            NodeErrorState::ErrorPassive => "Error Passive",
            NodeErrorState::BusOff => "Bus Off",
        }
    }
}

/// What a [`BusEvent`] reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusEventKind {
    /// A frame was transmitted successfully.
    #[default]
    Frame,
    /// An error frame: the transmission of `BusEvent::frame` failed.
    Error {
        error: CanErrorKind,
        /// The node that detected the error and sent the error flag (the
        /// transmitter, for errors the simulator generates).
        node: NodeId,
    },
}

/// A single frame placed onto a bus. Receivers are derivable from the
/// topology (every other node linked to `bus`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// For [`BusEventKind::Error`] events, the frame that was being
    /// transmitted when the error occurred.
    pub frame: Frame,
    /// Frame or error frame. Defaults to `Frame` when deserializing old data.
    #[serde(default)]
    pub kind: BusEventKind,
}

impl BusEvent {
    /// Whether this event is an error frame.
    pub fn is_error(&self) -> bool {
        matches!(self.kind, BusEventKind::Error { .. })
    }

    /// The error kind, for error events.
    pub fn error_kind(&self) -> Option<CanErrorKind> {
        match self.kind {
            BusEventKind::Error { error, .. } => Some(error),
            BusEventKind::Frame => None,
        }
    }
}
