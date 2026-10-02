use operow_core::{
    BusId, CanBusConfig, CanFrame, EcuConfig, Link, NodeId, NodeKind, SendType, Topology, TxMessage,
};

use crate::model::{Database, MessageDef};

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
/// back to cyclic when a cycle time is present, else spontaneous.
fn map_send_type(send_type: Option<&str>, has_cycle: bool) -> SendType {
    match send_type.map(str::to_ascii_lowercase).as_deref() {
        Some("cyclic") => SendType::Cyclic,
        Some("spontaneous" | "nomsgsendtype") => SendType::Spontaneous,
        Some("ifactive" | "cyclicifactive") => SendType::CyclicIfActive,
        Some(
            "cyclicandspontanx"
            | "cyclicandspontaneous"
            | "cyclicifactiveandspontanwithdelay"
            | "cyclicandspontanwithdelay",
        ) => SendType::CyclicAndSpontaneous,
        _ if has_cycle => SendType::Cyclic,
        _ => SendType::Spontaneous,
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

impl Database {
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
                    .filter_map(|m| {
                        Some(TxMessage {
                            name: m.name.clone(),
                            frame: m.initial_frame()?,
                            period_ms: m.cycle_time_ms.unwrap_or(DEFAULT_PERIOD_MS),
                            enabled: true,
                            bus: Some(bus),
                            send_type: map_send_type(
                                m.send_type.as_deref(),
                                m.cycle_time_ms.is_some(),
                            ),
                        })
                    })
                    .collect(),
                kind: NodeKind::Ecu,
                pos: (80.0 + 180.0 * i as f32, 120.0),
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
            }],
            links,
        }
    }
}
