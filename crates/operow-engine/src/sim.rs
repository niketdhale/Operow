use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use operow_core::{BusEvent, BusId, CanFrame, NodeId, Timestamp, Topology, TopologyError};

use crate::ecu::{Ecu, EcuCtx, PeriodicEcu};
use crate::timing::frame_duration_ns;

/// Per-bus utilization counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusStats {
    pub frames: u64,
    pub busy_ns: u64,
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
    TxComplete {
        bus: BusId,
        sender: NodeId,
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

/// A discrete-event CAN bus simulation built from a [`Topology`].
pub struct Simulation {
    node_buses: HashMap<NodeId, Vec<BusId>>,
    bus_nodes: HashMap<BusId, Vec<NodeId>>,
    bitrates: HashMap<BusId, u32>,
    ecus: HashMap<NodeId, Box<dyn Ecu>>,
    now: u64,
    heap: BinaryHeap<Reverse<Scheduled>>,
    seq: u64,
    bus_busy: HashMap<BusId, bool>,
    bus_pending: HashMap<BusId, Vec<(CanFrame, NodeId)>>,
    stats: HashMap<BusId, BusStats>,
    started: bool,
}

impl Simulation {
    /// Build a simulation from a validated topology. Every node gets a
    /// default [`PeriodicEcu`]; use [`Simulation::set_ecu`] to override a
    /// node's behavior before the first call to `run_until`.
    pub fn new(topology: &Topology) -> Result<Self, TopologyError> {
        topology.validate()?;

        let mut node_buses: HashMap<NodeId, Vec<BusId>> = HashMap::new();
        let mut bus_nodes: HashMap<BusId, Vec<NodeId>> = HashMap::new();
        for link in &topology.links {
            node_buses.entry(link.node).or_default().push(link.bus);
            bus_nodes.entry(link.bus).or_default().push(link.node);
        }

        let mut bitrates = HashMap::new();
        let mut bus_busy = HashMap::new();
        let mut bus_pending = HashMap::new();
        let mut stats = HashMap::new();
        for bus in &topology.buses {
            bitrates.insert(bus.id, bus.bitrate);
            bus_busy.insert(bus.id, false);
            bus_pending.insert(bus.id, Vec::new());
            stats.insert(bus.id, BusStats::default());
        }

        let mut ecus: HashMap<NodeId, Box<dyn Ecu>> = HashMap::new();
        for node in &topology.nodes {
            ecus.insert(node.id, Box::new(PeriodicEcu::new(node)));
            node_buses.entry(node.id).or_default();
        }

        Ok(Simulation {
            node_buses,
            bus_nodes,
            bitrates,
            ecus,
            now: 0,
            heap: BinaryHeap::new(),
            seq: 0,
            bus_busy,
            bus_pending,
            stats,
            started: false,
        })
    }

    /// Replace the ECU behavior for `node`. Must be called before the first
    /// `run_until`/`send_once` call, i.e. before the simulation has started.
    pub fn set_ecu(&mut self, node: NodeId, ecu: Box<dyn Ecu>) {
        self.ecus.insert(node, ecu);
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
    /// callback (e.g. from a UI "send" button).
    pub fn send_once(&mut self, node: NodeId, frame: CanFrame) {
        self.ensure_started();
        let buses = self.node_buses.get(&node).cloned().unwrap_or_default();
        for bus in buses {
            self.bus_pending.entry(bus).or_default().push((frame, node));
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
            self.run_callback(node, |ecu, ctx| ecu.on_start(ctx));
        }
    }

    fn run_callback(&mut self, node: NodeId, f: impl FnOnce(&mut dyn Ecu, &mut EcuCtx)) {
        let Some(mut ecu) = self.ecus.remove(&node) else {
            return;
        };
        let buses = self.node_buses.get(&node).cloned().unwrap_or_default();
        let mut ctx = EcuCtx::new(node, Timestamp(self.now), buses);
        f(ecu.as_mut(), &mut ctx);
        self.ecus.insert(node, ecu);
        self.apply_ctx(node, ctx);
    }

    fn apply_ctx(&mut self, node: NodeId, ctx: EcuCtx) {
        let mut touched: Vec<BusId> = Vec::new();
        for frame in ctx.sends {
            for &bus in &ctx.buses {
                self.bus_pending.entry(bus).or_default().push((frame, node));
                if !touched.contains(&bus) {
                    touched.push(bus);
                }
            }
        }
        for bus in touched {
            self.schedule(self.now, EventKind::Arbitrate { bus });
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
        let (frame, sender) = pending.remove(winner_idx);

        let bitrate = *self.bitrates.get(&bus).unwrap_or(&500_000);
        let duration_ns = frame_duration_ns(&frame, bitrate);
        self.bus_busy.insert(bus, true);
        self.schedule(
            self.now + duration_ns,
            EventKind::TxComplete {
                bus,
                sender,
                frame,
                duration_ns,
            },
        );
    }

    fn handle_event(&mut self, kind: EventKind, out: &mut Vec<BusEvent>) {
        match kind {
            EventKind::Timer { node, timer } => {
                self.run_callback(node, |ecu, ctx| ecu.on_timer(timer, ctx));
            }
            EventKind::Arbitrate { bus } => {
                self.try_arbitrate(bus);
            }
            EventKind::TxComplete {
                bus,
                sender,
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
                    sender,
                    frame,
                });

                let receivers = self.bus_nodes.get(&bus).cloned().unwrap_or_default();
                for node in receivers {
                    if node == sender {
                        continue;
                    }
                    self.run_callback(node, |ecu, ctx| ecu.on_frame(bus, &frame, ctx));
                }

                self.try_arbitrate(bus);
            }
        }
    }
}
