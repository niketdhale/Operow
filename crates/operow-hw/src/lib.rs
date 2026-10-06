//! CAN hardware abstraction for Operow.
//!
//! A [`Driver`] knows how to list and open channels of one kind of adapter
//! (`socketcan:can0`, `virtual:bench`, later PCAN and Vector XL, whose vendor
//! libraries are C libraries loaded at runtime). An opened channel is a
//! `Box<dyn CanChannel>`. [`drivers`] returns the drivers usable on this
//! operating system and [`open_channel`] opens a channel by its
//! `driver:channel` name.
//!
//! The [`UdpDriver`] (`udp:<bus-name>`) connects Operow processes on the same
//! machine; the [`VirtualDriver`] works everywhere: channels opened with the same name
//! are connected to each other, which is what tests and demos use.

mod error;
mod udp;
mod virtual_driver;

#[cfg(all(target_os = "linux", feature = "socketcan"))]
mod socketcan;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use operow_core::{CanErrorKind, CanFrame, NodeErrorState};

pub use error::HwError;
pub use udp::{
    Datagram, UdpChannel, UdpDriver, decode_datagram, default_endpoint, encode_datagram,
};
pub use virtual_driver::{VirtualDriver, virtual_inject_error};

#[cfg(all(target_os = "linux", feature = "socketcan"))]
pub use socketcan::SocketCanDriver;

/// How to open a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelConfig {
    /// `driver:channel`, e.g. `socketcan:can0` or `virtual:bench`.
    pub interface: String,
    /// Nominal bitrate (bit/s). Drivers that cannot set it from user space
    /// (SocketCAN) ignore it.
    pub bitrate: u32,
    /// Open the channel in CAN FD mode.
    pub fd: bool,
    /// Data-phase bitrate (bit/s) for FD.
    pub data_bitrate: u32,
    /// Never transmit; [`CanChannel::send`] fails with
    /// [`HwError::ListenOnly`].
    pub listen_only: bool,
    /// Also deliver the channel's own transmitted frames, flagged
    /// [`RxFrame::is_echo`].
    pub receive_own: bool,
}

impl ChannelConfig {
    /// A classic 500 kbit/s configuration for `interface`.
    pub fn new(interface: impl Into<String>) -> Self {
        ChannelConfig {
            interface: interface.into(),
            bitrate: 500_000,
            fd: false,
            data_bitrate: 2_000_000,
            listen_only: false,
            receive_own: false,
        }
    }

    /// Splits `interface` at the first `:` into driver and channel name.
    pub fn split(&self) -> Result<(&str, &str), HwError> {
        split_interface(&self.interface)
    }
}

/// Splits `driver:channel`.
pub fn split_interface(interface: &str) -> Result<(&str, &str), HwError> {
    match interface.split_once(':') {
        Some((d, c)) if !d.is_empty() && !c.is_empty() => Ok((d, c)),
        _ => Err(HwError::BadInterface(interface.to_string())),
    }
}

/// Description of a channel a driver can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub driver: String,
    /// Full `driver:channel` name, as accepted by [`ChannelConfig`].
    pub name: String,
    pub description: String,
    pub fd_capable: bool,
}

/// A frame (or error frame) received from hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxFrame {
    /// For error frames, an empty placeholder frame.
    pub frame: CanFrame,
    /// Nanoseconds on the hardware clock or, when the driver has none, on the
    /// process-wide monotonic clock ([`monotonic_ns`]).
    pub timestamp_ns: u64,
    /// The frame is the echo of one this channel transmitted
    /// ([`ChannelConfig::receive_own`]).
    pub is_echo: bool,
    /// Set for an error frame.
    pub error: Option<CanErrorKind>,
}

impl RxFrame {
    /// A received data frame stamped with the monotonic clock.
    pub fn data(frame: CanFrame) -> Self {
        RxFrame {
            frame,
            timestamp_ns: monotonic_ns(),
            is_echo: false,
            error: None,
        }
    }
}

/// Fault-confinement state of the adapter's CAN controller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HwBusState {
    pub state: NodeErrorState,
    /// Transmit / receive error counters when the driver reports them, else 0.
    pub tec: u16,
    pub rec: u16,
}

/// An opened CAN channel. Implementations are owned by one thread at a time
/// (`Send`), so a bus bridge typically moves it into a reader thread.
pub trait CanChannel: Send {
    /// Transmit a frame. Fails with [`HwError::ListenOnly`] on a listen-only
    /// channel.
    fn send(&mut self, frame: &CanFrame) -> Result<(), HwError>;

    /// Wait up to `timeout` for the next frame; `Ok(None)` on timeout.
    fn recv(&mut self, timeout: Duration) -> Result<Option<RxFrame>, HwError>;

    /// The controller's current fault-confinement state.
    fn bus_state(&mut self) -> Result<HwBusState, HwError>;

    /// Release the channel. Idempotent; further calls fail with
    /// [`HwError::Closed`].
    fn close(&mut self);

    fn info(&self) -> ChannelInfo;
}

/// One kind of adapter.
pub trait Driver: Send + Sync {
    /// Driver prefix as used in `driver:channel`, e.g. `socketcan`.
    fn name(&self) -> &str;

    /// `Ok` when the driver can be used on this machine, else why not (e.g.
    /// the vendor library is missing).
    fn available(&self) -> Result<(), String>;

    /// Channels currently present. Empty when the driver is unavailable.
    fn list_channels(&self) -> Vec<ChannelInfo>;

    /// Open the channel named by `cfg.interface` (its driver part is ignored
    /// here; it is the caller's job to pick the right driver).
    fn open(&self, cfg: &ChannelConfig) -> Result<Box<dyn CanChannel>, HwError>;
}

/// Drivers compiled in for this operating system, the virtual driver last.
#[allow(clippy::vec_init_then_push)]
pub fn drivers() -> Vec<Box<dyn Driver>> {
    let mut v: Vec<Box<dyn Driver>> = Vec::new();
    #[cfg(all(target_os = "linux", feature = "socketcan"))]
    v.push(Box::new(SocketCanDriver));
    v.push(Box::new(UdpDriver));
    v.push(Box::new(VirtualDriver));
    v
}

/// The driver called `name`, if compiled in.
pub fn driver(name: &str) -> Option<Box<dyn Driver>> {
    drivers().into_iter().find(|d| d.name() == name)
}

/// Open `cfg.interface` (`driver:channel`) with the matching driver.
pub fn open_channel(cfg: &ChannelConfig) -> Result<Box<dyn CanChannel>, HwError> {
    let (name, _) = cfg.split()?;
    let drv = driver(name).ok_or_else(|| HwError::UnknownDriver(name.to_string()))?;
    drv.available().map_err(HwError::NotAvailable)?;
    drv.open(cfg)
}

/// Every channel of every available driver.
pub fn list_all_channels() -> Vec<ChannelInfo> {
    drivers()
        .iter()
        .filter(|d| d.available().is_ok())
        .flat_map(|d| d.list_channels())
        .collect()
}

/// Nanoseconds since the first call in this process (monotonic).
pub fn monotonic_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests;
