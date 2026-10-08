//! Groups the ISO-TP frames of diagnostic channels into UDS messages for
//! the trace's "Group ISO-TP" view.
//!
//! [`Grouper`] is fed bus events in order and keeps per-(bus, id)
//! reassembly state; [`group_isotp`] is the same thing as a pure function
//! over a slice. Frames that do not belong to a diagnostic channel are
//! passed through as [`Item::Frame`].

use std::collections::{HashMap, VecDeque};

use operow_core::{BusEvent, BusId, CanFrame, NodeId, Timestamp};
use operow_isotp::{IsoTpConfig, PciType, frame_pci_type};

/// The CAN ids of one diagnostic server on one bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagChannel {
    pub bus: BusId,
    pub extended: bool,
    /// Physical request id (tester to ECU).
    pub req_id: u32,
    /// Response id (ECU to tester).
    pub resp_id: u32,
    pub functional_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Request,
    Response,
}

impl MsgKind {
    pub fn label(self) -> &'static str {
        match self {
            MsgKind::Request => "UDS request",
            MsgKind::Response => "UDS response",
        }
    }
}

/// One ISO-TP message, complete or still missing consecutive frames.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Arrival sequence number of the first frame.
    pub seq: u64,
    pub time: Timestamp,
    pub bus: BusId,
    pub sender: NodeId,
    pub id: u32,
    pub extended: bool,
    pub kind: MsgKind,
    /// The payload received so far.
    pub payload: Vec<u8>,
    /// Length announced by the first frame.
    pub expected_len: usize,
    pub complete: bool,
    /// Sequence numbers of every frame of the group, flow control included.
    pub frames: Vec<u64>,
}

/// A display row of the grouped trace.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A frame outside any diagnostic exchange (the store sequence number).
    Frame(u64),
    Message(Message),
}

impl Item {
    /// Sequence number of the first frame of the item.
    pub fn seq(&self) -> u64 {
        match self {
            Item::Frame(s) => *s,
            Item::Message(m) => m.seq,
        }
    }
}

/// Reassembly state of a message that is still receiving frames.
struct Partial {
    /// `Item::seq` of its message.
    seq: u64,
    next_sn: u8,
}

type Key = (BusId, u32, bool);

/// Incremental ISO-TP grouping. Feed events in arrival order.
#[derive(Default)]
pub struct Grouper {
    channels: Vec<DiagChannel>,
    items: VecDeque<Item>,
    partial: HashMap<Key, Partial>,
}

impl Grouper {
    pub fn new(channels: Vec<DiagChannel>) -> Self {
        Grouper {
            channels,
            ..Default::default()
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn get(&self, i: usize) -> Option<&Item> {
        self.items.get(i)
    }

    /// Index of the item starting at `seq`.
    pub fn position(&self, seq: u64) -> Option<usize> {
        self.items.binary_search_by_key(&seq, Item::seq).ok()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    /// Forget every item and reassembly state (the channels are kept).
    pub fn clear(&mut self) {
        self.items.clear();
        self.partial.clear();
    }

    /// Drop items that start before `seq`.
    pub fn evict_before(&mut self, seq: u64) {
        while self.items.front().is_some_and(|i| i.seq() < seq) {
            self.items.pop_front();
        }
        self.partial.retain(|_, p| p.seq >= seq);
    }

    fn item_mut(&mut self, seq: u64) -> Option<&mut Message> {
        let i = self.items.binary_search_by_key(&seq, Item::seq).ok()?;
        match &mut self.items[i] {
            Item::Message(m) => Some(m),
            Item::Frame(_) => None,
        }
    }

    /// The channel and direction of a frame, if it is a diagnostic one.
    fn classify(&self, ev: &BusEvent) -> Option<(u32, u32, MsgKind)> {
        if ev.hop != 0 || ev.is_error() {
            return None;
        }
        let f = ev.frame.as_can()?;
        self.channels.iter().find_map(|c| {
            if c.bus != ev.bus || c.extended != f.extended {
                return None;
            }
            if f.id == c.req_id || Some(f.id) == c.functional_id {
                Some((c.req_id, c.resp_id, MsgKind::Request))
            } else if f.id == c.resp_id {
                Some((c.req_id, c.resp_id, MsgKind::Response))
            } else {
                None
            }
        })
    }

    /// Add the next event.
    pub fn feed(&mut self, seq: u64, ev: &BusEvent) {
        let (Some((req_id, resp_id, kind)), Some(f)) = (self.classify(ev), ev.frame.as_can())
        else {
            self.items.push_back(Item::Frame(seq));
            return;
        };
        let key: Key = (ev.bus, f.id, f.extended);
        let other = match kind {
            MsgKind::Request => (ev.bus, resp_id, f.extended),
            MsgKind::Response => (ev.bus, req_id, f.extended),
        };
        let cfg = IsoTpConfig {
            tx_id: f.id,
            extended_ids: f.extended,
            ..IsoTpConfig::default()
        };
        let pci = frame_pci_type(f, &cfg);
        let p = f.payload();
        match pci {
            Some(PciType::Single) => {
                let data = single_data(p, f);
                // A new message ends any unfinished one on this id.
                self.partial.remove(&key);
                match data {
                    Some(d) => self.items.push_back(Item::Message(Message {
                        seq,
                        time: ev.time,
                        bus: ev.bus,
                        sender: ev.sender,
                        id: f.id,
                        extended: f.extended,
                        kind,
                        expected_len: d.len(),
                        payload: d.to_vec(),
                        complete: true,
                        frames: vec![seq],
                    })),
                    None => self.items.push_back(Item::Frame(seq)),
                }
            }
            Some(PciType::First) => {
                self.partial.remove(&key);
                match first_data(p) {
                    Some((len, d)) => {
                        let d = &d[..d.len().min(len)];
                        self.items.push_back(Item::Message(Message {
                            seq,
                            time: ev.time,
                            bus: ev.bus,
                            sender: ev.sender,
                            id: f.id,
                            extended: f.extended,
                            kind,
                            expected_len: len,
                            payload: d.to_vec(),
                            complete: d.len() >= len,
                            frames: vec![seq],
                        }));
                        if d.len() < len {
                            self.partial.insert(key, Partial { seq, next_sn: 1 });
                        }
                    }
                    None => self.items.push_back(Item::Frame(seq)),
                }
            }
            Some(PciType::Consecutive) => {
                let Some(part) = self.partial.get_mut(&key) else {
                    self.items.push_back(Item::Frame(seq));
                    return;
                };
                let (mseq, expect_sn) = (part.seq, part.next_sn);
                let sn = p.first().map_or(0xFF, |b| b & 0xF);
                if sn != expect_sn {
                    // Sequence error: the message stays incomplete.
                    self.partial.remove(&key);
                    match self.item_mut(mseq) {
                        Some(m) => m.frames.push(seq),
                        None => self.items.push_back(Item::Frame(seq)),
                    }
                    return;
                }
                part.next_sn = (expect_sn + 1) & 0xF;
                let Some(m) = self.item_mut(mseq) else {
                    self.partial.remove(&key);
                    self.items.push_back(Item::Frame(seq));
                    return;
                };
                let room = m.expected_len.saturating_sub(m.payload.len());
                let data = p.get(1..).unwrap_or_default();
                m.payload.extend_from_slice(&data[..data.len().min(room)]);
                m.frames.push(seq);
                if m.payload.len() >= m.expected_len {
                    m.complete = true;
                    self.partial.remove(&key);
                }
            }
            Some(PciType::FlowControl) => {
                // Flow control of the message the other side is sending.
                let target = self.partial.get(&other).map(|p| p.seq);
                match target.and_then(|s| self.item_mut(s)) {
                    Some(m) => m.frames.push(seq),
                    None => self.items.push_back(Item::Frame(seq)),
                }
            }
            None => self.items.push_back(Item::Frame(seq)),
        }
    }
}

/// Data of a single frame (classic or FD escape format).
fn single_data<'a>(p: &'a [u8], f: &CanFrame) -> Option<&'a [u8]> {
    let n = (*p.first()? & 0xF) as usize;
    if n == 0 && f.dlc > 8 {
        let len = *p.get(1)? as usize;
        p.get(2..2 + len)
    } else {
        p.get(1..1 + n)
    }
}

/// Announced length and first data bytes of a first frame.
fn first_data(p: &[u8]) -> Option<(usize, &[u8])> {
    let b0 = *p.first()?;
    let len = (((b0 & 0xF) as usize) << 8) | *p.get(1)? as usize;
    if len == 0 {
        let b = p.get(2..6)?;
        let len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        Some((len, p.get(6..)?))
    } else {
        Some((len, p.get(2..)?))
    }
}

/// Group `events` (store sequence number and event, in arrival order).
#[cfg(test)]
pub fn group_isotp(events: &[(u64, BusEvent)], channels: &[DiagChannel]) -> Vec<Item> {
    let mut g = Grouper::new(channels.to_vec());
    for (seq, ev) in events {
        g.feed(*seq, ev);
    }
    g.items.into_iter().collect()
}

/// Truncated hex of a payload: at most 32 bytes, then an ellipsis.
pub fn payload_hex(payload: &[u8]) -> String {
    const MAX: usize = 32;
    let mut s = payload
        .iter()
        .take(MAX)
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    if payload.len() > MAX {
        s.push_str(" \u{2026}");
    }
    s
}

/// The Info column of a grouped row.
pub fn message_info(m: &Message) -> String {
    if m.complete {
        operow_uds::describe(&m.payload, m.kind == MsgKind::Request)
    } else {
        format!(
            "incomplete: {} of {} bytes",
            m.payload.len(),
            m.expected_len
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::Direction;

    const BUS: BusId = BusId(1);

    fn chan(req: u32, resp: u32) -> DiagChannel {
        DiagChannel {
            bus: BUS,
            extended: false,
            req_id: req,
            resp_id: resp,
            functional_id: Some(0x7DF),
        }
    }

    fn ev(t_ms: u64, id: u32, data: &[u8]) -> BusEvent {
        BusEvent {
            time: Timestamp(t_ms * 1_000_000),
            bus: BUS,
            sender: NodeId(1),
            origin: NodeId(1),
            dir: Direction::Tx,
            frame_uid: 0,
            hop: 0,
            frame: CanFrame::new(id, false, data).unwrap().into(),
            kind: Default::default(),
        }
    }

    fn numbered(evs: Vec<BusEvent>) -> Vec<(u64, BusEvent)> {
        evs.into_iter()
            .enumerate()
            .map(|(i, e)| (i as u64, e))
            .collect()
    }

    fn msg(i: &Item) -> &Message {
        match i {
            Item::Message(m) => m,
            Item::Frame(_) => panic!("expected a message, got {i:?}"),
        }
    }

    #[test]
    fn single_frames_become_requests_and_responses() {
        let evs = numbered(vec![
            ev(0, 0x7E0, &[3, 0x22, 0xF1, 0x90]),
            ev(5, 0x7E8, &[4, 0x62, 0xF1, 0x90, 0x41]),
            ev(6, 0x100, &[1, 2, 3]),
        ]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8)]);
        assert_eq!(items.len(), 3);
        let (a, b) = (msg(&items[0]), msg(&items[1]));
        assert_eq!(a.kind, MsgKind::Request);
        assert_eq!(a.payload, [0x22, 0xF1, 0x90]);
        assert!(a.complete);
        assert_eq!(b.kind, MsgKind::Response);
        assert_eq!(b.payload, [0x62, 0xF1, 0x90, 0x41]);
        assert_eq!(items[2], Item::Frame(2), "other ids stay ungrouped");
        assert_eq!(message_info(a), "ReadDataByIdentifier F190");
    }

    #[test]
    fn functional_requests_are_requests() {
        let evs = numbered(vec![ev(0, 0x7DF, &[2, 0x3E, 0x80])]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8)]);
        assert_eq!(msg(&items[0]).kind, MsgKind::Request);
        assert_eq!(msg(&items[0]).id, 0x7DF);
    }

    #[test]
    fn multi_frame_with_flow_control_is_one_row() {
        // 10-byte response: FF carries 6 bytes, one CF carries the rest.
        let evs = numbered(vec![
            ev(0, 0x7E8, &[0x10, 10, 0x62, 0xF1, 0x90, 1, 2, 3]),
            ev(1, 0x7E0, &[0x30, 0, 0]),
            ev(2, 0x7E8, &[0x21, 4, 5, 6, 7, 0xAA, 0xAA, 0xAA]),
        ]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8)]);
        assert_eq!(items.len(), 1, "FC hidden in the group: {items:?}");
        let m = msg(&items[0]);
        assert!(m.complete);
        assert_eq!(m.payload, [0x62, 0xF1, 0x90, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(m.frames, [0, 1, 2]);
        assert_eq!(m.expected_len, 10);
        assert_eq!(m.time, Timestamp(0));
    }

    #[test]
    fn missing_consecutive_frame_leaves_an_incomplete_row() {
        // 20 bytes announced; CF 1 arrives, CF 2 is lost, CF 3 is out of order.
        let evs = numbered(vec![
            ev(0, 0x7E8, &[0x10, 20, 1, 2, 3, 4, 5, 6]),
            ev(1, 0x7E0, &[0x30, 0, 0]),
            ev(2, 0x7E8, &[0x21, 7, 8, 9, 10, 11, 12, 13]),
            ev(3, 0x7E8, &[0x23, 21, 22, 23, 24, 25, 26, 27]),
            ev(4, 0x7E0, &[2, 0x3E, 0]),
        ]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8)]);
        assert_eq!(items.len(), 2);
        let m = msg(&items[0]);
        assert!(!m.complete);
        assert_eq!(m.payload.len(), 13);
        assert_eq!(m.expected_len, 20);
        assert_eq!(message_info(m), "incomplete: 13 of 20 bytes");
        assert_eq!(msg(&items[1]).payload, [0x3E, 0]);
        // A first frame never followed by anything is incomplete too.
        let evs = numbered(vec![ev(0, 0x7E8, &[0x10, 20, 1, 2, 3, 4, 5, 6])]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8)]);
        assert!(!msg(&items[0]).complete);
    }

    #[test]
    fn concurrent_channels_do_not_mix() {
        let evs = numbered(vec![
            ev(0, 0x7E8, &[0x10, 9, 0x62, 0xF1, 0x90, 1, 2, 3]),
            ev(1, 0x7E9, &[0x10, 9, 0x62, 0xF1, 0x91, 9, 8, 7]),
            ev(2, 0x7E0, &[0x30, 0, 0]),
            ev(3, 0x7E1, &[0x30, 0, 0]),
            ev(4, 0x7E9, &[0x21, 6, 5, 0, 0, 0, 0, 0]),
            ev(5, 0x7E8, &[0x21, 4, 5, 6, 0, 0, 0, 0]),
        ]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8), chan(0x7E1, 0x7E9)]);
        assert_eq!(items.len(), 2);
        let (a, b) = (msg(&items[0]), msg(&items[1]));
        assert_eq!(a.id, 0x7E8);
        assert_eq!(a.payload, [0x62, 0xF1, 0x90, 1, 2, 3, 4, 5, 6]);
        assert_eq!(a.frames, [0, 2, 5]);
        assert_eq!(b.id, 0x7E9);
        assert_eq!(b.payload, [0x62, 0xF1, 0x91, 9, 8, 7, 6, 5, 0]);
        assert_eq!(b.frames, [1, 3, 4]);
        assert!(a.complete && b.complete);
    }

    #[test]
    fn orphans_and_other_buses_stay_ungrouped() {
        let mut other_bus = ev(2, 0x7E0, &[2, 0x3E, 0]);
        other_bus.bus = BusId(2);
        let evs = numbered(vec![
            ev(0, 0x7E8, &[0x21, 1, 2, 3]),
            ev(1, 0x7E0, &[0x30, 0, 0]),
            other_bus,
        ]);
        let items = group_isotp(&evs, &[chan(0x7E0, 0x7E8)]);
        assert_eq!(
            items,
            [Item::Frame(0), Item::Frame(1), Item::Frame(2)],
            "orphan CF, FC without a message and a foreign bus"
        );
    }

    #[test]
    fn incremental_feed_matches_the_pure_function_and_evicts() {
        let evs = numbered(vec![
            ev(0, 0x7E8, &[0x10, 9, 0x62, 0xF1, 0x90, 1, 2, 3]),
            ev(1, 0x7E0, &[0x30, 0, 0]),
            ev(2, 0x7E8, &[0x21, 4, 5, 6, 0, 0, 0, 0]),
            ev(3, 0x7E0, &[2, 0x3E, 0]),
        ]);
        let chans = [chan(0x7E0, 0x7E8)];
        let mut g = Grouper::new(chans.to_vec());
        for (s, e) in &evs {
            g.feed(*s, e);
        }
        assert_eq!(
            g.iter().cloned().collect::<Vec<_>>(),
            group_isotp(&evs, &chans)
        );
        g.evict_before(3);
        assert_eq!(g.len(), 1);
        assert_eq!(g.get(0).unwrap().seq(), 3);
    }

    #[test]
    fn long_payloads_are_truncated_in_the_cell() {
        let long: Vec<u8> = (0..40).collect();
        let s = payload_hex(&long);
        assert!(s.ends_with('\u{2026}'));
        assert_eq!(s.split(' ').count(), 33);
        assert_eq!(payload_hex(&[1, 0xAB]), "01 AB");
    }
}
