use std::sync::Arc;

use serde::de::{self, Deserializer};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur while constructing a [`CanFrame`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("CAN id 0x{0:X} exceeds the 11-bit standard identifier range (0x7FF)")]
    StandardIdTooLarge(u32),
    #[error("CAN id 0x{0:X} exceeds the 29-bit extended identifier range (0x1FFFFFFF)")]
    ExtendedIdTooLarge(u32),
    #[error("CAN payload length {0} exceeds the maximum of 8 bytes for a classic frame")]
    PayloadTooLong(usize),
    #[error(
        "CAN FD payload length {0} is not a valid FD length \
         (allowed: 0-8, 12, 16, 20, 24, 32, 48, 64)"
    )]
    InvalidFdLength(usize),
}

/// Valid CAN FD payload lengths in bytes.
const FD_LENGTHS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];

/// Returns `true` if `len` is a valid CAN FD payload length.
pub fn is_valid_fd_len(len: usize) -> bool {
    FD_LENGTHS.contains(&len)
}

/// Maps a payload byte length to its 4-bit wire DLC code, for lengths valid
/// on a CAN FD frame (`0..=8, 12, 16, 20, 24, 32, 48, 64`). Returns `None`
/// for any other length.
pub fn len_to_dlc(len: usize) -> Option<u8> {
    Some(match len {
        0..=8 => len as u8,
        12 => 9,
        16 => 10,
        20 => 11,
        24 => 12,
        32 => 13,
        48 => 14,
        64 => 15,
        _ => return None,
    })
}

/// Maps a 4-bit wire DLC code (0-15) to the payload byte length it encodes
/// on a CAN FD frame. On a classic CAN frame only codes 0-8 are meaningful
/// (9-15 also denote 8 bytes of data on the wire, but this crate never
/// produces frames with such a `dlc`).
pub fn dlc_to_len(dlc: u8) -> usize {
    match dlc {
        0..=8 => dlc as usize,
        9 => 12,
        10 => 16,
        11 => 20,
        12 => 24,
        13 => 32,
        14 => 48,
        _ => 64,
    }
}

/// A classic CAN 2.0 or CAN FD data frame.
///
/// `dlc` here is the **payload byte count** (0-8 for a classic frame; one of
/// `0..=8, 12, 16, 20, 24, 32, 48, 64` for an FD frame) rather than the 4-bit
/// wire DLC code — use [`CanFrame::dlc_code`] (or the free functions
/// [`len_to_dlc`]/[`dlc_to_len`]) to convert to/from that code. Keeping
/// `dlc` as a byte count keeps `payload()` a simple slice and matches the
/// pre-FD on-disk topology format.
///
/// `data` is always 64 bytes wide (to keep `CanFrame: Copy`); only the first
/// `dlc` bytes are meaningful, as returned by [`CanFrame::payload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanFrame {
    pub id: u32,
    pub extended: bool,
    /// FDF: this is a CAN FD frame (as opposed to classic CAN 2.0).
    pub fd: bool,
    /// BRS: the data phase of this FD frame is transmitted at the bus's
    /// data bitrate rather than its nominal bitrate. Only meaningful (and
    /// only ever set) when `fd` is `true`.
    pub brs: bool,
    pub dlc: u8,
    pub data: [u8; 64],
}

impl CanFrame {
    /// Build a new classic CAN frame, validating the id range (11-bit
    /// standard / 29-bit extended) and that the payload fits in at most 8
    /// bytes.
    pub fn new(id: u32, extended: bool, data: &[u8]) -> Result<Self, FrameError> {
        if data.len() > 8 {
            return Err(FrameError::PayloadTooLong(data.len()));
        }
        Self::check_id(id, extended)?;

        let mut buf = [0u8; 64];
        buf[..data.len()].copy_from_slice(data);
        Ok(CanFrame {
            id,
            extended,
            fd: false,
            brs: false,
            dlc: data.len() as u8,
            data: buf,
        })
    }

    /// Build a new CAN FD frame, validating the id range and that `data`'s
    /// length is one of the valid FD lengths (0-8, 12, 16, 20, 24, 32, 48,
    /// 64 bytes).
    pub fn new_fd(id: u32, extended: bool, brs: bool, data: &[u8]) -> Result<Self, FrameError> {
        if !is_valid_fd_len(data.len()) {
            return Err(FrameError::InvalidFdLength(data.len()));
        }
        Self::check_id(id, extended)?;

        let mut buf = [0u8; 64];
        buf[..data.len()].copy_from_slice(data);
        Ok(CanFrame {
            id,
            extended,
            fd: true,
            brs,
            dlc: data.len() as u8,
            data: buf,
        })
    }

    fn check_id(id: u32, extended: bool) -> Result<(), FrameError> {
        if extended {
            if id > 0x1FFF_FFFF {
                return Err(FrameError::ExtendedIdTooLarge(id));
            }
        } else if id > 0x7FF {
            return Err(FrameError::StandardIdTooLarge(id));
        }
        Ok(())
    }

    /// The payload bytes actually carried by this frame (length `dlc`).
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.dlc as usize]
    }

    /// The 4-bit wire DLC code corresponding to this frame's payload length.
    pub fn dlc_code(&self) -> u8 {
        len_to_dlc(self.dlc as usize).unwrap_or(self.dlc.min(15))
    }
}

/// Serializes as `{id, extended, fd, brs, dlc, data}` where `data` is a
/// sequence of exactly `dlc` bytes (not the full 64-byte backing array).
impl Serialize for CanFrame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("CanFrame", 6)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("extended", &self.extended)?;
        state.serialize_field("fd", &self.fd)?;
        state.serialize_field("brs", &self.brs)?;
        state.serialize_field("dlc", &self.dlc)?;
        state.serialize_field("data", self.payload())?;
        state.end()
    }
}

/// Deserialization shadow. `fd`/`brs` default to `false` so pre-FD JSON
/// (which has neither field) still loads; `data` is read as a `Vec<u8>` of
/// any length (old files always stored 8 elements regardless of `dlc`) and
/// only its first `dlc` bytes are kept, padding with zeros if shorter.
#[derive(Deserialize)]
struct CanFrameShadow {
    id: u32,
    extended: bool,
    #[serde(default)]
    fd: bool,
    #[serde(default)]
    brs: bool,
    dlc: u8,
    data: Vec<u8>,
}

impl<'de> Deserialize<'de> for CanFrame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let shadow = CanFrameShadow::deserialize(deserializer)?;
        let dlc = shadow.dlc as usize;

        if shadow.data.len() < dlc {
            return Err(de::Error::custom(format!(
                "data has {} byte(s), fewer than the declared dlc of {}",
                shadow.data.len(),
                dlc
            )));
        }
        // Legacy classic topology files always stored a fixed 8-element
        // `data` array regardless of `dlc`; only the first `dlc` bytes were
        // ever meaningful. Preserve that by truncating rather than
        // requiring an exact-length match for classic frames. FD frames
        // (a format that never existed pre-FD) require an exact match.
        let payload = &shadow.data[..dlc];

        if shadow.fd {
            if shadow.data.len() != dlc {
                return Err(de::Error::custom(format!(
                    "FD frame data has {} byte(s), expected exactly {} (the declared dlc)",
                    shadow.data.len(),
                    dlc
                )));
            }
            CanFrame::new_fd(shadow.id, shadow.extended, shadow.brs, payload)
                .map_err(de::Error::custom)
        } else {
            if shadow.brs {
                return Err(de::Error::custom(
                    "brs is set but fd is not; brs only applies to CAN FD frames",
                ));
            }
            CanFrame::new(shadow.id, shadow.extended, payload).map_err(de::Error::custom)
        }
    }
}

/// An Ethernet frame (without preamble and FCS). The payload is shared so
/// cloning a frame never copies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EthFrame {
    pub dst: [u8; 6],
    pub src: [u8; 6],
    pub ethertype: u16,
    pub payload: Arc<[u8]>,
}

/// A frame on any supported network type.
/// Untagged so `BusEvent` JSON written before `Frame` existed still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Frame {
    Can(CanFrame),
    Eth(EthFrame),
}

impl From<CanFrame> for Frame {
    fn from(f: CanFrame) -> Self {
        Frame::Can(f)
    }
}

impl Frame {
    /// The payload bytes of either kind of frame.
    pub fn payload(&self) -> &[u8] {
        match self {
            Frame::Can(f) => f.payload(),
            Frame::Eth(f) => &f.payload,
        }
    }

    /// The CAN frame, if this is one.
    pub fn as_can(&self) -> Option<&CanFrame> {
        match self {
            Frame::Can(f) => Some(f),
            Frame::Eth(_) => None,
        }
    }
}
