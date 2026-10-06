//! Cross-process virtual bus over UDP: `udp:<bus-name>[@<group>:<port>]`.
//!
//! Two or more Operow processes (app, CLI, ...) on the same machine that open
//! the same `udp:<bus-name>` see each other's frames. No admin rights are
//! needed on Windows or Linux.
//!
//! Transport: IPv4 multicast on the loopback interface (`127.0.0.1`) with
//! TTL 0, so datagrams never leave the machine. Every participant binds the
//! bus's port with `SO_REUSEADDR` (plus `SO_REUSEPORT` on Unix), joins the
//! group, and sends each frame to `group:port`. By default the group is
//! `239.255.<h>.<l>` and the port `47000 + h % 1000`, where `h` is the 32-bit
//! FNV-1a hash of the bus name; `@<group>:<port>` overrides both (the group
//! must be an IPv4 multicast address). A random per-process sender id in the
//! header lets a participant drop its own frames (they are delivered flagged
//! `is_echo` when `receive_own` is set). A unicast `@host:port` peer form for
//! LAN use is not implemented yet and is rejected with `Unsupported`.
//!
//! Datagram format (version 1, little endian):
//!
//! | offset | size | field |
//! |-------:|-----:|-------|
//! | 0  | 4 | magic `"OPCN"` |
//! | 4  | 1 | version (1) |
//! | 5  | 1 | flags: bit0 extended id, bit1 FD, bit2 BRS, bit3 error frame |
//! | 6  | 1 | error kind: 0 none, 1 Bit, 2 Stuff, 3 CRC, 4 Form, 5 ACK |
//! | 7  | 1 | reserved (0) |
//! | 8  | 8 | sender id (random per channel) |
//! | 16 | 8 | sender timestamp, ns (sender's monotonic clock) |
//! | 24 | 4 | CAN id |
//! | 28 | 1 | payload length (0-8, or a valid FD length up to 64) |
//! | 29 | n | payload |
//!
//! Malformed datagrams (bad magic, version, length or id) are ignored.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use operow_core::{CanErrorKind, CanFrame};
use socket2::{Domain, Protocol, Socket, Type};

use crate::{
    CanChannel, ChannelConfig, ChannelInfo, Driver, HwBusState, HwError, RxFrame, monotonic_ns,
    split_interface,
};

const DRIVER: &str = "udp";
const MAGIC: &[u8; 4] = b"OPCN";
const VERSION: u8 = 1;
const HEADER: usize = 29;

const F_EXT: u8 = 1;
const F_FD: u8 = 2;
const F_BRS: u8 = 4;
const F_ERR: u8 = 8;

/// Encode a datagram (see the module docs for the layout).
pub fn encode_datagram(
    sender: u64,
    ts_ns: u64,
    frame: &CanFrame,
    error: Option<CanErrorKind>,
) -> Vec<u8> {
    let mut flags = 0;
    if frame.extended {
        flags |= F_EXT;
    }
    if frame.fd {
        flags |= F_FD;
    }
    if frame.brs {
        flags |= F_BRS;
    }
    if error.is_some() {
        flags |= F_ERR;
    }
    let kind = error.map_or(0, |k| match k {
        CanErrorKind::Bit => 1,
        CanErrorKind::Stuff => 2,
        CanErrorKind::Crc => 3,
        CanErrorKind::Form => 4,
        CanErrorKind::Ack => 5,
    });
    let mut v = Vec::with_capacity(HEADER + frame.dlc as usize);
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&[VERSION, flags, kind, 0]);
    v.extend_from_slice(&sender.to_le_bytes());
    v.extend_from_slice(&ts_ns.to_le_bytes());
    v.extend_from_slice(&frame.id.to_le_bytes());
    v.push(frame.dlc);
    v.extend_from_slice(frame.payload());
    v
}

/// A decoded datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram {
    pub sender: u64,
    pub timestamp_ns: u64,
    pub frame: CanFrame,
    pub error: Option<CanErrorKind>,
}

/// Decode a datagram; `None` when it is not a valid version-1 frame.
pub fn decode_datagram(b: &[u8]) -> Option<Datagram> {
    if b.len() < HEADER || &b[0..4] != MAGIC || b[4] != VERSION {
        return None;
    }
    let flags = b[5];
    let error = match (flags & F_ERR != 0, b[6]) {
        (false, _) => None,
        (true, 1) => Some(CanErrorKind::Bit),
        (true, 2) => Some(CanErrorKind::Stuff),
        (true, 3) => Some(CanErrorKind::Crc),
        (true, 4) => Some(CanErrorKind::Form),
        (true, 5) => Some(CanErrorKind::Ack),
        _ => return None,
    };
    let sender = u64::from_le_bytes(b[8..16].try_into().ok()?);
    let timestamp_ns = u64::from_le_bytes(b[16..24].try_into().ok()?);
    let id = u32::from_le_bytes(b[24..28].try_into().ok()?);
    let len = b[28] as usize;
    if b.len() != HEADER + len {
        return None;
    }
    let data = &b[HEADER..];
    let ext = flags & F_EXT != 0;
    let frame = if flags & F_FD != 0 {
        CanFrame::new_fd(id, ext, flags & F_BRS != 0, data).ok()?
    } else {
        CanFrame::new(id, ext, data).ok()?
    };
    Some(Datagram {
        sender,
        timestamp_ns,
        frame,
        error,
    })
}

fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811C_9DC5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// Default `(group, port)` of a bus name.
pub fn default_endpoint(name: &str) -> (Ipv4Addr, u16) {
    let h = fnv1a(name);
    (
        Ipv4Addr::new(239, 255, (h >> 8) as u8, (h & 0xFF).max(1) as u8),
        47000 + (h % 1000) as u16,
    )
}

/// Splits `name[@group:port]`.
fn parse_target(spec: &str) -> Result<(String, Ipv4Addr, u16), HwError> {
    let Some((name, ep)) = spec.split_once('@') else {
        let (g, p) = default_endpoint(spec);
        return Ok((spec.to_string(), g, p));
    };
    let bad = || HwError::BadInterface(format!("udp:{spec}"));
    let (host, port) = ep.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().map_err(|_| bad())?;
    let group: Ipv4Addr = host.parse().map_err(|_| bad())?;
    if name.is_empty() || port == 0 {
        return Err(bad());
    }
    if !group.is_multicast() {
        return Err(HwError::Unsupported(
            "udp unicast peers (@host:port) are not implemented; use an IPv4 multicast group"
                .into(),
        ));
    }
    Ok((name.to_string(), group, port))
}

fn random_sender_id() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.finish()
}

pub struct UdpDriver;

impl Driver for UdpDriver {
    fn name(&self) -> &str {
        DRIVER
    }

    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    fn list_channels(&self) -> Vec<ChannelInfo> {
        // Buses are created by naming them; list one example as a hint.
        vec![ChannelInfo {
            driver: DRIVER.into(),
            name: "udp:operow0".into(),
            description: "Cross-process virtual bus; use any name: udp:<bus-name>".into(),
            fd_capable: true,
        }]
    }

    fn open(&self, cfg: &ChannelConfig) -> Result<Box<dyn CanChannel>, HwError> {
        Ok(Box::new(UdpChannel::open_with_sender(
            cfg,
            random_sender_id(),
        )?))
    }
}

pub struct UdpChannel {
    sock: Option<UdpSocket>,
    sender: u64,
    target: SocketAddr,
    name: String,
    fd: bool,
    listen_only: bool,
    receive_own: bool,
}

impl UdpChannel {
    /// Open with an explicit sender id (tests use this to simulate two
    /// processes inside one).
    pub fn open_with_sender(cfg: &ChannelConfig, sender: u64) -> Result<Self, HwError> {
        let (drv, spec) = split_interface(&cfg.interface)?;
        if drv != DRIVER {
            return Err(HwError::UnknownDriver(drv.to_string()));
        }
        let (name, group, port) = parse_target(spec)?;
        let open_err = |e: std::io::Error| HwError::Open {
            interface: cfg.interface.clone(),
            msg: e.to_string(),
        };
        let lo = Ipv4Addr::LOCALHOST;
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(open_err)?;
        s.set_reuse_address(true).map_err(open_err)?;
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        s.set_reuse_port(true).map_err(open_err)?;
        s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())
            .map_err(open_err)?;
        let sock: UdpSocket = s.into();
        sock.join_multicast_v4(&group, &lo).map_err(open_err)?;
        let s2 = socket2::SockRef::from(&sock);
        s2.set_multicast_if_v4(&lo).map_err(open_err)?;
        sock.set_multicast_loop_v4(true).map_err(open_err)?;
        sock.set_multicast_ttl_v4(0).map_err(open_err)?;
        Ok(UdpChannel {
            sock: Some(sock),
            sender,
            target: SocketAddrV4::new(group, port).into(),
            name,
            fd: cfg.fd,
            listen_only: cfg.listen_only,
            receive_own: cfg.receive_own,
        })
    }
}

impl CanChannel for UdpChannel {
    fn send(&mut self, frame: &CanFrame) -> Result<(), HwError> {
        let sock = self.sock.as_ref().ok_or(HwError::Closed)?;
        if self.listen_only {
            return Err(HwError::ListenOnly);
        }
        if frame.fd && !self.fd {
            return Err(HwError::Unsupported(
                "CAN FD frame on a classic channel".into(),
            ));
        }
        let d = encode_datagram(self.sender, monotonic_ns(), frame, None);
        sock.send_to(&d, self.target)
            .map(|_| ())
            .map_err(|e| HwError::Io(e.to_string()))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<RxFrame>, HwError> {
        let sock = self.sock.as_ref().ok_or(HwError::Closed)?;
        let deadline = std::time::Instant::now() + timeout;
        let mut buf = [0u8; 128];
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            // A zero read timeout means "block forever" to the OS.
            sock.set_read_timeout(Some(left.max(Duration::from_millis(1))))
                .map_err(|e| HwError::Io(e.to_string()))?;
            match sock.recv_from(&mut buf) {
                Ok((n, _)) => {
                    if let Some(d) = decode_datagram(&buf[..n]) {
                        let own = d.sender == self.sender;
                        if (!own || self.receive_own) && (self.fd || !d.frame.fd) {
                            return Ok(Some(RxFrame {
                                frame: d.frame,
                                timestamp_ns: monotonic_ns(),
                                is_echo: own,
                                error: d.error,
                            }));
                        }
                    }
                    // Own, foreign-FD or malformed: keep waiting.
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                // Windows reports ICMP-triggered resets on UDP sockets.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(e) => return Err(HwError::Io(e.to_string())),
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    fn bus_state(&mut self) -> Result<HwBusState, HwError> {
        if self.sock.is_none() {
            return Err(HwError::Closed);
        }
        Ok(HwBusState::default())
    }

    fn close(&mut self) {
        self.sock = None;
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            driver: DRIVER.into(),
            name: format!("{DRIVER}:{}", self.name),
            description: format!("UDP virtual bus {}", self.name),
            fd_capable: self.fd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(1000);

    fn open(name: &str, sender: u64, fd: bool, own: bool) -> UdpChannel {
        let mut c = ChannelConfig::new(format!("udp:{name}"));
        c.fd = fd;
        c.receive_own = own;
        UdpChannel::open_with_sender(&c, sender).unwrap()
    }

    #[test]
    fn datagram_roundtrip_and_malformed() {
        let f = CanFrame::new_fd(0x1234, true, true, &[1; 12]).unwrap();
        let d = encode_datagram(7, 99, &f, None);
        let back = decode_datagram(&d).unwrap();
        assert_eq!((back.sender, back.timestamp_ns, back.frame), (7, 99, f));
        let e = decode_datagram(&encode_datagram(1, 2, &f, Some(CanErrorKind::Form))).unwrap();
        assert_eq!(e.error, Some(CanErrorKind::Form));
        assert!(decode_datagram(&d[..10]).is_none());
        assert!(decode_datagram(&d[..d.len() - 1]).is_none());
        let mut bad = d.clone();
        bad[0] = b'X';
        assert!(decode_datagram(&bad).is_none());
        let mut ver = d.clone();
        ver[4] = 9;
        assert!(decode_datagram(&ver).is_none());
        let mut std_big = encode_datagram(1, 2, &CanFrame::new(1, false, &[]).unwrap(), None);
        std_big[24..28].copy_from_slice(&0x800u32.to_le_bytes());
        assert!(decode_datagram(&std_big).is_none());
    }

    #[test]
    fn endpoint_parsing() {
        let (n, g, p) = parse_target("bench").unwrap();
        assert_eq!(n, "bench");
        assert!(g.is_multicast() && (47000..48000).contains(&p));
        let (n, g, p) = parse_target("x@239.1.2.3:5555").unwrap();
        assert_eq!((n.as_str(), g, p), ("x", Ipv4Addr::new(239, 1, 2, 3), 5555));
        assert!(matches!(
            parse_target("x@192.168.1.5:5555"),
            Err(HwError::Unsupported(_))
        ));
        assert!(parse_target("x@nonsense").is_err());
    }

    #[test]
    fn two_channels_exchange_frames() {
        let mut a = open("t_udp_pair", 1, true, false);
        let mut b = open("t_udp_pair", 2, true, false);
        let classic = CanFrame::new(0x100, false, &[1, 2, 3]).unwrap();
        a.send(&classic).unwrap();
        assert_eq!(b.recv(T).unwrap().expect("classic").frame, classic);
        let fd = CanFrame::new_fd(0x1ABC, true, true, &[0x5A; 32]).unwrap();
        b.send(&fd).unwrap();
        assert_eq!(a.recv(T).unwrap().expect("fd").frame, fd);
        // Own frames are suppressed.
        assert!(a.recv(Duration::from_millis(50)).unwrap().is_none());
        assert!(b.recv(Duration::from_millis(50)).unwrap().is_none());
    }

    #[test]
    fn receive_own_and_listen_only() {
        let mut a = open("t_udp_own", 1, false, true);
        let f = CanFrame::new(5, false, &[9]).unwrap();
        a.send(&f).unwrap();
        let rx = a.recv(T).unwrap().expect("echo");
        assert!(rx.is_echo);
        let mut c = ChannelConfig::new("udp:t_udp_own");
        c.listen_only = true;
        let mut l = UdpChannel::open_with_sender(&c, 3).unwrap();
        assert_eq!(l.send(&f), Err(HwError::ListenOnly));
        l.close();
        assert_eq!(l.recv(Duration::ZERO), Err(HwError::Closed));
    }

    #[test]
    fn malformed_datagram_ignored() {
        let mut a = open("t_udp_bad", 1, false, false);
        let (g, p) = default_endpoint("t_udp_bad");
        let raw = UdpSocket::bind("127.0.0.1:0").unwrap();
        raw.send_to(b"garbage", (g, p)).ok();
        let f = CanFrame::new(1, false, &[1]).unwrap();
        let mut b = open("t_udp_bad", 2, false, false);
        b.send(&f).unwrap();
        assert_eq!(a.recv(T).unwrap().expect("frame").frame, f);
    }
}
