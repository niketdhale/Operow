use operow_core::{
    BusId, CanBusConfig, CanFrame, EcuConfig, Link, NodeId, NodeKind, SendType, Topology, TxMessage,
};
use thiserror::Error;

use crate::model::{Database, MessageDef};

/// Where [`Database::merge_into`] attaches the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusTarget {
    /// An existing bus of the topology.
    Existing(BusId),
    /// A bus created by the merge.
    New { name: String, bitrate: u32 },
}

/// Errors returned by [`Database::merge_into`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MergeError {
    #[error("bus {0:?} does not exist")]
    UnknownBus(BusId),
}

/// Canvas layout constants matching the app's bus placement
/// (`x = 160`, `y = 260 + 160 * index`).
const BUS_Y0: f32 = 260.0;
const BUS_Y_STEP: f32 = 160.0;
const ROW_X0: f32 = 60.0;
const COL_STEP: f32 = 200.0;
const ROW_STEP: f32 = 120.0;
const NODES_PER_ROW: usize = 5;

/// Cycle time used when a message has no `GenMsgCycleTime`.
const DEFAULT_PERIOD_MS: u32 = 100;

/// Smallest valid CAN FD payload length that holds `len` bytes.
fn fd_len(len: usize) -> usize {
    [12, 16, 20, 24, 32, 48, 64]
        .into_iter()
        .find(|&l| l >= len)
        .unwrap_or(64)
}

/// Map a `GenMsgSendType` value (case-insensitive); unknown or missing falls
/// back to cyclic when a cycle time is present, else event.
fn map_send_type(send_type: Option<&str>, has_cycle: bool) -> SendType {
    match send_type.map(str::to_ascii_lowercase).as_deref() {
        Some("cyclic") => SendType::Cyclic,
        Some("spontaneous" | "nomsgsendtype") => SendType::Event,
        Some("ifactive" | "cyclicifactive") => SendType::CyclicIfActive,
        Some(
            "cyclicandspontanx"
            | "cyclicandspontaneous"
            | "cyclicifactiveandspontanwithdelay"
            | "cyclicandspontanwithdelay",
        ) => SendType::CyclicAndEvent,
        _ if has_cycle => SendType::Cyclic,
        _ => SendType::Event,
    }
}

impl MessageDef {
    /// Build the initial frame: zeroed payload with every signal's
    /// `initial_raw` applied. Classic for `dlc <= 8`, otherwise FD (no BRS).
    fn initial_frame(&self) -> Option<CanFrame> {
        let fd = self.dlc > 8;
        let len = if fd {
            fd_len(self.dlc as usize)
        } else {
            self.dlc as usize
        };
        let mut data = vec![0u8; len];
        for s in &self.signals {
            if let Some(raw) = s.initial_raw {
                s.encode_raw(&mut data, raw);
            }
        }
        if fd {
            CanFrame::new_fd(self.id, self.extended, false, &data).ok()
        } else {
            CanFrame::new(self.id, self.extended, &data).ok()
        }
    }
}

impl MessageDef {
    /// The transmit entry for this message on `bus`; `None` when the id or
    /// length cannot form a valid frame.
    fn tx_message(&self, bus: BusId) -> Option<TxMessage> {
        Some(TxMessage {
            name: self.name.clone(),
            frame: self.initial_frame()?,
            period_ms: self.cycle_time_ms.unwrap_or(DEFAULT_PERIOD_MS),
            enabled: true,
            bus: Some(bus),
            send_type: map_send_type(self.send_type.as_deref(), self.cycle_time_ms.is_some()),
        })
    }
}

impl Database {
    /// Message definition with this id and id format.
    pub fn message(&self, id: u32, extended: bool) -> Option<&MessageDef> {
        self.messages
            .iter()
            .find(|m| m.id == id && m.extended == extended)
    }

    /// Merge this database into a copy of `base`: attach it to `target`
    /// (created when new) and, when `create_nodes` is set, add one node per
    /// `BU_` entry. Fresh node ids follow the highest existing one; a node
    /// that already has the same (case-sensitive) name is reused and only
    /// gains the transmit messages it lacks (matched by id and id format).
    /// New nodes are linked to the bus and laid out in rows above it.
    /// Returns the merged topology and the target bus id.
    pub fn merge_into(
        &self,
        base: &Topology,
        target: BusTarget,
        create_nodes: bool,
    ) -> Result<(Topology, BusId), MergeError> {
        let mut topo = base.clone();
        let (bus, bus_index) = match target {
            BusTarget::Existing(id) => {
                let idx = topo
                    .buses
                    .iter()
                    .position(|b| b.id == id)
                    .ok_or(MergeError::UnknownBus(id))?;
                (id, idx)
            }
            BusTarget::New { name, bitrate } => {
                let id = BusId(topo.buses.iter().map(|b| b.id.0).max().unwrap_or(0) + 1);
                topo.buses.push(CanBusConfig {
                    id,
                    name,
                    bitrate,
                    fd_enabled: false,
                    data_bitrate: 2_000_000,
                    simulate_ack: false,
                    kind: Default::default(),
                    hardware: None,
                });
                (id, topo.buses.len() - 1)
            }
        };
        if self.messages.iter().any(|m| m.dlc > 8)
            && let Some(b) = topo.buses.iter_mut().find(|b| b.id == bus)
        {
            b.fd_enabled = true;
        }
        if !create_nodes {
            return Ok((topo, bus));
        }

        let row_y = BUS_Y0 + BUS_Y_STEP * bus_index as f32 - 200.0;
        let mut next_id = topo.nodes.iter().map(|n| n.id.0).max().unwrap_or(0) + 1;
        // Start right of whatever already sits in the first row.
        let start_x = topo
            .nodes
            .iter()
            .filter(|n| (n.pos.1 - row_y).abs() < ROW_STEP / 2.0)
            .map(|n| n.pos.0 + COL_STEP)
            .fold(ROW_X0, f32::max);
        let mut placed = 0usize;
        for name in &self.nodes {
            let msgs: Vec<TxMessage> = self
                .messages
                .iter()
                .filter(|m| &m.transmitter == name)
                .filter_map(|m| m.tx_message(bus))
                .collect();
            let node_id = match topo.nodes.iter_mut().find(|n| &n.name == name) {
                Some(node) => {
                    for m in msgs {
                        let known = node.tx.iter().any(|t| {
                            t.frame.id == m.frame.id && t.frame.extended == m.frame.extended
                        });
                        if !known {
                            node.tx.push(m);
                        }
                    }
                    node.id
                }
                None => {
                    let id = NodeId(next_id);
                    next_id += 1;
                    let pos = (
                        start_x + COL_STEP * (placed % NODES_PER_ROW) as f32,
                        row_y - ROW_STEP * (placed / NODES_PER_ROW) as f32,
                    );
                    placed += 1;
                    topo.nodes.push(EcuConfig {
                        id,
                        name: name.clone(),
                        tx: msgs,
                        kind: NodeKind::Ecu,
                        pos,
                        script: None,
                        diag: None,
                    });
                    id
                }
            };
            if !topo.links.iter().any(|l| l.node == node_id && l.bus == bus) {
                topo.links.push(Link { node: node_id, bus });
            }
        }
        Ok((topo, bus))
    }

    /// Create a topology with one node per `BU_` entry (ids from 1), each
    /// transmitting the messages it owns, all linked to a single bus.
    /// Messages whose id or length cannot form a valid frame are skipped.
    pub fn to_topology(&self, bus: BusId, bus_name: &str, bitrate: u32) -> Topology {
        let nodes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, name)| EcuConfig {
                id: NodeId(i as u32 + 1),
                name: name.clone(),
                tx: self
                    .messages
                    .iter()
                    .filter(|m| &m.transmitter == name)
                    .filter_map(|m| m.tx_message(bus))
                    .collect(),
                kind: NodeKind::Ecu,
                pos: (80.0 + 180.0 * i as f32, 120.0),
                script: None,
                diag: None,
            })
            .collect::<Vec<_>>();
        let links = nodes.iter().map(|n| Link { node: n.id, bus }).collect();
        Topology {
            nodes,
            buses: vec![CanBusConfig {
                id: bus,
                name: bus_name.to_string(),
                bitrate,
                fd_enabled: self.messages.iter().any(|m| m.dlc > 8),
                data_bitrate: 2_000_000,
                simulate_ack: false,
                kind: Default::default(),
                hardware: None,
            }],
            links,
            databases: Vec::new(),
            user_signals: Vec::new(),
            tests: Vec::new(),
            workspace: None,
            ..Default::default()
        }
    }
}
