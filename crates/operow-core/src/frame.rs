use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur while constructing a [`CanFrame`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("CAN id 0x{0:X} exceeds the 11-bit standard identifier range (0x7FF)")]
    StandardIdTooLarge(u32),
    #[error("CAN id 0x{0:X} exceeds the 29-bit extended identifier range (0x1FFFFFFF)")]
    ExtendedIdTooLarge(u32),
    #[error("CAN payload length {0} exceeds the maximum of 8 bytes")]
    PayloadTooLong(usize),
}

/// A classic CAN 2.0 data frame (max 8 data bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanFrame {
    pub id: u32,
    pub extended: bool,
    pub dlc: u8,
    pub data: [u8; 8],
}

impl CanFrame {
    /// Build a new frame, validating the id range (11-bit standard / 29-bit
    /// extended) and that the payload fits in at most 8 bytes.
    pub fn new(id: u32, extended: bool, data: &[u8]) -> Result<Self, FrameError> {
        if data.len() > 8 {
            return Err(FrameError::PayloadTooLong(data.len()));
        }
        if extended {
            if id > 0x1FFF_FFFF {
                return Err(FrameError::ExtendedIdTooLarge(id));
            }
        } else if id > 0x7FF {
            return Err(FrameError::StandardIdTooLarge(id));
        }

        let mut buf = [0u8; 8];
        buf[..data.len()].copy_from_slice(data);
        Ok(CanFrame {
            id,
            extended,
            dlc: data.len() as u8,
            data: buf,
        })
    }

    /// The payload bytes actually carried by this frame (length `dlc`).
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.dlc as usize]
    }
}
