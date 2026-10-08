//! Core data types shared across Operow: CAN frames, network topology and
//! simulation events. No simulation logic lives here.

mod event;
mod frame;
mod idexpr;
mod ids;
mod topology;

pub use event::{BusEvent, BusEventKind, CanErrorKind, Direction, NodeErrorState};
pub use frame::{CanFrame, EthFrame, Frame, FrameError, dlc_to_len, is_valid_fd_len, len_to_dlc};
pub use idexpr::IdExpr;
pub use ids::{BusId, NodeId, Timestamp};
pub use topology::{
    BusKind, CanBusConfig, DbcRef, DiagConfig, DidEntry, Domain, DtcEntry, EcuConfig, HwBinding,
    IdFilter, KeyAlgo, Link, NodeKind, RouteRule, SecurityConfig, SendType, SignalByteOrder,
    Topology, TopologyError, TopologyJsonError, TxMessage, UserSignalDef, UserSignalId, WireArrow,
    WireKind, WireLine, WireOverride, WireStyle,
};

#[cfg(test)]
mod tests;
