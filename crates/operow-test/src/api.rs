//! The context a test runs against and the test API registered on the Rhai
//! engine. Blocking calls advance the simulation in virtual time.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard};

use operow_core::{
    BusEvent, BusId, CanErrorKind, CanFrame, Direction, NodeId, Timestamp, Topology,
};
use operow_engine::{
    DiagRequestSpec, EcuCommand, GENERATOR_NODE_BASE, GeneratorId, InjectMode, InjectSpec,
    MsgControl, Simulation, TESTER_NODE_BASE,
};
use operow_project::DbcStore;
use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map, Position};

use crate::project::Project;
use crate::report::{Step, TraceRow};

/// Virtual sender of every frame a test sends.
pub(crate) const TEST_GEN: GeneratorId = GeneratorId(0x7E57);
/// Events kept for waits and trace extracts.
const RING_CAP: usize = 200_000;
/// Steps kept per case; later steps are dropped (failures are always kept).
const MAX_STEPS: usize = 5_000;
/// Rows of a trace extract.
const MAX_TRACE_ROWS: usize = 500;

/// Why a test function stopped early.
#[derive(Debug, Clone)]
pub(crate) enum Abort {
    /// An expectation or wait failed.
    Fail(String),
    /// The test is wrong or cannot run (unknown name, bad argument).
    Error(String),
    Skip(String),
    /// The runner's stop flag was set.
    Stopped,
}

fn err<T>(msg: impl Into<String>) -> Result<T, Abort> {
    Err(Abort::Error(msg.into()))
}

fn fail<T>(msg: impl Into<String>) -> Result<T, Abort> {
    Err(Abort::Fail(msg.into()))
}

/// A signal resolved to its message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SigKey {
    bus: BusId,
    id: u32,
    ext: bool,
    msg: String,
    sig: String,
}

impl SigKey {
    fn msg_key(&self) -> (BusId, u32, bool) {
        (self.bus, self.id, self.ext)
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl Op {
    fn parse(s: &str) -> Result<Op, Abort> {
        Ok(match s {
            "==" => Op::Eq,
            "!=" => Op::Ne,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            "<" => Op::Lt,
            "<=" => Op::Le,
            _ => {
                return err(format!(
                    "unknown comparison {s:?} (use ==, !=, >, >=, <, <=)"
                ));
            }
        })
    }

    fn apply(self, a: f64, b: f64) -> bool {
        match self {
            Op::Eq => close(a, b),
            Op::Ne => !close(a, b),
            Op::Gt => a > b,
            Op::Ge => a > b || close(a, b),
            Op::Lt => a < b,
            Op::Le => a < b || close(a, b),
        }
    }

    fn text(self) -> &'static str {
        match self {
            Op::Eq => "==",
            Op::Ne => "!=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
        }
    }
}

/// Equality of physical values: DBC scaling makes exact float comparison
/// unreliable.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

pub(crate) fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn num(d: &Dynamic) -> Option<f64> {
    d.as_int()
        .ok()
        .map(|i| i as f64)
        .or_else(|| d.as_float().ok())
}

fn want_num(d: &Dynamic, what: &str) -> Result<f64, Abort> {
    num(d).map_or_else(
        || err(format!("{what} must be a number, got {}", d.type_name())),
        Ok,
    )
}

fn fmt_dyn(d: &Dynamic) -> String {
    if let Some(s) = d.read_lock::<ImmutableString>() {
        return format!("{:?}", s.as_str());
    }
    if let Ok(a) = d.clone().into_array() {
        let items: Vec<String> = a.iter().map(fmt_dyn).collect();
        return format!("[{}]", items.join(", "));
    }
    if let Some(m) = d.read_lock::<Map>() {
        let items: Vec<String> = m
            .iter()
            .map(|(k, v)| format!("{k}: {}", fmt_dyn(v)))
            .collect();
        return format!("#{{{}}}", items.join(", "));
    }
    d.to_string()
}

fn dyn_eq(a: &Dynamic, b: &Dynamic) -> bool {
    if let (Ok(x), Ok(y)) = (a.as_int(), b.as_int()) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return close(x, y);
    }
    if let (Ok(x), Ok(y)) = (a.clone().into_string(), b.clone().into_string()) {
        return x == y;
    }
    if let (Ok(x), Ok(y)) = (a.as_bool(), b.as_bool()) {
        return x == y;
    }
    if let (Ok(x), Ok(y)) = (a.as_char(), b.as_char()) {
        return x == y;
    }
    if a.is_unit() && b.is_unit() {
        return true;
    }
    if let (Ok(x), Ok(y)) = (a.clone().into_array(), b.clone().into_array()) {
        return x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| dyn_eq(p, q));
    }
    if let (Some(x), Some(y)) = (a.read_lock::<Map>(), b.read_lock::<Map>()) {
        return x.len() == y.len()
            && x.iter()
                .all(|(k, v)| y.get(k).is_some_and(|w| dyn_eq(v, w)));
    }
    false
}

fn dyn_cmp(a: &Dynamic, b: &Dynamic) -> Result<Ordering, Abort> {
    if let (Ok(x), Ok(y)) = (a.as_int(), b.as_int()) {
        return Ok(x.cmp(&y));
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return x
            .partial_cmp(&y)
            .map_or_else(|| err("cannot order NaN"), Ok);
    }
    if let (Ok(x), Ok(y)) = (a.clone().into_string(), b.clone().into_string()) {
        return Ok(x.cmp(&y));
    }
    err(format!(
        "cannot order {} and {} (numbers or strings expected)",
        a.type_name(),
        b.type_name()
    ))
}

pub(crate) fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn bytes_arg(a: &Array) -> Result<Vec<u8>, Abort> {
    a.iter()
        .map(|v| match v.as_int() {
            Ok(n) if (0..=255).contains(&n) => Ok(n as u8),
            _ => err(format!(
                "byte arrays hold integers 0..255, got {}",
                fmt_dyn(v)
            )),
        })
        .collect()
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn can_id(id: i64) -> Result<u32, Abort> {
    u32::try_from(id)
        .ok()
        .filter(|i| *i <= 0x1FFF_FFFF)
        .map_or_else(|| err(format!("invalid CAN id {id}")), Ok)
}

fn ms_to_ns(v: f64) -> Result<u64, Abort> {
    if v.is_finite() && v >= 0.0 {
        Ok((v * 1e6).round() as u64)
    } else {
        err(format!("invalid duration {v} ms"))
    }
}

/// Everything a running test touches.
pub(crate) struct Ctx {
    pub sim: Simulation,
    topo: Topology,
    dbcs: DbcStore,
    bus_names: HashMap<BusId, String>,
    node_names: HashMap<NodeId, String>,
    pub now_ns: u64,
    step_ns: u64,
    deadline_ns: u64,
    timeout_ms: u64,
    stop: Arc<AtomicBool>,
    ring: VecDeque<BusEvent>,
    last_frames: HashMap<(BusId, u32), BusEvent>,
    signal_values: HashMap<(BusId, u32, bool), HashMap<String, f64>>,
    payloads: HashMap<(BusId, u32, bool), Vec<u8>>,
    pub steps: Vec<Step>,
    pub abort: Option<(Abort, u64)>,
    sink: Option<crate::runner::EventSink>,
}

pub(crate) type Shared = Arc<Mutex<Ctx>>;

pub(crate) fn lock(c: &Shared) -> MutexGuard<'_, Ctx> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ctx {
    pub(crate) fn new(
        project: &Project,
        seed: u64,
        step_ns: u64,
        timeout_ms: u64,
        stop: Arc<AtomicBool>,
        sink: Option<crate::runner::EventSink>,
    ) -> Result<Ctx, String> {
        let mut sim = Simulation::new(&project.topology).map_err(|e| e.to_string())?;
        sim.set_seed(seed);
        Ok(Ctx {
            sim,
            topo: project.topology.clone(),
            dbcs: project.dbcs.clone(),
            bus_names: project
                .topology
                .buses
                .iter()
                .map(|b| (b.id, b.name.clone()))
                .collect(),
            node_names: project
                .topology
                .nodes
                .iter()
                .map(|n| (n.id, n.name.clone()))
                .collect(),
            now_ns: 0,
            step_ns: step_ns.max(1),
            deadline_ns: timeout_ms.saturating_mul(1_000_000),
            timeout_ms,
            stop,
            ring: VecDeque::new(),
            last_frames: HashMap::new(),
            signal_values: HashMap::new(),
            payloads: HashMap::new(),
            steps: Vec::new(),
            abort: None,
            sink,
        })
    }

    /// Reset per-function state; the deadline restarts from now.
    pub(crate) fn begin_phase(&mut self) {
        self.abort = None;
        self.deadline_ns = self
            .now_ns
            .saturating_add(self.timeout_ms.saturating_mul(1_000_000));
    }

    pub(crate) fn clear_steps(&mut self) {
        self.steps.clear();
    }

    /// Remember the first abort; returns the error that unwinds the script.
    pub(crate) fn raise(&mut self, a: Abort) -> Box<EvalAltResult> {
        if self.abort.is_none() {
            self.abort = Some((a.clone(), self.now_ns));
        }
        EvalAltResult::ErrorRuntime(Dynamic::from(a), Position::NONE).into()
    }

    pub(crate) fn step(&mut self, kind: &str, text: impl Into<String>, ok: bool) {
        if self.steps.len() < MAX_STEPS || !ok {
            self.steps.push(Step {
                time_ms: ms(self.now_ns),
                kind: kind.into(),
                text: text.into(),
                ok,
            });
        }
    }

    // --- names ----------------------------------------------------------

    fn bus_id(&self, name: &str) -> Result<BusId, Abort> {
        match self.topo.buses.iter().find(|b| b.name == name) {
            Some(b) => Ok(b.id),
            None => {
                let known: Vec<&str> = self.topo.buses.iter().map(|b| b.name.as_str()).collect();
                err(format!(
                    "unknown bus {name:?} (known: {})",
                    known.join(", ")
                ))
            }
        }
    }

    fn bus_name(&self, bus: BusId) -> String {
        self.bus_names
            .get(&bus)
            .cloned()
            .unwrap_or_else(|| format!("bus{}", bus.0))
    }

    fn node_id(&self, name: &str) -> Result<NodeId, Abort> {
        match self.topo.nodes.iter().find(|n| n.name == name) {
            Some(n) => Ok(n.id),
            None => {
                let known: Vec<&str> = self.topo.nodes.iter().map(|n| n.name.as_str()).collect();
                err(format!(
                    "unknown node {name:?} (known: {})",
                    known.join(", ")
                ))
            }
        }
    }

    pub(crate) fn sender_name(&self, node: NodeId) -> String {
        if let Some(g) = GeneratorId::from_node(node) {
            return if g == TEST_GEN {
                "Test".into()
            } else {
                format!("Gen{}", node.0 - GENERATOR_NODE_BASE)
            };
        }
        if node.0 >= TESTER_NODE_BASE {
            return "Tester".into();
        }
        self.node_names
            .get(&node)
            .cloned()
            .unwrap_or_else(|| format!("node{}", node.0))
    }

    /// Extended flag for a message id: from the node's (or any node's) tx
    /// list when it is defined there, else by id range.
    fn ext_for(&self, node: Option<NodeId>, id: u32) -> bool {
        self.topo
            .nodes
            .iter()
            .filter(|n| node.is_none_or(|x| x == n.id))
            .flat_map(|n| n.tx.iter())
            .find(|m| m.frame.id == id)
            .map_or(id > 0x7FF, |m| m.frame.extended)
    }

    // --- signals --------------------------------------------------------

    fn resolve_signal(&self, name: &str) -> Result<SigKey, Abort> {
        let (bus_filter, rest) = match name.split_once("::") {
            Some((b, r)) => (Some(b), r),
            None => (None, name),
        };
        let Some((msg_name, sig_name)) = rest.split_once('.') else {
            return err(format!(
                "bad signal name {name:?}: use \"Message.Signal\" or \"Bus::Message.Signal\""
            ));
        };
        if let Some(b) = bus_filter {
            self.bus_id(b)?;
        }
        let mut buses: Vec<BusId> = self.dbcs.by_bus.keys().copied().collect();
        buses.sort();
        let mut found = Vec::new();
        for bus in buses {
            if bus_filter.is_some_and(|b| self.bus_name(bus) != b) {
                continue;
            }
            for m in &self.dbcs.by_bus[&bus].messages {
                if m.name == msg_name && m.signals.iter().any(|s| s.name == sig_name) {
                    found.push(SigKey {
                        bus,
                        id: m.id,
                        ext: m.extended,
                        msg: m.name.clone(),
                        sig: sig_name.to_string(),
                    });
                }
            }
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => err(format!(
                "unknown signal {name:?}: no loaded DBC has message {msg_name:?} with signal {sig_name:?}"
            )),
            _ => {
                let on: Vec<String> = found.iter().map(|k| self.bus_name(k.bus)).collect();
                err(format!(
                    "ambiguous signal {name:?}: defined on buses {}; write \"Bus::{rest}\"",
                    on.join(", ")
                ))
            }
        }
    }

    fn latest(&self, k: &SigKey) -> Option<f64> {
        self.signal_values.get(&k.msg_key())?.get(&k.sig).copied()
    }

    // --- time -----------------------------------------------------------

    fn ingest(&mut self, ev: BusEvent) {
        if self.ring.len() >= RING_CAP {
            self.ring.pop_front();
        }
        self.ring.push_back(ev);
        if ev.is_error() {
            return;
        }
        let (id, ext) = (ev.frame.id, ev.frame.extended);
        self.last_frames.insert((ev.bus, id), ev);
        if let Some(msg) = self
            .dbcs
            .by_bus
            .get(&ev.bus)
            .and_then(|d| d.message(id, ext))
        {
            let vals = self.signal_values.entry((ev.bus, id, ext)).or_default();
            for (n, v) in msg.decode(&ev.frame) {
                vals.insert(n, v);
            }
        }
    }

    /// Run the simulation up to `target_ns` in steps. `pred` sees every
    /// event after it was ingested; the first `Some` ends the wait at the
    /// end of the current step.
    fn advance<T>(
        &mut self,
        target_ns: u64,
        mut pred: impl FnMut(&Ctx, &BusEvent) -> Option<T>,
    ) -> Result<Option<T>, Abort> {
        let mut buf = Vec::new();
        while self.now_ns < target_ns {
            if self.stop.load(AtomicOrdering::Relaxed) {
                return Err(Abort::Stopped);
            }
            let next = (self.now_ns + self.step_ns).min(target_ns);
            if next > self.deadline_ns {
                return fail(format!(
                    "test exceeded the per-test timeout of {} ms (virtual)",
                    self.timeout_ms
                ));
            }
            buf.clear();
            self.sim.run_until(Timestamp(next), &mut buf);
            self.now_ns = next;
            let mut found = None;
            if let Some(sink) = &self.sink
                && !buf.is_empty()
            {
                sink(&buf);
            }
            for ev in buf.drain(..) {
                self.ingest(ev);
                if found.is_none() {
                    found = pred(self, &ev);
                }
            }
            self.sim.drain_logs();
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    fn wait(&mut self, ms_: f64) -> Result<(), Abort> {
        let target = self.now_ns + ms_to_ns(ms_)?;
        self.advance(target, |_, _| None::<()>)?;
        self.step("wait", format!("wait {} ms", fmt_num(ms_)), true);
        Ok(())
    }

    fn frame_map(&self, ev: &BusEvent) -> Map {
        let mut m = Map::new();
        m.insert("id".into(), Dynamic::from(ev.frame.id as i64));
        m.insert("extended".into(), Dynamic::from(ev.frame.extended));
        let data: Array = ev
            .frame
            .payload()
            .iter()
            .map(|&b| Dynamic::from(b as i64))
            .collect();
        m.insert("data".into(), Dynamic::from(data));
        m.insert("dlc".into(), Dynamic::from(ev.frame.dlc as i64));
        m.insert("bus".into(), Dynamic::from(self.bus_name(ev.bus)));
        m.insert("sender".into(), Dynamic::from(self.sender_name(ev.sender)));
        m.insert("time_ms".into(), Dynamic::from(ms(ev.time.0)));
        m
    }

    fn wait_message(&mut self, bus: Option<&str>, id: i64, timeout_ms: i64) -> Result<Map, Abort> {
        let bus_id = bus.map(|b| self.bus_id(b)).transpose()?;
        let id = can_id(id)?;
        let target = self.now_ns + ms_to_ns(timeout_ms as f64)?;
        let hit = self.advance(target, |_, ev| {
            (!ev.is_error() && ev.frame.id == id && bus_id.is_none_or(|b| b == ev.bus))
                .then_some(*ev)
        })?;
        let on = bus.map(|b| format!(" on {b}")).unwrap_or_default();
        match hit {
            Some(ev) => {
                let m = self.frame_map(&ev);
                self.step(
                    "wait",
                    format!(
                        "got message 0x{id:X}{on} at {:.3} ms [{}]",
                        ms(ev.time.0),
                        hex(ev.frame.payload())
                    ),
                    true,
                );
                Ok(m)
            }
            None => {
                let msg = format!("timeout after {timeout_ms} ms waiting for message 0x{id:X}{on}");
                self.step("wait", msg.clone(), false);
                fail(msg)
            }
        }
    }

    fn wait_signal(
        &mut self,
        sig: &str,
        op: &str,
        value: &Dynamic,
        timeout_ms: i64,
    ) -> Result<f64, Abort> {
        let k = self.resolve_signal(sig)?;
        let op = Op::parse(op)?;
        let want = want_num(value, "value")?;
        if let Some(v) = self.latest(&k).filter(|v| op.apply(*v, want)) {
            self.step(
                "wait",
                format!("signal {sig} already {} (= {})", cond(op, want), fmt_num(v)),
                true,
            );
            return Ok(v);
        }
        let target = self.now_ns + ms_to_ns(timeout_ms as f64)?;
        let hit = self.advance(target, |c, ev| {
            if ev.is_error() || (ev.bus, ev.frame.id, ev.frame.extended) != k.msg_key() {
                return None;
            }
            c.latest(&k).filter(|v| op.apply(*v, want))
        })?;
        match hit {
            Some(v) => {
                self.step(
                    "wait",
                    format!("signal {sig} became {} (= {})", cond(op, want), fmt_num(v)),
                    true,
                );
                Ok(v)
            }
            None => {
                let last = self.latest(&k).map_or("never seen".to_string(), |v| {
                    format!("last value {}", fmt_num(v))
                });
                let msg = format!(
                    "timeout after {timeout_ms} ms waiting for signal {sig} {} ({last})",
                    cond(op, want)
                );
                self.step("wait", msg.clone(), false);
                fail(msg)
            }
        }
    }

    // --- expectations ---------------------------------------------------

    fn check(&mut self, kind: &str, ok: bool, pass: String, failure: String) -> Result<(), Abort> {
        if ok {
            self.step(kind, pass, true);
            Ok(())
        } else {
            self.step(kind, failure.clone(), false);
            fail(failure)
        }
    }

    fn expect_cmp(
        &mut self,
        kind: &str,
        sym: &str,
        a: &Dynamic,
        b: &Dynamic,
        want: impl Fn(Ordering) -> bool,
    ) -> Result<(), Abort> {
        let ord = dyn_cmp(a, b)?;
        let text = format!("{} {sym} {}", fmt_dyn(a), fmt_dyn(b));
        self.check(
            kind,
            want(ord),
            text.clone(),
            format!("{kind} failed: {text} is false"),
        )
    }

    fn expect_signal(&mut self, sig: &str, op: &str, value: &Dynamic) -> Result<(), Abort> {
        let k = self.resolve_signal(sig)?;
        let op = Op::parse(op)?;
        let want = want_num(value, "value")?;
        match self.latest(&k) {
            None => {
                let msg = format!(
                    "signal {sig} has not been observed (no {} frame seen yet)",
                    k.msg
                );
                self.step("expect_signal", msg.clone(), false);
                fail(msg)
            }
            Some(v) => self.check(
                "expect_signal",
                op.apply(v, want),
                format!("{sig} = {} ({})", fmt_num(v), cond(op, want)),
                format!(
                    "signal {sig} is {}, expected {}",
                    fmt_num(v),
                    cond(op, want)
                ),
            ),
        }
    }

    fn expect_no_message(
        &mut self,
        bus: Option<&str>,
        id: i64,
        window_ms: i64,
    ) -> Result<(), Abort> {
        let bus_id = bus.map(|b| self.bus_id(b)).transpose()?;
        let id = can_id(id)?;
        let target = self.now_ns + ms_to_ns(window_ms as f64)?;
        let hit = self.advance(target, |_, ev| {
            (!ev.is_error() && ev.frame.id == id && bus_id.is_none_or(|b| b == ev.bus))
                .then_some(*ev)
        })?;
        let on = bus.map(|b| format!(" on {b}")).unwrap_or_default();
        match hit {
            None => {
                self.step(
                    "expect_no_message",
                    format!("no message 0x{id:X}{on} for {window_ms} ms"),
                    true,
                );
                Ok(())
            }
            Some(ev) => {
                let msg = format!(
                    "unexpected message 0x{id:X}{on} at {:.3} ms (expected none for {window_ms} ms)",
                    ms(ev.time.0)
                );
                self.step("expect_no_message", msg.clone(), false);
                fail(msg)
            }
        }
    }

    fn expect_cycle_time(
        &mut self,
        bus: Option<&str>,
        id: i64,
        period_ms: f64,
        tol_pct: f64,
        window_ms: i64,
    ) -> Result<f64, Abort> {
        let bus_id = bus.map(|b| self.bus_id(b)).transpose()?;
        let id = can_id(id)?;
        if period_ms <= 0.0 {
            return err("period_ms must be positive");
        }
        let target = self.now_ns + ms_to_ns(window_ms as f64)?;
        let mut times: Vec<u64> = Vec::new();
        let mut first_bus: Option<BusId> = bus_id;
        self.advance(target, |_, ev| {
            if !ev.is_error() && ev.frame.id == id && first_bus.is_none_or(|b| b == ev.bus) {
                first_bus = Some(ev.bus);
                times.push(ev.time.0);
            }
            None::<()>
        })?;
        let on = bus.map(|b| format!(" on {b}")).unwrap_or_default();
        if times.len() < 2 {
            let msg = format!(
                "cycle time of 0x{id:X}{on}: {} frame(s) in {window_ms} ms, need at least 2",
                times.len()
            );
            self.step("expect_cycle_time", msg.clone(), false);
            return fail(msg);
        }
        let mean = ms(times[times.len() - 1] - times[0]) / (times.len() - 1) as f64;
        let dev = (mean - period_ms).abs() / period_ms * 100.0;
        let text = format!(
            "cycle time of 0x{id:X}{on}: mean {mean:.3} ms over {} frames, expected {} ms +-{}%",
            times.len(),
            fmt_num(period_ms),
            fmt_num(tol_pct)
        );
        self.check(
            "expect_cycle_time",
            dev <= tol_pct,
            text.clone(),
            format!("{text} (deviation {dev:.1}%)"),
        )?;
        Ok(mean)
    }

    // --- stimulus -------------------------------------------------------

    fn make_frame(&self, bus: BusId, id: u32, ext: bool, data: &[u8]) -> Result<CanFrame, Abort> {
        let frame = if data.len() > 8 {
            let brs = self.topo.buses.iter().any(|b| b.id == bus && b.fd_enabled);
            CanFrame::new_fd(id, ext, brs, data)
        } else {
            CanFrame::new(id, ext, data)
        };
        frame.map_err(|e| Abort::Error(format!("invalid frame 0x{id:X}: {e}")))
    }

    fn send(&mut self, bus: &str, id: i64, data: &Array) -> Result<(), Abort> {
        let bus_id = self.bus_id(bus)?;
        let id = can_id(id)?;
        let data = bytes_arg(data)?;
        let frame = self.make_frame(bus_id, id, id > 0x7FF, &data)?;
        self.sim.gen_send(TEST_GEN, Some(bus_id), frame);
        self.step(
            "send",
            format!("send {bus} 0x{id:X} [{}]", hex(&data)),
            true,
        );
        Ok(())
    }

    fn set_signal(&mut self, sig: &str, value: &Dynamic) -> Result<(), Abort> {
        let k = self.resolve_signal(sig)?;
        let value = want_num(value, "value")?;
        let Some(db) = self.dbcs.by_bus.get(&k.bus).cloned() else {
            return err(format!("no DBC for the bus of {sig}"));
        };
        let Some(msg) = db.message(k.id, k.ext) else {
            return err(format!("message of {sig} not found"));
        };
        let key = k.msg_key();
        let len = msg.dlc as usize;
        let mut data = match self.payloads.get(&key) {
            Some(p) => p.clone(),
            None => match self.last_frames.get(&(k.bus, k.id)) {
                Some(ev) => ev.frame.payload().to_vec(),
                None => {
                    let mut d = vec![0u8; len];
                    for s in &msg.signals {
                        if let Some(raw) = s.initial_raw {
                            s.encode_raw(&mut d, raw);
                        }
                    }
                    d
                }
            },
        };
        data.resize(len, 0);
        let Some(def) = msg.signals.iter().find(|s| s.name == k.sig) else {
            return err(format!("signal {sig} not found"));
        };
        if let Some(operow_dbc::Mux::Multiplexed(n)) = def.multiplexer
            && let Some(sel) = msg
                .signals
                .iter()
                .find(|s| s.multiplexer == Some(operow_dbc::Mux::Multiplexor))
        {
            sel.encode_raw(&mut data, n);
        }
        def.encode(&mut data, value);
        let frame = self.make_frame(k.bus, k.id, k.ext, &data)?;
        self.payloads.insert(key, data.clone());
        self.sim.gen_send(TEST_GEN, Some(k.bus), frame);
        self.step(
            "send",
            format!(
                "set {sig} = {} -> {} 0x{:X} [{}]",
                fmt_num(value),
                self.bus_name(k.bus),
                k.id,
                hex(&data)
            ),
            true,
        );
        Ok(())
    }

    fn msg_index(&self, node: NodeId, msg: &Dynamic) -> Result<usize, Abort> {
        let cfg = self.topo.nodes.iter().find(|n| n.id == node);
        let tx = cfg.map_or(0, |n| n.tx.len());
        if let Ok(i) = msg.as_int() {
            return match usize::try_from(i) {
                Ok(i) if i < tx => Ok(i),
                _ => err(format!(
                    "message index {i} out of range (node has {tx} messages)"
                )),
            };
        }
        if let Ok(name) = msg.clone().into_string() {
            return cfg
                .and_then(|n| n.tx.iter().position(|m| m.name == name))
                .map_or_else(|| err(format!("node has no message named {name:?}")), Ok);
        }
        err("message must be an index or a name")
    }

    fn trigger(&mut self, node: &str, msg: &Dynamic) -> Result<(), Abort> {
        let n = self.node_id(node)?;
        let i = self.msg_index(n, msg)?;
        self.sim.command(n, EcuCommand::Trigger { msg: i });
        self.step(
            "send",
            format!("trigger {node} message {}", fmt_dyn(msg)),
            true,
        );
        Ok(())
    }

    fn set_payload(&mut self, node: &str, msg: &Dynamic, data: &Array) -> Result<(), Abort> {
        let n = self.node_id(node)?;
        let i = self.msg_index(n, msg)?;
        let data = bytes_arg(data)?;
        let text = format!(
            "set payload {node} message {} = [{}]",
            fmt_dyn(msg),
            hex(&data)
        );
        self.sim.command(n, EcuCommand::SetPayload { msg: i, data });
        self.step("send", text, true);
        Ok(())
    }

    // --- faults ---------------------------------------------------------

    fn inject(&mut self, spec: &Map) -> Result<(), Abort> {
        let get = |k: &str| spec.get(k);
        let Some(bus) = get("bus").and_then(|v| v.clone().into_string().ok()) else {
            return err("inject_errors needs a `bus` name");
        };
        let bus_id = self.bus_id(&bus)?;
        let node = match get("node") {
            Some(v) => {
                let name = v
                    .clone()
                    .into_string()
                    .map_err(|_| Abort::Error("`node` must be a name".into()))?;
                Some(self.node_id(&name)?)
            }
            None => None,
        };
        let id = match get("id") {
            Some(v) => {
                let i = can_id(
                    v.as_int()
                        .map_err(|_| Abort::Error("`id` must be an integer".into()))?,
                )?;
                Some((i, self.ext_for(node, i)))
            }
            None => None,
        };
        let kind_name = get("kind")
            .and_then(|v| v.clone().into_string().ok())
            .unwrap_or_else(|| "crc".into());
        let kind = match kind_name.to_ascii_lowercase().as_str() {
            "bit" => CanErrorKind::Bit,
            "stuff" => CanErrorKind::Stuff,
            "crc" => CanErrorKind::Crc,
            "form" => CanErrorKind::Form,
            "ack" => CanErrorKind::Ack,
            other => {
                return err(format!(
                    "unknown error kind {other:?} (bit, stuff, crc, form, ack)"
                ));
            }
        };
        let int = |k: &str| -> Result<Option<i64>, Abort> {
            get(k).map_or(Ok(None), |v| {
                v.as_int()
                    .map(Some)
                    .map_err(|_| Abort::Error(format!("`{k}` must be an integer")))
            })
        };
        let mode = if let Some(n) = int("count")? {
            InjectMode::Count(n.max(0) as u32)
        } else if let Some(n) = int("every")? {
            InjectMode::EveryNth(n.max(0) as u32)
        } else if let Some(p) = get("probability") {
            let p = want_num(p, "probability")?;
            if !(0.0..=1.0).contains(&p) {
                return err("probability must be between 0 and 1");
            }
            InjectMode::Probability(p)
        } else {
            InjectMode::Count(1)
        };
        let remaining = int("limit")?.map(|n| n.max(0) as u32);
        self.sim.inject_errors(InjectSpec {
            bus: bus_id,
            node,
            id,
            kind,
            mode,
            remaining,
        });
        self.step(
            "fault",
            format!(
                "inject {} errors on {bus}{}{} ({mode:?})",
                kind.label(),
                node.map_or(String::new(), |n| format!(" from {}", self.sender_name(n))),
                id.map_or(String::new(), |(i, _)| format!(" id 0x{i:X}"))
            ),
            true,
        );
        Ok(())
    }

    fn node_offline(&mut self, name: &str, online: bool) -> Result<(), Abort> {
        let n = self.node_id(name)?;
        self.sim.set_node_online(n, None, online);
        self.step(
            "fault",
            format!("node {name} {}", if online { "online" } else { "offline" }),
            true,
        );
        Ok(())
    }

    fn force_bus_off(&mut self, node: &str, bus: &str) -> Result<(), Abort> {
        let (n, b) = (self.node_id(node)?, self.bus_id(bus)?);
        self.sim.force_bus_off(n, b);
        self.step("fault", format!("force {node} bus-off on {bus}"), true);
        Ok(())
    }

    fn recover_bus_off(&mut self, node: &str, bus: &str) -> Result<(), Abort> {
        let (n, b) = (self.node_id(node)?, self.bus_id(bus)?);
        self.sim.recover_bus_off(n, b);
        self.step(
            "fault",
            format!("recover {node} from bus-off on {bus}"),
            true,
        );
        Ok(())
    }

    fn node_state(&mut self, node: &str, bus: &str) -> Result<(String, u16, u16), Abort> {
        let (n, b) = (self.node_id(node)?, self.bus_id(bus)?);
        let (state, tec, rec) = self.sim.node_state(n, b);
        Ok((format!("{state:?}"), tec, rec))
    }

    fn msg_control(&mut self, node: &str, id: i64, map: &Map) -> Result<(), Abort> {
        let n = self.node_id(node)?;
        let id = can_id(id)?;
        let ext = self.ext_for(Some(n), id);
        let f = |k: &str| -> Result<f32, Abort> {
            map.get(k)
                .map_or(Ok(0.0), |v| want_num(v, k).map(|x| x as f32))
        };
        let paused = map
            .get("paused")
            .and_then(|v| v.as_bool().ok())
            .unwrap_or(false);
        let ctl = MsgControl {
            paused,
            drop_pct: f("drop_pct")?,
            delay_ms: f("delay_ms")?,
            jitter_ms: f("jitter_ms")?,
        };
        self.sim.set_msg_control(n, (id, ext), ctl);
        self.step(
            "fault",
            format!("msg_control {node} 0x{id:X}: {ctl:?}"),
            true,
        );
        Ok(())
    }

    // --- diagnostics ----------------------------------------------------

    fn uds(&mut self, node: &str, req: &Array) -> Result<Vec<u8>, Abort> {
        let n = self.node_id(node)?;
        let payload = bytes_arg(req)?;
        let cfg = self.topo.nodes.iter().find(|c| c.id == n);
        let Some(diag) = cfg.and_then(|c| c.diag.as_ref()) else {
            return err(format!("node {node:?} has no diagnostics configuration"));
        };
        let bus = diag
            .bus
            .or_else(|| self.topo.links.iter().find(|l| l.node == n).map(|l| l.bus));
        let Some(bus) = bus else {
            return err(format!("node {node:?} is not linked to a bus"));
        };
        let spec = DiagRequestSpec {
            bus,
            req_id: diag.req_id,
            resp_id: diag.resp_id,
            extended: diag.extended_ids,
            fd: diag.fd,
            payload: payload.clone(),
            functional: false,
        };
        let tester = self.sim.diag_request(spec).map_err(Abort::Error)?;
        let result = loop {
            let target = self.now_ns + self.step_ns;
            self.advance(target, |_, _| None::<()>)?;
            if let Some(r) = self
                .sim
                .take_diag_results()
                .into_iter()
                .find(|r| r.node == tester)
            {
                break r;
            }
        };
        match result.resp {
            Ok(resp) => {
                self.step(
                    "uds",
                    format!(
                        "{node}: {} -> {} ({:.1} ms)",
                        hex(&payload),
                        hex(&resp),
                        result.elapsed_ms
                    ),
                    true,
                );
                Ok(resp)
            }
            Err(e) => {
                let msg = format!("UDS request {} to {node} failed: {e}", hex(&payload));
                self.step("uds", msg.clone(), false);
                fail(msg)
            }
        }
    }

    fn uds_expect_nrc(&mut self, node: &str, req: &Array, nrc: i64) -> Result<(), Abort> {
        let resp = self.uds(node, req)?;
        let ok = resp.len() == 3 && resp[0] == 0x7F && i64::from(resp[2]) == nrc;
        let sid = bytes_arg(req)?.first().copied().unwrap_or(0);
        self.check(
            "expect_nrc",
            ok,
            format!("{node}: service 0x{sid:02X} rejected with NRC 0x{nrc:02X}"),
            format!(
                "expected NRC 0x{nrc:02X} from {node} for service 0x{sid:02X}, got [{}]",
                hex(&resp)
            ),
        )
    }

    // --- trace ----------------------------------------------------------

    pub(crate) fn trace_extract(&self, window_ms: u64, until_ns: u64) -> Vec<TraceRow> {
        let from = until_ns.saturating_sub(window_ms.saturating_mul(1_000_000));
        let rows: Vec<TraceRow> = self
            .ring
            .iter()
            .filter(|e| e.time.0 >= from && e.time.0 <= until_ns)
            .map(|e| TraceRow {
                time_ms: ms(e.time.0),
                bus: self.bus_name(e.bus),
                dir: match e.dir {
                    Direction::Tx => "Tx",
                    Direction::Rx => "Rx",
                }
                .into(),
                id: e.frame.id,
                ext: e.frame.extended,
                dlc: e.frame.dlc,
                data: e.frame.payload().to_vec(),
                sender: self.sender_name(e.sender),
                error: e.error_kind().map(|k| k.label().to_string()),
            })
            .collect();
        let skip = rows.len().saturating_sub(MAX_TRACE_ROWS);
        rows.into_iter().skip(skip).collect()
    }
}

fn cond(op: Op, want: f64) -> String {
    format!("{} {}", op.text(), fmt_num(want))
}

fn to_rhai<T>(c: &Shared, r: Result<T, Abort>) -> Result<T, Box<EvalAltResult>> {
    r.map_err(|a| lock(c).raise(a))
}

/// Register a test API function: `reg!(engine, shared, "name", |ctx, a: T, ...| body)`.
macro_rules! reg {
    ($engine:expr, $shared:expr, $name:expr, |$c:ident $(, $a:ident : $t:ty)*| $body:expr) => {{
        let s = $shared.clone();
        $engine.register_fn($name, move |$($a: $t),*| {
            #[allow(clippy::redundant_closure_call, unused_braces)]
            let r: Result<_, Abort> = (|| { let mut g = lock(&s); let $c = &mut *g; $body })();
            to_rhai(&s, r)
        });
    }};
}

/// Register the whole test API on `engine`, bound to `shared`.
pub(crate) fn register(engine: &mut Engine, shared: &Shared) {
    // Output.
    {
        let s = shared.clone();
        engine.on_print(move |t| lock(&s).step("log", t.to_string(), true));
        let s = shared.clone();
        engine.on_debug(move |t, _, _| lock(&s).step("log", t.to_string(), true));
    }
    reg!(engine, shared, "log", |c, m: ImmutableString| {
        c.step("log", m.to_string(), true);
        Ok(())
    });

    // Timing.
    reg!(engine, shared, "wait", |c, t: i64| c.wait(t as f64));
    reg!(engine, shared, "wait", |c, t: f64| c.wait(t));
    reg!(engine, shared, "now_ms", |c| Ok::<i64, Abort>(
        (c.now_ns / 1_000_000) as i64
    ));

    // Waits.
    reg!(engine, shared, "wait_for_message", |c, id: i64, t: i64| c
        .wait_message(None, id, t));
    reg!(
        engine,
        shared,
        "wait_for_message_on",
        |c, bus: ImmutableString, id: i64, t: i64| { c.wait_message(Some(&bus), id, t) }
    );
    reg!(
        engine,
        shared,
        "wait_for_signal",
        |c, sig: ImmutableString, op: ImmutableString, v: Dynamic, t: i64| {
            c.wait_signal(&sig, &op, &v, t)
        }
    );

    // Expectations.
    reg!(engine, shared, "expect_eq", |c, a: Dynamic, b: Dynamic| {
        let eq = dyn_eq(&a, &b);
        c.check(
            "expect_eq",
            eq,
            format!("{} == {}", fmt_dyn(&a), fmt_dyn(&b)),
            format!("expect_eq failed: {} != {}", fmt_dyn(&a), fmt_dyn(&b)),
        )
    });
    reg!(engine, shared, "expect_ne", |c, a: Dynamic, b: Dynamic| {
        let eq = dyn_eq(&a, &b);
        c.check(
            "expect_ne",
            !eq,
            format!("{} != {}", fmt_dyn(&a), fmt_dyn(&b)),
            format!("expect_ne failed: both are {}", fmt_dyn(&a)),
        )
    });
    reg!(engine, shared, "expect_lt", |c, a: Dynamic, b: Dynamic| {
        c.expect_cmp("expect_lt", "<", &a, &b, |o| o == Ordering::Less)
    });
    reg!(engine, shared, "expect_gt", |c, a: Dynamic, b: Dynamic| {
        c.expect_cmp("expect_gt", ">", &a, &b, |o| o == Ordering::Greater)
    });
    reg!(
        engine,
        shared,
        "expect_near",
        |c, a: Dynamic, b: Dynamic, tol: Dynamic| {
            let (x, y, t) = (
                want_num(&a, "a")?,
                want_num(&b, "b")?,
                want_num(&tol, "tolerance")?,
            );
            c.check(
                "expect_near",
                (x - y).abs() <= t,
                format!("{} ~ {} (+-{})", fmt_num(x), fmt_num(y), fmt_num(t)),
                format!(
                    "expect_near failed: {} is not within {} of {}",
                    fmt_num(x),
                    fmt_num(t),
                    fmt_num(y)
                ),
            )
        }
    );
    reg!(engine, shared, "expect_true", |c, cnd: bool| {
        c.check(
            "expect_true",
            cnd,
            "condition is true".into(),
            "expect_true failed".into(),
        )
    });
    reg!(
        engine,
        shared,
        "expect_true",
        |c, cnd: bool, m: ImmutableString| {
            c.check(
                "expect_true",
                cnd,
                m.to_string(),
                format!("expect_true failed: {m}"),
            )
        }
    );
    reg!(
        engine,
        shared,
        "expect_signal",
        |c, sig: ImmutableString, op: ImmutableString, v: Dynamic| {
            c.expect_signal(&sig, &op, &v)
        }
    );
    reg!(engine, shared, "expect_no_message", |c, id: i64, t: i64| c
        .expect_no_message(None, id, t));
    reg!(
        engine,
        shared,
        "expect_no_message_on",
        |c, bus: ImmutableString, id: i64, t: i64| { c.expect_no_message(Some(&bus), id, t) }
    );
    reg!(
        engine,
        shared,
        "expect_cycle_time",
        |c, id: i64, p: Dynamic, tol: Dynamic, w: i64| {
            c.expect_cycle_time(
                None,
                id,
                want_num(&p, "period_ms")?,
                want_num(&tol, "tol_pct")?,
                w,
            )
        }
    );
    reg!(
        engine,
        shared,
        "expect_cycle_time_on",
        |c, bus: ImmutableString, id: i64, p: Dynamic, tol: Dynamic, w: i64| {
            c.expect_cycle_time(
                Some(&bus),
                id,
                want_num(&p, "period_ms")?,
                want_num(&tol, "tol_pct")?,
                w,
            )
        }
    );
    reg!(engine, shared, "fail", |c, m: ImmutableString| {
        c.step("fail", m.to_string(), false);
        fail::<()>(m.to_string())
    });
    reg!(engine, shared, "skip", |c, m: ImmutableString| {
        c.step("skip", m.to_string(), true);
        Err::<(), Abort>(Abort::Skip(m.to_string()))
    });

    // Stimulus.
    reg!(
        engine,
        shared,
        "send",
        |c, bus: ImmutableString, id: i64, d: Array| c.send(&bus, id, &d)
    );
    reg!(
        engine,
        shared,
        "set_signal",
        |c, sig: ImmutableString, v: Dynamic| c.set_signal(&sig, &v)
    );
    reg!(
        engine,
        shared,
        "trigger",
        |c, node: ImmutableString, m: Dynamic| c.trigger(&node, &m)
    );
    reg!(
        engine,
        shared,
        "set_payload",
        |c, node: ImmutableString, m: Dynamic, d: Array| { c.set_payload(&node, &m, &d) }
    );

    // Faults.
    reg!(engine, shared, "inject_errors", |c, m: Map| c.inject(&m));
    reg!(engine, shared, "node_offline", |c, n: ImmutableString| c
        .node_offline(&n, false));
    reg!(engine, shared, "node_online", |c, n: ImmutableString| c
        .node_offline(&n, true));
    reg!(
        engine,
        shared,
        "force_bus_off",
        |c, n: ImmutableString, b: ImmutableString| c.force_bus_off(&n, &b)
    );
    reg!(
        engine,
        shared,
        "recover_bus_off",
        |c, n: ImmutableString, b: ImmutableString| c.recover_bus_off(&n, &b)
    );
    reg!(
        engine,
        shared,
        "node_state",
        |c, n: ImmutableString, b: ImmutableString| { c.node_state(&n, &b).map(|(s, _, _)| s) }
    );
    reg!(
        engine,
        shared,
        "node_tec",
        |c, n: ImmutableString, b: ImmutableString| {
            c.node_state(&n, &b).map(|(_, t, _)| t as i64)
        }
    );
    reg!(
        engine,
        shared,
        "node_rec",
        |c, n: ImmutableString, b: ImmutableString| {
            c.node_state(&n, &b).map(|(_, _, r)| r as i64)
        }
    );
    reg!(
        engine,
        shared,
        "msg_control",
        |c, n: ImmutableString, id: i64, m: Map| c.msg_control(&n, id, &m)
    );

    // Diagnostics.
    reg!(engine, shared, "uds", |c, n: ImmutableString, d: Array| {
        c.uds(&n, &d).map(|r| {
            r.into_iter()
                .map(|b| Dynamic::from(i64::from(b)))
                .collect::<Array>()
        })
    });
    reg!(
        engine,
        shared,
        "uds_expect_nrc",
        |c, n: ImmutableString, d: Array, nrc: i64| { c.uds_expect_nrc(&n, &d, nrc) }
    );

    // Data.
    reg!(engine, shared, "signal", |c, sig: ImmutableString| {
        let k = c.resolve_signal(&sig)?;
        Ok::<Dynamic, Abort>(c.latest(&k).map_or(Dynamic::UNIT, Dynamic::from))
    });
    reg!(
        engine,
        shared,
        "last_frame",
        |c, bus: ImmutableString, id: i64| {
            let b = c.bus_id(&bus)?;
            let id = can_id(id)?;
            Ok::<Dynamic, Abort>(
                c.last_frames
                    .get(&(b, id))
                    .map_or(Dynamic::UNIT, |ev| Dynamic::from(c.frame_map(ev))),
            )
        }
    );
}
