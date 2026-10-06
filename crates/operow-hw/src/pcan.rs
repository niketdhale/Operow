//! PEAK PCAN-Basic driver (`pcan:<channel>`).
//!
//! The vendor library (`PCANBasic.dll` on Windows, `libpcanbasic.so` on
//! Linux) is loaded at run time; there is no link-time dependency and no SDK
//! header. [`ffi`] holds the hand-written subset of the public `PCANBasic.h`
//! that is used. The driver logic is written against the small [`PcanApi`]
//! function table, so it is compiled and unit-tested (with a mock) on every
//! operating system; only the library loader (`dll.rs`) is Windows / Linux
//! only. On macOS (where only the third-party PCBUSB library from the MacCAN
//! project exists; it could be added later) and other systems the driver
//! reports itself unavailable.
//!
//! Status: tested only against a mock, never against real PEAK hardware or
//! the real library. Please report issues.
//!
//! Notes:
//! - Interface names: `pcan:PCAN_USBBUS1`, `pcan:USBBUS1`, `pcan:usb1` or the
//!   raw handle `pcan:0x51`; the `PCI`, `LAN` and `PCC` families work alike.
//! - On Linux, PEAK's mainline kernel driver (`peak_usb`) exposes PCAN
//!   devices as SocketCAN interfaces (`can0`): SocketCAN is the recommended
//!   path there. `pcan:` is for PEAK's out-of-tree character device driver
//!   (`peak-linux-driver`) together with PCAN-Basic for Linux.
//! - Classic channels use `CAN_Initialize` with a BTR0/BTR1 register value,
//!   so only the standard rates (5 kbit/s to 1 Mbit/s) are supported; other
//!   rates fail to open. FD channels use `CAN_InitializeFD` with a bit timing
//!   string computed for an 80 MHz CAN clock and about 80 % sample point (see
//!   [`fd_timing`]); no secondary sample point is configured.
//! - `listen_only` sets `PCAN_LISTEN_ONLY` before the channel is initialised.
//! - `receive_own` enables `PCAN_ALLOW_ECHO_FRAMES`; when the library does not
//!   support that parameter, opening with `receive_own` fails with
//!   [`HwError::Unsupported`].
//! - Error frames are enabled (`PCAN_ALLOW_ERROR_FRAMES`); PCAN-Basic does
//!   not document the payload of an error frame, so all are reported as
//!   [`CanErrorKind::Bit`], the most generic kind. Remote frames and status
//!   messages are ignored.
//! - Reception is polled every millisecond (no receive event).
//! - Timestamps are the driver's microsecond time stamps.
//! - The bus state comes from `CAN_GetStatus`; the library has no error
//!   counters, so `tec` / `rec` are 0.

pub mod ffi;

#[cfg(any(windows, target_os = "linux"))]
mod dll;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, Instant};

use operow_core::{CanErrorKind, CanFrame, NodeErrorState, dlc_to_len};

use crate::{
    CanChannel, ChannelConfig, ChannelInfo, Driver, HwBusState, HwError, RxFrame, split_interface,
};
use ffi::*;

const DRIVER: &str = "pcan";
const CAN_CLOCK_HZ: u64 = 80_000_000;
/// Status bits that describe the bus or the queues but do not make a
/// `CAN_Read` / `CAN_GetStatus` call a failure.
const BENIGN_STATUS: PcanStatus = PCAN_ERROR_ANYBUSERR
    | PCAN_ERROR_XMTFULL
    | PCAN_ERROR_OVERRUN
    | PCAN_ERROR_QRCVEMPTY
    | PCAN_ERROR_QOVERRUN
    | PCAN_ERROR_QXMTFULL
    | PCAN_ERROR_CAUTION;

/// One attached channel as reported by the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PcanChannel {
    pub handle: PcanHandle,
    /// Device name, e.g. `PCAN-USB FD`; empty when unknown.
    pub device_name: String,
    pub features: u32,
    pub condition: u32,
}

/// The PCAN-Basic functions Operow needs. The real implementation calls the
/// library; tests use a mock. Errors are raw `TPCANStatus` codes.
pub(crate) trait PcanApi: Send + Sync {
    fn initialize(&self, channel: PcanHandle, btr0btr1: u16) -> PcanStatus;
    fn initialize_fd(&self, channel: PcanHandle, bitrate: &str) -> PcanStatus;
    fn uninitialize(&self, channel: PcanHandle) -> PcanStatus;
    fn get_status(&self, channel: PcanHandle) -> PcanStatus;
    /// `Err(PCAN_ERROR_QRCVEMPTY)` when the queue is empty.
    fn read(&self, channel: PcanHandle) -> Result<(PcanMsg, PcanTimestamp), PcanStatus>;
    /// The timestamp is in microseconds.
    fn read_fd(&self, channel: PcanHandle) -> Result<(PcanMsgFd, u64), PcanStatus>;
    fn write(&self, channel: PcanHandle, msg: &PcanMsg) -> PcanStatus;
    fn write_fd(&self, channel: PcanHandle, msg: &PcanMsgFd) -> PcanStatus;
    /// `CAN_SetValue` of a 32-bit parameter.
    fn set_param(&self, channel: PcanHandle, param: u8, value: u32) -> PcanStatus;
    /// `PCAN_ATTACHED_CHANNELS`; `Err` when the library does not support it.
    fn attached_channels(&self) -> Result<Vec<PcanChannel>, PcanStatus>;
    /// `PCAN_CHANNEL_CONDITION`.
    fn channel_condition(&self, channel: PcanHandle) -> Result<u32, PcanStatus>;
    /// `PCAN_CHANNEL_FEATURES`.
    fn channel_features(&self, channel: PcanHandle) -> Result<u32, PcanStatus>;
    /// `PCAN_HARDWARE_NAME`.
    fn hardware_name(&self, channel: PcanHandle) -> Result<String, PcanStatus>;
    fn error_text(&self, status: PcanStatus) -> String;
}

/// The PCAN driver. Without the library it is unavailable.
pub struct PcanDriver {
    api: Result<Arc<dyn PcanApi>, String>,
}

impl PcanDriver {
    /// Tries to load the vendor library.
    pub fn new() -> Self {
        PcanDriver { api: load_system() }
    }

    #[cfg(test)]
    pub(crate) fn with_api(api: Arc<dyn PcanApi>) -> Self {
        PcanDriver { api: Ok(api) }
    }
}

impl Default for PcanDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn load_system() -> Result<Arc<dyn PcanApi>, String> {
    dll::load().map(|d| Arc::new(d) as Arc<dyn PcanApi>)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn load_system() -> Result<Arc<dyn PcanApi>, String> {
    Err(
        "PCAN-Basic is only supported on Windows and Linux (macOS would need the PCBUSB library)"
            .into(),
    )
}

/// Message for a library that cannot be loaded.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(crate) fn missing_library(lib: &str, err: &str) -> String {
    format!(
        "{lib} could not be loaded ({err}). Install PEAK PCAN-Basic (Windows: PCAN driver \
         package; Linux: PCAN-Basic for Linux / peak-linux-driver)"
    )
}

/// Channel families: `(name, first handle of channels 1..=8, first handle of
/// channels 9..=16, channel count)`.
const FAMILIES: [(&str, PcanHandle, PcanHandle, u16); 4] = [
    ("USBBUS", 0x50, 0x500, 16),
    ("PCIBUS", 0x40, 0x400, 16),
    ("PCCBUS", 0x60, 0x60, 2),
    ("LANBUS", 0x800, 0x800, 16),
];

/// Handle of channel `n` (1-based) of a family.
fn family_handle(family: usize, n: u16) -> Option<PcanHandle> {
    let (_, low, high, count) = FAMILIES[family];
    match n {
        1..=8 if n <= count => Some(low + n),
        9..=16 if n <= count => Some(high + n),
        _ => None,
    }
}

/// `PCAN_USBBUS1` style name of a handle.
fn handle_name(handle: PcanHandle) -> String {
    for (i, (name, ..)) in FAMILIES.iter().enumerate() {
        if let Some(n) = (1..=16).find(|&n| family_handle(i, n) == Some(handle)) {
            return format!("PCAN_{name}{n}");
        }
    }
    format!("PCAN_0x{handle:X}")
}

/// Parses `PCAN_USBBUS1`, `USBBUS1`, `usb1`, `0x51` (case-insensitive).
fn parse_handle(sel: &str) -> Option<PcanHandle> {
    let s = sel.trim().to_ascii_uppercase();
    if let Some(hex) = s.strip_prefix("0X") {
        return PcanHandle::from_str_radix(hex, 16)
            .ok()
            .filter(|&h| h != PCAN_NONEBUS);
    }
    let s = s.strip_prefix("PCAN_").unwrap_or(&s);
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (word, digits) = s.split_at(split);
    let n: u16 = digits.parse().ok()?;
    let family = FAMILIES.iter().position(|(name, ..)| {
        let short = &name[..3];
        word == *name || word == short
    })?;
    family_handle(family, n)
}

/// BTR0/BTR1 register value for a classic bitrate.
pub(crate) fn baud_register(bitrate: u32) -> Option<u16> {
    Some(match bitrate {
        1_000_000 => PCAN_BAUD_1M,
        800_000 => PCAN_BAUD_800K,
        500_000 => PCAN_BAUD_500K,
        250_000 => PCAN_BAUD_250K,
        125_000 => PCAN_BAUD_125K,
        100_000 => PCAN_BAUD_100K,
        95_238 | 95_000 => PCAN_BAUD_95K,
        83_333 | 83_000 => PCAN_BAUD_83K,
        50_000 => PCAN_BAUD_50K,
        47_619 | 47_000 => PCAN_BAUD_47K,
        33_333 | 33_000 => PCAN_BAUD_33K,
        20_000 => PCAN_BAUD_20K,
        10_000 => PCAN_BAUD_10K,
        5_000 => PCAN_BAUD_5K,
        _ => return None,
    })
}

/// Bit timing of one FD phase, in time quanta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FdTiming {
    pub brp: u32,
    pub tseg1: u32,
    pub tseg2: u32,
    pub sjw: u32,
}

/// Bit timing for `bitrate` assuming an 80 MHz CAN clock: the quanta per bit
/// are chosen so that the prescaler is an integer (preferring 20, 16, 10 and
/// 8), with a sample point of about 80 %. `None` when no timing fits.
pub(crate) fn fd_timing(bitrate: u32) -> Option<FdTiming> {
    let rate = u64::from(bitrate);
    if rate == 0 {
        return None;
    }
    let total = [20u64, 16, 10, 8]
        .into_iter()
        .chain((8..=25).rev())
        .find(|n| CAN_CLOCK_HZ.is_multiple_of(rate * n))?;
    let brp = CAN_CLOCK_HZ / (rate * total);
    if !(1..=1024).contains(&brp) {
        return None;
    }
    let tseg2 = ((total * 2 + 5) / 10).max(2);
    let tseg1 = total - 1 - tseg2;
    Some(FdTiming {
        brp: brp as u32,
        tseg1: tseg1 as u32,
        tseg2: tseg2 as u32,
        sjw: tseg2 as u32,
    })
}

/// The `CAN_InitializeFD` bit timing string.
pub(crate) fn fd_bitrate_string(bitrate: u32, data_bitrate: u32) -> Result<String, String> {
    let nom = fd_timing(bitrate)
        .ok_or_else(|| format!("unsupported nominal bitrate {bitrate} bit/s for CAN FD"))?;
    let data = fd_timing(data_bitrate)
        .ok_or_else(|| format!("unsupported data bitrate {data_bitrate} bit/s for CAN FD"))?;
    Ok(format!(
        "f_clock_mhz={}, nom_brp={}, nom_tseg1={}, nom_tseg2={}, nom_sjw={}, data_brp={}, \
         data_tseg1={}, data_tseg2={}, data_sjw={}",
        CAN_CLOCK_HZ / 1_000_000,
        nom.brp,
        nom.tseg1,
        nom.tseg2,
        nom.sjw,
        data.brp,
        data.tseg1,
        data.tseg2,
        data.sjw
    ))
}

/// Attached channels: `PCAN_ATTACHED_CHANNELS`, or when the library does not
/// support it, a probe of `PCAN_CHANNEL_CONDITION` of USBBUS1..16.
fn attached(api: &dyn PcanApi) -> Vec<PcanChannel> {
    let present = |c: u32| c == PCAN_CHANNEL_AVAILABLE || c == PCAN_CHANNEL_OCCUPIED;
    let v = match api.attached_channels() {
        Ok(v) => v,
        Err(_) => (1..=16)
            .filter_map(|n| family_handle(0, n))
            .filter_map(|handle| {
                let condition = api.channel_condition(handle).ok()?;
                present(condition).then(|| PcanChannel {
                    handle,
                    device_name: api.hardware_name(handle).unwrap_or_default(),
                    features: api.channel_features(handle).unwrap_or(0),
                    condition,
                })
            })
            .collect(),
    };
    v.into_iter()
        .filter(|c| c.condition != PCAN_CHANNEL_UNAVAILABLE)
        .collect()
}

fn channel_info(handle: PcanHandle, device_name: &str, features: u32) -> ChannelInfo {
    let full = handle_name(handle);
    let short = full.strip_prefix("PCAN_").unwrap_or(&full);
    let device = if device_name.is_empty() {
        "PCAN"
    } else {
        device_name
    };
    ChannelInfo {
        driver: DRIVER.into(),
        name: format!("{DRIVER}:{full}"),
        description: format!("{device} ({short})"),
        fd_capable: features & FEATURE_FD_CAPABLE != 0,
        is_virtual: false,
    }
}

impl Driver for PcanDriver {
    fn name(&self) -> &str {
        DRIVER
    }

    fn available(&self) -> Result<(), String> {
        self.api.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    fn list_channels(&self) -> Vec<ChannelInfo> {
        let Ok(api) = &self.api else {
            return Vec::new();
        };
        attached(api.as_ref())
            .iter()
            .map(|c| channel_info(c.handle, &c.device_name, c.features))
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
        PcanChannelImpl::open(api.clone(), cfg, sel).map(|c| Box::new(c) as Box<dyn CanChannel>)
    }
}

struct PcanChannelImpl {
    api: Arc<dyn PcanApi>,
    handle: PcanHandle,
    closed: bool,
    info: ChannelInfo,
    fd_mode: bool,
    listen_only: bool,
    receive_own: bool,
}

impl PcanChannelImpl {
    fn open(api: Arc<dyn PcanApi>, cfg: &ChannelConfig, sel: &str) -> Result<Self, HwError> {
        let open_err = |msg: String| HwError::Open {
            interface: cfg.interface.clone(),
            msg,
        };
        let st = |what: &str, s: PcanStatus| format!("{what} failed: {}", api.error_text(s));
        let handle = parse_handle(sel).ok_or_else(|| {
            open_err(format!(
                "unknown PCAN channel {sel:?} (use e.g. PCAN_USBBUS1, usb1 or 0x51)"
            ))
        })?;
        // Validate the bitrate before touching the hardware.
        enum Rate {
            Classic(u16),
            Fd(String),
        }
        let rate = if cfg.fd {
            Rate::Fd(fd_bitrate_string(cfg.bitrate, cfg.data_bitrate).map_err(open_err)?)
        } else {
            Rate::Classic(baud_register(cfg.bitrate).ok_or_else(|| {
                open_err(format!(
                    "unsupported bitrate {} bit/s (PCAN-Basic supports 5k, 10k, 20k, 33.333k, 47.619k, \
                     50k, 83.333k, 95.238k, 100k, 125k, 250k, 500k, 800k and 1M for classic CAN)",
                    cfg.bitrate
                ))
            })?)
        };
        let features = api.channel_features(handle);
        if cfg.fd && matches!(features, Ok(f) if f & FEATURE_FD_CAPABLE == 0) {
            return Err(open_err("this channel does not support CAN FD".into()));
        }
        let features = features.unwrap_or(0);
        // Listen-only has to be set while the channel is not initialised yet.
        if cfg.listen_only {
            let s = api.set_param(handle, PCAN_LISTEN_ONLY, PCAN_PARAMETER_ON);
            if s != PCAN_ERROR_OK {
                return Err(open_err(st("setting PCAN_LISTEN_ONLY", s)));
            }
        }
        let s = match &rate {
            Rate::Classic(btr) => api.initialize(handle, *btr),
            Rate::Fd(text) => api.initialize_fd(handle, text),
        };
        if s != PCAN_ERROR_OK {
            return Err(open_err(st(
                if cfg.fd {
                    "CAN_InitializeFD"
                } else {
                    "CAN_Initialize"
                },
                s,
            )));
        }
        // Error frames are best effort; a library without them still works.
        let _ = api.set_param(handle, PCAN_ALLOW_ERROR_FRAMES, PCAN_PARAMETER_ON);
        if cfg.receive_own {
            let s = api.set_param(handle, PCAN_ALLOW_ECHO_FRAMES, PCAN_PARAMETER_ON);
            if s != PCAN_ERROR_OK {
                api.uninitialize(handle);
                return Err(HwError::Unsupported(format!(
                    "receive_own: this PCAN-Basic library cannot echo transmitted frames ({})",
                    api.error_text(s)
                )));
            }
        }
        let name = api.hardware_name(handle).unwrap_or_default();
        Ok(PcanChannelImpl {
            info: channel_info(handle, &name, features),
            api,
            handle,
            closed: false,
            fd_mode: cfg.fd,
            listen_only: cfg.listen_only,
            receive_own: cfg.receive_own,
        })
    }

    fn io(&self, what: &str, s: PcanStatus) -> HwError {
        HwError::Io(format!("{what}: {}", self.api.error_text(s)))
    }

    fn check_open(&self) -> Result<(), HwError> {
        if self.closed {
            Err(HwError::Closed)
        } else {
            Ok(())
        }
    }

    /// Reads and decodes one message.
    fn poll(&mut self) -> Result<Poll, HwError> {
        let decoded = if self.fd_mode {
            match self.api.read_fd(self.handle) {
                Ok((m, ts_us)) => decode_fd(&m, ts_us),
                Err(s) => return self.read_failed("CAN_ReadFD", s),
            }
        } else {
            match self.api.read(self.handle) {
                Ok((m, ts)) => decode_classic(&m, &ts),
                Err(s) => return self.read_failed("CAN_Read", s),
            }
        };
        Ok(match decoded {
            Decoded::Frame(f) if f.is_echo && !self.receive_own => Poll::Skip,
            Decoded::Frame(f) => Poll::Frame(f),
            Decoded::Ignored => Poll::Skip,
        })
    }

    fn read_failed(&self, what: &str, s: PcanStatus) -> Result<Poll, HwError> {
        // An empty queue, a bus status reported by the read, or an invalid
        // frame on the bus (reported before its error frame) is not a failure.
        if s & PCAN_ERROR_QRCVEMPTY != 0 || s == PCAN_ERROR_ILLDATA || s & !BENIGN_STATUS == 0 {
            Ok(Poll::Empty)
        } else {
            Err(self.io(what, s))
        }
    }
}

enum Poll {
    Frame(RxFrame),
    /// A message that is not delivered; try the next one.
    Skip,
    Empty,
}

enum Decoded {
    Frame(RxFrame),
    Ignored,
}

/// Total microseconds of a classic timestamp, as nanoseconds.
pub(crate) fn timestamp_ns(ts: &PcanTimestamp) -> u64 {
    let micros = u64::from(ts.micros)
        + 1000 * u64::from(ts.millis)
        + 0x1_0000_0000 * 1000 * u64::from(ts.millis_overflow);
    micros * 1000
}

fn split_id(raw: u32, msg_type: u8) -> (u32, bool) {
    if msg_type & PCAN_MESSAGE_EXTENDED != 0 {
        (raw & 0x1FFF_FFFF, true)
    } else {
        (raw & 0x7FF, false)
    }
}

/// Handling shared by the classic and FD decoders: error frames, status
/// messages and remote frames.
fn special(msg_type: u8, ts: u64) -> Option<Decoded> {
    if msg_type & PCAN_MESSAGE_ERRFRAME != 0 {
        return Some(Decoded::Frame(RxFrame {
            frame: CanFrame::new(0, false, &[]).expect("empty frame"),
            timestamp_ns: ts,
            is_echo: false,
            error: Some(CanErrorKind::Bit),
        }));
    }
    if msg_type & (PCAN_MESSAGE_STATUS | PCAN_MESSAGE_RTR) != 0 {
        return Some(Decoded::Ignored); // remote frames are not modelled
    }
    None
}

fn decode_classic(m: &PcanMsg, ts: &PcanTimestamp) -> Decoded {
    let ts = timestamp_ns(ts);
    if let Some(d) = special(m.msg_type, ts) {
        return d;
    }
    let (id, extended) = split_id(m.id, m.msg_type);
    match CanFrame::new(id, extended, &m.data[..(m.len as usize).min(8)]) {
        Ok(frame) => Decoded::Frame(RxFrame {
            frame,
            timestamp_ns: ts,
            is_echo: m.msg_type & PCAN_MESSAGE_ECHO != 0,
            error: None,
        }),
        Err(_) => Decoded::Ignored,
    }
}

fn decode_fd(m: &PcanMsgFd, ts_us: u64) -> Decoded {
    let ts = ts_us.saturating_mul(1000);
    if let Some(d) = special(m.msg_type, ts) {
        return d;
    }
    let (id, extended) = split_id(m.id, m.msg_type);
    let fd = m.msg_type & PCAN_MESSAGE_FD != 0;
    let len = dlc_to_len(m.dlc & 0x0F).min(if fd { 64 } else { 8 });
    let frame = if fd {
        CanFrame::new_fd(
            id,
            extended,
            m.msg_type & PCAN_MESSAGE_BRS != 0,
            &m.data[..len],
        )
    } else {
        CanFrame::new(id, extended, &m.data[..len])
    };
    match frame {
        Ok(frame) => Decoded::Frame(RxFrame {
            frame,
            timestamp_ns: ts,
            is_echo: m.msg_type & PCAN_MESSAGE_ECHO != 0,
            error: None,
        }),
        Err(_) => Decoded::Ignored,
    }
}

/// Maps a `CAN_GetStatus` result to the controller state; `None` for a
/// status that is a failure rather than a bus state.
pub(crate) fn bus_state_of(status: PcanStatus) -> Option<HwBusState> {
    if status & !BENIGN_STATUS != 0 {
        return None;
    }
    let state = if status & PCAN_ERROR_BUSOFF != 0 {
        NodeErrorState::BusOff
    } else if status & PCAN_ERROR_BUSPASSIVE != 0 {
        NodeErrorState::ErrorPassive
    } else {
        NodeErrorState::ErrorActive
    };
    Some(HwBusState {
        state,
        tec: 0,
        rec: 0,
    })
}

fn tx_type(frame: &CanFrame) -> u8 {
    let mut t = if frame.extended {
        PCAN_MESSAGE_EXTENDED
    } else {
        PCAN_MESSAGE_STANDARD
    };
    if frame.fd {
        t |= PCAN_MESSAGE_FD;
        if frame.brs {
            t |= PCAN_MESSAGE_BRS;
        }
    }
    t
}

fn tx_id(frame: &CanFrame) -> u32 {
    if frame.extended {
        frame.id & 0x1FFF_FFFF
    } else {
        frame.id & 0x7FF
    }
}

impl CanChannel for PcanChannelImpl {
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
        let status = if self.fd_mode {
            let mut m = PcanMsgFd {
                id: tx_id(frame),
                msg_type: tx_type(frame),
                dlc: frame.dlc_code(),
                ..Default::default()
            };
            m.data[..frame.dlc as usize].copy_from_slice(frame.payload());
            self.api.write_fd(self.handle, &m)
        } else {
            let mut m = PcanMsg {
                id: tx_id(frame),
                msg_type: tx_type(frame),
                len: frame.dlc,
                ..Default::default()
            };
            m.data[..frame.dlc as usize].copy_from_slice(frame.payload());
            self.api.write(self.handle, &m)
        };
        if status == PCAN_ERROR_OK {
            Ok(())
        } else if status & (PCAN_ERROR_XMTFULL | PCAN_ERROR_QXMTFULL) != 0 {
            Err(HwError::TxQueueFull)
        } else {
            Err(self.io("CAN_Write", status))
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<RxFrame>, HwError> {
        self.check_open()?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.poll()? {
                Poll::Frame(f) => return Ok(Some(f)),
                Poll::Skip => continue,
                Poll::Empty => {}
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
        let s = self.api.get_status(self.handle);
        bus_state_of(s).ok_or_else(|| self.io("CAN_GetStatus", s))
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.api.uninitialize(self.handle);
        }
    }

    fn info(&self) -> ChannelInfo {
        self.info.clone()
    }
}

impl Drop for PcanChannelImpl {
    fn drop(&mut self) {
        self.close();
    }
}
