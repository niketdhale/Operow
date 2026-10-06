//! The subset of the PEAK PCAN-Basic API that Operow uses.
//!
//! Types and constants are transcribed by hand from the public
//! `PCANBasic.h` (only what is needed; no SDK headers or import library are
//! involved) and cross-checked against the PCAN binding of python-can. All
//! structures are plain `#[repr(C)]` with natural alignment; their sizes and
//! field offsets are asserted at compile time, so a disagreement with the
//! header fails the build rather than corrupting memory. These definitions
//! have only been checked against a mock, never against the real library.
#![allow(dead_code)] // complete subset kept for reference; some items are test-only

use std::mem::{offset_of, size_of};

/// `TPCANHandle` (`WORD`): a channel handle such as `PCAN_USBBUS1`.
pub type PcanHandle = u16;
/// `TPCANStatus` (`DWORD`): `PCAN_ERROR_OK` or a mask of `PCAN_ERROR_*` bits.
pub type PcanStatus = u32;

// Channel handles.
pub const PCAN_NONEBUS: PcanHandle = 0x00;
pub const PCAN_PCIBUS1: PcanHandle = 0x41;
pub const PCAN_USBBUS1: PcanHandle = 0x51;
pub const PCAN_PCCBUS1: PcanHandle = 0x61;
pub const PCAN_LANBUS1: PcanHandle = 0x801;
/// Channels 9..=16 of the PCI and USB families are `0x400 / 0x500 + n`
/// (`PCAN_USBBUS9` is `0x509`), unlike 1..=8 which are `0x40 / 0x50 + n`.
pub const PCAN_USBBUS9: PcanHandle = 0x509;
pub const PCAN_PCIBUS9: PcanHandle = 0x409;

// Status codes.
pub const PCAN_ERROR_OK: PcanStatus = 0x00000;
pub const PCAN_ERROR_XMTFULL: PcanStatus = 0x00001;
pub const PCAN_ERROR_OVERRUN: PcanStatus = 0x00002;
pub const PCAN_ERROR_BUSLIGHT: PcanStatus = 0x00004;
pub const PCAN_ERROR_BUSHEAVY: PcanStatus = 0x00008;
pub const PCAN_ERROR_BUSOFF: PcanStatus = 0x00010;
pub const PCAN_ERROR_BUSPASSIVE: PcanStatus = 0x40000;
pub const PCAN_ERROR_ANYBUSERR: PcanStatus =
    PCAN_ERROR_BUSLIGHT | PCAN_ERROR_BUSHEAVY | PCAN_ERROR_BUSOFF | PCAN_ERROR_BUSPASSIVE;
pub const PCAN_ERROR_QRCVEMPTY: PcanStatus = 0x00020;
pub const PCAN_ERROR_QOVERRUN: PcanStatus = 0x00040;
pub const PCAN_ERROR_QXMTFULL: PcanStatus = 0x00080;
pub const PCAN_ERROR_NODRIVER: PcanStatus = 0x00200;
pub const PCAN_ERROR_HWINUSE: PcanStatus = 0x00400;
pub const PCAN_ERROR_ILLPARAMTYPE: PcanStatus = 0x04000;
pub const PCAN_ERROR_ILLPARAMVAL: PcanStatus = 0x08000;
pub const PCAN_ERROR_ILLDATA: PcanStatus = 0x20000;
pub const PCAN_ERROR_CAUTION: PcanStatus = 0x2000000;
pub const PCAN_ERROR_INITIALIZE: PcanStatus = 0x4000000;
pub const PCAN_ERROR_ILLOPERATION: PcanStatus = 0x8000000;

// Device types (`TPCANDevice`).
pub const PCAN_NONE: u8 = 0x00;
pub const PCAN_PCI: u8 = 0x04;
pub const PCAN_USB: u8 = 0x05;
pub const PCAN_PCC: u8 = 0x06;
pub const PCAN_VIRTUAL: u8 = 0x07;
pub const PCAN_LAN: u8 = 0x08;

// Parameters (`TPCANParameter`) for `CAN_GetValue` / `CAN_SetValue`.
pub const PCAN_LISTEN_ONLY: u8 = 0x08;
pub const PCAN_CHANNEL_CONDITION: u8 = 0x0D;
pub const PCAN_HARDWARE_NAME: u8 = 0x0E;
pub const PCAN_CHANNEL_FEATURES: u8 = 0x16;
pub const PCAN_ALLOW_ERROR_FRAMES: u8 = 0x20;
pub const PCAN_ATTACHED_CHANNELS_COUNT: u8 = 0x2A;
pub const PCAN_ATTACHED_CHANNELS: u8 = 0x2B;
pub const PCAN_ALLOW_ECHO_FRAMES: u8 = 0x2C;

pub const PCAN_PARAMETER_OFF: u32 = 0;
pub const PCAN_PARAMETER_ON: u32 = 1;

// `PCAN_CHANNEL_CONDITION` values.
pub const PCAN_CHANNEL_UNAVAILABLE: u32 = 0;
pub const PCAN_CHANNEL_AVAILABLE: u32 = 1;
pub const PCAN_CHANNEL_OCCUPIED: u32 = 2;
/// Used by PCAN-View but still available to connect.
pub const PCAN_CHANNEL_PCANVIEW: u32 = PCAN_CHANNEL_AVAILABLE | PCAN_CHANNEL_OCCUPIED;

/// `PCAN_CHANNEL_FEATURES` / `TPCANChannelInformation.device_features` bit.
pub const FEATURE_FD_CAPABLE: u32 = 0x01;

/// `MAX_LENGTH_HARDWARE_NAME`: 32 characters plus the terminator.
pub const MAX_LENGTH_HARDWARE_NAME: usize = 33;

// Message types (`TPCANMessageType`, a bit mask).
pub const PCAN_MESSAGE_STANDARD: u8 = 0x00;
pub const PCAN_MESSAGE_RTR: u8 = 0x01;
pub const PCAN_MESSAGE_EXTENDED: u8 = 0x02;
pub const PCAN_MESSAGE_FD: u8 = 0x04;
pub const PCAN_MESSAGE_BRS: u8 = 0x08;
pub const PCAN_MESSAGE_ESI: u8 = 0x10;
pub const PCAN_MESSAGE_ECHO: u8 = 0x20;
pub const PCAN_MESSAGE_ERRFRAME: u8 = 0x40;
pub const PCAN_MESSAGE_STATUS: u8 = 0x80;

// Baud rate registers (`TPCANBaudrate`, BTR0/BTR1) for `CAN_Initialize`.
pub const PCAN_BAUD_1M: u16 = 0x0014;
pub const PCAN_BAUD_800K: u16 = 0x0016;
pub const PCAN_BAUD_500K: u16 = 0x001C;
pub const PCAN_BAUD_250K: u16 = 0x011C;
pub const PCAN_BAUD_125K: u16 = 0x031C;
pub const PCAN_BAUD_100K: u16 = 0x432F;
pub const PCAN_BAUD_95K: u16 = 0xC34E;
pub const PCAN_BAUD_83K: u16 = 0x852B;
pub const PCAN_BAUD_50K: u16 = 0x472F;
pub const PCAN_BAUD_47K: u16 = 0x1414;
pub const PCAN_BAUD_33K: u16 = 0x8B2F;
pub const PCAN_BAUD_20K: u16 = 0x532F;
pub const PCAN_BAUD_10K: u16 = 0x672F;
pub const PCAN_BAUD_5K: u16 = 0x7F7F;

/// `TPCANMsg`: a classic frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcanMsg {
    pub id: u32,
    pub msg_type: u8,
    /// Data length (0..=8).
    pub len: u8,
    pub data: [u8; 8],
}

/// `TPCANTimestamp`: total microseconds = `micros + 1000 * millis +
/// 0x1_0000_0000 * 1000 * millis_overflow`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcanTimestamp {
    pub millis: u32,
    pub millis_overflow: u16,
    pub micros: u16,
}

/// `TPCANMsgFD`: a CAN FD frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcanMsgFd {
    pub id: u32,
    pub msg_type: u8,
    /// Data length code (0..=15).
    pub dlc: u8,
    pub data: [u8; 64],
}

impl Default for PcanMsgFd {
    fn default() -> Self {
        PcanMsgFd {
            id: 0,
            msg_type: 0,
            dlc: 0,
            data: [0; 64],
        }
    }
}

/// `TPCANChannelInformation`: one entry of `PCAN_ATTACHED_CHANNELS`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcanChannelInformation {
    pub channel_handle: PcanHandle,
    pub device_type: u8,
    pub controller_number: u8,
    pub device_features: u32,
    pub device_name: [u8; MAX_LENGTH_HARDWARE_NAME],
    pub device_id: u32,
    pub channel_condition: u32,
}

impl Default for PcanChannelInformation {
    fn default() -> Self {
        PcanChannelInformation {
            channel_handle: 0,
            device_type: 0,
            controller_number: 0,
            device_features: 0,
            device_name: [0; MAX_LENGTH_HARDWARE_NAME],
            device_id: 0,
            channel_condition: 0,
        }
    }
}

const _: () = {
    // 14 bytes of fields, padded to the u32 alignment.
    assert!(size_of::<PcanMsg>() == 16);
    assert!(offset_of!(PcanMsg, msg_type) == 4);
    assert!(offset_of!(PcanMsg, len) == 5);
    assert!(offset_of!(PcanMsg, data) == 6);
    assert!(size_of::<PcanTimestamp>() == 8);
    assert!(offset_of!(PcanTimestamp, millis_overflow) == 4);
    assert!(offset_of!(PcanTimestamp, micros) == 6);
    // 70 bytes of fields, padded.
    assert!(size_of::<PcanMsgFd>() == 72);
    assert!(offset_of!(PcanMsgFd, msg_type) == 4);
    assert!(offset_of!(PcanMsgFd, dlc) == 5);
    assert!(offset_of!(PcanMsgFd, data) == 6);
    assert!(offset_of!(PcanChannelInformation, device_type) == 2);
    assert!(offset_of!(PcanChannelInformation, controller_number) == 3);
    assert!(offset_of!(PcanChannelInformation, device_features) == 4);
    assert!(offset_of!(PcanChannelInformation, device_name) == 8);
    assert!(offset_of!(PcanChannelInformation, device_id) == 44);
    assert!(offset_of!(PcanChannelInformation, channel_condition) == 48);
    assert!(size_of::<PcanChannelInformation>() == 52);
};
