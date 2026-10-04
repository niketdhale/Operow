/// Addressing format of an ISO-TP channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    /// Normal addressing: the CAN id alone identifies the channel; PCI starts
    /// at data byte 0.
    Normal,
    /// Extended addressing: the first data byte carries a target address
    /// (`tx_ta` on frames we send, `rx_ta` expected on frames we receive).
    Extended { tx_ta: u8, rx_ta: u8 },
    /// Mixed addressing: the first data byte carries the address extension
    /// `ae` in both directions (requires 29-bit ids).
    Mixed { ae: u8 },
}

/// Separation time minimum, stored as the raw wire byte.
///
/// `0x00..=0x7F` is 0-127 ms, `0xF1..=0xF9` is 100-900 us. Reserved values
/// are treated as 127 ms, as the standard requires of a sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StMin(pub u8);

impl StMin {
    /// Encode whole milliseconds (clamped to 127).
    pub fn from_millis(ms: u8) -> Self {
        StMin(ms.min(0x7F))
    }

    /// Encode 100..=900 microseconds in steps of 100 (other values round down
    /// to the nearest step; below 100 yields 0 ms).
    pub fn from_micros(us: u16) -> Self {
        match (us / 100).min(9) {
            0 => StMin(0),
            n => StMin(0xF0 + n as u8),
        }
    }

    /// The separation time in nanoseconds.
    pub fn as_nanos(self) -> u64 {
        match self.0 {
            0x00..=0x7F => self.0 as u64 * 1_000_000,
            0xF1..=0xF9 => (self.0 - 0xF0) as u64 * 100_000,
            _ => 127_000_000,
        }
    }
}

/// ISO-TP timeouts in milliseconds.
///
/// Only `n_bs` and `n_cr` are enforced: the sans-IO channel has no transmit
/// confirmation, so `n_as`/`n_ar` cannot fire, and `n_br`/`n_cs` are
/// performance requirements (default 0 = respond immediately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub n_as: u32,
    pub n_ar: u32,
    pub n_bs: u32,
    pub n_br: u32,
    pub n_cs: u32,
    pub n_cr: u32,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            n_as: 1000,
            n_ar: 1000,
            n_bs: 1000,
            n_br: 0,
            n_cs: 0,
            n_cr: 1000,
        }
    }
}

/// Configuration of one ISO-TP channel (one tx id / rx id pair).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoTpConfig {
    /// CAN id of frames we transmit.
    pub tx_id: u32,
    /// CAN id of frames we accept.
    pub rx_id: u32,
    /// Use 29-bit identifiers.
    pub extended_ids: bool,
    pub addressing: Addressing,
    /// Transmit CAN FD frames (up to 64 bytes, escape SF/FF sequences).
    pub fd: bool,
    /// Transmit data length: 8 for classic CAN, or a valid FD length
    /// (8, 12, 16, 20, 24, 32, 48, 64) when `fd` is set.
    pub tx_dl: u8,
    /// Padding byte. `None` sends the shortest valid frame (FD frames longer
    /// than 8 bytes are still padded, with 0xCC).
    pub padding: Option<u8>,
    /// Block size we advertise as receiver (0 = send everything).
    pub block_size: u8,
    /// STmin we advertise as receiver.
    pub st_min: StMin,
    pub timeouts: Timeouts,
    /// Maximum number of consecutive FC WAIT frames tolerated as sender.
    pub max_wft: u8,
    /// Largest message accepted as receiver; larger FFs get FC OVFLW.
    pub max_rx_len: u32,
}

impl IsoTpConfig {
    /// Classic CAN, normal addressing, 11-bit ids, defaults elsewhere.
    pub fn new(tx_id: u32, rx_id: u32) -> Self {
        IsoTpConfig {
            tx_id,
            rx_id,
            ..Self::default()
        }
    }

    pub(crate) fn addr_offset(&self) -> usize {
        match self.addressing {
            Addressing::Normal => 0,
            _ => 1,
        }
    }

    pub(crate) fn tx_addr_byte(&self) -> Option<u8> {
        match self.addressing {
            Addressing::Normal => None,
            Addressing::Extended { tx_ta, .. } => Some(tx_ta),
            Addressing::Mixed { ae } => Some(ae),
        }
    }

    pub(crate) fn rx_addr_byte(&self) -> Option<u8> {
        match self.addressing {
            Addressing::Normal => None,
            Addressing::Extended { rx_ta, .. } => Some(rx_ta),
            Addressing::Mixed { ae } => Some(ae),
        }
    }

    /// Frame data length used for full frames.
    pub(crate) fn can_dl(&self) -> usize {
        if self.fd { self.tx_dl as usize } else { 8 }
    }
}

impl Default for IsoTpConfig {
    fn default() -> Self {
        IsoTpConfig {
            tx_id: 0x7E0,
            rx_id: 0x7E8,
            extended_ids: false,
            addressing: Addressing::Normal,
            fd: false,
            tx_dl: 8,
            padding: None,
            block_size: 0,
            st_min: StMin(0),
            timeouts: Timeouts::default(),
            max_wft: 10,
            max_rx_len: 1 << 20,
        }
    }
}
