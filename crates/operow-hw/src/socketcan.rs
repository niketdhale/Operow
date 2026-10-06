//! Linux SocketCAN driver (`socketcan:<interface>`) on raw libc sockets.
//!
//! Notes:
//! - The bitrate cannot be set from user space without netlink privileges;
//!   configure the interface yourself, e.g.
//!   `sudo ip link set can0 type can bitrate 500000 dbitrate 2000000 fd on`
//!   and `sudo ip link set can0 up`. `ChannelConfig::bitrate` is ignored.
//! - Listen-only is a controller mode (`ip link set can0 type can
//!   listen-only on`); it cannot be selected through the socket. A
//!   `listen_only` channel therefore only refuses to transmit in software.
//! - Timestamps are the process-wide monotonic clock at reception.
//! - The controller counters come from the error frames the kernel reports
//!   (state changes and bus errors); without errors the state reads as
//!   error active with zero counters.

use std::ffi::CString;
use std::time::Duration;

use operow_core::{CanErrorKind, CanFrame, NodeErrorState};

use crate::{
    CanChannel, ChannelConfig, ChannelInfo, Driver, HwBusState, HwError, RxFrame, monotonic_ns,
    split_interface,
};

const DRIVER: &str = "socketcan";

const CAN_EFF_FLAG: u32 = 0x8000_0000;
const CAN_RTR_FLAG: u32 = 0x4000_0000;
const CAN_ERR_FLAG: u32 = 0x2000_0000;
const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;
const CAN_MTU: usize = 16;
const CANFD_MTU: usize = 72;
const CANFD_BRS: u8 = 0x01;
/// `ARPHRD_CAN` in /sys/class/net/*/type.
const ARPHRD_CAN: u32 = 280;

// Error frame classes (linux/can/error.h).
const ERR_CRTL: u32 = 0x04;
const ERR_PROT: u32 = 0x08;
const ERR_ACK: u32 = 0x20;
const ERR_BUSOFF: u32 = 0x40;
const ERR_BUSERROR: u32 = 0x80;
// Controller status in data[1].
const CRTL_RX_PASSIVE: u8 = 0x10;
const CRTL_TX_PASSIVE: u8 = 0x20;
// Protocol violation bits in data[2] and locations in data[3].
const PROT_BIT: u8 = 0x01;
const PROT_FORM: u8 = 0x02;
const PROT_STUFF: u8 = 0x04;
const PROT_TX: u8 = 0x08 | 0x10;
const LOC_CRC_SEQ: u8 = 0x08;
const LOC_CRC_DEL: u8 = 0x18;

pub struct SocketCanDriver;

fn sys_class_net() -> std::path::PathBuf {
    std::path::PathBuf::from("/sys/class/net")
}

impl Driver for SocketCanDriver {
    fn name(&self) -> &str {
        DRIVER
    }

    fn available(&self) -> Result<(), String> {
        // SAFETY: plain socket(2) probe, closed right away.
        let fd = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, libc::CAN_RAW) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            return Err(format!("SocketCAN is not supported by this kernel ({e})"));
        }
        // SAFETY: fd was just opened by us.
        unsafe { libc::close(fd) };
        Ok(())
    }

    fn list_channels(&self) -> Vec<ChannelInfo> {
        let Ok(dir) = std::fs::read_dir(sys_class_net()) else {
            return Vec::new();
        };
        let mut out: Vec<ChannelInfo> = dir
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let ty: u32 = std::fs::read_to_string(path.join("type"))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?;
                if ty != ARPHRD_CAN {
                    return None;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let mtu: usize = std::fs::read_to_string(path.join("mtu"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                Some(ChannelInfo {
                    driver: DRIVER.into(),
                    name: format!("{DRIVER}:{name}"),
                    description: format!("SocketCAN interface {name}"),
                    fd_capable: mtu >= CANFD_MTU,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn open(&self, cfg: &ChannelConfig) -> Result<Box<dyn CanChannel>, HwError> {
        let (drv, ifname) = split_interface(&cfg.interface)?;
        if drv != DRIVER {
            return Err(HwError::UnknownDriver(drv.to_string()));
        }
        let open_err = |msg: String| HwError::Open {
            interface: cfg.interface.clone(),
            msg,
        };
        let cname = CString::new(ifname).map_err(|_| open_err("invalid interface name".into()))?;
        if ifname.len() >= libc::IFNAMSIZ {
            return Err(open_err("interface name too long".into()));
        }
        // SAFETY: if_nametoindex reads a NUL-terminated string.
        let index = unsafe { libc::if_nametoindex(cname.as_ptr()) };
        if index == 0 {
            return Err(open_err(format!(
                "no such interface ({})",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: plain socket(2) call.
        let fd = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, libc::CAN_RAW) };
        if fd < 0 {
            return Err(open_err(std::io::Error::last_os_error().to_string()));
        }
        // From here on `ch` closes the fd on any early return.
        let ch = SocketCanChannel {
            fd,
            name: ifname.to_string(),
            fd_mode: cfg.fd,
            listen_only: cfg.listen_only,
            state: HwBusState::default(),
        };
        let one: libc::c_int = 1;
        if cfg.fd {
            ch.setsockopt(libc::CAN_RAW_FD_FRAMES, one).map_err(|e| {
                open_err(format!("CAN FD is not supported on this interface ({e})"))
            })?;
        }
        ch.setsockopt(
            libc::CAN_RAW_RECV_OWN_MSGS,
            libc::c_int::from(cfg.receive_own),
        )
        .map_err(|e| open_err(e.to_string()))?;
        let err_mask: libc::can_err_mask_t = 0x1FFF_FFFF;
        ch.setsockopt(libc::CAN_RAW_ERR_FILTER, err_mask)
            .map_err(|e| open_err(e.to_string()))?;

        // SAFETY: sockaddr_can is plain data; zeroed is a valid value.
        let mut addr: libc::sockaddr_can = unsafe { std::mem::zeroed() };
        addr.can_family = libc::AF_CAN as libc::sa_family_t;
        addr.can_ifindex = index as libc::c_int;
        // SAFETY: addr outlives the call and the length matches its type.
        let rc = unsafe {
            libc::bind(
                fd,
                (&addr as *const libc::sockaddr_can).cast(),
                std::mem::size_of::<libc::sockaddr_can>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(open_err(format!(
                "bind failed ({}); is the interface up?",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Box::new(ch))
    }
}

struct SocketCanChannel {
    /// -1 once closed.
    fd: libc::c_int,
    name: String,
    fd_mode: bool,
    listen_only: bool,
    state: HwBusState,
}

impl SocketCanChannel {
    fn setsockopt<T>(&self, opt: libc::c_int, val: T) -> std::io::Result<()> {
        // SAFETY: `val` is a live value of size_of::<T>() bytes.
        let rc = unsafe {
            libc::setsockopt(
                self.fd,
                libc::SOL_CAN_RAW,
                opt,
                (&val as *const T).cast(),
                std::mem::size_of::<T>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Update the tracked controller state from an error frame.
    fn track_error(&mut self, class: u32, data: &[u8]) {
        if class & ERR_BUSOFF != 0 {
            self.state.state = NodeErrorState::BusOff;
        } else if class & ERR_CRTL != 0 {
            let status = data.get(1).copied().unwrap_or(0);
            self.state.state = if status & (CRTL_RX_PASSIVE | CRTL_TX_PASSIVE) != 0 {
                NodeErrorState::ErrorPassive
            } else {
                NodeErrorState::ErrorActive
            };
        }
        if class & (ERR_CRTL | ERR_BUSERROR) != 0 {
            self.state.tec = u16::from(data.get(6).copied().unwrap_or(0));
            self.state.rec = u16::from(data.get(7).copied().unwrap_or(0));
        }
    }
}

/// Which CAN error an error frame reports; `None` for frames that are not
/// bus errors (arbitration lost, restarted, TX timeout, ...).
fn classify_error(can_id: u32, data: &[u8]) -> Option<CanErrorKind> {
    let class = can_id & !CAN_ERR_FLAG;
    let d2 = data.get(2).copied().unwrap_or(0);
    let d3 = data.get(3).copied().unwrap_or(0);
    if class & ERR_ACK != 0 {
        Some(CanErrorKind::Ack)
    } else if class & ERR_PROT != 0 || class & ERR_BUSERROR != 0 {
        if d3 == LOC_CRC_SEQ || d3 == LOC_CRC_DEL {
            Some(CanErrorKind::Crc)
        } else if d2 & PROT_STUFF != 0 {
            Some(CanErrorKind::Stuff)
        } else if d2 & PROT_FORM != 0 {
            Some(CanErrorKind::Form)
        } else if d2 & (PROT_BIT | PROT_TX) != 0 || class & ERR_BUSERROR != 0 {
            Some(CanErrorKind::Bit)
        } else {
            None
        }
    } else {
        None
    }
}

/// Decode a received SocketCAN frame of `len` bytes.
fn decode(buf: &[u8; CANFD_MTU], len: usize) -> Option<Decoded> {
    if len != CAN_MTU && len != CANFD_MTU {
        return None;
    }
    let can_id = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
    let plen = buf[4] as usize;
    if can_id & CAN_ERR_FLAG != 0 {
        let data = &buf[8..16];
        return Some(Decoded::Error {
            class: can_id & !CAN_ERR_FLAG,
            kind: classify_error(can_id, data),
            data: data.try_into().unwrap(),
        });
    }
    if can_id & CAN_RTR_FLAG != 0 {
        return Some(Decoded::Ignored); // remote frames are not modelled
    }
    let extended = can_id & CAN_EFF_FLAG != 0;
    let id = can_id & if extended { CAN_EFF_MASK } else { 0x7FF };
    let fd = len == CANFD_MTU;
    let data = &buf[8..8 + plen.min(if fd { 64 } else { 8 })];
    let frame = if fd {
        CanFrame::new_fd(id, extended, buf[5] & CANFD_BRS != 0, data).ok()?
    } else {
        CanFrame::new(id, extended, data).ok()?
    };
    Some(Decoded::Frame(frame))
}

enum Decoded {
    Frame(CanFrame),
    Error {
        class: u32,
        kind: Option<CanErrorKind>,
        data: [u8; 8],
    },
    Ignored,
}

fn encode(frame: &CanFrame) -> ([u8; CANFD_MTU], usize) {
    let mut buf = [0u8; CANFD_MTU];
    let mut can_id = frame.id;
    if frame.extended {
        can_id = (can_id & CAN_EFF_MASK) | CAN_EFF_FLAG;
    }
    buf[0..4].copy_from_slice(&can_id.to_ne_bytes());
    buf[4] = frame.dlc;
    if frame.fd {
        buf[5] = if frame.brs { CANFD_BRS } else { 0 };
    }
    buf[8..8 + frame.dlc as usize].copy_from_slice(frame.payload());
    (buf, if frame.fd { CANFD_MTU } else { CAN_MTU })
}

impl CanChannel for SocketCanChannel {
    fn send(&mut self, frame: &CanFrame) -> Result<(), HwError> {
        if self.fd < 0 {
            return Err(HwError::Closed);
        }
        if self.listen_only {
            return Err(HwError::ListenOnly);
        }
        if frame.fd && !self.fd_mode {
            return Err(HwError::Unsupported(
                "CAN FD frame on a channel opened without fd".into(),
            ));
        }
        let (buf, len) = encode(frame);
        // SAFETY: buf is valid for `len` (<= 72) bytes.
        let n = unsafe {
            libc::send(
                self.fd,
                buf.as_ptr().cast(),
                len,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n == len as isize {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::ENOBUFS) => Err(HwError::TxQueueFull),
            _ => Err(HwError::Io(e.to_string())),
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<RxFrame>, HwError> {
        if self.fd < 0 {
            return Err(HwError::Closed);
        }
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            return if e.kind() == std::io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(HwError::Io(e.to_string()))
            };
        }
        if rc == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; CANFD_MTU];
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        // SAFETY: msghdr is plain data; zeroed is valid.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        // SAFETY: msg points at a live iovec over `buf`.
        let n = unsafe { libc::recvmsg(self.fd, &mut msg, libc::MSG_DONTWAIT) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            return match e.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => Ok(None),
                _ => Err(HwError::Io(e.to_string())),
            };
        }
        let is_echo = msg.msg_flags & libc::MSG_CONFIRM != 0;
        let ts = monotonic_ns();
        Ok(match decode(&buf, n as usize) {
            Some(Decoded::Frame(frame)) => Some(RxFrame {
                frame,
                timestamp_ns: ts,
                is_echo,
                error: None,
            }),
            Some(Decoded::Error { class, kind, data }) => {
                self.track_error(class, &data);
                kind.map(|k| RxFrame {
                    frame: CanFrame::new(0, false, &[]).expect("empty frame"),
                    timestamp_ns: ts,
                    is_echo: false,
                    error: Some(k),
                })
            }
            Some(Decoded::Ignored) | None => None,
        })
    }

    fn bus_state(&mut self) -> Result<HwBusState, HwError> {
        if self.fd < 0 {
            return Err(HwError::Closed);
        }
        Ok(self.state)
    }

    fn close(&mut self) {
        if self.fd >= 0 {
            // SAFETY: fd is open and owned by this channel.
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            driver: DRIVER.into(),
            name: format!("{DRIVER}:{}", self.name),
            description: format!("SocketCAN interface {}", self.name),
            fd_capable: self.fd_mode,
        }
    }
}

impl Drop for SocketCanChannel {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let c = CanFrame::new(0x123, false, &[1, 2, 3]).unwrap();
        let (buf, len) = encode(&c);
        assert_eq!(len, CAN_MTU);
        match decode(&buf, len) {
            Some(Decoded::Frame(f)) => assert_eq!(f, c),
            _ => panic!("expected frame"),
        }
        let fd = CanFrame::new_fd(0x1ABC_DEF0, true, true, &[7; 12]).unwrap();
        let (buf, len) = encode(&fd);
        assert_eq!(len, CANFD_MTU);
        match decode(&buf, len) {
            Some(Decoded::Frame(f)) => assert_eq!(f, fd),
            _ => panic!("expected frame"),
        }
    }

    #[test]
    fn error_classification() {
        let mut d = [0u8; 8];
        assert_eq!(
            classify_error(CAN_ERR_FLAG | ERR_ACK, &d),
            Some(CanErrorKind::Ack)
        );
        d[2] = PROT_STUFF;
        assert_eq!(
            classify_error(CAN_ERR_FLAG | ERR_PROT, &d),
            Some(CanErrorKind::Stuff)
        );
        d[2] = PROT_FORM;
        assert_eq!(
            classify_error(CAN_ERR_FLAG | ERR_PROT, &d),
            Some(CanErrorKind::Form)
        );
        d[2] = PROT_BIT;
        d[3] = LOC_CRC_SEQ;
        assert_eq!(
            classify_error(CAN_ERR_FLAG | ERR_PROT, &d),
            Some(CanErrorKind::Crc)
        );
        assert_eq!(classify_error(CAN_ERR_FLAG | 0x02, &[0; 8]), None);
    }

    #[test]
    fn missing_interface_fails() {
        let cfg = ChannelConfig::new("socketcan:operow_nonexistent0");
        assert!(matches!(
            SocketCanDriver.open(&cfg),
            Err(HwError::Open { .. })
        ));
    }

    /// Needs a vcan interface:
    /// `sudo modprobe vcan; sudo ip link add dev vcan0 type vcan;
    /// sudo ip link set up vcan0`, then
    /// `cargo test -p operow-hw -- --ignored`.
    #[test]
    #[ignore = "needs vcan0"]
    fn vcan_loopback() {
        let mut a_cfg = ChannelConfig::new("socketcan:vcan0");
        a_cfg.fd = true;
        let mut b_cfg = a_cfg.clone();
        b_cfg.receive_own = true;
        let mut a = SocketCanDriver.open(&a_cfg).unwrap();
        let mut b = SocketCanDriver.open(&b_cfg).unwrap();
        let classic = CanFrame::new(0x321, false, &[9, 8, 7]).unwrap();
        a.send(&classic).unwrap();
        let rx = b.recv(Duration::from_secs(1)).unwrap().expect("frame");
        assert_eq!(rx.frame, classic);
        assert!(!rx.is_echo);
        let fd = CanFrame::new_fd(0x1234567, true, true, &[0xAB; 32]).unwrap();
        a.send(&fd).unwrap();
        let rx = b.recv(Duration::from_secs(1)).unwrap().expect("fd frame");
        assert_eq!(rx.frame, fd);
        // b sends: its own socket sees the echo.
        b.send(&classic).unwrap();
        let echo = b.recv(Duration::from_secs(1)).unwrap().expect("echo");
        assert!(echo.is_echo);
        // a (no receive_own) sees b's frame as a normal one.
        let rx = a.recv(Duration::from_secs(1)).unwrap().expect("frame");
        assert!(!rx.is_echo);
        assert!(a.recv(Duration::from_millis(50)).unwrap().is_none());
        a.close();
        assert_eq!(a.send(&classic), Err(HwError::Closed));
    }

    #[test]
    #[ignore = "needs vcan0"]
    fn vcan_listed() {
        assert!(
            SocketCanDriver
                .list_channels()
                .iter()
                .any(|c| c.name == "socketcan:vcan0")
        );
    }
}
