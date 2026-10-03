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
    /// Bus to transmit on. `None` sends on every bus the node is linked to.
    #[serde(default)]
    pub bus: Option<BusId>,
    /// When the message is transmitted (CANoe Interaction-Layer style).
    #[serde(default)]
    pub send_type: SendType,
}

/// Transmission trigger of a [`TxMessage`]. `enabled == false` suppresses
/// every send type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SendType {
    /// Sent at t=0 and then every `period_ms`.
    #[default]
    Cyclic,
    /// Sent only when triggered.
    #[serde(alias = "Spontaneous")]
    Event,
    /// Sent when the payload is set to a different value; sends closer than
    /// `min_gap_ms` to the previous one are deferred and coalesced.
    OnChange { min_gap_ms: u32 },
    /// Cyclic, but only while the message is active.
    CyclicIfActive,
    /// Cyclic plus an extra send on trigger or payload set.
    #[serde(alias = "CyclicAndSpontaneous")]
    CyclicAndEvent,
}

/// Selects which frames a [`RouteRule`] applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdFilter {
    /// Every frame.
    Any,
    /// Frames with exactly this id and id format.
    Exact { id: u32, extended: bool },
    /// Frames whose id lies in `lo..=hi`.
    Range { lo: u32, hi: u32 },
    /// Frames where `frame.id & mask == id & mask`.
    Mask { id: u32, mask: u32 },
}

impl IdFilter {
    /// Whether `frame` is selected by this filter.
    pub fn matches(&self, frame: &CanFrame) -> bool {
        match *self {
            IdFilter::Any => true,
            IdFilter::Exact { id, extended } => frame.id == id && frame.extended == extended,
            IdFilter::Range { lo, hi } => (lo..=hi).contains(&frame.id),
            IdFilter::Mask { id, mask } => frame.id & mask == id & mask,
        }
    }
}

/// A gateway forwarding rule: frames arriving on `from_bus` that match
/// `filter` are re-sent on every bus in `to_buses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRule {
    pub from_bus: BusId,
    pub to_buses: Vec<BusId>,
    pub filter: IdFilter,
    /// Replace the frame id when forwarding.
    #[serde(default)]
    pub remap_id: Option<u32>,
    /// Forwarding latency in microseconds.
    #[serde(default)]
    pub delay_us: u32,
}

/// What a node does besides transmitting its own messages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NodeKind {
    /// A plain ECU.
    #[default]
    Ecu,
    /// Forwards frames between the buses it is linked to.
    Gateway { routes: Vec<RouteRule> },
}

/// Static configuration of a simulated ECU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EcuConfig {
    pub id: NodeId,
    pub name: String,
    pub tx: Vec<TxMessage>,
    #[serde(default)]
    pub kind: NodeKind,
    /// UI canvas position; not used by the simulation itself.
    #[serde(default)]
    pub pos: (f32, f32),
    /// Optional inline Rhai script run alongside the node's built-in behavior.
    #[serde(default)]
    pub script: Option<String>,
}

fn default_data_bitrate() -> u32 {
    2_000_000
}

/// Static configuration of a CAN bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanBusConfig {
    pub id: BusId,
    pub name: String,
    /// Arbitration/nominal bitrate (bit/s). Also the only bitrate used when
    /// `fd_enabled` is `false`.
    pub bitrate: u32,
    /// Whether this bus carries CAN FD frames. When `false`, any FD frame
    /// sent onto it is dropped and counted as an error.
    #[serde(default)]
    pub fd_enabled: bool,
    /// Data-phase bitrate (bit/s) used by FD frames with BRS set. Ignored
    /// when `fd_enabled` is `false`.
    #[serde(default = "default_data_bitrate")]
    pub data_bitrate: u32,
}

/// Attaches a node to a bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub node: NodeId,
    pub bus: BusId,
}

/// A DBC database file referenced by a project (CANoe-style: by path, not
/// embedded). Relative paths are resolved against the project file's folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbcRef {
    pub path: String,
    /// The bus this database describes.
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
    /// DBC files attached to buses; used for trace decoding only.
    #[serde(default)]
    pub databases: Vec<DbcRef>,
    /// Opaque UI workspace (window layout) saved with the project. The core
    /// crate does not interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<serde_json::Value>,
}

/// Errors returned by [`Topology::validate`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopologyError {
    #[error("link references unknown node {0:?}")]
    UnknownNode(NodeId),
    #[error("link references unknown bus {0:?}")]
    UnknownBus(BusId),
    #[error("database {path:?} references unknown bus {bus:?}")]
    DatabaseUnknownBus { path: String, bus: BusId },
    #[error("bus {0:?} has a non-positive bitrate")]
    InvalidBitrate(BusId),
    #[error("bus {0:?} has FD enabled but a non-positive data bitrate")]
    InvalidDataBitrate(BusId),
    #[error("node {node:?} transmits on bus {bus:?} which it is not linked to")]
    TxBusNotLinked { node: NodeId, bus: BusId },
    #[error("gateway {node:?} routes via bus {bus:?} which it is not linked to")]
    RouteBusNotLinked { node: NodeId, bus: BusId },
    #[error("gateway {node:?} routes bus {bus:?} back onto itself")]
    RouteToSameBus { node: NodeId, bus: BusId },
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

    /// Check that every link references a node/bus that exists, that every
    /// bus has a positive bitrate, and that tx buses and gateway routes only
    /// use buses their node is linked to.
    pub fn validate(&self) -> Result<(), TopologyError> {
        for bus in &self.buses {
            if bus.bitrate == 0 {
                return Err(TopologyError::InvalidBitrate(bus.id));
            }
            if bus.fd_enabled && bus.data_bitrate == 0 {
                return Err(TopologyError::InvalidDataBitrate(bus.id));
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
        for db in &self.databases {
            if !self.buses.iter().any(|b| b.id == db.bus) {
                return Err(TopologyError::DatabaseUnknownBus {
                    path: db.path.clone(),
                    bus: db.bus,
                });
            }
        }
        for node in &self.nodes {
            let linked = |bus: BusId| self.links.iter().any(|l| l.node == node.id && l.bus == bus);
            for bus in node.tx.iter().filter_map(|m| m.bus) {
                if !linked(bus) {
                    return Err(TopologyError::TxBusNotLinked { node: node.id, bus });
                }
            }
            if let NodeKind::Gateway { routes } = &node.kind {
                for route in routes {
                    for &bus in std::iter::once(&route.from_bus).chain(&route.to_buses) {
                        if !linked(bus) {
                            return Err(TopologyError::RouteBusNotLinked { node: node.id, bus });
                        }
                    }
                    if route.to_buses.contains(&route.from_bus) {
                        return Err(TopologyError::RouteToSameBus {
                            node: node.id,
                            bus: route.from_bus,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}
