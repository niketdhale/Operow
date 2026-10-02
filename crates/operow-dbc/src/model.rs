use serde::{Deserialize, Serialize};

/// A parsed DBC file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Database {
    pub version: String,
    /// Node names from the `BU_` section.
    pub nodes: Vec<String>,
    pub messages: Vec<MessageDef>,
}

/// A CAN message (`BO_`) definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDef {
    /// CAN identifier with the DBC extended-format flag (bit 31) removed.
    pub id: u32,
    pub extended: bool,
    pub name: String,
    /// Payload length in bytes.
    pub dlc: u8,
    pub transmitter: String,
    pub signals: Vec<SignalDef>,
    /// From the `GenMsgCycleTime` attribute.
    pub cycle_time_ms: Option<u32>,
    /// From the `GenMsgSendType` attribute.
    pub send_type: Option<String>,
    pub comment: Option<String>,
}

/// Bit layout of a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByteOrder {
    /// Little-endian (`@1`).
    Intel,
    /// Big-endian (`@0`); `start_bit` is the position of the MSB.
    Motorola,
}

/// Signedness of the raw value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Signed,
    Unsigned,
}

/// Multiplexing role of a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mux {
    /// The selector signal (`M`).
    Multiplexor,
    /// Present only when the selector equals this raw value (`m<n>`).
    Multiplexed(u64),
}

/// A signal (`SG_`) inside a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalDef {
    pub name: String,
    pub start_bit: u16,
    /// Width in bits.
    pub size: u16,
    pub byte_order: ByteOrder,
    pub value_type: ValueType,
    pub factor: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
    pub unit: String,
    pub receivers: Vec<String>,
    pub multiplexer: Option<Mux>,
    /// From the `GenSigStartValue` attribute (raw value).
    pub initial_raw: Option<u64>,
    /// From `VAL_` entries.
    pub value_descriptions: Vec<(i64, String)>,
    pub comment: Option<String>,
}
