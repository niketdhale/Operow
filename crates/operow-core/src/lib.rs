//! Core data types shared across Operow: CAN frames, network topology and
//! simulation events. No simulation logic lives here.

mod event;
mod frame;
mod ids;
mod topology;

pub use event::{BusEvent, Direction};
pub use frame::{CanFrame, FrameError, dlc_to_len, is_valid_fd_len, len_to_dlc};
pub use ids::{BusId, NodeId, Timestamp};
pub use topology::{
    CanBusConfig, DbcRef, EcuConfig, IdFilter, Link, NodeKind, RouteRule, SendType, Topology,
    TopologyError, TopologyJsonError, TxMessage,
};

#[cfg(test)]
mod tests;
