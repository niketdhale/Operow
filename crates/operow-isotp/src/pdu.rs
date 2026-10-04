use operow_core::{CanFrame, is_valid_fd_len};

use crate::config::IsoTpConfig;

/// ISO-TP protocol control information type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciType {
    Single,
    First,
    Consecutive,
    FlowControl,
}

/// A parsed PDU borrowing its data from the frame.
pub(crate) enum Pdu<'a> {
    Single(&'a [u8]),
    First { len: u32, data: &'a [u8] },
    Consecutive { sn: u8, data: &'a [u8] },
    FlowControl { fs: u8, bs: u8, st: u8 },
}

/// Parse the bytes after the address byte. `fd_frame_len` is true when the
/// whole CAN frame is longer than 8 bytes (escape SF format applies).
pub(crate) fn parse_pdu(p: &[u8], fd_frame_len: bool) -> Option<Pdu<'_>> {
    let b0 = *p.first()?;
    match b0 >> 4 {
        0 => {
            let n = (b0 & 0xF) as usize;
            if n == 0 && fd_frame_len {
                let len = *p.get(1)? as usize;
                Some(Pdu::Single(p.get(2..2 + len)?))
            } else {
                Some(Pdu::Single(p.get(1..1 + n)?))
            }
        }
        1 => {
            let len = (((b0 & 0xF) as u32) << 8) | *p.get(1)? as u32;
            if len == 0 {
                let b = p.get(2..6)?;
                let len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                Some(Pdu::First { len, data: &p[6..] })
            } else {
                Some(Pdu::First { len, data: &p[2..] })
            }
        }
        2 => Some(Pdu::Consecutive {
            sn: b0 & 0xF,
            data: &p[1..],
        }),
        3 => Some(Pdu::FlowControl {
            fs: b0 & 0xF,
            bs: *p.get(1)?,
            st: *p.get(2)?,
        }),
        _ => None,
    }
}

fn pdu_of<'a>(frame: &'a CanFrame, cfg: &IsoTpConfig) -> Option<Pdu<'a>> {
    if frame.extended != cfg.extended_ids || (frame.id != cfg.tx_id && frame.id != cfg.rx_id) {
        return None;
    }
    let p = frame.payload().get(cfg.addr_offset()..)?;
    parse_pdu(p, frame.dlc > 8)
}

/// Classify `frame` as an ISO-TP PDU of the channel described by `cfg`
/// (either direction: its tx id or rx id). Returns `None` for other ids or
/// malformed PCI. The address byte value is not checked.
pub fn frame_pci_type(frame: &CanFrame, cfg: &IsoTpConfig) -> Option<PciType> {
    Some(match pdu_of(frame, cfg)? {
        Pdu::Single(_) => PciType::Single,
        Pdu::First { .. } => PciType::First,
        Pdu::Consecutive { .. } => PciType::Consecutive,
        Pdu::FlowControl { .. } => PciType::FlowControl,
    })
}

/// Human-readable PCI summary, e.g. `"SF len=3"`, `"FF len=20"`, `"CF sn=1"`,
/// `"FC CTS bs=0 st=0"` (`st` is the raw STmin byte).
pub fn describe_pci(frame: &CanFrame, cfg: &IsoTpConfig) -> String {
    match pdu_of(frame, cfg) {
        Some(Pdu::Single(d)) => format!("SF len={}", d.len()),
        Some(Pdu::First { len, .. }) => format!("FF len={len}"),
        Some(Pdu::Consecutive { sn, .. }) => format!("CF sn={sn}"),
        Some(Pdu::FlowControl { fs, bs, st }) => {
            let fs = match fs {
                0 => "CTS".to_string(),
                1 => "WAIT".to_string(),
                2 => "OVFLW".to_string(),
                n => format!("FS{n}"),
            };
            format!("FC {fs} bs={bs} st={st}")
        }
        None => "not ISO-TP".to_string(),
    }
}

/// Smallest valid FD frame length that is at least `n`.
pub(crate) fn round_up_fd(n: usize) -> usize {
    (n..=64).find(|&l| is_valid_fd_len(l)).unwrap_or(64)
}
