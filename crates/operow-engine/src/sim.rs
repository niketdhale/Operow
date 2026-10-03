use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use operow_core::{
    BusEvent, BusEventKind, BusId, CanBusConfig, CanErrorKind, CanFrame, Direction, NodeErrorState,
    NodeId, NodeKind, Timestamp, Topology, TopologyError,
};

use crate::ecu::{Ecu, EcuCommand, EcuCtx, FrameMeta, PeriodicEcu};
use crate::gateway::GatewayEcu;
use crate::replay::ReplayEcu;
use crate::script::ScriptEcu;
use crate::timing::frame_duration_ns_any;

/// Maximum number of gateway hops a frame may take; forwards beyond it are
/// dropped and counted in [`BusStats::routing_drops`].
pub const MAX_HOPS: u8 = 8;

/// First [`NodeId`] value reserved for interactive generators; topology
/// nodes must stay below it.
pub const GENERATOR_NODE_BASE: u32 = 0xF000_0000;

/// Identifier of an interactive generator (a virtual sender that is linked
/// to every bus and runs no ECU behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GeneratorId(pub u32);

impl GeneratorId {
    /// The virtual sender id shown in [`BusEvent::sender`] and `origin`.
    pub fn node(self) -> NodeId {
        NodeId(GENERATOR_NODE_BASE.saturating_add(self.0))
    }

    /// The generator behind `node`, if it is a virtual generator id.
    pub fn from_node(node: NodeId) -> Option<GeneratorId> {
        node.0.checked_sub(GENERATOR_NODE_BASE).map(GeneratorId)
    }
}

// --- CAN error model -------------------------------------------------------
//
// Fault confinement follows ISO 11898-1 in a simplified form, tracked per
// (node, bus):
//
// - A transmitter whose frame is corrupted adds 8 to its TEC. Exception: an
//   ACK error seen by an error-passive transmitter leaves the TEC alone, so a
//   lone node on a `simulate_ack` bus ends up error passive but never bus-off.
// - Every receiver that is online adds 1 to its REC when it sees an error
//   frame. The "+8 when a receiver detects a dominant bit after its own error
//   flag" rule is not modelled.
// - A successful transmission lowers the TEC by 1 (minimum 0). A successful
//   reception lowers the REC by 1 (minimum 0), or sets it to 120 when it was
//   above 127.
// - Error passive when TEC > 127 or REC > 127 (back to error active once both
//   are <= 127); bus-off when TEC > 255.
// - A bus-off node drops everything it would send, receives nothing and, after
//   128 * 11 = 1408 bit times at the nominal bitrate, comes back error active
//   with both counters at 0 (the "bus must be idle" condition is not checked).
// - A frame hit by an error occupies the bus until the error is detected, then
//   for an error frame of 20 bit times (6 flag + up to 6 superposed flag + 8
//   delimiter) when the transmitter is error active or 14 (6 + 8, no
//   superposition) when it is error passive, then a 3-bit intermission. The
//   frame is queued again and re-arbitrates, as real CAN does.
// - An error-passive node waits an extra 8 bit times ("suspend transmission")
//   after each of its own transmissions, failed or successful, before it may
//   start the next one.

/// Counter limit above which a node becomes error passive.
const ERROR_PASSIVE_LIMIT: u16 = 127;
/// Counter limit above which a transmitter goes bus-off.
const BUS_OFF_LIMIT: u16 = 255;
/// TEC increase for a transmit error.
const TEC_ERROR_STEP: u16 = 8;
/// REC value after a successful reception while it was above 127.
const REC_RECOVERED: u16 = 120;
/// Bit times a node stays bus-off (128 occurrences of 11 recessive bits).
const BUS_OFF_RECOVERY_BITS: u64 = 128 * 11;
/// Bit times of an error frame sent by an error-active node.
const ACTIVE_ERROR_FRAME_BITS: u64 = 6 + 6 + 8;
/// Bit times of an error frame sent by an error-passive node.
const PASSIVE_ERROR_FRAME_BITS: u64 = 6 + 8;
/// Bit times of intermission after an error frame.
const INTERMISSION_BITS: u64 = 3;
/// Extra bit times an error-passive node waits after its own transmission.
const SUSPEND_BITS: u64 = 8;
/// Bit times between the end of a frame's ACK delimiter region and its end
/// (EOF 7 + IFS 3); ACK and CRC errors are flagged that long before the end.
const FRAME_TAIL_BITS: u64 = 10;
/// Seed of the injection random generator unless [`Simulation::set_seed`] is
/// called.
const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Small deterministic xorshift64* generator for probabilistic injection.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(if seed == 0 { DEFAULT_SEED } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How often matching transmissions are corrupted by an [`InjectSpec`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InjectMode {
    /// Corrupt the next `n` matching transmissions (retransmissions count).
    Count(u32),
    /// Corrupt every `n`th matching transmission (`n == 0` acts like 1).
    EveryNth(u32),
    /// Corrupt each matching transmission with probability `p` (0.0..=1.0),
    /// drawn from the simulation's seeded generator.
    Probability(f64),
}

/// A fault-injection rule for [`Simulation::inject_errors`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InjectSpec {
    pub bus: BusId,
    /// Only transmissions by this node match; `None` matches any sender.
    pub node: Option<NodeId>,
    /// Only frames with this `(id, extended)` match; `None` matches any.
    pub id: Option<(u32, bool)>,
    pub kind: CanErrorKind,
    pub mode: InjectMode,
    /// Upper bound on the number of corruptions; the rule is removed when it
    /// reaches 0. `None` is unbounded, except that `Count(n)` is always
    /// bounded by `n`.
    pub remaining: Option<u32>,
}

struct ActiveInject {
    spec: InjectSpec,
    /// Matching transmissions seen so far (for `EveryNth`).
    matched: u64,
    left: Option<u32>,
}

/// Fault-confinement counters and state of one node on one bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeErrorInfo {
    pub node: NodeId,
    pub bus: BusId,
    pub state: NodeErrorState,
    pub tec: u16,
    pub rec: u16,
}

#[derive(Default, Clone, Copy)]
struct NodeErr {
    tec: u16,
    rec: u16,
    state: NodeErrorState,
    /// The node may not start a transmission before this time.
    suspend_until: u64,
    /// When a bus-off node comes back.
    recover_at: u64,
}

impl NodeErr {
    /// Recompute the state from the counters (bus-off is left only through
    /// recovery).
    fn refresh(&mut self) {
        if self.state == NodeErrorState::BusOff {
            return;
        }
        self.state = if self.tec > BUS_OFF_LIMIT {
            NodeErrorState::BusOff
        } else if self.tec > ERROR_PASSIVE_LIMIT || self.rec > ERROR_PASSIVE_LIMIT {
            NodeErrorState::ErrorPassive
        } else {
            NodeErrorState::ErrorActive
        };
    }
}

/// Errors returned by [`Simulation::new`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SimError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("script of node {node:?} failed to compile: {msg}")]
    Script { node: NodeId, msg: String },
    #[error("replay node {node:?}: {msg}")]
    Replay { node: String, msg: String },
}

/// Number of CAN error frames per [`CanErrorKind`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CanErrorCounts {
    pub bit: u64,
    pub stuff: u64,
    pub crc: u64,
    pub form: u64,
    pub ack: u64,
}

impl CanErrorCounts {
    pub fn get(&self, kind: CanErrorKind) -> u64 {
        match kind {
            CanErrorKind::Bit => self.bit,
            CanErrorKind::Stuff => self.stuff,
            CanErrorKind::Crc => self.crc,
            CanErrorKind::Form => self.form,
            CanErrorKind::Ack => self.ack,
        }
    }

    pub fn total(&self) -> u64 {
        self.bit + self.stuff + self.crc + self.form + self.ack
    }

    fn add(&mut self, kind: CanErrorKind) {
        match kind {
            CanErrorKind::Bit => self.bit += 1,
            CanErrorKind::Stuff => self.stuff += 1,
            CanErrorKind::Crc => self.crc += 1,
            CanErrorKind::Form => self.form += 1,
            CanErrorKind::Ack => self.ack += 1,
        }
    }
}

/// Per-bus utilization counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusStats {
    pub frames: u64,
    pub busy_ns: u64,
    /// Frames that could not be transmitted (currently: CAN FD frames sent
    /// onto a bus that does not have `fd_enabled` set). CAN error frames are
    /// counted separately in [`BusStats::can_errors`].
    pub error_frames: u64,
    /// CAN error frames (bit, stuff, CRC, form, ACK) seen on the bus.
    pub can_errors: CanErrorCounts,
    /// Frames dropped because their sender was bus-off (new ones, queued
    /// ones, and one aborted or abandoned in flight).
    pub dropped_bus_off: u64,
    /// Forwarded frames dropped because they exceeded [`MAX_HOPS`].
    pub routing_drops: u64,
}

impl BusStats {
    /// Fraction of `window_ns` nanoseconds during which the bus was busy.
    pub fn load(&self, window_ns: u64) -> f64 {
        if window_ns == 0 {
            0.0
        } else {
            self.busy_ns as f64 / window_ns as f64
        }
    }
}

enum EventKind {
    Timer {
        node: NodeId,
        timer: u32,
    },
    Arbitrate {
        bus: BusId,
    },
    /// A cyclic generator row is due; stale when `chain` no longer matches.
    GenTimer {
        gen_id: GeneratorId,
        row: u32,
        chain: u64,
    },
    TxComplete {
        bus: BusId,
        meta: FrameMeta,
        frame: CanFrame,
        duration_ns: u64,
    },
    /// An error is detected `elapsed_ns` into a transmission: the error event
    /// is emitted and the counters change.
    TxError {
        bus: BusId,
        meta: FrameMeta,
        frame: CanFrame,
        error: CanErrorKind,
        elapsed_ns: u64,
    },
    /// The error frame and intermission are over: the bus is free and the
    /// frame is retransmitted.
    ErrorEnd {
        bus: BusId,
        meta: FrameMeta,
        frame: CanFrame,
        busy_ns: u64,
    },
    /// A bus-off node may rejoin the bus (stale unless `recover_at` matches).
    Recover {
        node: NodeId,
        bus: BusId,
    },
}

struct Scheduled {
    time: u64,
    seq: u64,
    kind: EventKind,
}

impl PartialEq for Scheduled {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
    }
}
impl Eq for Scheduled {}
impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time.cmp(&other.time).then(self.seq.cmp(&other.seq))
    }
}

/// Arbitration key: lower sorts first onto the bus. Standard (11-bit)
/// identifiers are shifted up 18 bits so they compare against extended
/// (29-bit) ids as real CAN arbitration would; on an exact tie the standard
/// frame wins.
fn arbitration_key(frame: &CanFrame) -> (u32, bool) {
    if frame.extended {
        (frame.id, true)
    } else {
        (frame.id << 18, false)
    }
}

/// A running cyclic generator row.
struct GenCyclic {
    bus: Option<BusId>,
    frame: CanFrame,
    period_ns: u64,
    /// Generation counter: timers of older chains are ignored.
    chain: u64,
}

/// A discrete-event CAN bus simulation built from a [`Topology`].
pub struct Simulation {
    node_buses: HashMap<NodeId, Vec<BusId>>,
    bus_nodes: HashMap<BusId, Vec<NodeId>>,
    bus_configs: HashMap<BusId, CanBusConfig>,
    ecus: HashMap<NodeId, Box<dyn Ecu>>,
    now: u64,
    heap: BinaryHeap<Reverse<Scheduled>>,
    seq: u64,
    bus_busy: HashMap<BusId, bool>,
    bus_pending: HashMap<BusId, Vec<(CanFrame, FrameMeta)>>,
    next_uid: u64,
    stats: HashMap<BusId, BusStats>,
    started: bool,
    all_buses: Vec<BusId>,
    gen_rows: HashMap<(GeneratorId, u32), GenCyclic>,
    gen_chain: u64,
    node_err: HashMap<(NodeId, BusId), NodeErr>,
    injections: Vec<ActiveInject>,
    rng: Rng,
}

impl Simulation {
    /// Build a simulation from a validated topology. Every node gets a
    /// default [`PeriodicEcu`] (a [`GatewayEcu`] for gateways, a [`ReplayEcu`] for replay nodes); use [`Simulation::set_ecu`] to override a
    /// node's behavior before the first call to `run_until`.
    pub fn new(topology: &Topology) -> Result<Self, SimError> {
        topology.validate()?;

        let mut node_buses: HashMap<NodeId, Vec<BusId>> = HashMap::new();
        let mut bus_nodes: HashMap<BusId, Vec<NodeId>> = HashMap::new();
        for link in &topology.links {
            node_buses.entry(link.node).or_default().push(link.bus);
            bus_nodes.entry(link.bus).or_default().push(link.node);
        }

        let mut bus_configs = HashMap::new();
        let mut bus_busy = HashMap::new();
        let mut bus_pending = HashMap::new();
        let mut stats = HashMap::new();
        let all_buses: Vec<BusId> = topology.buses.iter().map(|b| b.id).collect();
        for bus in &topology.buses {
            bus_configs.insert(bus.id, bus.clone());
            bus_busy.insert(bus.id, false);
            bus_pending.insert(bus.id, Vec::new());
            stats.insert(bus.id, BusStats::default());
        }

        let mut ecus: HashMap<NodeId, Box<dyn Ecu>> = HashMap::new();
        for node in &topology.nodes {
            let mut ecu: Box<dyn Ecu> = match node.kind {
                NodeKind::Ecu => Box::new(PeriodicEcu::new(node)),
                NodeKind::Gateway { .. } => Box::new(GatewayEcu::new(node)),
                NodeKind::Replay { .. } => {
                    Box::new(ReplayEcu::new(node).map_err(|msg| SimError::Replay {
                        node: node.name.clone(),
                        msg,
                    })?)
                }
            };
            if let Some(source) = &node.script {
                ecu = Box::new(
                    ScriptEcu::new(ecu, &node.name, source)
                        .map_err(|msg| SimError::Script { node: node.id, msg })?,
                );
            }
            ecus.insert(node.id, ecu);
            node_buses.entry(node.id).or_default();
        }

        Ok(Simulation {
            node_buses,
            bus_nodes,
            bus_configs,
            ecus,
            now: 0,
            heap: BinaryHeap::new(),
            seq: 0,
            bus_busy,
            bus_pending,
            next_uid: 0,
            stats,
            started: false,
            all_buses,
            gen_rows: HashMap::new(),
            gen_chain: 0,
            node_err: HashMap::new(),
            injections: Vec::new(),
            rng: Rng::new(DEFAULT_SEED),
        })
    }

    /// Replace the ECU behavior for `node`. Must be called before the first
    /// `run_until`/`send_once` call, i.e. before the simulation has started.
    pub fn set_ecu(&mut self, node: NodeId, ecu: Box<dyn Ecu>) {
        self.ecus.insert(node, ecu);
    }

    /// Take the log lines (e.g. script output) produced by all ECUs since
    /// the last call, ordered by node id.
    pub fn drain_logs(&mut self) -> Vec<String> {
        let mut nodes: Vec<NodeId> = self.ecus.keys().copied().collect();
        nodes.sort();
        nodes
            .into_iter()
            .flat_map(|n| {
                self.ecus
                    .get_mut(&n)
                    .map(|e| e.drain_logs())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Current virtual simulation time.
    pub fn now(&self) -> Timestamp {
        Timestamp(self.now)
    }

    /// Per-bus statistics collected so far.
    pub fn stats(&self) -> &HashMap<BusId, BusStats> {
        &self.stats
    }

    /// Reseed the generator behind [`InjectMode::Probability`], for
    /// reproducible runs.
    pub fn set_seed(&mut self, seed: u64) {
        self.rng = Rng::new(seed);
    }

    /// Add a fault-injection rule. Matching transmissions are corrupted: an
    /// error event is emitted, the counters change and the frame is
    /// retransmitted. When several rules match, the first one that fires
    /// wins.
    pub fn inject_errors(&mut self, spec: InjectSpec) {
        let left = match spec.mode {
            InjectMode::Count(n) => Some(spec.remaining.map_or(n, |r| r.min(n))),
            _ => spec.remaining,
        };
        if left == Some(0) {
            return;
        }
        self.injections.push(ActiveInject {
            spec,
            matched: 0,
            left,
        });
    }

    /// Remove every injection rule.
    pub fn clear_injections(&mut self) {
        self.injections.clear();
    }

    /// Fault-confinement state, TEC and REC of `node` on `bus`.
    pub fn node_state(&self, node: NodeId, bus: BusId) -> (NodeErrorState, u16, u16) {
        self.node_err
            .get(&(node, bus))
            .map_or((NodeErrorState::ErrorActive, 0, 0), |e| {
                (e.state, e.tec, e.rec)
            })
    }

    /// State of every (node, bus) link of the topology, ordered by node and
    /// bus. Generators are not included.
    pub fn node_states(&self) -> Vec<NodeErrorInfo> {
        let mut v: Vec<NodeErrorInfo> = self
            .node_buses
            .iter()
            .flat_map(|(node, buses)| buses.iter().map(move |bus| (*node, *bus)))
            .map(|(node, bus)| {
                let (state, tec, rec) = self.node_state(node, bus);
                NodeErrorInfo {
                    node,
                    bus,
                    state,
                    tec,
                    rec,
                }
            })
            .collect();
        v.sort_by_key(|i| (i.node, i.bus));
        v
    }

    /// Force `node` bus-off on `bus` now; it recovers after the usual
    /// 1408 bit times. Does nothing for a node that is not linked to `bus`
    /// (generators count as linked to every bus) or is already bus-off.
    pub fn force_bus_off(&mut self, node: NodeId, bus: BusId) {
        self.ensure_started();
        let linked = GeneratorId::from_node(node).is_some()
            || self.node_buses.get(&node).is_some_and(|b| b.contains(&bus));
        if !linked || self.is_bus_off(node, bus) {
            return;
        }
        let e = self.node_err.entry((node, bus)).or_default();
        e.tec = BUS_OFF_LIMIT + 1;
        e.state = NodeErrorState::BusOff;
        self.enter_bus_off(node, bus);
    }

    fn is_bus_off(&self, node: NodeId, bus: BusId) -> bool {
        self.node_err
            .get(&(node, bus))
            .is_some_and(|e| e.state == NodeErrorState::BusOff)
    }

    fn err_state(&self, node: NodeId, bus: BusId) -> NodeErrorState {
        self.node_state(node, bus).0
    }

    /// Duration of one nominal bit on `bus`.
    fn bit_ns(&self, bus: BusId) -> u64 {
        let rate = self.bus_configs.get(&bus).map_or(500_000, |c| c.bitrate);
        1_000_000_000u64.div_ceil(u64::from(rate.max(1)))
    }

    /// Nodes that see a frame sent by `sender` on `bus`: every other linked
    /// node that is not bus-off.
    fn receivers(&self, bus: BusId, sender: NodeId) -> Vec<NodeId> {
        self.bus_nodes
            .get(&bus)
            .map(|nodes| {
                nodes
                    .iter()
                    .copied()
                    .filter(|n| *n != sender && !self.is_bus_off(*n, bus))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Finish entering bus-off: drop the node's queued frames on `bus` and
    /// schedule its recovery. The caller has set the state.
    fn enter_bus_off(&mut self, node: NodeId, bus: BusId) {
        let recover_at = self.now + BUS_OFF_RECOVERY_BITS * self.bit_ns(bus);
        if let Some(e) = self.node_err.get_mut(&(node, bus)) {
            e.recover_at = recover_at;
        }
        let mut dropped = 0;
        if let Some(pending) = self.bus_pending.get_mut(&bus) {
            let before = pending.len();
            pending.retain(|(_, m)| m.sender != node);
            dropped = (before - pending.len()) as u64;
        }
        self.stats.entry(bus).or_default().dropped_bus_off += dropped;
        self.schedule(recover_at, EventKind::Recover { node, bus });
    }

    fn count_tx_ok(&mut self, node: NodeId, bus: BusId) {
        let e = self.node_err.entry((node, bus)).or_default();
        e.tec = e.tec.saturating_sub(1);
        e.refresh();
    }

    fn count_rx_ok(&mut self, node: NodeId, bus: BusId) {
        let e = self.node_err.entry((node, bus)).or_default();
        e.rec = if e.rec > ERROR_PASSIVE_LIMIT {
            REC_RECOVERED
        } else {
            e.rec.saturating_sub(1)
        };
        e.refresh();
    }

    fn count_tx_error(&mut self, node: NodeId, bus: BusId, error: CanErrorKind) {
        let e = self.node_err.entry((node, bus)).or_default();
        if e.state == NodeErrorState::BusOff {
            return;
        }
        if !(error == CanErrorKind::Ack && e.state == NodeErrorState::ErrorPassive) {
            e.tec = e.tec.saturating_add(TEC_ERROR_STEP);
        }
        e.refresh();
        if e.state == NodeErrorState::BusOff {
            self.enter_bus_off(node, bus);
        }
    }

    fn count_rx_error(&mut self, node: NodeId, bus: BusId) {
        let e = self.node_err.entry((node, bus)).or_default();
        e.rec = e.rec.saturating_add(1);
        e.refresh();
    }

    /// Hold an error-passive `node` back for the suspend-transmission time
    /// after a transmission that ends at `end`.
    fn suspend_if_passive(&mut self, node: NodeId, bus: BusId, end: u64) {
        let suspend = SUSPEND_BITS * self.bit_ns(bus);
        if let Some(e) = self.node_err.get_mut(&(node, bus))
            && e.state == NodeErrorState::ErrorPassive
        {
            e.suspend_until = end + suspend;
        }
    }

    /// The error, if any, that corrupts the transmission of `frame` by
    /// `meta.sender` on `bus`: injection rules first, then a missing ACK.
    fn pick_error(
        &mut self,
        bus: BusId,
        frame: &CanFrame,
        meta: &FrameMeta,
    ) -> Option<CanErrorKind> {
        let mut hit = None;
        for inj in &mut self.injections {
            let s = inj.spec;
            if s.bus != bus
                || s.node.is_some_and(|n| n != meta.sender)
                || s.id
                    .is_some_and(|(id, ext)| id != frame.id || ext != frame.extended)
            {
                continue;
            }
            inj.matched += 1;
            let fire = match s.mode {
                InjectMode::Count(_) => true,
                InjectMode::EveryNth(n) => inj.matched % u64::from(n.max(1)) == 0,
                InjectMode::Probability(p) => self.rng.next_f64() < p,
            };
            if fire {
                if let Some(l) = &mut inj.left {
                    *l -= 1;
                }
                hit = Some(s.kind);
                break;
            }
        }
        self.injections.retain(|i| i.left != Some(0));
        if hit.is_some() {
            return hit;
        }
        let simulate_ack = self.bus_configs.get(&bus).is_some_and(|c| c.simulate_ack);
        (simulate_ack && self.receivers(bus, meta.sender).is_empty()).then_some(CanErrorKind::Ack)
    }

    /// Inject a one-off frame transmission from `node`, outside of any ECU
    /// callback (e.g. from a UI "send" button). `bus` selects a single bus;
    /// `None` sends on every bus the node is linked to.
    pub fn send_once(&mut self, node: NodeId, bus: Option<BusId>, frame: CanFrame) {
        self.ensure_started();
        self.enqueue_origin(node, bus, frame);
    }

    /// Send one frame from generator `gen_id` on `bus` (`None` = every bus).
    /// The frame takes part in normal arbitration and gateway forwarding.
    pub fn gen_send(&mut self, gen_id: GeneratorId, bus: Option<BusId>, frame: CanFrame) {
        self.ensure_started();
        self.enqueue_origin(gen_id.node(), bus, frame);
    }

    /// Start (`Some(period_ns)`, first frame immediately) or stop (`None`)
    /// the cyclic row `row` of a generator. Restarting replaces the old
    /// chain.
    pub fn gen_set_cyclic(
        &mut self,
        gen_id: GeneratorId,
        row: u32,
        bus: Option<BusId>,
        frame: CanFrame,
        period_ns: Option<u64>,
    ) {
        self.ensure_started();
        let Some(period_ns) = period_ns.filter(|p| *p > 0) else {
            self.gen_rows.remove(&(gen_id, row));
            return;
        };
        self.gen_chain += 1;
        let chain = self.gen_chain;
        self.gen_rows.insert(
            (gen_id, row),
            GenCyclic {
                bus,
                frame,
                period_ns,
                chain,
            },
        );
        self.enqueue_origin(gen_id.node(), bus, frame);
        self.schedule(
            self.now + period_ns,
            EventKind::GenTimer { gen_id, row, chain },
        );
    }

    /// Change the payload of a running cyclic row; used from its next send.
    pub fn gen_update_frame(&mut self, gen_id: GeneratorId, row: u32, frame: CanFrame) {
        if let Some(r) = self.gen_rows.get_mut(&(gen_id, row)) {
            r.frame = frame;
        }
    }

    /// Stop every cyclic row of `gen_id`.
    pub fn gen_stop_all(&mut self, gen_id: GeneratorId) {
        self.gen_rows.retain(|(g, _), _| *g != gen_id);
    }

    /// Deliver `cmd` to `node`'s ECU at the current virtual time.
    pub fn command(&mut self, node: NodeId, cmd: EcuCommand) {
        self.ensure_started();
        self.run_callback(node, None, |ecu, ctx| ecu.on_command(&cmd, ctx));
    }

    /// Queue a newly originated frame (fresh uid, hop 0) on `bus`, or on all
    /// of the node's buses when `bus` is `None`. Unlinked buses are ignored.
    fn enqueue_origin(&mut self, node: NodeId, bus: Option<BusId>, frame: CanFrame) {
        let uid = self.next_uid;
        self.next_uid += 1;
        let meta = FrameMeta {
            sender: node,
            origin: node,
            uid,
            hop: 0,
        };
        self.enqueue(node, bus, frame, meta);
    }

    fn enqueue(&mut self, node: NodeId, bus: Option<BusId>, frame: CanFrame, meta: FrameMeta) {
        let linked = if GeneratorId::from_node(node).is_some() {
            self.all_buses.clone()
        } else {
            self.node_buses.get(&node).cloned().unwrap_or_default()
        };
        let targets: Vec<BusId> = match bus {
            Some(b) if linked.contains(&b) => vec![b],
            Some(_) => Vec::new(),
            None => linked,
        };
        for bus in targets {
            if self.is_bus_off(node, bus) {
                self.stats.entry(bus).or_default().dropped_bus_off += 1;
                continue;
            }
            self.bus_pending.entry(bus).or_default().push((frame, meta));
            self.schedule(self.now, EventKind::Arbitrate { bus });
        }
    }

    /// Advance the simulation, processing all events up to and including
    /// `target`, appending every [`BusEvent`] produced to `out`.
    pub fn run_until(&mut self, target: Timestamp, out: &mut Vec<BusEvent>) {
        self.ensure_started();
        while let Some(Reverse(sch)) = self.heap.peek() {
            if sch.time > target.0 {
                break;
            }
            let Reverse(sch) = self.heap.pop().unwrap();
            self.now = sch.time;
            self.handle_event(sch.kind, out);
        }
        if target.0 > self.now {
            self.now = target.0;
        }
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let mut nodes: Vec<NodeId> = self.ecus.keys().copied().collect();
        nodes.sort();
        for node in nodes {
            self.run_callback(node, None, |ecu, ctx| ecu.on_start(ctx));
        }
    }

    fn run_callback(
        &mut self,
        node: NodeId,
        incoming: Option<FrameMeta>,
        f: impl FnOnce(&mut dyn Ecu, &mut EcuCtx),
    ) {
        let Some(mut ecu) = self.ecus.remove(&node) else {
            return;
        };
        let mut ctx = EcuCtx::new(node, Timestamp(self.now), incoming);
        f(ecu.as_mut(), &mut ctx);
        self.ecus.insert(node, ecu);
        self.apply_ctx(node, ctx);
    }

    fn apply_ctx(&mut self, node: NodeId, ctx: EcuCtx) {
        for out in ctx.sends {
            match out.forward_of {
                None => self.enqueue_origin(node, out.bus, out.frame),
                Some(prev) => {
                    let meta = FrameMeta {
                        sender: node,
                        origin: prev.origin,
                        uid: prev.uid,
                        hop: prev.hop.saturating_add(1),
                    };
                    if meta.hop > MAX_HOPS {
                        if let Some(bus) = out.bus {
                            self.stats.entry(bus).or_default().routing_drops += 1;
                        }
                        continue;
                    }
                    self.enqueue(node, out.bus, out.frame, meta);
                }
            }
        }
        for (timer, delay_ns) in ctx.timers {
            self.schedule(self.now + delay_ns, EventKind::Timer { node, timer });
        }
    }

    fn schedule(&mut self, time: u64, kind: EventKind) {
        let seq = self.seq;
        self.seq += 1;
        self.heap.push(Reverse(Scheduled { time, seq, kind }));
    }

    fn try_arbitrate(&mut self, bus: BusId) {
        if *self.bus_busy.get(&bus).unwrap_or(&false) {
            return;
        }
        loop {
            let Some(pending) = self.bus_pending.get(&bus) else {
                return;
            };
            // Lowest arbitration key among the frames whose sender is not
            // suspended; remember when the earliest suspended one is due.
            let mut winner: Option<(usize, (u32, bool))> = None;
            let mut wake: Option<u64> = None;
            for (i, (frame, meta)) in pending.iter().enumerate() {
                let until = self
                    .node_err
                    .get(&(meta.sender, bus))
                    .map_or(0, |e| e.suspend_until);
                if until > self.now {
                    wake = Some(wake.map_or(until, |w| w.min(until)));
                    continue;
                }
                let key = arbitration_key(frame);
                if winner.is_none_or(|(_, best)| key < best) {
                    winner = Some((i, key));
                }
            }
            let Some((winner_idx, _)) = winner else {
                if let Some(at) = wake {
                    self.schedule(at, EventKind::Arbitrate { bus });
                }
                return;
            };
            let (frame, meta) = self
                .bus_pending
                .get_mut(&bus)
                .expect("pending exists")
                .remove(winner_idx);

            let fd_enabled = self
                .bus_configs
                .get(&bus)
                .map(|c| c.fd_enabled)
                .unwrap_or(false);
            if frame.fd && !fd_enabled {
                // Dropped: this bus does not carry CAN FD frames.
                self.stats.entry(bus).or_default().error_frames += 1;
                continue;
            }

            let (nominal, data_rate) = self
                .bus_configs
                .get(&bus)
                .map(|c| (c.bitrate, c.data_bitrate))
                .unwrap_or((500_000, 500_000));
            let duration_ns = frame_duration_ns_any(&frame, nominal, data_rate);
            self.bus_busy.insert(bus, true);
            match self.pick_error(bus, &frame, &meta) {
                None => self.schedule(
                    self.now + duration_ns,
                    EventKind::TxComplete {
                        bus,
                        meta,
                        frame,
                        duration_ns,
                    },
                ),
                Some(error) => {
                    // Bit, stuff and form errors strike mid-frame; CRC and
                    // ACK errors are flagged after the ACK slot, shortly
                    // before the frame would have ended.
                    let half = duration_ns / 2;
                    let elapsed_ns = match error {
                        CanErrorKind::Crc | CanErrorKind::Ack => duration_ns
                            .saturating_sub(FRAME_TAIL_BITS * self.bit_ns(bus))
                            .max(half),
                        _ => half,
                    }
                    .max(1);
                    self.schedule(
                        self.now + elapsed_ns,
                        EventKind::TxError {
                            bus,
                            meta,
                            frame,
                            error,
                            elapsed_ns,
                        },
                    );
                }
            }
            return;
        }
    }

    fn handle_event(&mut self, kind: EventKind, out: &mut Vec<BusEvent>) {
        match kind {
            EventKind::Timer { node, timer } => {
                self.run_callback(node, None, |ecu, ctx| ecu.on_timer(timer, ctx));
            }
            EventKind::Arbitrate { bus } => {
                self.try_arbitrate(bus);
            }
            EventKind::GenTimer { gen_id, row, chain } => {
                let Some(r) = self.gen_rows.get(&(gen_id, row)) else {
                    return;
                };
                if r.chain != chain {
                    return;
                }
                let (bus, frame, period_ns) = (r.bus, r.frame, r.period_ns);
                self.enqueue_origin(gen_id.node(), bus, frame);
                self.schedule(
                    self.now + period_ns,
                    EventKind::GenTimer { gen_id, row, chain },
                );
            }
            EventKind::TxComplete {
                bus,
                meta,
                frame,
                duration_ns,
            } => {
                self.bus_busy.insert(bus, false);
                self.stats.entry(bus).or_default().busy_ns += duration_ns;
                if self.is_bus_off(meta.sender, bus) {
                    // The sender was forced bus-off mid-frame: the frame is lost.
                    self.stats.entry(bus).or_default().dropped_bus_off += 1;
                    self.try_arbitrate(bus);
                    return;
                }
                self.stats.entry(bus).or_default().frames += 1;

                out.push(BusEvent {
                    time: Timestamp(self.now),
                    bus,
                    sender: meta.sender,
                    origin: meta.origin,
                    dir: if meta.hop == 0 {
                        Direction::Tx
                    } else {
                        Direction::Rx
                    },
                    frame_uid: meta.uid,
                    hop: meta.hop,
                    frame,
                    kind: BusEventKind::Frame,
                });

                self.count_tx_ok(meta.sender, bus);
                self.suspend_if_passive(meta.sender, bus, self.now);
                for node in self.receivers(bus, meta.sender) {
                    self.count_rx_ok(node, bus);
                    self.run_callback(node, Some(meta), |ecu, ctx| ecu.on_frame(bus, &frame, ctx));
                }

                self.try_arbitrate(bus);
            }
            EventKind::TxError {
                bus,
                meta,
                frame,
                error,
                elapsed_ns,
            } => {
                let sender = meta.sender;
                let passive = self.err_state(sender, bus) == NodeErrorState::ErrorPassive;
                let flag_bits = if passive {
                    PASSIVE_ERROR_FRAME_BITS
                } else {
                    ACTIVE_ERROR_FRAME_BITS
                };
                let tail_ns = (flag_bits + INTERMISSION_BITS) * self.bit_ns(bus);
                self.stats.entry(bus).or_default().can_errors.add(error);
                out.push(BusEvent {
                    time: Timestamp(self.now),
                    bus,
                    sender,
                    origin: meta.origin,
                    dir: if meta.hop == 0 {
                        Direction::Tx
                    } else {
                        Direction::Rx
                    },
                    frame_uid: meta.uid,
                    hop: meta.hop,
                    frame,
                    kind: BusEventKind::Error {
                        error,
                        node: sender,
                    },
                });
                let receivers = self.receivers(bus, sender);
                self.count_tx_error(sender, bus, error);
                for node in receivers {
                    self.count_rx_error(node, bus);
                }
                self.suspend_if_passive(sender, bus, self.now + tail_ns);
                self.schedule(
                    self.now + tail_ns,
                    EventKind::ErrorEnd {
                        bus,
                        meta,
                        frame,
                        busy_ns: elapsed_ns + tail_ns,
                    },
                );
            }
            EventKind::ErrorEnd {
                bus,
                meta,
                frame,
                busy_ns,
            } => {
                self.bus_busy.insert(bus, false);
                self.stats.entry(bus).or_default().busy_ns += busy_ns;
                if self.is_bus_off(meta.sender, bus) {
                    self.stats.entry(bus).or_default().dropped_bus_off += 1;
                } else {
                    // Retransmit: the frame takes part in arbitration again.
                    self.bus_pending.entry(bus).or_default().push((frame, meta));
                }
                self.try_arbitrate(bus);
            }
            EventKind::Recover { node, bus } => {
                let now = self.now;
                if let Some(e) = self.node_err.get_mut(&(node, bus))
                    && e.state == NodeErrorState::BusOff
                    && e.recover_at == now
                {
                    *e = NodeErr::default();
                }
            }
        }
    }
}
