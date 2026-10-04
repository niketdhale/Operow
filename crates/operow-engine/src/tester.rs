//! A virtual diagnostic tester: sends one UDS request over ISO-TP and
//! collects the response.

use std::sync::{Arc, Mutex};

use operow_core::{BusId, CanFrame, NodeId};
use operow_isotp::{IsoTpAction, IsoTpChannel, IsoTpConfig};

use crate::ecu::{Ecu, EcuCtx};

/// First [`NodeId`] value of virtual testers (`TESTER_NODE_BASE + n`).
pub const TESTER_NODE_BASE: u32 = 0xD000_0000;
const MS: u64 = 1_000_000;
/// How long the tester waits for the first response.
const P2_CLIENT_NS: u64 = 1000 * MS;
/// How long it keeps waiting after a responsePending (NRC 0x78).
const P2_STAR_CLIENT_NS: u64 = 5000 * MS;
const TIMER: u32 = 1;

/// A diagnostic request from a virtual tester.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagRequestSpec {
    pub bus: BusId,
    /// CAN id the request is sent on (physical or functional).
    pub req_id: u32,
    /// CAN id the response is expected on.
    pub resp_id: u32,
    pub extended: bool,
    pub fd: bool,
    pub payload: Vec<u8>,
    pub functional: bool,
}

/// Outcome of a [`DiagRequestSpec`].
#[derive(Debug, Clone, PartialEq)]
pub struct DiagResult {
    pub node: NodeId,
    pub req: Vec<u8>,
    pub resp: Result<Vec<u8>, String>,
    /// Virtual time from sending to the final response.
    pub elapsed_ms: f64,
}

pub(crate) type SharedResults = Arc<Mutex<Vec<DiagResult>>>;

pub(crate) struct TesterEcu {
    node: NodeId,
    bus: BusId,
    chan: IsoTpChannel,
    req: Vec<u8>,
    started: u64,
    deadline: u64,
    done: bool,
    results: SharedResults,
}

impl TesterEcu {
    pub(crate) fn new(
        node: NodeId,
        spec: DiagRequestSpec,
        results: SharedResults,
    ) -> Result<Self, String> {
        let cfg = IsoTpConfig {
            tx_id: spec.req_id,
            rx_id: spec.resp_id,
            extended_ids: spec.extended,
            fd: spec.fd,
            tx_dl: if spec.fd { 64 } else { 8 },
            ..IsoTpConfig::default()
        };
        Ok(TesterEcu {
            node,
            bus: spec.bus,
            chan: IsoTpChannel::new(cfg).map_err(|e| e.to_string())?,
            req: spec.payload,
            started: 0,
            deadline: 0,
            done: false,
            results,
        })
    }

    pub(crate) fn bus(&self) -> BusId {
        self.bus
    }

    fn finish(&mut self, resp: Result<Vec<u8>, String>, now: u64) {
        if self.done {
            return;
        }
        self.done = true;
        let result = DiagResult {
            node: self.node,
            req: std::mem::take(&mut self.req),
            resp,
            elapsed_ms: (now - self.started) as f64 / 1e6,
        };
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(result);
    }

    fn pump(&mut self, ctx: &mut EcuCtx) {
        let now = ctx.now().0;
        for act in self.chan.poll(now) {
            match act {
                IsoTpAction::SendFrame(f) => ctx.send_on(self.bus, f),
                IsoTpAction::Received(r) => {
                    let pending = r.len() == 3
                        && r[0] == 0x7F
                        && self.req.first() == Some(&r[1])
                        && r[2] == 0x78;
                    if pending {
                        self.deadline = now + P2_STAR_CLIENT_NS;
                    } else {
                        self.finish(Ok(r), now);
                    }
                }
                IsoTpAction::TxDone => {}
                IsoTpAction::Error(e) => self.finish(Err(e.to_string()), now),
            }
        }
        if !self.done && now >= self.deadline {
            self.finish(Err("timeout: no response".into()), now);
        }
        if self.done {
            return;
        }
        let next = self
            .chan
            .next_deadline()
            .map_or(self.deadline, |d| d.min(self.deadline));
        ctx.set_timer(TIMER, next.saturating_sub(now).max(1));
    }
}

impl Ecu for TesterEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        let now = ctx.now().0;
        self.started = now;
        self.deadline = now + P2_CLIENT_NS;
        if let Err(e) = self.chan.send(&self.req, now) {
            self.finish(Err(e.to_string()), now);
            return;
        }
        self.pump(ctx);
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if timer == TIMER && !self.done {
            self.pump(ctx);
        }
    }

    fn on_frame(&mut self, bus: BusId, frame: &CanFrame, ctx: &mut EcuCtx) {
        if bus == self.bus && !self.done {
            self.chan.on_frame(frame, ctx.now().0);
            self.pump(ctx);
        }
    }
}
