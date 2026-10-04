//! [`DiagEcu`]: a simulated UDS (ISO 14229) server layered over an ECU.

use std::collections::VecDeque;

use operow_core::{BusId, CanFrame, DiagConfig, DidEntry, DtcEntry, KeyAlgo};
use operow_isotp::{IsoTpAction, IsoTpChannel, IsoTpConfig, IsoTpError, StMin};
use operow_uds::{Dtc, Nrc, Request, Response, service_name};

use crate::ecu::{Ecu, EcuCommand, EcuCtx};

/// The one timer id of the diagnostic server; the range up to
/// `DIAG_TIMER_END` is reserved for it (disjoint from the inner ECU's
/// ids below 0x5000_0000, the script's 0x7000_0000.. and gateway's
/// 0x8000_0000..).
pub(crate) const DIAG_TIMER: u32 = 0x6000_0000;
pub(crate) const DIAG_TIMER_END: u32 = 0x7000_0000;

const MS: u64 = 1_000_000;
/// S3 server timeout: back to the default session without requests.
const S3_NS: u64 = 5000 * MS;
/// Lock-out after too many wrong keys.
const SECURITY_DELAY_NS: u64 = 10_000 * MS;
const MAX_KEY_ATTEMPTS: u8 = 3;
/// A script answering responsePending is asked again after this long.
const PENDING_RETRY_NS: u64 = 100 * MS;
const MAX_PENDING_RETRIES: u32 = 50;
const DEFAULT_SESSION: u8 = 0x01;
const EXTENDED_SESSION: u8 = 0x03;
const ERASE_RID: u16 = 0xFF00;
/// Status availability mask reported with DTC lists.
const DTC_AVAILABILITY: u8 = 0xFF;

struct Retry {
    due: u64,
    req: Vec<u8>,
    functional: bool,
    count: u32,
}

/// Wraps a node's ECU and answers UDS requests received over ISO-TP on the
/// configured ids (physical, plus single-frame functional requests).
///
/// Simplifications: one request is processed at a time (a new request
/// replaces a pending script retry), DID writes must keep the data length,
/// DID/DTC changes are runtime state that survives ECU reset but not a
/// measurement restart, and `0x31` knows only the `0xFF00` erase routine.
/// With a script, `on_diag(req)` may replace any answer and
/// `on_security_key(seed)` supplies keys for `KeyAlgo::Script`; see
/// [`crate::ScriptEcu`].
pub struct DiagEcu {
    inner: Box<dyn Ecu>,
    name: String,
    cfg: DiagConfig,
    chan: IsoTpChannel,
    /// Bus the latest request arrived on; responses go there unless the
    /// config pins a bus.
    rx_bus: Option<BusId>,
    dids: Vec<DidEntry>,
    dtcs: Vec<DtcEntry>,
    session: u8,
    s3_deadline: Option<u64>,
    unlocked: bool,
    seed_sent: Option<Vec<u8>>,
    attempts: u8,
    locked_until: Option<u64>,
    /// Responses with the time they may be handed to ISO-TP.
    outbox: VecDeque<(u64, Vec<u8>)>,
    retry: Option<Retry>,
    armed: Option<u64>,
    logs: Vec<String>,
}

impl DiagEcu {
    pub fn new(inner: Box<dyn Ecu>, name: &str, cfg: &DiagConfig) -> Result<Self, String> {
        let isotp = IsoTpConfig {
            tx_id: cfg.resp_id,
            rx_id: cfg.req_id,
            extended_ids: cfg.extended_ids,
            fd: cfg.fd,
            tx_dl: if cfg.fd { 64 } else { 8 },
            padding: cfg.padding,
            block_size: cfg.block_size,
            st_min: StMin::from_millis(cfg.st_min_ms),
            ..IsoTpConfig::default()
        };
        let chan = IsoTpChannel::new(isotp).map_err(|e| e.to_string())?;
        Ok(DiagEcu {
            inner,
            name: name.to_string(),
            cfg: cfg.clone(),
            chan,
            rx_bus: None,
            dids: cfg.dids.clone(),
            dtcs: cfg.dtcs.clone(),
            session: DEFAULT_SESSION,
            s3_deadline: None,
            unlocked: false,
            seed_sent: None,
            attempts: 0,
            locked_until: None,
            outbox: VecDeque::new(),
            retry: None,
            armed: None,
            logs: Vec::new(),
        })
    }

    fn log(&mut self, now: u64, text: &str) {
        self.logs.push(format!(
            "[{} {:.3}ms] {}",
            self.name,
            now as f64 / 1e6,
            text
        ));
    }

    fn reset_state(&mut self) {
        self.session = DEFAULT_SESSION;
        self.s3_deadline = None;
        self.lock();
        self.attempts = 0;
        self.locked_until = None;
    }

    fn lock(&mut self) {
        self.unlocked = false;
        self.seed_sent = None;
    }

    fn send_frame(&self, frame: CanFrame, ctx: &mut EcuCtx) {
        match self.cfg.bus.or(self.rx_bus) {
            Some(bus) => ctx.send_on(bus, frame),
            None => ctx.send(frame),
        }
    }

    /// Run expiries, then drive ISO-TP and the outbox until nothing moves,
    /// and arm the timer for the next deadline.
    fn pump(&mut self, ctx: &mut EcuCtx) {
        let now = ctx.now().0;
        self.expire(now, ctx);
        loop {
            let mut progress = false;
            for act in self.chan.poll(now) {
                match act {
                    IsoTpAction::SendFrame(f) => self.send_frame(f, ctx),
                    IsoTpAction::Received(req) => self.handle_request(req, false, ctx),
                    IsoTpAction::TxDone => progress = true,
                    IsoTpAction::Error(e) => {
                        let tx_failed = matches!(
                            e,
                            IsoTpError::TimeoutNBs
                                | IsoTpError::Overflow
                                | IsoTpError::WftExceeded
                                | IsoTpError::InvalidFlowStatus(_)
                        );
                        self.log(now, &format!("diag transport error: {e}"));
                        progress |= tx_failed;
                    }
                }
            }
            while let Some((due, _)) = self.outbox.front() {
                if *due > now {
                    break;
                }
                let resp = self.outbox.front().expect("front exists").1.clone();
                match self.chan.send(&resp, now) {
                    Ok(()) => {
                        self.outbox.pop_front();
                        progress = true;
                    }
                    Err(IsoTpError::Busy) => break,
                    Err(e) => {
                        self.outbox.pop_front();
                        self.log(now, &format!("diag response dropped: {e}"));
                    }
                }
            }
            if !progress {
                break;
            }
        }
        self.rearm(ctx);
    }

    fn expire(&mut self, now: u64, ctx: &mut EcuCtx) {
        if self.s3_deadline.is_some_and(|d| now >= d) {
            self.reset_state();
            self.log(now, "diag session timeout (S3): back to default session");
        }
        if self.locked_until.is_some_and(|d| now >= d) {
            self.locked_until = None;
            self.attempts = 0;
        }
        if self.retry.as_ref().is_some_and(|r| now >= r.due) {
            let r = self.retry.take().expect("checked above");
            self.process(&r.req, r.functional, r.count, ctx);
        }
    }

    fn rearm(&mut self, ctx: &mut EcuCtx) {
        let now = ctx.now().0;
        let mut next: Option<u64> = None;
        let mut take = |t: Option<u64>| {
            if let Some(t) = t.filter(|t| *t > now) {
                next = Some(next.map_or(t, |n| n.min(t)));
            }
        };
        take(self.chan.next_deadline());
        take(self.outbox.front().map(|(d, _)| *d));
        take(self.s3_deadline);
        take(self.retry.as_ref().map(|r| r.due));
        let Some(next) = next else { return };
        if self.armed.is_some_and(|a| a > now && a <= next) {
            return;
        }
        self.armed = Some(next);
        ctx.set_timer(DIAG_TIMER, next - now);
    }

    fn handle_request(&mut self, req: Vec<u8>, functional: bool, ctx: &mut EcuCtx) {
        if req.is_empty() {
            return;
        }
        let now = ctx.now().0;
        self.retry = None;
        if self.session != DEFAULT_SESSION {
            self.s3_deadline = Some(now + S3_NS);
        }
        self.process(&req, functional, 0, ctx);
    }

    fn queue(&mut self, resp: Vec<u8>, now: u64) {
        let delay = u64::from(self.cfg.p2_ms) * MS / 5;
        self.outbox.push_back((now + delay, resp));
    }

    /// Answer `req`: the script's `on_diag` first, else the built-in server.
    fn process(&mut self, req: &[u8], functional: bool, retries: u32, ctx: &mut EcuCtx) {
        let now = ctx.now().0;
        if let Some(resp) = self.inner.script_hook("on_diag", req, ctx) {
            if resp.is_empty() {
                return;
            }
            if resp.len() == 3
                && resp[0] == 0x7F
                && resp[2] == Nrc::ResponsePending.to_u8()
                && retries < MAX_PENDING_RETRIES
            {
                self.retry = Some(Retry {
                    due: now + PENDING_RETRY_NS,
                    req: req.to_vec(),
                    functional,
                    count: retries + 1,
                });
            }
            self.queue(resp, now);
            return;
        }
        match self.service(req, now, ctx) {
            Ok(Some(resp)) => self.queue(resp.encode(), now),
            Ok(None) => {}
            Err(nrc) => {
                // Functional requests are not answered with "not supported"
                // style codes (ISO 14229-1), so absent services stay silent.
                let silent = matches!(
                    nrc,
                    Nrc::ServiceNotSupported
                        | Nrc::SubFunctionNotSupported
                        | Nrc::RequestOutOfRange
                        | Nrc::SubFunctionNotSupportedInActiveSession
                        | Nrc::ServiceNotSupportedInActiveSession
                );
                if !(functional && silent) {
                    self.queue(Response::Negative { sid: req[0], nrc }.encode(), now);
                }
            }
        }
    }

    fn key_for(&mut self, algo: &KeyAlgo, seed: &[u8], ctx: &mut EcuCtx) -> Option<Vec<u8>> {
        match algo {
            KeyAlgo::XorConst(c) if c.is_empty() => Some(seed.to_vec()),
            KeyAlgo::XorConst(c) => Some(
                seed.iter()
                    .enumerate()
                    .map(|(i, b)| b ^ c[i % c.len()])
                    .collect(),
            ),
            KeyAlgo::AddConst(k) => {
                let mut key = seed.to_vec();
                let add = k.to_be_bytes();
                let mut carry = 0u16;
                for (i, byte) in key.iter_mut().rev().enumerate() {
                    let a = if i < 4 { add[3 - i] as u16 } else { 0 };
                    let sum = *byte as u16 + a + carry;
                    *byte = sum as u8;
                    carry = sum >> 8;
                }
                Some(key)
            }
            KeyAlgo::Script => self.inner.script_hook("on_security_key", seed, ctx),
        }
    }

    /// Built-in services. `Ok(None)` is a positive response suppressed by
    /// the suppress-positive-response bit.
    fn service(&mut self, req: &[u8], now: u64, ctx: &mut EcuCtx) -> Result<Option<Response>, Nrc> {
        let request =
            Request::decode(req).map_err(|_| Nrc::IncorrectMessageLengthOrInvalidFormat)?;
        let suppress = request.suppress_positive_response();
        let resp = match request {
            Request::DiagnosticSessionControl(s) => {
                let s = s & 0x7F;
                if !self.cfg.sessions_supported.contains(&s) {
                    return Err(Nrc::SubFunctionNotSupported);
                }
                if s != self.session {
                    self.lock();
                }
                self.session = s;
                self.s3_deadline = (s != DEFAULT_SESSION).then_some(now + S3_NS);
                Response::DiagnosticSessionControl {
                    session: s,
                    p2_ms: self.cfg.p2_ms,
                    p2_star_ms: self.cfg.p2_star_ms,
                }
            }
            Request::EcuReset(k) => {
                let k = k & 0x7F;
                if !(1..=3).contains(&k) {
                    return Err(Nrc::SubFunctionNotSupported);
                }
                self.reset_state();
                self.log(now, "diag ECU reset");
                Response::EcuReset(k)
            }
            Request::SecurityAccessRequestSeed(l) => {
                let sec = self.cfg.security.clone().ok_or(Nrc::ServiceNotSupported)?;
                if l & 0x7F != sec.level {
                    return Err(Nrc::SubFunctionNotSupported);
                }
                if self.locked_until.is_some_and(|d| now < d) {
                    return Err(Nrc::RequiredTimeDelayNotExpired);
                }
                let seed = if self.unlocked {
                    vec![0; sec.seed.len()]
                } else {
                    self.seed_sent = Some(sec.seed.clone());
                    sec.seed
                };
                Response::SecuritySeed {
                    level: sec.level,
                    seed,
                }
            }
            Request::SecurityAccessSendKey { level, key } => {
                let sec = self.cfg.security.clone().ok_or(Nrc::ServiceNotSupported)?;
                if (level & 0x7F) != sec.level.wrapping_add(1) {
                    return Err(Nrc::SubFunctionNotSupported);
                }
                if self.locked_until.is_some_and(|d| now < d) {
                    return Err(Nrc::RequiredTimeDelayNotExpired);
                }
                let seed = self.seed_sent.take().ok_or(Nrc::RequestSequenceError)?;
                if self.key_for(&sec.key_algo, &seed, ctx).as_deref() == Some(key.as_slice()) {
                    self.unlocked = true;
                    self.attempts = 0;
                    self.log(now, "diag security unlocked");
                    Response::SecurityKeyAccepted {
                        level: level & 0x7F,
                    }
                } else {
                    self.attempts += 1;
                    if self.attempts >= MAX_KEY_ATTEMPTS {
                        self.locked_until = Some(now + SECURITY_DELAY_NS);
                        return Err(Nrc::ExceededNumberOfAttempts);
                    }
                    return Err(Nrc::InvalidKey);
                }
            }
            Request::ReadDataByIdentifier(dids) => {
                let mut data = Vec::new();
                for (i, did) in dids.iter().enumerate() {
                    let e = self
                        .dids
                        .iter()
                        .find(|e| e.did == *did)
                        .ok_or(Nrc::RequestOutOfRange)?;
                    if i > 0 {
                        data.extend_from_slice(&did.to_be_bytes());
                    }
                    data.extend_from_slice(&e.data);
                }
                if data.len() + 3 > 4095 && !self.cfg.fd {
                    return Err(Nrc::ResponseTooLong);
                }
                Response::ReadDataByIdentifier { did: dids[0], data }
            }
            Request::WriteDataByIdentifier { did, data } => {
                let idx = self
                    .dids
                    .iter()
                    .position(|e| e.did == did && e.writable)
                    .ok_or(Nrc::RequestOutOfRange)?;
                if self.session != EXTENDED_SESSION {
                    return Err(Nrc::ServiceNotSupportedInActiveSession);
                }
                if self.cfg.security.is_some() && !self.unlocked {
                    return Err(Nrc::SecurityAccessDenied);
                }
                if data.len() != self.dids[idx].data.len() {
                    return Err(Nrc::IncorrectMessageLengthOrInvalidFormat);
                }
                self.dids[idx].data = data;
                Response::WriteDataByIdentifier { did }
            }
            Request::RoutineControl { sub, rid, .. } => {
                if rid != ERASE_RID {
                    return Err(Nrc::RequestOutOfRange);
                }
                Response::RoutineControl {
                    sub,
                    rid,
                    status: vec![0x00],
                }
            }
            Request::ReadDtcInformation { sub, mask } => {
                let sub = sub & 0x7F;
                let hits = |m: u8| {
                    self.dtcs
                        .iter()
                        .filter(move |d| d.status & m != 0)
                        .copied()
                        .collect::<Vec<_>>()
                };
                match (sub, mask) {
                    (0x01, Some(m)) => Response::DtcCount {
                        availability_mask: DTC_AVAILABILITY,
                        format: 0x01,
                        count: hits(m).len() as u16,
                    },
                    (0x02, Some(m)) => Response::DtcList {
                        sub,
                        availability_mask: DTC_AVAILABILITY,
                        dtcs: to_uds_dtcs(&hits(m)),
                    },
                    (0x0A, _) => Response::DtcList {
                        sub,
                        availability_mask: DTC_AVAILABILITY,
                        dtcs: to_uds_dtcs(&self.dtcs),
                    },
                    _ => return Err(Nrc::SubFunctionNotSupported),
                }
            }
            Request::ClearDiagnosticInformation(group) => {
                let before = self.dtcs.len();
                if group == 0xFF_FFFF {
                    self.dtcs.clear();
                } else {
                    self.dtcs.retain(|d| d.code != group);
                    if self.dtcs.len() == before {
                        return Err(Nrc::RequestOutOfRange);
                    }
                }
                Response::ClearDiagnosticInformation
            }
            Request::TesterPresent { .. } => Response::TesterPresent,
            Request::Raw(d) => {
                return Err(if service_name(d[0]).is_some() {
                    Nrc::SubFunctionNotSupported
                } else {
                    Nrc::ServiceNotSupported
                });
            }
        };
        Ok((!suppress).then_some(resp))
    }
}

fn to_uds_dtcs(d: &[DtcEntry]) -> Vec<Dtc> {
    d.iter()
        .map(|e| Dtc {
            code: e.code,
            status: e.status,
        })
        .collect()
}

/// The data of a single-frame PDU (classic or FD escape format), for
/// functional requests.
fn single_frame_data(frame: &CanFrame) -> Option<&[u8]> {
    let p = frame.payload();
    let b0 = *p.first()?;
    if b0 >> 4 != 0 {
        return None;
    }
    let n = (b0 & 0xF) as usize;
    if n == 0 && frame.dlc > 8 {
        let len = *p.get(1)? as usize;
        p.get(2..2 + len)
    } else {
        p.get(1..1 + n)
    }
}

impl Ecu for DiagEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        self.inner.on_start(ctx);
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if (DIAG_TIMER..DIAG_TIMER_END).contains(&timer) {
            self.armed = None;
            self.pump(ctx);
        } else {
            self.inner.on_timer(timer, ctx);
        }
    }

    fn on_frame(&mut self, bus: BusId, frame: &CanFrame, ctx: &mut EcuCtx) {
        self.inner.on_frame(bus, frame, ctx);
        if self.cfg.bus.is_some_and(|b| b != bus) || frame.extended != self.cfg.extended_ids {
            return;
        }
        let now = ctx.now().0;
        if frame.id == self.cfg.req_id {
            self.rx_bus = Some(bus);
            self.chan.on_frame(frame, now);
            self.pump(ctx);
        } else if Some(frame.id) == self.cfg.functional_id
            && let Some(data) = single_frame_data(frame)
        {
            self.rx_bus = Some(bus);
            self.handle_request(data.to_vec(), true, ctx);
            self.pump(ctx);
        }
    }

    fn on_command(&mut self, cmd: &EcuCommand, ctx: &mut EcuCtx) {
        self.inner.on_command(cmd, ctx);
    }

    fn script_hook(&mut self, name: &str, arg: &[u8], ctx: &mut EcuCtx) -> Option<Vec<u8>> {
        self.inner.script_hook(name, arg, ctx)
    }

    fn drain_logs(&mut self) -> Vec<String> {
        let mut logs = self.inner.drain_logs();
        logs.append(&mut self.logs);
        logs
    }
}
