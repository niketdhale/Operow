use operow_core::{CanFrame, is_valid_fd_len};

use crate::*;

const MS: u64 = 1_000_000;

fn pair_cfg() -> (IsoTpConfig, IsoTpConfig) {
    (
        IsoTpConfig::new(0x7E0, 0x7E8),
        IsoTpConfig::new(0x7E8, 0x7E0),
    )
}

fn fd_pair() -> (IsoTpConfig, IsoTpConfig) {
    let (mut a, mut b) = pair_cfg();
    for c in [&mut a, &mut b] {
        c.fd = true;
        c.tx_dl = 64;
    }
    (a, b)
}

#[derive(Default)]
struct Log {
    /// (time, sent by A, frame)
    frames: Vec<(u64, bool, CanFrame)>,
    /// (time, is A, non-frame action)
    events: Vec<(u64, bool, IsoTpAction)>,
}

impl Log {
    fn received(&self, a: bool) -> Vec<Vec<u8>> {
        self.events
            .iter()
            .filter_map(|(_, w, act)| match act {
                IsoTpAction::Received(d) if *w == a => Some(d.clone()),
                _ => None,
            })
            .collect()
    }
    fn errors(&self, a: bool) -> Vec<IsoTpError> {
        self.events
            .iter()
            .filter_map(|(_, w, act)| match act {
                IsoTpAction::Error(e) if *w == a => Some(e.clone()),
                _ => None,
            })
            .collect()
    }
    fn tx_done(&self, a: bool) -> usize {
        self.events
            .iter()
            .filter(|(_, w, act)| *w == a && *act == IsoTpAction::TxDone)
            .count()
    }
}

/// Wire two channels together with zero latency, advancing virtual time to
/// the next deadline until quiescent.
fn run(a: &mut IsoTpChannel, b: &mut IsoTpChannel, start: u64) -> Log {
    let mut log = Log::default();
    let mut t = start;
    for _ in 0..200_000 {
        loop {
            let mut any = false;
            for is_a in [true, false] {
                let (me, peer) = if is_a {
                    (&mut *a, &mut *b)
                } else {
                    (&mut *b, &mut *a)
                };
                for act in me.poll(t) {
                    any = true;
                    match act {
                        IsoTpAction::SendFrame(f) => {
                            log.frames.push((t, is_a, f));
                            peer.on_frame(&f, t);
                        }
                        other => log.events.push((t, is_a, other)),
                    }
                }
            }
            if !any {
                break;
            }
        }
        match [a.next_deadline(), b.next_deadline()]
            .into_iter()
            .flatten()
            .min()
        {
            Some(n) => t = n.max(t),
            None => break,
        }
    }
    log
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + 3) as u8).collect()
}

fn chan(c: IsoTpConfig) -> IsoTpChannel {
    IsoTpChannel::new(c).unwrap()
}

fn take_frames(acts: Vec<IsoTpAction>) -> Vec<CanFrame> {
    acts.into_iter()
        .filter_map(|a| match a {
            IsoTpAction::SendFrame(f) => Some(f),
            _ => None,
        })
        .collect()
}

#[test]
fn roundtrip_classic() {
    for n in (0..=8).chain([62, 63, 100, 4095]) {
        let (ca, cb) = pair_cfg();
        let (mut a, mut b) = (chan(ca), chan(cb));
        let p = payload(n);
        a.send(&p, 0).unwrap();
        let log = run(&mut a, &mut b, 0);
        assert_eq!(log.received(false), vec![p], "len {n}");
        assert_eq!(log.tx_done(true), 1, "len {n}");
        assert!(log.errors(true).is_empty() && log.errors(false).is_empty());
        assert!(log.frames.iter().all(|(_, _, f)| !f.fd && f.dlc <= 8));
    }
}

#[test]
fn roundtrip_fd_and_boundaries() {
    for n in (0..=8).chain([61, 62, 63, 64, 100, 4095, 4096, 5000]) {
        let (ca, cb) = fd_pair();
        let (mut a, mut b) = (chan(ca.clone()), chan(cb));
        let p = payload(n);
        a.send(&p, 0).unwrap();
        let log = run(&mut a, &mut b, 0);
        assert_eq!(log.received(false), vec![p], "len {n}");
        assert_eq!(log.tx_done(true), 1);
        for (_, _, f) in log.frames.iter().filter(|(_, from_a, _)| *from_a) {
            assert!(
                f.fd && is_valid_fd_len(f.dlc as usize),
                "len {n} dlc {}",
                f.dlc
            );
        }
        let first = log.frames[0].2;
        let ty = frame_pci_type(&first, &ca).unwrap();
        match n {
            0..=62 => assert_eq!(ty, PciType::Single, "len {n}"),
            _ => assert_eq!(ty, PciType::First, "len {n}"),
        }
        if n == 62 {
            assert_eq!(first.dlc, 64);
        }
        if n == 5000 {
            assert_eq!(&first.payload()[..6], &[0x10, 0, 0, 0, 0x13, 0x88]);
        }
    }
}

#[test]
fn full_duplex() {
    let (ca, cb) = pair_cfg();
    let (mut a, mut b) = (chan(ca), chan(cb));
    a.send(&payload(50), 0).unwrap();
    b.send(&payload(70), 0).unwrap();
    let log = run(&mut a, &mut b, 0);
    assert_eq!(log.received(false), vec![payload(50)]);
    assert_eq!(log.received(true), vec![payload(70)]);
}

#[test]
fn length_limits_and_busy() {
    let (ca, _) = pair_cfg();
    let mut a = chan(ca);
    assert_eq!(
        a.send(&payload(4096), 0),
        Err(IsoTpError::MessageTooLong(4096))
    );
    a.send(&payload(100), 0).unwrap();
    assert_eq!(a.send(&payload(5), 0), Err(IsoTpError::Busy));
}

#[test]
fn invalid_config() {
    let (mut c, _) = pair_cfg();
    c.tx_dl = 12;
    assert!(matches!(
        IsoTpChannel::new(c),
        Err(IsoTpError::InvalidConfig(_))
    ));
    let (mut c, _) = pair_cfg();
    c.addressing = Addressing::Mixed { ae: 1 };
    assert!(IsoTpChannel::new(c).is_err());
    let (mut c, _) = fd_pair();
    c.tx_dl = 10;
    assert!(IsoTpChannel::new(c).is_err());
    assert!(IsoTpChannel::new(IsoTpConfig::new(0x800, 1)).is_err());
}

fn cf_times(log: &Log) -> Vec<u64> {
    log.frames
        .iter()
        .filter(|(_, a, f)| *a && f.payload()[0] >> 4 == 2)
        .map(|(t, _, _)| *t)
        .collect()
}

#[test]
fn st_min_enforced() {
    for (st, ns) in [
        (StMin(5), 5 * MS),
        (StMin(0xF3), 300_000),
        (StMin(0xF9), 900_000),
    ] {
        let (ca, mut cb) = pair_cfg();
        cb.st_min = st;
        let (mut a, mut b) = (chan(ca), chan(cb));
        a.send(&payload(60), 1000).unwrap();
        let log = run(&mut a, &mut b, 1000);
        assert_eq!(log.received(false), vec![payload(60)]);
        let t = cf_times(&log);
        assert_eq!(t.len(), 8);
        assert!(t.windows(2).all(|w| w[1] - w[0] >= ns), "{st:?}: {t:?}");
    }
    assert_eq!(StMin::from_millis(200), StMin(127));
    assert_eq!(StMin::from_micros(450), StMin(0xF4));
    assert_eq!(StMin(0x80).as_nanos(), 127 * MS);
}

#[test]
fn block_size_multiple_fcs() {
    let (ca, mut cb) = pair_cfg();
    cb.block_size = 4;
    let (mut a, mut b) = (chan(ca.clone()), chan(cb));
    a.send(&payload(100), 0).unwrap();
    let log = run(&mut a, &mut b, 0);
    assert_eq!(log.received(false), vec![payload(100)]);
    let fcs = log
        .frames
        .iter()
        .filter(|(_, from_a, f)| !*from_a && frame_pci_type(f, &ca) == Some(PciType::FlowControl))
        .count();
    assert_eq!(fcs, 4); // after FF, then after CF 4, 8, 12 of 14
    assert_eq!(cf_times(&log).len(), 14);
}

fn sender_after_ff() -> (IsoTpChannel, Vec<CanFrame>) {
    let (ca, _) = pair_cfg();
    let mut a = chan(ca);
    a.send(&payload(100), 0).unwrap();
    let f = take_frames(a.poll(0));
    (a, f)
}

fn fc(fs: u8, bs: u8, st: u8) -> CanFrame {
    CanFrame::new(0x7E8, false, &[0x30 | fs, bs, st]).unwrap()
}

#[test]
fn fc_wait_then_cts() {
    let (mut a, ff) = sender_after_ff();
    assert_eq!(ff.len(), 1);
    a.on_frame(&fc(1, 0, 0), 10 * MS);
    assert!(a.poll(10 * MS).is_empty());
    assert_eq!(a.next_deadline(), Some(10 * MS + 1000 * MS));
    a.on_frame(&fc(0, 0, 0), 20 * MS);
    let acts = a.poll(20 * MS);
    assert_eq!(take_frames(acts.clone()).len(), 14);
    assert_eq!(acts.last(), Some(&IsoTpAction::TxDone));
}

#[test]
fn wft_exceeded() {
    let (ca, _) = pair_cfg();
    let mut ca = ca;
    ca.max_wft = 2;
    let mut a = chan(ca);
    a.send(&payload(100), 0).unwrap();
    a.poll(0);
    for i in 1..=3 {
        a.on_frame(&fc(1, 0, 0), i);
    }
    assert_eq!(a.poll(3), vec![IsoTpAction::Error(IsoTpError::WftExceeded)]);
    assert_eq!(a.next_deadline(), None);
    a.send(&payload(3), 4).unwrap();
}

#[test]
fn overflow_and_invalid_fs() {
    let (mut a, _) = sender_after_ff();
    a.on_frame(&fc(2, 0, 0), 1);
    assert_eq!(a.poll(1), vec![IsoTpAction::Error(IsoTpError::Overflow)]);
    a.send(&payload(100), 2).unwrap();
    a.poll(2);
    a.on_frame(&fc(7, 0, 0), 3);
    assert_eq!(
        a.poll(3),
        vec![IsoTpAction::Error(IsoTpError::InvalidFlowStatus(7))]
    );
}

#[test]
fn receiver_overflow_sends_ovflw() {
    let (_, mut cb) = pair_cfg();
    cb.max_rx_len = 50;
    let mut b = chan(cb);
    b.on_frame(
        &CanFrame::new(0x7E0, false, &[0x10, 100, 1, 2, 3, 4, 5, 6]).unwrap(),
        0,
    );
    let acts = b.poll(0);
    assert_eq!(acts[0], IsoTpAction::SendFrame(fc(2, 0, 0).with_id(0x7E8)));
    assert_eq!(acts[1], IsoTpAction::Error(IsoTpError::RxOverflow(100)));
    assert_eq!(b.next_deadline(), None);
}

trait WithId {
    fn with_id(self, id: u32) -> Self;
}
impl WithId for CanFrame {
    fn with_id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }
}

#[test]
fn n_bs_timeout() {
    let (ca, _) = pair_cfg();
    let mut a = chan(ca);
    a.send(&payload(20), 5).unwrap();
    assert_eq!(take_frames(a.poll(5)).len(), 1);
    assert_eq!(a.next_deadline(), Some(5 + 1000 * MS));
    assert!(a.poll(5 + 1000 * MS - 1).is_empty());
    assert_eq!(
        a.poll(5 + 1000 * MS),
        vec![IsoTpAction::Error(IsoTpError::TimeoutNBs)]
    );
    assert_eq!(a.next_deadline(), None);
}

fn ff20() -> CanFrame {
    CanFrame::new(0x7E0, false, &[0x10, 20, 0, 1, 2, 3, 4, 5]).unwrap()
}

#[test]
fn n_cr_timeout() {
    let (_, cb) = pair_cfg();
    let mut b = chan(cb);
    b.on_frame(&ff20(), 0);
    assert_eq!(take_frames(b.poll(0)).len(), 1);
    assert_eq!(b.next_deadline(), Some(1000 * MS));
    assert_eq!(
        b.poll(1000 * MS),
        vec![IsoTpAction::Error(IsoTpError::TimeoutNCr)]
    );
}

#[test]
fn wrong_sequence_number() {
    let (_, cb) = pair_cfg();
    let mut b = chan(cb);
    b.on_frame(&ff20(), 0);
    b.poll(0);
    b.on_frame(
        &CanFrame::new(0x7E0, false, &[0x22, 6, 7, 8, 9, 10, 11, 12]).unwrap(),
        1,
    );
    assert_eq!(
        b.poll(1),
        vec![IsoTpAction::Error(IsoTpError::WrongSequenceNumber {
            expected: 1,
            got: 2
        })]
    );
    assert_eq!(b.next_deadline(), None);
}

#[test]
fn padding() {
    let (mut ca, cb) = pair_cfg();
    ca.padding = Some(0xCC);
    let mut a = chan(ca.clone());
    a.send(&[1, 2, 3], 0).unwrap();
    let f = take_frames(a.poll(0));
    assert_eq!(f[0].payload(), &[3, 1, 2, 3, 0xCC, 0xCC, 0xCC, 0xCC]);
    let mut b = chan(cb);
    b.on_frame(&ff20(), 0);
    assert_eq!(b.poll(0).len(), 1); // unpadded FC is 3 bytes
    let mut b2cfg = IsoTpConfig::new(0x7E8, 0x7E0);
    b2cfg.padding = Some(0xAA);
    let mut b2 = chan(b2cfg);
    b2.on_frame(&ff20(), 0);
    let fcf = take_frames(b2.poll(0));
    assert_eq!(
        fcf[0].payload(),
        &[0x30, 0, 0, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]
    );
    // No padding: shortest frame.
    let (c0, _) = pair_cfg();
    let mut a0 = chan(c0);
    a0.send(&[1, 2, 3], 0).unwrap();
    assert_eq!(take_frames(a0.poll(0))[0].dlc, 4);
    // Last CF padded to 8.
    let mut a = chan(ca);
    a.send(&payload(10), 0).unwrap();
    a.poll(0);
    a.on_frame(&fc(0, 0, 0), 0);
    let f = take_frames(a.poll(0));
    assert_eq!(f[0].payload(), &[0x21, 45, 52, 59, 66, 0xCC, 0xCC, 0xCC]);
}

#[test]
fn fd_frame_lengths_padded() {
    let (mut ca, _) = fd_pair();
    ca.tx_dl = 32;
    let mut a = chan(ca.clone());
    a.send(&payload(20), 0).unwrap(); // escape SF: 22 bytes -> 24
    let f = take_frames(a.poll(0));
    assert_eq!(f[0].dlc, 24);
    assert_eq!(&f[0].payload()[..2], &[0, 20]);
    assert_eq!(&f[0].payload()[22..], &[0xCC, 0xCC]);
    // Multi-frame: FF fills 32, last CF rounds up.
    let mut a = chan(ca);
    a.send(&payload(100), 0).unwrap();
    let ff = take_frames(a.poll(0));
    assert_eq!(ff[0].dlc, 32);
    a.on_frame(&fc(0, 0, 0), 0);
    let cfs = take_frames(a.poll(0));
    assert_eq!(cfs.len(), 3); // FF carries 30; CFs carry 31, 31, 8
    assert!(cfs.iter().all(|f| is_valid_fd_len(f.dlc as usize)));
}

#[test]
fn extended_addressing() {
    let (mut ca, mut cb) = pair_cfg();
    ca.addressing = Addressing::Extended {
        tx_ta: 0x12,
        rx_ta: 0x34,
    };
    cb.addressing = Addressing::Extended {
        tx_ta: 0x34,
        rx_ta: 0x12,
    };
    let (mut a, mut b) = (chan(ca.clone()), chan(cb));
    a.send(&payload(100), 0).unwrap();
    let log = run(&mut a, &mut b, 0);
    assert_eq!(log.received(false), vec![payload(100)]);
    assert_eq!(log.frames[0].2.payload()[0], 0x12);
    assert_eq!(log.frames[1].2.payload()[0], 0x34);
    assert_eq!(describe_pci(&log.frames[0].2, &ca), "FF len=100");
    // Short message: 6 byte SF limit.
    let mut a = chan(ca);
    a.send(&payload(6), 0).unwrap();
    assert_eq!(take_frames(a.poll(0))[0].payload()[1], 6);
    // Wrong target address is ignored.
    let mut b = chan(IsoTpConfig {
        addressing: Addressing::Extended {
            tx_ta: 0x34,
            rx_ta: 0x12,
        },
        ..IsoTpConfig::new(0x7E8, 0x7E0)
    });
    b.on_frame(&CanFrame::new(0x7E0, false, &[0x99, 1, 7]).unwrap(), 0);
    assert!(b.poll(0).is_empty());
    b.on_frame(&CanFrame::new(0x7E0, false, &[0x12, 1, 7]).unwrap(), 0);
    assert_eq!(b.poll(0), vec![IsoTpAction::Received(vec![7])]);
}

#[test]
fn mixed_addressing() {
    let (mut ca, mut cb) = pair_cfg();
    for (c, tx, rx) in [
        (&mut ca, 0x18DA_F100, 0x18DA_00F1),
        (&mut cb, 0x18DA_00F1, 0x18DA_F100),
    ] {
        c.tx_id = tx;
        c.rx_id = rx;
        c.extended_ids = true;
        c.addressing = Addressing::Mixed { ae: 0x55 };
    }
    let (mut a, mut b) = (chan(ca), chan(cb));
    a.send(&payload(300), 0).unwrap();
    let log = run(&mut a, &mut b, 0);
    assert_eq!(log.received(false), vec![payload(300)]);
    assert!(
        log.frames
            .iter()
            .all(|(_, _, f)| f.extended && f.payload()[0] == 0x55)
    );
}

#[test]
fn unexpected_ff_restarts_reception() {
    let (_, cb) = pair_cfg();
    let mut b = chan(cb);
    b.on_frame(&ff20(), 0);
    b.on_frame(
        &CanFrame::new(0x7E0, false, &[0x21, 6, 7, 8, 9, 10, 11, 12]).unwrap(),
        1,
    );
    let new_ff = CanFrame::new(0x7E0, false, &[0x10, 10, 0, 1, 2, 3, 4, 5]).unwrap();
    b.on_frame(&new_ff, 2);
    let acts = b.poll(2);
    assert!(acts.contains(&IsoTpAction::Error(IsoTpError::UnexpectedPdu)));
    assert_eq!(take_frames(acts).len(), 2);
    b.on_frame(
        &CanFrame::new(0x7E0, false, &[0x21, 6, 7, 8, 9]).unwrap(),
        3,
    );
    assert_eq!(b.poll(3), vec![IsoTpAction::Received((0..10).collect())]);
    // SF during reception also aborts it.
    b.on_frame(&ff20(), 4);
    b.on_frame(&CanFrame::new(0x7E0, false, &[1, 9]).unwrap(), 5);
    let acts = b.poll(5);
    assert!(acts.contains(&IsoTpAction::Received(vec![9])));
    assert!(acts.contains(&IsoTpAction::Error(IsoTpError::UnexpectedPdu)));
}

#[test]
fn ignores_foreign_frames() {
    let (_, cb) = pair_cfg();
    let mut b = chan(cb);
    b.on_frame(&CanFrame::new(0x123, false, &[1, 2]).unwrap(), 0);
    b.on_frame(&CanFrame::new(0x7E0, true, &[1, 2]).unwrap(), 0);
    b.on_frame(&CanFrame::new(0x7E0, false, &[0x25, 1]).unwrap(), 0); // stray CF
    b.on_frame(&CanFrame::new(0x7E0, false, &[0x30, 0, 0]).unwrap(), 0); // stray FC
    b.on_frame(&CanFrame::new(0x7E0, false, &[0xF0, 0]).unwrap(), 0);
    assert!(b.poll(0).is_empty());
    assert_eq!(b.next_deadline(), None);
}

#[test]
fn describe_and_classify() {
    let (ca, _) = pair_cfg();
    let d = |b: &[u8]| describe_pci(&CanFrame::new(0x7E0, false, b).unwrap(), &ca);
    assert_eq!(d(&[3, 1, 2, 3]), "SF len=3");
    assert_eq!(d(&[0x10, 20, 0, 0, 0, 0, 0, 0]), "FF len=20");
    assert_eq!(d(&[0x21, 0]), "CF sn=1");
    assert_eq!(d(&[0x30, 0, 0]), "FC CTS bs=0 st=0");
    assert_eq!(d(&[0x31, 8, 0xF1]), "FC WAIT bs=8 st=241");
    assert_eq!(d(&[0x32, 0, 0]), "FC OVFLW bs=0 st=0");
    let other = CanFrame::new(0x100, false, &[1, 2]).unwrap();
    assert_eq!(describe_pci(&other, &ca), "not ISO-TP");
    assert_eq!(frame_pci_type(&other, &ca), None);
}
