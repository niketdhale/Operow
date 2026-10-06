//! Vector XL Driver Library driver (`vector:<channel name or index>`).
//!
//! The vendor library (`vxlapi64.dll`, or `vxlapi.dll` in 32-bit builds) is
//! loaded at run time on Windows; there is no link-time dependency and no SDK
//! header. [`ffi`] holds the hand-written subset of the public `vxlapi.h`
//! that is used. The driver logic is written against the small [`XlApi`]
//! function table, so it is compiled and unit-tested (with a mock) on every
//! operating system; only the DLL loader (`dll.rs`) is Windows-only. On other
//! systems the driver reports itself unavailable.
//!
//! Status: tested only against a mock, never against real Vector hardware or
//! the real library. Please report issues.
//!
//! Notes:
//! - Interface names: `vector:<channel name as shown by the driver
//!   configuration>`, e.g. `vector:VN1630 Channel 1` or
//!   `vector:Virtual Channel 1`, or `vector:<channelIndex>`. When several
//!   channels share a name the list uses the index form.
//! - The channel is opened with init access: the bitrate (and the silent
//!   output mode for `listen_only`) is set. If another application holds init
//!   access the channel is still opened, the bitrate is left as configured
//!   there and the channel description says so.
//! - Classic channels use interface version V3 (`xlReceive` / `xlCanTransmit`);
//!   FD channels V4 (`xlCanReceive` / `xlCanTransmitEx`).
//! - FD bit timing assumes an 80 MHz CAN clock and about 80 % sample point
//!   (see [`fd_timing`]).
//! - Classic error frames carry no error type; they are reported as
//!   [`CanErrorKind::Bit`], the most generic kind.
//! - Reception is polled every millisecond (no notification event).
//! - Timestamps are the driver's nanosecond event time stamps.

pub mod ffi;

#[cfg(windows)]
mod dll;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use operow_core::{CanErrorKind, CanFrame, NodeErrorState, dlc_to_len};

use crate::{
    CanChannel, ChannelConfig, ChannelInfo, Driver, HwBusState, HwError, RxFrame, split_interface,
};
use ffi::*;

const DRIVER: &str = "vector";
#[cfg_attr(not(windows), allow(dead_code))]
const APP_NAME: &str = "Operow";
/// Largest backlog kept while looking for a chip state event.
const MAX_PENDING: usize = 4096;
/// Receive queue: events for V3, bytes for V4 (powers of two).
const RX_QUEUE_V3: u32 = 4096;
const RX_QUEUE_V4: u32 = 1 << 14;
const CAN_CLOCK_HZ: u64 = 80_000_000;

/// One channel of the driver configuration (`XLchannelConfig` subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct XlChannel {
    pub name: String,
    pub hw_type: u8,
    pub hw_index: u8,
    pub hw_channel: u8,
    pub channel_index: u8,
    pub mask: XlAccess,
    pub capabilities: u32,
    pub bus_capabilities: u32,
    pub is_on_bus: bool,
}

impl XlChannel {
    fn can_capable(&self) -> bool {
        self.bus_capabilities & (XL_BUS_COMPATIBLE_CAN | XL_BUS_ACTIVE_CAP_CAN) != 0
    }

    fn fd_capable(&self) -> bool {
        self.capabilities
            & (XL_CHANNEL_FLAG_CANFD_BOSCH_SUPPORT | XL_CHANNEL_FLAG_CANFD_ISO_SUPPORT)
            != 0
    }

    fn is_virtual(&self) -> bool {
        self.hw_type == XL_HWTYPE_VIRTUAL
    }
}

/// The XL Driver Library functions Operow needs. The real implementation
/// calls the DLL; tests use a mock. Errors are raw `XLstatus` codes.
pub(crate) trait XlApi: Send + Sync {
    fn open_driver(&self) -> Result<(), XlStatus>;
    fn close_driver(&self);
    fn channels(&self) -> Result<Vec<XlChannel>, XlStatus>;
    /// Returns the port handle and the granted init-access mask.
    fn open_port(
        &self,
        access: XlAccess,
        permission: XlAccess,
        rx_queue: u32,
        interface_version: u32,
    ) -> Result<(XlPortHandle, XlAccess), XlStatus>;
    fn set_bitrate(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        bitrate: u32,
    ) -> Result<(), XlStatus>;
    fn fd_set_configuration(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        conf: &XlCanFdConf,
    ) -> Result<(), XlStatus>;
    fn set_output(&self, port: XlPortHandle, access: XlAccess, mode: u8) -> Result<(), XlStatus>;
    fn activate(&self, port: XlPortHandle, access: XlAccess, flags: u32) -> Result<(), XlStatus>;
    fn deactivate(&self, port: XlPortHandle, access: XlAccess) -> Result<(), XlStatus>;
    fn close_port(&self, port: XlPortHandle);
    /// Number of events accepted.
    fn transmit(&self, port: XlPortHandle, access: XlAccess, ev: &XlEvent)
    -> Result<u32, XlStatus>;
    fn transmit_fd(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        ev: &XlCanTxEvent,
    ) -> Result<u32, XlStatus>;
    /// `Ok(None)` when the queue is empty.
    fn receive(&self, port: XlPortHandle) -> Result<Option<XlEvent>, XlStatus>;
    fn receive_fd(&self, port: XlPortHandle) -> Result<Option<XlCanRxEvent>, XlStatus>;
    fn request_chip_state(&self, port: XlPortHandle, access: XlAccess) -> Result<(), XlStatus>;
    fn error_string(&self, status: XlStatus) -> String;
}

/// The Vector driver. Without the library it is unavailable.
pub struct VectorDriver {
    api: Result<Arc<dyn XlApi>, String>,
}

impl VectorDriver {
    /// Tries to load the vendor library.
    pub fn new() -> Self {
        VectorDriver { api: load_system() }
    }

    #[cfg(test)]
    pub(crate) fn with_api(api: Arc<dyn XlApi>) -> Self {
        VectorDriver { api: Ok(api) }
    }
}

impl Default for VectorDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
fn load_system() -> Result<Arc<dyn XlApi>, String> {
    dll::load().map(|d| Arc::new(d) as Arc<dyn XlApi>)
}

#[cfg(not(windows))]
fn load_system() -> Result<Arc<dyn XlApi>, String> {
    Err("the Vector XL Driver Library is only available on Windows".into())
}

/// Message for a library that cannot be loaded.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn missing_library(dll: &str, err: &str) -> String {
    format!(
        "{dll} could not be loaded ({err}). Install the Vector XL Driver Library / Vector \
         Driver Setup"
    )
}

/// Opens the driver, runs `f`, closes the driver again.
fn with_driver<T>(api: &dyn XlApi, f: impl FnOnce() -> Result<T, XlStatus>) -> Result<T, String> {
    api.open_driver()
        .map_err(|s| format!("xlOpenDriver failed: {}", api.error_string(s)))?;
    let r = f();
    api.close_driver();
    r.map_err(|s| api.error_string(s))
}

fn can_channels(api: &dyn XlApi) -> Result<Vec<XlChannel>, String> {
    with_driver(api, || api.channels())
        .map(|v| v.into_iter().filter(XlChannel::can_capable).collect())
}

impl Driver for VectorDriver {
    fn name(&self) -> &str {
        DRIVER
    }

    fn available(&self) -> Result<(), String> {
        let api = self.api.as_ref().map_err(Clone::clone)?;
        with_driver(api.as_ref(), || Ok(()))
    }

    fn list_channels(&self) -> Vec<ChannelInfo> {
        let Ok(api) = &self.api else {
            return Vec::new();
        };
        let Ok(chs) = can_channels(api.as_ref()) else {
            return Vec::new();
        };
        chs.iter()
            .map(|c| {
                let dup = chs.iter().filter(|o| o.name == c.name).count() > 1;
                let sel = if dup {
                    c.channel_index.to_string()
                } else {
                    c.name.clone()
                };
                channel_info(c, &sel, None)
            })
            .collect()
    }

    fn open(&self, cfg: &ChannelConfig) -> Result<Box<dyn CanChannel>, HwError> {
        let (drv, sel) = split_interface(&cfg.interface)?;
        if drv != DRIVER {
            return Err(HwError::UnknownDriver(drv.to_string()));
        }
        let api = self
            .api
            .as_ref()
            .map_err(|e| HwError::NotAvailable(e.clone()))?;
        let open_err = |msg: String| HwError::Open {
            interface: cfg.interface.clone(),
            msg,
        };
        api.open_driver()
            .map_err(|s| open_err(format!("xlOpenDriver failed: {}", api.error_string(s))))?;
        match VectorChannel::open_port(api.clone(), cfg, sel) {
            Ok(ch) => Ok(Box::new(ch)),
            Err(msg) => {
                api.close_driver();
                Err(open_err(msg))
            }
        }
    }
}

fn channel_info(c: &XlChannel, sel: &str, warning: Option<&str>) -> ChannelInfo {
    let mut description = format!(
        "{}hw {} ch {} (index {})",
        if c.is_virtual() { "Virtual, " } else { "" },
        c.hw_index,
        c.hw_channel + 1,
        c.channel_index
    );
    if c.fd_capable() {
        description.push_str(", FD");
    }
    if let Some(w) = warning {
        description.push_str("; ");
        description.push_str(w);
    }
    ChannelInfo {
        driver: DRIVER.into(),
        name: format!("{DRIVER}:{sel}"),
        description,
        fd_capable: c.fd_capable(),
        is_virtual: c.is_virtual(),
    }
}

/// Picks the channel for `sel`: by name (case-insensitive), else by index.
fn resolve<'a>(chs: &'a [XlChannel], sel: &str) -> Option<&'a XlChannel> {
    chs.iter()
        .find(|c| c.name.eq_ignore_ascii_case(sel))
        .or_else(|| {
            let idx: u8 = sel.trim().parse().ok()?;
            chs.iter().find(|c| c.channel_index == idx)
        })
}

/// `(sjw, tseg1, tseg2)` in time quanta for `bitrate`, assuming an 80 MHz CAN
/// clock: the quanta per bit are chosen so that the prescaler is an integer,
/// with a sample point of about 80 %.
pub(crate) fn fd_timing(bitrate: u32) -> (u32, u32, u32) {
    let bitrate = u64::from(bitrate.max(1));
    let total = [20u64, 16, 10, 8]
        .into_iter()
        .find(|n| CAN_CLOCK_HZ.is_multiple_of(bitrate * n))
        .unwrap_or(10);
    let tseg2 = (total / 5).max(2);
    let tseg1 = total - 1 - tseg2;
    (tseg2 as u32, tseg1 as u32, tseg2 as u32)
}

struct VectorChannel {
    api: Arc<dyn XlApi>,
    port: XlPortHandle,
    access: XlAccess,
    closed: bool,
    info: ChannelInfo,
    fd_mode: bool,
    listen_only: bool,
    receive_own: bool,
    state: HwBusState,
    pending: VecDeque<RxFrame>,
}

impl VectorChannel {
    /// Opens, configures and activates a port; the driver is already open.
    fn open_port(api: Arc<dyn XlApi>, cfg: &ChannelConfig, sel: &str) -> Result<Self, String> {
        let st = |what: &str, s: XlStatus| format!("{what} failed: {}", api.error_string(s));
        let chs = api
            .channels()
            .map_err(|s| st("xlGetDriverConfig", s))?
            .into_iter()
            .filter(XlChannel::can_capable)
            .collect::<Vec<_>>();
        let ch = resolve(&chs, sel).ok_or_else(|| {
            format!("no Vector CAN channel {sel:?} (see the interface list for names)")
        })?;
        if cfg.fd && !ch.fd_capable() {
            return Err("this channel does not support CAN FD".into());
        }
        let (version, queue) = if cfg.fd {
            (XL_INTERFACE_VERSION_V4, RX_QUEUE_V4)
        } else {
            (XL_INTERFACE_VERSION_V3, RX_QUEUE_V3)
        };
        let (port, granted) = api
            .open_port(ch.mask, ch.mask, queue, version)
            .map_err(|s| st("xlOpenPort", s))?;

        let mut warnings: Vec<&str> = Vec::new();
        let configured = (|| -> Result<(), String> {
            if granted & ch.mask == ch.mask {
                let r = if cfg.fd {
                    let (sa, t1a, t2a) = fd_timing(cfg.bitrate);
                    let (sd, t1d, t2d) = fd_timing(cfg.data_bitrate);
                    api.fd_set_configuration(
                        port,
                        ch.mask,
                        &XlCanFdConf {
                            arbitration_bit_rate: cfg.bitrate,
                            sjw_abr: sa,
                            tseg1_abr: t1a,
                            tseg2_abr: t2a,
                            data_bit_rate: cfg.data_bitrate,
                            sjw_dbr: sd,
                            tseg1_dbr: t1d,
                            tseg2_dbr: t2d,
                            ..Default::default()
                        },
                    )
                } else {
                    api.set_bitrate(port, ch.mask, cfg.bitrate)
                };
                match r {
                    Ok(()) => {}
                    Err(XL_ERR_INVALID_ACCESS) => warnings.push("bitrate not set (no init access)"),
                    Err(s) => return Err(st("setting the bitrate", s)),
                }
                let mode = if cfg.listen_only {
                    XL_OUTPUT_MODE_SILENT
                } else {
                    XL_OUTPUT_MODE_NORMAL
                };
                match api.set_output(port, ch.mask, mode) {
                    Ok(()) | Err(XL_ERR_INVALID_ACCESS) => {}
                    Err(s) => return Err(st("xlCanSetChannelOutput", s)),
                }
            } else {
                warnings.push("bitrate not set (another application has init access)");
            }
            api.activate(port, ch.mask, XL_ACTIVATE_RESET_CLOCK)
                .map_err(|s| st("xlActivateChannel", s))
        })();
        if let Err(e) = configured {
            api.close_port(port);
            return Err(e);
        }
        let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
        Ok(VectorChannel {
            info: channel_info(ch, sel, warning.as_deref()),
            access: ch.mask,
            api,
            port,
            closed: false,
            fd_mode: cfg.fd,
            listen_only: cfg.listen_only,
            receive_own: cfg.receive_own,
            state: HwBusState::default(),
            pending: VecDeque::new(),
        })
    }

    fn io(&self, what: &str, s: XlStatus) -> HwError {
        HwError::Io(format!("{what}: {}", self.api.error_string(s)))
    }

    /// Reads and processes one event. `Ok(false)` when the queue is empty.
    fn pump_one(&mut self) -> Result<bool, HwError> {
        let decoded = if self.fd_mode {
            match self.api.receive_fd(self.port) {
                Ok(Some(ev)) => decode_fd(&ev),
                Ok(None) => return Ok(false),
                Err(s) => return Err(self.io("xlCanReceive", s)),
            }
        } else {
            match self.api.receive(self.port) {
                Ok(Some(ev)) => decode_classic(&ev),
                Ok(None) => return Ok(false),
                Err(s) => return Err(self.io("xlReceive", s)),
            }
        };
        match decoded {
            Decoded::Frame(f) => {
                if (!f.is_echo || self.receive_own) && self.pending.len() < MAX_PENDING {
                    self.pending.push_back(f);
                }
            }
            Decoded::State(s) => self.state = s,
            Decoded::Ignored => {}
        }
        Ok(true)
    }

    fn check_open(&self) -> Result<(), HwError> {
        if self.closed {
            Err(HwError::Closed)
        } else {
            Ok(())
        }
    }
}

enum Decoded {
    Frame(RxFrame),
    State(HwBusState),
    Ignored,
}

fn error_frame(ts: u64, kind: CanErrorKind) -> Decoded {
    Decoded::Frame(RxFrame {
        frame: CanFrame::new(0, false, &[]).expect("empty frame"),
        timestamp_ns: ts,
        is_echo: false,
        error: Some(kind),
    })
}

fn split_id(raw: u32) -> (u32, bool) {
    if raw & XL_CAN_EXT_MSG_ID != 0 {
        (raw & 0x1FFF_FFFF, true)
    } else {
        (raw & 0x7FF, false)
    }
}

fn chip_state(bus_status: u8, tec: u8, rec: u8) -> HwBusState {
    let state = if bus_status & XL_CHIPSTAT_BUSOFF != 0 {
        NodeErrorState::BusOff
    } else if bus_status & XL_CHIPSTAT_ERROR_PASSIVE != 0 {
        NodeErrorState::ErrorPassive
    } else {
        NodeErrorState::ErrorActive
    };
    HwBusState {
        state,
        tec: u16::from(tec),
        rec: u16::from(rec),
    }
}

fn decode_classic(ev: &XlEvent) -> Decoded {
    match ev.tag {
        XL_RECEIVE_MSG => {
            let (id, flags, dlc, data) = ev.can_msg();
            if flags & XL_CAN_MSG_FLAG_ERROR_FRAME != 0 {
                return error_frame(ev.time_stamp, CanErrorKind::Bit);
            }
            if flags & (XL_CAN_MSG_FLAG_REMOTE_FRAME | XL_CAN_MSG_FLAG_TX_REQUEST) != 0 {
                return Decoded::Ignored; // remote frames are not modelled
            }
            let (id, extended) = split_id(id);
            match CanFrame::new(id, extended, &data[..(dlc as usize).min(8)]) {
                Ok(frame) => Decoded::Frame(RxFrame {
                    frame,
                    timestamp_ns: ev.time_stamp,
                    is_echo: flags & XL_CAN_MSG_FLAG_TX_COMPLETED != 0,
                    error: None,
                }),
                Err(_) => Decoded::Ignored,
            }
        }
        XL_CHIP_STATE => Decoded::State(chip_state(ev.tag_data[0], ev.tag_data[1], ev.tag_data[2])),
        _ => Decoded::Ignored,
    }
}

fn decode_fd(ev: &XlCanRxEvent) -> Decoded {
    let ts = ev.time_stamp_sync;
    match ev.tag {
        XL_CAN_EV_TAG_RX_OK | XL_CAN_EV_TAG_TX_OK => {
            let (id, flags, dlc, data) = ev.rx_msg();
            if flags & XL_CAN_RXMSG_FLAG_EF != 0 {
                return error_frame(ts, CanErrorKind::Bit);
            }
            if flags & XL_CAN_RXMSG_FLAG_RTR != 0 {
                return Decoded::Ignored;
            }
            let (id, extended) = split_id(id);
            let fd = flags & XL_CAN_RXMSG_FLAG_EDL != 0;
            let len = dlc_to_len(dlc & 0x0F).min(if fd { 64 } else { 8 });
            let frame = if fd {
                CanFrame::new_fd(
                    id,
                    extended,
                    flags & XL_CAN_RXMSG_FLAG_BRS != 0,
                    &data[..len],
                )
            } else {
                CanFrame::new(id, extended, &data[..len])
            };
            match frame {
                Ok(frame) => Decoded::Frame(RxFrame {
                    frame,
                    timestamp_ns: ts,
                    is_echo: ev.tag == XL_CAN_EV_TAG_TX_OK,
                    error: None,
                }),
                Err(_) => Decoded::Ignored,
            }
        }
        XL_CAN_EV_TAG_RX_ERROR | XL_CAN_EV_TAG_TX_ERROR => match ev.tag_data[0] {
            XL_CAN_ERRC_BIT_ERROR | XL_CAN_ERRC_OTHER_ERROR => error_frame(ts, CanErrorKind::Bit),
            XL_CAN_ERRC_FORM_ERROR => error_frame(ts, CanErrorKind::Form),
            XL_CAN_ERRC_STUFF_ERROR => error_frame(ts, CanErrorKind::Stuff),
            XL_CAN_ERRC_CRC_ERROR => error_frame(ts, CanErrorKind::Crc),
            XL_CAN_ERRC_ACK_ERROR | XL_CAN_ERRC_NACK_ERROR => error_frame(ts, CanErrorKind::Ack),
            _ => Decoded::Ignored, // overload / exception frames
        },
        XL_CAN_EV_TAG_CHIP_STATE => {
            Decoded::State(chip_state(ev.tag_data[0], ev.tag_data[1], ev.tag_data[2]))
        }
        _ => Decoded::Ignored,
    }
}

fn tx_id(frame: &CanFrame) -> u32 {
    if frame.extended {
        (frame.id & 0x1FFF_FFFF) | XL_CAN_EXT_MSG_ID
    } else {
        frame.id & 0x7FF
    }
}

impl CanChannel for VectorChannel {
    fn send(&mut self, frame: &CanFrame) -> Result<(), HwError> {
        self.check_open()?;
        if self.listen_only {
            return Err(HwError::ListenOnly);
        }
        if frame.fd && !self.fd_mode {
            return Err(HwError::Unsupported(
                "CAN FD frame on a channel opened without fd".into(),
            ));
        }
        let sent = if self.fd_mode {
            let mut ev = XlCanTxEvent::zeroed();
            ev.tag = XL_CAN_EV_TAG_TX_MSG;
            ev.can_id = tx_id(frame);
            if frame.fd {
                ev.msg_flags =
                    XL_CAN_TXMSG_FLAG_EDL | if frame.brs { XL_CAN_TXMSG_FLAG_BRS } else { 0 };
            }
            ev.dlc = frame.dlc_code();
            ev.data[..frame.dlc as usize].copy_from_slice(frame.payload());
            self.api.transmit_fd(self.port, self.access, &ev)
        } else {
            let mut ev = XlEvent {
                tag: XL_TRANSMIT_MSG,
                ..Default::default()
            };
            ev.set_can_msg(tx_id(frame), 0, u16::from(frame.dlc), frame.payload());
            self.api.transmit(self.port, self.access, &ev)
        };
        match sent {
            Ok(0) | Err(XL_ERR_QUEUE_IS_FULL) => Err(HwError::TxQueueFull),
            Ok(_) => Ok(()),
            Err(s) => Err(self.io("transmit", s)),
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<RxFrame>, HwError> {
        self.check_open()?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(f) = self.pending.pop_front() {
                return Ok(Some(f));
            }
            if self.pump_one()? {
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
        }
    }

    fn bus_state(&mut self) -> Result<HwBusState, HwError> {
        self.check_open()?;
        self.api
            .request_chip_state(self.port, self.access)
            .map_err(|s| self.io("xlCanRequestChipState", s))?;
        // Frames read meanwhile stay queued for `recv`.
        while self.pending.len() < MAX_PENDING && self.pump_one()? {}
        Ok(self.state)
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.api.deactivate(self.port, self.access);
            self.api.close_port(self.port);
            self.api.close_driver();
        }
    }

    fn info(&self) -> ChannelInfo {
        self.info.clone()
    }
}

impl Drop for VectorChannel {
    fn drop(&mut self) {
        self.close();
    }
}
