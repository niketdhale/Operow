//! The shared frame store: one ring buffer of every bus event, read by all
//! windows through their own cursors.

use std::collections::VecDeque;

use operow_core::BusEvent;

/// Ring buffer of [`BusEvent`]s. Each event gets a monotonic arrival
/// sequence number (`seq`); the buffer holds the contiguous range
/// `first_seq()..next_seq()`. Windows remember the last `seq` they saw.
pub struct FrameStore {
    events: VecDeque<BusEvent>,
    capacity: usize,
    /// Seq of `events[0]`.
    first_seq: u64,
    /// When full, ignore new events instead of dropping the oldest.
    reject_when_full: bool,
    /// Bumped by `clear`, so windows can reset derived state.
    epoch: u64,
}

impl FrameStore {
    pub fn new(capacity: usize) -> Self {
        FrameStore {
            events: VecDeque::new(),
            capacity: capacity.max(1),
            first_seq: 0,
            reject_when_full: false,
            epoch: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_full(&self) -> bool {
        self.events.len() >= self.capacity
    }

    /// Number of events ever pushed (and kept): the seq of the next one.
    pub fn total_pushed(&self) -> u64 {
        self.next_seq()
    }

    /// Seq the next pushed event will get.
    pub fn next_seq(&self) -> u64 {
        self.first_seq + self.events.len() as u64
    }

    /// Seq of the oldest event still buffered.
    pub fn first_seq(&self) -> u64 {
        self.first_seq
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Change the capacity, dropping the oldest events if it shrinks.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.trim();
    }

    pub fn set_reject_when_full(&mut self, reject: bool) {
        self.reject_when_full = reject;
    }

    fn trim(&mut self) {
        while self.events.len() > self.capacity {
            self.events.pop_front();
            self.first_seq += 1;
        }
    }

    pub fn push_batch(&mut self, batch: &[BusEvent]) {
        for ev in batch {
            if self.reject_when_full && self.is_full() {
                return;
            }
            self.events.push_back(*ev);
            self.trim();
        }
    }

    pub fn get(&self, seq: u64) -> Option<&BusEvent> {
        let idx = seq.checked_sub(self.first_seq)?;
        self.events.get(usize::try_from(idx).ok()?)
    }

    /// Events with `seq >= from` that are still buffered (older ones were
    /// dropped), oldest first.
    pub fn iter_from(&self, from: u64) -> impl Iterator<Item = (u64, &BusEvent)> {
        let start = from.max(self.first_seq);
        let idx = usize::try_from(start - self.first_seq)
            .unwrap_or(usize::MAX)
            .min(self.events.len());
        self.events
            .range(idx..)
            .enumerate()
            .map(move |(i, e)| (start + i as u64, e))
    }

    /// The newest event matching `pred`, looking at no more than the last
    /// `limit` events.
    pub fn find_latest(&self, limit: usize, pred: impl Fn(&BusEvent) -> bool) -> Option<&BusEvent> {
        self.events.iter().rev().take(limit).find(|e| pred(e))
    }

    /// Drop every event. Sequence numbers keep increasing, so cursors held
    /// by windows stay valid (they simply see nothing older).
    pub fn clear(&mut self) {
        self.first_seq = self.next_seq();
        self.events.clear();
        self.epoch += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::{BusId, CanFrame, Direction, NodeId, Timestamp};

    pub(crate) fn ev(id: u32, t_ms: u64) -> BusEvent {
        BusEvent {
            time: Timestamp(t_ms * 1_000_000),
            bus: BusId(1),
            sender: NodeId(1),
            origin: NodeId(1),
            dir: Direction::Tx,
            frame_uid: 0,
            hop: 0,
            frame: CanFrame::new(id, false, &[1, 2, 3]).unwrap(),
            kind: Default::default(),
        }
    }

    fn ids(s: &FrameStore, from: u64) -> Vec<(u64, u32)> {
        s.iter_from(from).map(|(q, e)| (q, e.frame.id)).collect()
    }

    #[test]
    fn push_get_and_iterate() {
        let mut s = FrameStore::new(10);
        s.push_batch(&[ev(1, 0), ev(2, 1), ev(3, 2)]);
        assert_eq!((s.len(), s.total_pushed(), s.capacity()), (3, 3, 10));
        assert_eq!(s.get(1).unwrap().frame.id, 2);
        assert!(s.get(3).is_none());
        assert_eq!(ids(&s, 1), [(1, 2), (2, 3)]);
        assert!(ids(&s, 99).is_empty());
    }

    #[test]
    fn oldest_dropped_when_full() {
        let mut s = FrameStore::new(3);
        s.push_batch(&(1..=5).map(|i| ev(i, 0)).collect::<Vec<_>>());
        assert!(s.is_full());
        assert_eq!((s.len(), s.first_seq(), s.total_pushed()), (3, 2, 5));
        assert!(s.get(1).is_none());
        // A cursor older than the buffer sees what is left.
        assert_eq!(ids(&s, 0), [(2, 3), (3, 4), (4, 5)]);
    }

    #[test]
    fn reject_when_full_keeps_old_events() {
        let mut s = FrameStore::new(2);
        s.set_reject_when_full(true);
        s.push_batch(&[ev(1, 0), ev(2, 0), ev(3, 0)]);
        assert_eq!(ids(&s, 0), [(0, 1), (1, 2)]);
    }

    #[test]
    fn clear_keeps_sequence_and_bumps_epoch() {
        let mut s = FrameStore::new(5);
        s.push_batch(&[ev(1, 0), ev(2, 0)]);
        let e = s.epoch();
        s.clear();
        assert_eq!((s.len(), s.first_seq(), s.next_seq()), (0, 2, 2));
        assert_ne!(s.epoch(), e);
        s.push_batch(&[ev(9, 0)]);
        assert_eq!(ids(&s, 0), [(2, 9)]);
    }

    #[test]
    fn shrinking_capacity_drops_oldest() {
        let mut s = FrameStore::new(5);
        s.push_batch(&(1..=5).map(|i| ev(i, 0)).collect::<Vec<_>>());
        s.set_capacity(2);
        assert_eq!(ids(&s, 0), [(3, 4), (4, 5)]);
    }

    #[test]
    fn find_latest_scans_backwards_with_limit() {
        let mut s = FrameStore::new(10);
        s.push_batch(&[ev(1, 0), ev(2, 1), ev(1, 2), ev(3, 3)]);
        assert_eq!(
            s.find_latest(10, |e| e.frame.id == 1).unwrap().time.0,
            2_000_000
        );
        assert!(s.find_latest(1, |e| e.frame.id == 1).is_none());
    }
}
