//! Core data types shared across Operow: CAN frames, network topology and
//! simulation events. No simulation logic lives here.

mod event;
mod frame;
mod idexpr;
mod ids;
mod topology;

pub use event::{BusEvent, Direction};
pub use frame::{CanFrame, FrameError, dlc_to_len, is_valid_fd_len, len_to_dlc};
pub use idexpr::IdExpr;
pub use ids::{BusId, NodeId, Timestamp};
pub use topology::{
    CanBusConfig, DbcRef, EcuConfig, IdFilter, Link, NodeKind, RouteRule, SendType,
    SignalByteOrder, Topology, TopologyError, TopologyJsonError, TxMessage, UserSignalDef,
    UserSignalId,
};

#[cfg(test)]
mod tests;
