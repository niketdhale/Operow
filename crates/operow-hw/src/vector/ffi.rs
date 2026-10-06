//! The subset of the Vector XL Driver Library API that Operow uses.
//!
//! Types and constants are transcribed by hand from the public `vxlapi.h`
//! (only what is needed; no SDK headers or import library are involved).
//! The layouts are plain `#[repr(C)]`: `vxlapi.h` wraps its structures in
//! `#pragma pack(push, 8)`, which equals natural alignment for every
//! structure below, so no `packed` attribute is needed. Sizes are asserted
//! at compile time; if they ever disagree with the header the build fails
//! rather than corrupting memory. These definitions have only been checked
//! against a mock, never against the real DLL.
//!
//! Event payload unions are kept as byte arrays and decoded with explicit
//! offsets, which avoids union alignment surprises.
#![allow(dead_code)] // complete subset kept for reference; some items are test-only

use std::mem::size_of;

/// `XLstatus` (`short`).
pub type XlStatus = i16;
/// `XLportHandle` (`long`, which is 32 bits on Windows, also in 64-bit builds).
pub type XlPortHandle = i32;
/// `XLaccess` (`unsigned __int64`): a bit mask of channels.
pub type XlAccess = u64;

pub const XL_SUCCESS: XlStatus = 0;
pub const XL_ERR_QUEUE_IS_EMPTY: XlStatus = 10;
pub const XL_ERR_QUEUE_IS_FULL: XlStatus = 11;
pub const XL_ERR_INVALID_ACCESS: XlStatus = 112;

pub const XL_BUS_TYPE_CAN: u32 = 1;
/// Low 16 bits of `channelBusCapabilities`: buses the channel is compatible with.
pub const XL_BUS_COMPATIBLE_CAN: u32 = 1;
/// High 16 bits of `channelBusCapabilities`: buses the channel is active for.
pub const XL_BUS_ACTIVE_CAP_CAN: u32 = XL_BUS_COMPATIBLE_CAN << 16;

pub const XL_INTERFACE_VERSION_V3: u32 = 3;
pub const XL_INTERFACE_VERSION_V4: u32 = 4;

pub const XL_HWTYPE_VIRTUAL: u8 = 1;

pub const XL_CHANNEL_FLAG_CANFD_BOSCH_SUPPORT: u32 = 0x2000_0000;
pub const XL_CHANNEL_FLAG_CANFD_ISO_SUPPORT: u32 = 0x8000_0000;

pub const XL_OUTPUT_MODE_SILENT: u8 = 0;
pub const XL_OUTPUT_MODE_NORMAL: u8 = 1;

pub const XL_ACTIVATE_RESET_CLOCK: u32 = 8;

pub const XL_CONFIG_MAX_CHANNELS: usize = 64;
pub const XL_MAX_LENGTH: usize = 31;

// Classic (V3) event tags.
pub const XL_RECEIVE_MSG: u8 = 1;
pub const XL_CHIP_STATE: u8 = 4;
pub const XL_TRANSMIT_MSG: u8 = 10;

// Classic message flags (`s_xl_can_msg.flags`).
pub const XL_CAN_MSG_FLAG_ERROR_FRAME: u16 = 0x01;
pub const XL_CAN_MSG_FLAG_REMOTE_FRAME: u16 = 0x10;
pub const XL_CAN_MSG_FLAG_TX_COMPLETED: u16 = 0x40;
pub const XL_CAN_MSG_FLAG_TX_REQUEST: u16 = 0x80;

/// Set in a CAN identifier for a 29-bit identifier.
pub const XL_CAN_EXT_MSG_ID: u32 = 0x8000_0000;

// Chip state bits (`busStatus`).
pub const XL_CHIPSTAT_BUSOFF: u8 = 0x01;
pub const XL_CHIPSTAT_ERROR_PASSIVE: u8 = 0x02;
pub const XL_CHIPSTAT_ERROR_WARNING: u8 = 0x04;
pub const XL_CHIPSTAT_ERROR_ACTIVE: u8 = 0x08;

// CAN FD (V4) event tags.
pub const XL_CAN_EV_TAG_RX_OK: u16 = 0x0400;
pub const XL_CAN_EV_TAG_RX_ERROR: u16 = 0x0401;
pub const XL_CAN_EV_TAG_TX_ERROR: u16 = 0x0402;
pub const XL_CAN_EV_TAG_TX_REQUEST: u16 = 0x0403;
pub const XL_CAN_EV_TAG_TX_OK: u16 = 0x0404;
pub const XL_CAN_EV_TAG_CHIP_STATE: u16 = 0x0409;
pub const XL_CAN_EV_TAG_TX_MSG: u16 = 0x0440;

// CAN FD transmit flags (`XL_CAN_TX_MSG.msgFlags`).
pub const XL_CAN_TXMSG_FLAG_EDL: u32 = 0x0001;
pub const XL_CAN_TXMSG_FLAG_BRS: u32 = 0x0002;
pub const XL_CAN_TXMSG_FLAG_RTR: u32 = 0x0010;

// CAN FD receive flags (`XL_CAN_EV_RX_MSG.msgFlags`).
pub const XL_CAN_RXMSG_FLAG_EDL: u32 = 0x0001;
pub const XL_CAN_RXMSG_FLAG_BRS: u32 = 0x0002;
pub const XL_CAN_RXMSG_FLAG_RTR: u32 = 0x0010;
pub const XL_CAN_RXMSG_FLAG_EF: u32 = 0x0200;

// CAN FD error codes (`XL_CAN_EV_ERROR.errorCode`).
pub const XL_CAN_ERRC_BIT_ERROR: u8 = 1;
pub const XL_CAN_ERRC_FORM_ERROR: u8 = 2;
pub const XL_CAN_ERRC_STUFF_ERROR: u8 = 3;
pub const XL_CAN_ERRC_OTHER_ERROR: u8 = 4;
pub const XL_CAN_ERRC_CRC_ERROR: u8 = 5;
pub const XL_CAN_ERRC_ACK_ERROR: u8 = 6;
pub const XL_CAN_ERRC_NACK_ERROR: u8 = 7;
pub const XL_CAN_ERRC_OVLD_ERROR: u8 = 8;
pub const XL_CAN_ERRC_EXCPT_ERROR: u8 = 9;

/// `XLevent`: classic event. `tag_data` is the 32-byte union; for
/// `XL_RECEIVE_MSG` / `XL_TRANSMIT_MSG` it holds `s_xl_can_msg`
/// (`id: u32, flags: u16, dlc: u16, res1: u64, data: [u8; 8], res2: u64`),
/// for `XL_CHIP_STATE` `busStatus, txErrorCounter, rxErrorCounter` at 0..3.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XlEvent {
    pub tag: u8,
    pub chan_index: u8,
    pub trans_id: u16,
    pub port_handle: u16,
    pub flags: u8,
    pub reserved: u8,
    /// Nanoseconds.
    pub time_stamp: u64,
    pub tag_data: [u8; 32],
}

impl XlEvent {
    /// `s_xl_can_msg` view: (id, flags, dlc, data).
    pub fn can_msg(&self) -> (u32, u16, u16, [u8; 8]) {
        let d = &self.tag_data;
        (
            u32::from_ne_bytes(d[0..4].try_into().unwrap()),
            u16::from_ne_bytes(d[4..6].try_into().unwrap()),
            u16::from_ne_bytes(d[6..8].try_into().unwrap()),
            d[16..24].try_into().unwrap(),
        )
    }

    pub fn set_can_msg(&mut self, id: u32, flags: u16, dlc: u16, data: &[u8]) {
        self.tag_data[0..4].copy_from_slice(&id.to_ne_bytes());
        self.tag_data[4..6].copy_from_slice(&flags.to_ne_bytes());
        self.tag_data[6..8].copy_from_slice(&dlc.to_ne_bytes());
        self.tag_data[16..16 + data.len()].copy_from_slice(data);
    }
}

/// `XLcanTxEvent` (CAN FD transmit). `XL_CAN_TX_MSG` is inlined.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XlCanTxEvent {
    pub tag: u16,
    pub trans_id: u16,
    pub channel_index: u8,
    pub reserved: [u8; 3],
    pub can_id: u32,
    pub msg_flags: u32,
    /// DLC code (0-15), not a byte count.
    pub dlc: u8,
    pub reserved2: [u8; 7],
    pub data: [u8; 64],
}

impl XlCanTxEvent {
    pub fn zeroed() -> Self {
        XlCanTxEvent {
            tag: 0,
            trans_id: 0,
            channel_index: 0,
            reserved: [0; 3],
            can_id: 0,
            msg_flags: 0,
            dlc: 0,
            reserved2: [0; 7],
            data: [0; 64],
        }
    }
}

/// `XLcanRxEvent` (CAN FD receive). `tag_data` is the 96-byte union; for
/// RX_OK / TX_OK it holds `XL_CAN_EV_RX_MSG`
/// (`canId: u32, msgFlags: u32, crc: u32, reserved1: [u8; 12],
/// totalBitCnt: u16, dlc: u8, reserved: [u8; 5], data: [u8; 64]`), for
/// RX_ERROR / TX_ERROR `errorCode: u8` at 0, for CHIP_STATE
/// `busStatus, txErrorCounter, rxErrorCounter` at 0..3.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XlCanRxEvent {
    pub size: u32,
    pub tag: u16,
    pub channel_index: u16,
    pub user_handle: u32,
    pub flags_chip: u16,
    pub reserved0: u16,
    pub reserved1: u64,
    /// Nanoseconds, synchronised to the PC clock.
    pub time_stamp_sync: u64,
    pub tag_data: [u8; 96],
}

impl XlCanRxEvent {
    pub fn zeroed() -> Self {
        XlCanRxEvent {
            size: size_of::<XlCanRxEvent>() as u32,
            tag: 0,
            channel_index: 0,
            user_handle: 0,
            flags_chip: 0,
            reserved0: 0,
            reserved1: 0,
            time_stamp_sync: 0,
            tag_data: [0; 96],
        }
    }

    /// `XL_CAN_EV_RX_MSG` view: (canId, msgFlags, dlc code, data).
    pub fn rx_msg(&self) -> (u32, u32, u8, [u8; 64]) {
        let d = &self.tag_data;
        (
            u32::from_ne_bytes(d[0..4].try_into().unwrap()),
            u32::from_ne_bytes(d[4..8].try_into().unwrap()),
            d[26],
            d[32..96].try_into().unwrap(),
        )
    }

    pub fn set_rx_msg(&mut self, can_id: u32, msg_flags: u32, dlc: u8, data: &[u8]) {
        self.tag_data[0..4].copy_from_slice(&can_id.to_ne_bytes());
        self.tag_data[4..8].copy_from_slice(&msg_flags.to_ne_bytes());
        self.tag_data[26] = dlc;
        self.tag_data[32..32 + data.len()].copy_from_slice(data);
    }
}

/// `XLcanFdConf`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XlCanFdConf {
    pub arbitration_bit_rate: u32,
    pub sjw_abr: u32,
    pub tseg1_abr: u32,
    pub tseg2_abr: u32,
    pub data_bit_rate: u32,
    pub sjw_dbr: u32,
    pub tseg1_dbr: u32,
    pub tseg2_dbr: u32,
    pub reserved: u8,
    /// `CANFD_CONFOPT_*`; 0 selects ISO CAN FD.
    pub options: u8,
    pub reserved1: [u8; 2],
    pub reserved2: u32,
}

/// `XLchannelConfig` (`#pragma pack(1)`, 227 bytes). `XLbusParams` is kept
/// as an opaque 4 + 28 byte block. Fields are packed: read them by value.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct XlChannelConfig {
    pub name: [u8; XL_MAX_LENGTH + 1],
    pub hw_type: u8,
    pub hw_index: u8,
    pub hw_channel: u8,
    pub transceiver_type: u16,
    pub transceiver_state: u16,
    pub config_error: u16,
    pub channel_index: u8,
    pub channel_mask: u64,
    pub channel_capabilities: u32,
    pub channel_bus_capabilities: u32,
    pub is_on_bus: u8,
    pub connected_bus_type: u32,
    pub bus_params_type: u32,
    pub bus_params_data: [u8; 28],
    pub do_not_use: u32,
    pub driver_version: u32,
    pub interface_version: u32,
    pub raw_data: [u32; 10],
    pub serial_number: u32,
    pub article_number: u32,
    pub transceiver_name: [u8; XL_MAX_LENGTH + 1],
    pub special_cab_flags: u32,
    pub dominant_timeout: u32,
    pub dominant_recessive_delay: u8,
    pub recessive_dominant_delay: u8,
    pub connection_info: u8,
    pub currently_available_timestamps: u8,
    pub minimal_supply_voltage: u16,
    pub maximal_supply_voltage: u16,
    pub maximal_baudrate: u32,
    pub fpga_core_capabilities: u8,
    pub special_device_status: u8,
    pub channel_bus_active_capabilities: u16,
    pub break_offset: u16,
    pub delimiter_offset: u16,
    pub reserved: [u32; 3],
}

/// `XLdriverConfig`.
#[repr(C)]
pub struct XlDriverConfig {
    pub dll_version: u32,
    pub channel_count: u32,
    pub reserved: [u32; 10],
    pub channel: [XlChannelConfig; XL_CONFIG_MAX_CHANNELS],
}

const _: () = {
    assert!(size_of::<XlEvent>() == 48);
    assert!(size_of::<XlCanTxEvent>() == 88);
    assert!(size_of::<XlCanRxEvent>() == 128);
    assert!(size_of::<XlCanFdConf>() == 40);
    assert!(std::mem::offset_of!(XlChannelConfig, channel_mask) == 42);
    assert!(std::mem::offset_of!(XlChannelConfig, channel_capabilities) == 50);
    assert!(std::mem::offset_of!(XlChannelConfig, bus_params_type) == 63);
    assert!(std::mem::offset_of!(XlChannelConfig, transceiver_name) == 155);
    assert!(size_of::<XlChannelConfig>() == 227);
    assert!(size_of::<XlDriverConfig>() == 48 + 64 * 227);
    assert!(size_of::<XlDriverConfig>() == 14576);
};
