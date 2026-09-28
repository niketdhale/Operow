use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::frame::CanFrame;
use crate::ids::{BusId, NodeId};

/// A periodically (or manually) transmitted message owned by an ECU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxMessage {
    pub name: String,
    pub frame: CanFrame,
    pub period_ms: u32,
    pub enabled: bool,
}

/// Static configuration of a simulated ECU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EcuConfig {
    pub id: NodeId,
    pub name: String,
    pub tx: Vec<TxMessage>,
    /// UI canvas position; not used by the simulation itself.
    #[serde(default)]
    pub pos: (f32, f32),
}

/// Static configuration of a CAN bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanBusConfig {
    pub id: BusId,
    pub name: String,
    pub bitrate: u32,
}

/// Attaches a node to a bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub node: NodeId,
    pub bus: BusId,
}

/// The full static network description of a simulation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    #[serde(default)]
    pub nodes: Vec<EcuConfig>,
    #[serde(default)]
    pub buses: Vec<CanBusConfig>,
    #[serde(default)]
    pub links: Vec<Link>,
}

/// Errors returned by [`Topology::validate`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopologyError {
    #[error("link references unknown node {0:?}")]
    UnknownNode(NodeId),
    #[error("link references unknown bus {0:?}")]
    UnknownBus(BusId),
    #[error("bus {0:?} has a non-positive bitrate")]
    InvalidBitrate(BusId),
}

/// Errors returned by [`Topology::from_json`].
#[derive(Debug, Error)]
pub enum TopologyJsonError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl Topology {
    /// Serialize this topology to a pretty JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("Topology serialization cannot fail")
    }

    /// Deserialize a topology from a JSON string.
    pub fn from_json(s: &str) -> Result<Self, TopologyJsonError> {
        Ok(serde_json::from_str(s)?)
    }

    /// Check that every link references a node/bus that exists, and that
    /// every bus has a positive bitrate.
    pub fn validate(&self) -> Result<(), TopologyError> {
        for bus in &self.buses {
            if bus.bitrate == 0 {
                return Err(TopologyError::InvalidBitrate(bus.id));
            }
        }
        for link in &self.links {
            if !self.nodes.iter().any(|n| n.id == link.node) {
                return Err(TopologyError::UnknownNode(link.node));
            }
            if !self.buses.iter().any(|b| b.id == link.bus) {
                return Err(TopologyError::UnknownBus(link.bus));
            }
        }
        Ok(())
    }
}
