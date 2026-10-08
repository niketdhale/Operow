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
    /// Injects the frames of an ASC log onto the buses it is mapped to.
    Replay {
        /// Log file; relative paths resolve against the project folder.
        #[serde(default)]
        path: String,
        /// ASC channel -> bus. Records of other channels are skipped.
        #[serde(default)]
        channel_map: Vec<(u8, BusId)>,
        /// Start over after the last record.
        #[serde(default)]
        looped: bool,
        /// Shift every record in time (milliseconds, may be negative).
        #[serde(default)]
        time_offset_ms: i64,
        /// Only replay these ids, e.g. `100-1FF, 3A0, !7DF`.
        #[serde(default)]
        id_filter: Option<String>,
    },
}

impl NodeKind {
    /// A Replay node with no file and no channel mapping yet.
    pub fn new_replay() -> Self {
        NodeKind::Replay {
            path: String::new(),
            channel_map: Vec::new(),
            looped: false,
            time_offset_ms: 0,
            id_filter: None,
        }
    }
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
    /// Optional simulated UDS diagnostic server (ISO 14229 over ISO-TP).
    #[serde(default)]
    pub diag: Option<DiagConfig>,
}

/// A readable (and optionally writable) data identifier of a diagnostic ECU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DidEntry {
    pub did: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub data: Vec<u8>,
    #[serde(default)]
    pub writable: bool,
}

/// A stored diagnostic trouble code. `code` is the 24-bit DTC (two OBD
/// bytes plus failure type); `status` the ISO 14229 status byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DtcEntry {
    pub code: u32,
    #[serde(default)]
    pub status: u8,
}

/// How the key for SecurityAccess is derived from the seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyAlgo {
    /// Each seed byte is XORed with the constant (repeated cyclically).
    XorConst(Vec<u8>),
    /// The seed as big-endian number plus the constant, truncated to the
    /// seed length.
    AddConst(u32),
    /// The node script's `on_security_key(seed)` returns the key.
    Script,
}

/// SecurityAccess (0x27) settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Seed request level (odd); the key is sent with `level + 1`.
    pub level: u8,
    pub seed: Vec<u8>,
    pub key_algo: KeyAlgo,
}

fn default_req_id() -> u32 {
    0x7E0
}
fn default_resp_id() -> u32 {
    0x7E8
}
fn default_functional_id() -> Option<u32> {
    Some(0x7DF)
}
fn default_sessions() -> Vec<u8> {
    vec![1, 2, 3]
}
fn default_p2_ms() -> u16 {
    50
}
fn default_p2_star_ms() -> u16 {
    5000
}

/// Configuration of the UDS server simulated by an ECU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagConfig {
    /// Bus the server listens on; `None` means every bus the node is on.
    #[serde(default)]
    pub bus: Option<BusId>,
    /// Physical request id (tester to ECU).
    #[serde(default = "default_req_id")]
    pub req_id: u32,
    /// Response id (ECU to tester).
    #[serde(default = "default_resp_id")]
    pub resp_id: u32,
    /// Functional request id; single-frame requests only.
    #[serde(default = "default_functional_id")]
    pub functional_id: Option<u32>,
    #[serde(default)]
    pub extended_ids: bool,
    #[serde(default)]
    pub fd: bool,
    #[serde(default)]
    pub padding: Option<u8>,
    #[serde(default)]
    pub block_size: u8,
    #[serde(default)]
    pub st_min_ms: u8,
    #[serde(default)]
    pub dids: Vec<DidEntry>,
    #[serde(default)]
    pub dtcs: Vec<DtcEntry>,
    #[serde(default = "default_sessions")]
    pub sessions_supported: Vec<u8>,
    #[serde(default)]
    pub security: Option<SecurityConfig>,
    #[serde(default = "default_p2_ms")]
    pub p2_ms: u16,
    #[serde(default = "default_p2_star_ms")]
    pub p2_star_ms: u16,
}

impl Default for DiagConfig {
    fn default() -> Self {
        DiagConfig {
            bus: None,
            req_id: default_req_id(),
            resp_id: default_resp_id(),
            functional_id: default_functional_id(),
            extended_ids: false,
            fd: false,
            padding: None,
            block_size: 0,
            st_min_ms: 0,
            dids: Vec::new(),
            dtcs: Vec::new(),
            sessions_supported: default_sessions(),
            security: None,
            p2_ms: default_p2_ms(),
            p2_star_ms: default_p2_star_ms(),
        }
    }
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
    /// Opt-in ACK modelling. When `true`, a frame transmitted while no other
    /// node on the bus is online (linked and not bus-off) gets no
    /// acknowledgement: the transmitter sees an ACK error, its TEC rises and
    /// the frame is retransmitted. When `false` (the default) a lone node
    /// transmits happily, as in the pre-error-model simulator.
    #[serde(default)]
    pub simulate_ack: bool,
    /// Binds the bus to a real CAN adapter. The engine then runs in real
    /// time and this bus is no longer simulated.
    #[serde(default)]
    pub hardware: Option<HwBinding>,
}

fn default_true() -> bool {
    true
}

/// A real CAN channel a bus is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwBinding {
    /// `driver:channel`, e.g. `socketcan:can0`.
    pub interface: String,
    /// Never transmit onto the real bus (the safe default).
    #[serde(default = "default_true")]
    pub listen_only: bool,
    /// Request the adapter's echo of own frames.
    #[serde(default)]
    pub receive_own: bool,
}

impl HwBinding {
    /// A listen-only binding to `interface`.
    pub fn new(interface: impl Into<String>) -> Self {
        HwBinding {
            interface: interface.into(),
            listen_only: true,
            receive_own: false,
        }
    }
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

/// Identifier of a project-level user signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserSignalId(pub u32);

/// Bit numbering of a [`UserSignalDef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SignalByteOrder {
    /// Little-endian (`@1` in DBC).
    #[default]
    Intel,
    /// Big-endian (`@0`); `start_bit` is the MSB position.
    Motorola,
}

/// A signal defined by the user on a raw CAN message, independent of any
/// DBC. Plain fields only: the application turns it into a decoder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSignalDef {
    pub id: UserSignalId,
    pub name: String,
    pub bus: BusId,
    /// CAN identifier of the carrying message.
    pub msg_id: u32,
    #[serde(default)]
    pub extended: bool,
    pub start_bit: u16,
    /// Width in bits.
    pub size: u16,
    #[serde(default)]
    pub byte_order: SignalByteOrder,
    #[serde(default)]
    pub signed: bool,
    pub factor: f64,
    pub offset: f64,
    #[serde(default)]
    pub unit: String,
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
    /// Signals the user defined on raw messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_signals: Vec<UserSignalDef>,
    /// Rhai test modules (`.rhai` files) run by the test runner; relative
    /// paths resolve against the project file's folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    /// Opaque UI workspace (window layout) saved with the project. The core
    /// crate does not interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<serde_json::Value>,
    /// Network domains: named groups of nodes (and buses) on the canvas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<Domain>,
    /// Style of every wire that has no override of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_default: Option<WireStyle>,
    /// Per-wire style overrides (kept beside `links` so `Link` stays `Copy`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wires: Vec<WireOverride>,
}

/// A named group of nodes on the canvas; purely organisational.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Domain {
    pub id: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub members: Vec<NodeId>,
    /// Buses placed in the domain (free-form view only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_members: Vec<BusId>,
    #[serde(default)]
    pub collapsed: bool,
    /// The domain this one is nested in.
    #[serde(default)]
    pub parent: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireKind {
    Bezier,
    Straight,
    Step,
    SmoothStep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireLine {
    Solid,
    Dashed,
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireArrow {
    None,
    Triangle,
    Open,
    Circle,
    Diamond,
}

/// How a wire is drawn. Every field is optional: unset fields fall back to
/// the project default, then to the automatic styling.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WireStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WireKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<WireLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow: Option<WireArrow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_at_source: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl WireStyle {
    /// This style, with unset fields taken from `base`.
    pub fn over(&self, base: &WireStyle) -> WireStyle {
        WireStyle {
            kind: self.kind.or(base.kind),
            line: self.line.or(base.line),
            color: self.color.or(base.color),
            width: self.width.or(base.width),
            arrow: self.arrow.or(base.arrow),
            arrow_at_source: self.arrow_at_source.or(base.arrow_at_source),
            animated: self.animated.or(base.animated),
            label: self.label.clone().or_else(|| base.label.clone()),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == WireStyle::default()
    }
}

/// Style override of the wire between `node` and `bus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireOverride {
    pub node: NodeId,
    pub bus: BusId,
    pub style: WireStyle,
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
    #[error("replay node {node:?} maps a channel to bus {bus:?} which it is not linked to")]
    ReplayBusNotLinked { node: NodeId, bus: BusId },
    #[error("node {node:?} serves diagnostics on bus {bus:?} which it is not linked to")]
    DiagBusNotLinked { node: NodeId, bus: BusId },
    #[error("domain {0} appears more than once")]
    DuplicateDomain(u32),
    #[error("domain {domain} contains unknown node {node:?}")]
    DomainUnknownNode { domain: u32, node: NodeId },
    #[error("domain {domain} contains unknown bus {bus:?}")]
    DomainUnknownBus { domain: u32, bus: BusId },
    #[error("node {0:?} is in more than one domain")]
    NodeInTwoDomains(NodeId),
    #[error("bus {0:?} is in more than one domain")]
    BusInTwoDomains(BusId),
    #[error("domain {domain} has unknown parent {parent}")]
    DomainUnknownParent { domain: u32, parent: u32 },
    #[error("domain {0} is nested inside itself")]
    DomainCycle(u32),
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
            if let Some(bus) = node.diag.as_ref().and_then(|d| d.bus)
                && !linked(bus)
            {
                return Err(TopologyError::DiagBusNotLinked { node: node.id, bus });
            }
            if let NodeKind::Replay { channel_map, .. } = &node.kind {
                for &(_, bus) in channel_map {
                    if !linked(bus) {
                        return Err(TopologyError::ReplayBusNotLinked { node: node.id, bus });
                    }
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
        self.validate_domains()
    }

    fn validate_domains(&self) -> Result<(), TopologyError> {
        let mut seen = std::collections::HashSet::new();
        let mut nodes = std::collections::HashSet::new();
        let mut buses = std::collections::HashSet::new();
        for d in &self.domains {
            if !seen.insert(d.id) {
                return Err(TopologyError::DuplicateDomain(d.id));
            }
            for &node in &d.members {
                if !self.nodes.iter().any(|n| n.id == node) {
                    return Err(TopologyError::DomainUnknownNode { domain: d.id, node });
                }
                if !nodes.insert(node) {
                    return Err(TopologyError::NodeInTwoDomains(node));
                }
            }
            for &bus in &d.bus_members {
                if !self.buses.iter().any(|b| b.id == bus) {
                    return Err(TopologyError::DomainUnknownBus { domain: d.id, bus });
                }
                if !buses.insert(bus) {
                    return Err(TopologyError::BusInTwoDomains(bus));
                }
            }
        }
        for d in &self.domains {
            if let Some(parent) = d.parent
                && !seen.contains(&parent)
            {
                return Err(TopologyError::DomainUnknownParent {
                    domain: d.id,
                    parent,
                });
            }
            // Walk up; a chain longer than the domain count must loop.
            let mut at = d.parent;
            for _ in 0..self.domains.len() {
                let Some(p) = at else { break };
                if p == d.id {
                    return Err(TopologyError::DomainCycle(d.id));
                }
                at = self
                    .domains
                    .iter()
                    .find(|x| x.id == p)
                    .and_then(|x| x.parent);
            }
            if at.is_some() {
                return Err(TopologyError::DomainCycle(d.id));
            }
        }
        Ok(())
    }
}
