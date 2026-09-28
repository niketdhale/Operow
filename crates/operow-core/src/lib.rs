//! Core data types shared across Operow: CAN frames, network topology and
//! simulation events. No simulation logic lives here.

mod event;
mod frame;
mod ids;
mod topology;

pub use event::{BusEvent, Direction};
pub use frame::{CanFrame, FrameError};
pub use ids::{BusId, NodeId, Timestamp};
pub use topology::{
    CanBusConfig, EcuConfig, Link, Topology, TopologyError, TopologyJsonError, TxMessage,
};

#[cfg(test)]
mod tests;
