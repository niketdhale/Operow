use operow_core::{CanFrame, is_valid_fd_len};
use thiserror::Error;

use crate::config::{IsoTpConfig, StMin};
use crate::pdu::{Pdu, parse_pdu, round_up_fd};

/// Errors reported by an [`IsoTpChannel`], either returned from
/// [`IsoTpChannel::send`] or delivered as [`IsoTpAction::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IsoTpError {
    #[error("a transmission is already in progress")]
    Busy,
    #[error("message of {0} bytes exceeds the maximum length for this channel")]
    MessageTooLong(usize),
    #[error("invalid ISO-TP configuration: {0}")]
    InvalidConfig(String),
    #[error("N_Bs timeout: no flow control frame from receiver")]
    TimeoutNBs,
    #[error("N_Cr timeout: consecutive frame not received in time")]
    TimeoutNCr,
    #[error("unexpected PDU interrupted a reception in progress")]
    UnexpectedPdu,
    #[error("wrong sequence number: expected {expected}, got {got}")]
    WrongSequenceNumber { expected: u8, got: u8 },
    #[error("receiver signalled buffer overflow (FC OVFLW)")]
    Overflow,
    #[error("incoming message of {0} bytes exceeds the receive limit")]
    RxOverflow(u32),
    #[error("too many consecutive FC WAIT frames")]
    WftExceeded,
    #[error("invalid flow status {0} in flow control frame")]
    InvalidFlowStatus(u8),
}

/// Output of [`IsoTpChannel::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsoTpAction {
    /// Transmit this frame now.
    SendFrame(CanFrame),
    /// A complete message was received.
    Received(Vec<u8>),
    /// The queued message was fully sent.
    TxDone,
    /// A session was aborted or a protocol error occurred.
    Error(IsoTpError),
}

enum TxState {
    WaitFc { deadline: u64 },
    SendCf { next_at: u64 },
}

struct TxSession {
    data: Vec<u8>,
    pos: usize,
    sn: u8,
    st_min_ns: u64,
    bs: u8,
    bs_left: u8,
    wft: u8,
    state: TxState,
}

struct RxSession {
    buf: Vec<u8>,
    total: usize,
    sn: u8,
    bs_left: u8,
    deadline: u64,
}

/// Sans-IO ISO 15765-2 channel: one transmit and one receive session at a
/// time (full duplex).
///
/// Simplifications versus the standard: there is no transmit confirmation so
/// N_As/N_Ar never fire and frames are assumed sent when emitted; the
/// receiver never sends FC WAIT and answers at once (N_Br = 0); the first CF
/// after an FC is sent immediately; a zero-length SF is accepted and sent;
/// unexpected CF/FC frames are silently ignored; a malformed first frame
/// (length fitting a SF) is ignored.
pub struct IsoTpChannel {
    cfg: IsoTpConfig,
    tx: Option<TxSession>,
    rx: Option<RxSession>,
    out: Vec<IsoTpAction>,
    last_now: u64,
}

const MS: u64 = 1_000_000;

impl IsoTpChannel {
    /// Create a channel, validating the configuration.
    pub fn new(cfg: IsoTpConfig) -> Result<Self, IsoTpError> {
        let bad = |m: &str| Err(IsoTpError::InvalidConfig(m.to_string()));
        if cfg.fd {
            if cfg.tx_dl < 8 || !is_valid_fd_len(cfg.tx_dl as usize) {
                return bad("tx_dl must be 8, 12, 16, 20, 24, 32, 48 or 64 for FD");
            }
        } else if cfg.tx_dl != 8 {
            return bad("tx_dl must be 8 for classic CAN");
        }
        if matches!(cfg.addressing, crate::Addressing::Mixed { .. }) && !cfg.extended_ids {
            return bad("mixed addressing requires 29-bit ids");
        }
        for id in [cfg.tx_id, cfg.rx_id] {
            if let Err(e) = CanFrame::new(id, cfg.extended_ids, &[]) {
                return Err(IsoTpError::InvalidConfig(e.to_string()));
            }
        }
        Ok(IsoTpChannel {
            cfg,
            tx: None,
            rx: None,
            out: Vec::new(),
            last_now: 0,
        })
    }

    /// The channel configuration.
    pub fn config(&self) -> &IsoTpConfig {
        &self.cfg
    }

    /// Queue a message for transmission. The first frame is delivered by the
    /// next [`poll`](Self::poll).
    pub fn send(&mut self, payload: &[u8], now_ns: u64) -> Result<(), IsoTpError> {
        self.last_now = now_ns;
        if self.tx.is_some() {
            return Err(IsoTpError::Busy);
        }
        let too_long = if self.cfg.fd {
            payload.len() as u64 > u32::MAX as u64
        } else {
            payload.len() > 4095
        };
        if too_long {
            return Err(IsoTpError::MessageTooLong(payload.len()));
        }
        let off = self.cfg.addr_offset();
        let cap = self.cfg.can_dl() - off;
        let len = payload.len();
        let (body, consumed): (Vec<u8>, usize) = if len <= 7 - off {
            let mut b = vec![len as u8];
            b.extend_from_slice(payload);
            (b, len)
        } else if self.cfg.fd && self.cfg.tx_dl > 8 && len <= cap - 2 {
            let mut b = vec![0, len as u8];
            b.extend_from_slice(payload);
            (b, len)
        } else if len <= 4095 {
            let take = cap - 2;
            let mut b = vec![0x10 | (len >> 8) as u8, len as u8];
            b.extend_from_slice(&payload[..take]);
            (b, take)
        } else {
            let take = cap - 6;
            let mut b = vec![0x10, 0];
            b.extend_from_slice(&(len as u32).to_be_bytes());
            b.extend_from_slice(&payload[..take]);
            (b, take)
        };
        let frame = self.make_frame(body);
        self.out.push(IsoTpAction::SendFrame(frame));
        if consumed == len && len < cap {
            self.out.push(IsoTpAction::TxDone);
        } else {
            self.tx = Some(TxSession {
                data: payload.to_vec(),
                pos: consumed,
                sn: 1,
                st_min_ns: 0,
                bs: 0,
                bs_left: 0,
                wft: 0,
                state: TxState::WaitFc {
                    deadline: now_ns + self.cfg.timeouts.n_bs as u64 * MS,
                },
            });
        }
        Ok(())
    }

    /// Feed a received CAN frame. Frames with other ids, wrong address bytes
    /// or malformed PCI are ignored. Results appear in the next `poll`.
    pub fn on_frame(&mut self, frame: &CanFrame, now_ns: u64) {
        self.last_now = now_ns;
        if frame.id != self.cfg.rx_id || frame.extended != self.cfg.extended_ids {
            return;
        }
        let p = frame.payload();
        if let Some(a) = self.cfg.rx_addr_byte()
            && p.first() != Some(&a)
        {
            return;
        }
        let Some(p) = p.get(self.cfg.addr_offset()..) else {
            return;
        };
        match parse_pdu(p, frame.dlc > 8) {
            Some(Pdu::Single(d)) => {
                self.abort_rx_unexpected();
                self.out.push(IsoTpAction::Received(d.to_vec()));
            }
            Some(Pdu::First { len, data }) => {
                if len as usize <= data.len() {
                    return;
                }
                self.abort_rx_unexpected();
                if len > self.cfg.max_rx_len {
                    let fc = self.fc_frame(2);
                    self.out.push(IsoTpAction::SendFrame(fc));
                    self.out
                        .push(IsoTpAction::Error(IsoTpError::RxOverflow(len)));
                    return;
                }
                self.rx = Some(RxSession {
                    buf: data.to_vec(),
                    total: len as usize,
                    sn: 1,
                    bs_left: self.cfg.block_size,
                    deadline: now_ns + self.cfg.timeouts.n_cr as u64 * MS,
                });
                let fc = self.fc_frame(0);
                self.out.push(IsoTpAction::SendFrame(fc));
            }
            Some(Pdu::Consecutive { sn, data }) => self.on_cf(sn, data, now_ns),
            Some(Pdu::FlowControl { fs, bs, st }) => self.on_fc(fs, bs, st, now_ns),
            None => {}
        }
    }

    fn abort_rx_unexpected(&mut self) {
        if self.rx.take().is_some() {
            self.out.push(IsoTpAction::Error(IsoTpError::UnexpectedPdu));
        }
    }

    fn on_cf(&mut self, sn: u8, data: &[u8], now: u64) {
        let Some(rx) = self.rx.as_mut() else { return };
        if sn != rx.sn {
            let err = IsoTpError::WrongSequenceNumber {
                expected: rx.sn,
                got: sn,
            };
            self.rx = None;
            self.out.push(IsoTpAction::Error(err));
            return;
        }
        let take = data.len().min(rx.total - rx.buf.len());
        rx.buf.extend_from_slice(&data[..take]);
        rx.sn = (rx.sn + 1) & 0xF;
        if rx.buf.len() >= rx.total {
            let rx = self.rx.take().unwrap();
            self.out.push(IsoTpAction::Received(rx.buf));
            return;
        }
        rx.deadline = now + self.cfg.timeouts.n_cr as u64 * MS;
        let mut need_fc = false;
        if self.cfg.block_size != 0 {
            rx.bs_left -= 1;
            if rx.bs_left == 0 {
                rx.bs_left = self.cfg.block_size;
                need_fc = true;
            }
        }
        if need_fc {
            let fc = self.fc_frame(0);
            self.out.push(IsoTpAction::SendFrame(fc));
        }
    }

    fn on_fc(&mut self, fs: u8, bs: u8, st: u8, now: u64) {
        let n_bs = self.cfg.timeouts.n_bs as u64 * MS;
        let max_wft = self.cfg.max_wft;
        let Some(tx) = self.tx.as_mut() else { return };
        if !matches!(tx.state, TxState::WaitFc { .. }) {
            return;
        }
        let err = match fs {
            0 => {
                tx.bs = bs;
                tx.bs_left = bs;
                tx.st_min_ns = StMin(st).as_nanos();
                tx.wft = 0;
                tx.state = TxState::SendCf { next_at: now };
                return;
            }
            1 => {
                tx.wft += 1;
                if tx.wft > max_wft {
                    IsoTpError::WftExceeded
                } else {
                    tx.state = TxState::WaitFc {
                        deadline: now + n_bs,
                    };
                    return;
                }
            }
            2 => IsoTpError::Overflow,
            n => IsoTpError::InvalidFlowStatus(n),
        };
        self.tx = None;
        self.out.push(IsoTpAction::Error(err));
    }

    /// Advance timers and drain pending actions.
    pub fn poll(&mut self, now_ns: u64) -> Vec<IsoTpAction> {
        self.last_now = now_ns;
        let mut acts = std::mem::take(&mut self.out);
        self.poll_tx(now_ns, &mut acts);
        if let Some(rx) = &self.rx
            && now_ns >= rx.deadline
        {
            self.rx = None;
            acts.push(IsoTpAction::Error(IsoTpError::TimeoutNCr));
        }
        acts
    }

    fn poll_tx(&mut self, now: u64, acts: &mut Vec<IsoTpAction>) {
        let Some(mut tx) = self.tx.take() else { return };
        let off = self.cfg.addr_offset();
        let cap = self.cfg.can_dl() - off;
        loop {
            match tx.state {
                TxState::WaitFc { deadline } => {
                    if now >= deadline {
                        acts.push(IsoTpAction::Error(IsoTpError::TimeoutNBs));
                        return;
                    }
                    break;
                }
                TxState::SendCf { next_at } => {
                    if now < next_at {
                        break;
                    }
                    let take = (cap - 1).min(tx.data.len() - tx.pos);
                    let mut body = vec![0x20 | tx.sn];
                    body.extend_from_slice(&tx.data[tx.pos..tx.pos + take]);
                    tx.pos += take;
                    tx.sn = (tx.sn + 1) & 0xF;
                    acts.push(IsoTpAction::SendFrame(self.make_frame(body)));
                    if tx.pos >= tx.data.len() {
                        acts.push(IsoTpAction::TxDone);
                        return;
                    }
                    tx.state = TxState::SendCf {
                        next_at: now + tx.st_min_ns,
                    };
                    if tx.bs != 0 {
                        tx.bs_left -= 1;
                        if tx.bs_left == 0 {
                            tx.state = TxState::WaitFc {
                                deadline: now + self.cfg.timeouts.n_bs as u64 * MS,
                            };
                        }
                    }
                }
            }
        }
        self.tx = Some(tx);
    }

    /// The earliest time (ns) at which `poll` has something to do, if any.
    pub fn next_deadline(&self) -> Option<u64> {
        let mut d: Option<u64> = None;
        let mut take = |t: u64| d = Some(d.map_or(t, |x| x.min(t)));
        if !self.out.is_empty() {
            take(self.last_now);
        }
        if let Some(tx) = &self.tx {
            take(match tx.state {
                TxState::WaitFc { deadline } => deadline,
                TxState::SendCf { next_at } => next_at,
            });
        }
        if let Some(rx) = &self.rx {
            take(rx.deadline);
        }
        d
    }

    fn fc_frame(&self, fs: u8) -> CanFrame {
        self.make_frame(vec![0x30 | fs, self.cfg.block_size, self.cfg.st_min.0])
    }

    /// Prepend the address byte, pad and build the CAN frame.
    fn make_frame(&self, body: Vec<u8>) -> CanFrame {
        let mut bytes = Vec::with_capacity(self.cfg.can_dl());
        bytes.extend(self.cfg.tx_addr_byte());
        bytes.extend(body);
        let pad = self.cfg.padding.unwrap_or(0xCC);
        let target = if self.cfg.fd {
            let min = if self.cfg.padding.is_some() { 8 } else { 0 };
            round_up_fd(bytes.len().max(min))
        } else if self.cfg.padding.is_some() {
            8
        } else {
            bytes.len()
        };
        bytes.resize(target, pad);
        let (id, ext) = (self.cfg.tx_id, self.cfg.extended_ids);
        if self.cfg.fd {
            CanFrame::new_fd(id, ext, false, &bytes)
        } else {
            CanFrame::new(id, ext, &bytes)
        }
        .expect("validated configuration yields valid frames")
    }
}
