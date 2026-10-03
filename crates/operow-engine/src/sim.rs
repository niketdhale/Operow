use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use operow_core::{
    BusEvent, BusId, CanBusConfig, CanFrame, Direction, NodeId, NodeKind, Timestamp, Topology,
    TopologyError,
};

use crate::ecu::{Ecu, EcuCommand, EcuCtx, FrameMeta, PeriodicEcu};
use crate::gateway::GatewayEcu;
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

/// Errors returned by [`Simulation::new`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SimError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("script of node {node:?} failed to compile: {msg}")]
    Script { node: NodeId, msg: String },
}

/// Per-bus utilization counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusStats {
    pub frames: u64,
    pub busy_ns: u64,
    /// Frames that could not be transmitted (currently: CAN FD frames sent
    /// onto a bus that does not have `fd_enabled` set).
    pub error_frames: u64,
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
}

impl Simulation {
    /// Build a simulation from a validated topology. Every node gets a
    /// default [`PeriodicEcu`] (or a [`GatewayEcu`] for gateways); use [`Simulation::set_ecu`] to override a
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
            let Some(pending) = self.bus_pending.get_mut(&bus) else {
                return;
            };
            if pending.is_empty() {
                return;
            }
            let winner_idx = pending
                .iter()
                .enumerate()
                .min_by_key(|(_, (frame, _))| arbitration_key(frame))
                .map(|(i, _)| i)
                .expect("pending is non-empty");
            let (frame, meta) = pending.remove(winner_idx);

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
            self.schedule(
                self.now + duration_ns,
                EventKind::TxComplete {
                    bus,
                    meta,
                    frame,
                    duration_ns,
                },
            );
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
                let stats = self.stats.entry(bus).or_default();
                stats.frames += 1;
                stats.busy_ns += duration_ns;

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
                });

                let receivers = self.bus_nodes.get(&bus).cloned().unwrap_or_default();
                for node in receivers {
                    if node == meta.sender {
                        continue;
                    }
                    self.run_callback(node, Some(meta), |ecu, ctx| ecu.on_frame(bus, &frame, ctx));
                }

                self.try_arbitrate(bus);
            }
        }
    }
}
