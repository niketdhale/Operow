use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};

use operow_core::{BusEvent, BusId, CanFrame, NodeId, Timestamp, Topology};

use crate::ecu::EcuCommand;
use crate::hw::{HwBridge, HwBusStatus, HwLink, HwNotice};
use crate::sim::{
    BusStats, GeneratorId, InjectSpec, MsgControl, NodeErrorInfo, SimError, Simulation,
};
use crate::tester::{DiagRequestSpec, TesterPresentSpec};

/// Coarse run state broadcast to listeners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Stopped,
    Running,
    Paused,
}

/// Commands accepted by the engine thread.
pub enum Command {
    Load(Topology),
    Start,
    Stop,
    Pause,
    Resume,
    /// Real-time speed multiplier; 0 means "as fast as possible".
    SetSpeed(f64),
    SendOnce(NodeId, Option<BusId>, CanFrame),
    /// Deliver a command to an ECU.
    Ecu(NodeId, EcuCommand),
    /// One frame from an interactive generator (`None` = every bus).
    GenSend {
        gen_id: GeneratorId,
        bus: Option<BusId>,
        frame: CanFrame,
    },
    /// Start (`Some(period)`) or stop (`None`) a cyclic generator row; the
    /// engine's virtual clock drives it.
    GenSetCyclic {
        gen_id: GeneratorId,
        row: u32,
        bus: Option<BusId>,
        frame: CanFrame,
        period_ns: Option<u64>,
    },
    /// Change the payload of a running cyclic generator row.
    GenUpdateFrame {
        gen_id: GeneratorId,
        row: u32,
        frame: CanFrame,
    },
    /// Stop all cyclic rows of a generator.
    GenStopAll {
        gen_id: GeneratorId,
    },
    /// Add a fault-injection rule (see [`Simulation::inject_errors`]).
    InjectErrors(InjectSpec),
    /// Remove every fault-injection rule.
    ClearInjections,
    /// Force a node bus-off on a bus; it stays so until `RecoverBusOff`.
    ForceBusOff(NodeId, BusId),
    /// Bring a bus-off node back (see [`Simulation::recover_bus_off`]).
    RecoverBusOff {
        node: NodeId,
        bus: BusId,
    },
    /// Reseed the random generator used by probabilistic injection.
    SetSeed(u64),
    /// Take a node offline or online (`bus: None` = every bus it is linked
    /// to); see [`Simulation::set_node_online`].
    SetNodeOnline {
        node: NodeId,
        bus: Option<BusId>,
        online: bool,
    },
    /// Set the runtime control of frames with `id` (`(id, extended)`) sent
    /// by `node`; see [`Simulation::set_msg_control`].
    SetMsgControl {
        node: NodeId,
        id: (u32, bool),
        control: MsgControl,
    },
    /// Send a diagnostic request from a virtual "Tester" node on
    /// `tester_bus` (see [`Simulation::diag_request`]). The outcome arrives
    /// as [`EngineEvent::DiagResponse`]. Needs a running simulation.
    DiagRequest {
        tester_bus: BusId,
        req_id: u32,
        resp_id: u32,
        extended: bool,
        fd: bool,
        payload: Vec<u8>,
        functional: bool,
    },
    /// Start or stop a periodic TesterPresent (`3E 80`, single frame) from
    /// the virtual "Tester" on `tester_bus`: sent on `functional_id` when
    /// `functional`, else on `req_id`; as a CAN FD frame when `fd`.
    TesterPresent {
        enable: bool,
        tester_bus: BusId,
        req_id: u32,
        functional_id: u32,
        functional: bool,
        extended: bool,
        fd: bool,
        period_ms: u32,
    },
    Shutdown,
}

/// Events emitted by the engine thread.
pub enum EngineEvent {
    Frames(Vec<BusEvent>),
    Stats {
        time: Timestamp,
        buses: Vec<(BusId, BusStats)>,
    },
    /// Fault-confinement state of every node on every bus it is linked to;
    /// sent right after each `Stats`.
    NodeStates {
        time: Timestamp,
        nodes: Vec<NodeErrorInfo>,
    },
    /// Outcome of a [`Command::DiagRequest`]; `resp` is the final response
    /// (possibly a negative one) or why none arrived. `elapsed_ms` is
    /// virtual time.
    DiagResponse {
        req: Vec<u8>,
        resp: Result<Vec<u8>, String>,
        elapsed_ms: f64,
    },
    /// State of every hardware bus (empty when the topology has none or the
    /// measurement is stopped); sent right after each `Stats`, and when the
    /// channels open, fail to open or close.
    HwStatus(Vec<HwBusStatus>),
    State(RunState),
    Log(String),
    Error(String),
}

const EVENT_CHANNEL_CAPACITY: usize = 1024;
const TICK: Duration = Duration::from_millis(5);
/// Loop period while hardware buses are open (bounds the latency of frames
/// between the adapter and the simulation).
const HW_TICK: Duration = Duration::from_millis(1);
const STATS_INTERVAL: Duration = Duration::from_millis(200);
/// Virtual nanoseconds advanced per tick when running at "as fast as
/// possible" (speed 0).
const FAST_FORWARD_STEP_NS: u64 = 50_000_000;

/// Handle to a running engine thread.
pub struct EngineHandle {
    pub cmd: Sender<Command>,
    pub events: Receiver<EngineEvent>,
    thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// Send `Command::Shutdown` and block until the engine thread exits.
    pub fn shutdown(mut self) {
        let _ = self.cmd.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Wait for the engine thread to exit (e.g. after sending `Shutdown`
    /// separately via `self.cmd`).
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Owns and drives a [`Simulation`] on a background thread.
pub struct Engine;

impl Engine {
    /// Spawn the engine thread and return a handle to control it.
    pub fn spawn() -> EngineHandle {
        let (cmd_tx, cmd_rx) = unbounded();
        let (ev_tx, ev_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let thread = thread::spawn(move || engine_loop(cmd_rx, ev_tx));
        EngineHandle {
            cmd: cmd_tx,
            events: ev_rx,
            thread: Some(thread),
        }
    }
}

struct EngineState {
    topology: Option<Topology>,
    sim: Option<Simulation>,
    run_state: RunState,
    speed: f64,
    virtual_ns: u64,
    /// Buses we have already logged a "dropped FD frame(s)" warning for, so
    /// we only warn once per bus.
    warned_fd_buses: std::collections::HashSet<BusId>,
    /// The topology has a bus bound to hardware: real time only.
    realtime_only: bool,
    /// Open hardware channels while running.
    hw: Option<HwBridge>,
    paused_at: Option<Instant>,
}

const REALTIME_MSG: &str = "hardware buses present: the engine runs in real time only (speed 1.0)";

fn send_notices(notices: Vec<HwNotice>, ev_tx: &Sender<EngineEvent>) {
    for n in notices {
        let _ = ev_tx.try_send(match n {
            HwNotice::Log(l) => EngineEvent::Log(l),
            HwNotice::Error(e) => EngineEvent::Error(e),
        });
    }
}

/// Status of the hardware buses of `topology` while no channel is open:
/// `Closed`, or `Error(why)` when opening failed.
fn closed_status(topology: Option<&Topology>, error: Option<&str>) -> Vec<HwBusStatus> {
    topology
        .into_iter()
        .flat_map(|t| &t.buses)
        .filter_map(|b| {
            let hw = b.hardware.as_ref()?;
            Some(HwBusStatus {
                bus: b.id,
                interface: hw.interface.clone(),
                listen_only: hw.listen_only,
                link: error.map_or(HwLink::Closed, |e| HwLink::Error(e.to_string())),
                controller: Default::default(),
                rx_frames: 0,
                tx_frames: 0,
            })
        })
        .collect()
}

fn engine_loop(cmd_rx: Receiver<Command>, ev_tx: Sender<EngineEvent>) {
    let mut state = EngineState {
        topology: None,
        sim: None,
        run_state: RunState::Stopped,
        speed: 1.0,
        virtual_ns: 0,
        warned_fd_buses: std::collections::HashSet::new(),
        realtime_only: false,
        hw: None,
        paused_at: None,
    };
    let mut last_tick = Instant::now();
    let mut last_stats = Instant::now();
    let mut frames_buf = Vec::new();
    let mut dropped_batches: u64 = 0;

    loop {
        let tick = if state.hw.is_some() { HW_TICK } else { TICK };
        match cmd_rx.recv_timeout(tick) {
            Ok(cmd) => {
                if !handle_command(cmd, &mut state, &ev_tx) {
                    return;
                }
                last_tick = Instant::now();
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if state.run_state != RunState::Running {
            if let Some(hw) = state.hw.as_mut() {
                hw.discard_pending();
            }
            last_tick = Instant::now();
            continue;
        }

        let Some(sim) = state.sim.as_mut() else {
            last_tick = Instant::now();
            continue;
        };

        let elapsed = last_tick.elapsed();
        last_tick = Instant::now();
        if let Some(hw) = state.hw.as_mut() {
            // Real time: virtual time is the wall clock.
            let (now, notices) = hw.step(sim, &mut frames_buf);
            state.virtual_ns = now;
            send_notices(notices, &ev_tx);
        } else {
            let advance_ns = if state.speed == 0.0 {
                FAST_FORWARD_STEP_NS
            } else {
                (elapsed.as_secs_f64() * state.speed * 1_000_000_000.0).round() as u64
            };
            state.virtual_ns = state.virtual_ns.saturating_add(advance_ns);
            sim.run_until(Timestamp(state.virtual_ns), &mut frames_buf);
        }
        for line in sim.drain_logs() {
            let _ = ev_tx.try_send(EngineEvent::Log(line));
        }
        for r in sim.take_diag_results() {
            let _ = ev_tx.try_send(EngineEvent::DiagResponse {
                req: r.req,
                resp: r.resp,
                elapsed_ms: r.elapsed_ms,
            });
        }
        if !frames_buf.is_empty() {
            let batch = std::mem::take(&mut frames_buf);
            if ev_tx.try_send(EngineEvent::Frames(batch)).is_err() {
                dropped_batches += 1;
            }
        }

        if last_stats.elapsed() >= STATS_INTERVAL {
            last_stats = Instant::now();
            let buses: Vec<_> = sim.stats().iter().map(|(b, s)| (*b, *s)).collect();
            for (bus, s) in &buses {
                if s.error_frames > 0 && state.warned_fd_buses.insert(*bus) {
                    let _ = ev_tx.try_send(EngineEvent::Log(format!(
                        "bus {bus:?}: dropped {} CAN FD frame(s) sent on a non-FD bus",
                        s.error_frames
                    )));
                }
            }
            let _ = ev_tx.try_send(EngineEvent::Stats {
                time: Timestamp(state.virtual_ns),
                buses,
            });
            let _ = ev_tx.try_send(EngineEvent::NodeStates {
                time: Timestamp(state.virtual_ns),
                nodes: sim.node_states(),
            });
            if let Some(hw) = state.hw.as_ref() {
                let _ = ev_tx.try_send(EngineEvent::HwStatus(hw.status()));
            }
            if dropped_batches > 0 {
                let _ = ev_tx.try_send(EngineEvent::Log(format!(
                    "dropped {dropped_batches} frame batches (consumer too slow)"
                )));
                dropped_batches = 0;
            }
        }
    }
}

/// Returns `false` when the engine should shut down.
fn handle_command(cmd: Command, state: &mut EngineState, ev_tx: &Sender<EngineEvent>) -> bool {
    match cmd {
        Command::Load(topology) => match Simulation::new(&topology) {
            Ok(sim) => {
                state.hw = None;
                state.paused_at = None;
                state.realtime_only = topology.buses.iter().any(|b| b.hardware.is_some());
                if state.realtime_only && state.speed != 1.0 {
                    state.speed = 1.0;
                    let _ = ev_tx.try_send(EngineEvent::Log(REALTIME_MSG.into()));
                }
                state.sim = Some(sim);
                state.topology = Some(topology);
                state.virtual_ns = 0;
                state.run_state = RunState::Stopped;
                state.warned_fd_buses.clear();
                let _ = ev_tx.try_send(EngineEvent::Log("topology loaded".into()));
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            }
            Err(e) => {
                let _ = ev_tx.try_send(EngineEvent::Error(match e {
                    SimError::Topology(e) => format!("invalid topology: {e}"),
                    e => e.to_string(),
                }));
            }
        },
        Command::Start => {
            if state.sim.is_some() {
                if state.realtime_only && state.hw.is_none() {
                    let opened = state.topology.as_ref().map_or(Ok(None), HwBridge::open);
                    match opened {
                        Ok(bridge) => {
                            if let Some(b) = &bridge {
                                let _ = ev_tx.try_send(EngineEvent::HwStatus(b.status()));
                            }
                            state.hw = bridge;
                        }
                        Err(e) => {
                            let status = closed_status(state.topology.as_ref(), Some(&e));
                            let _ = ev_tx.try_send(EngineEvent::HwStatus(status));
                            let _ = ev_tx.try_send(EngineEvent::Error(e));
                            return true;
                        }
                    }
                }
                state.run_state = RunState::Running;
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            } else {
                let _ = ev_tx.try_send(EngineEvent::Error("no topology loaded".into()));
            }
        }
        Command::Stop => {
            if state.hw.take().is_some() {
                let status = closed_status(state.topology.as_ref(), None);
                let _ = ev_tx.try_send(EngineEvent::HwStatus(status));
            }
            state.paused_at = None;
            if let Some(topology) = &state.topology {
                state.sim = Simulation::new(topology).ok();
            }
            state.virtual_ns = 0;
            state.run_state = RunState::Stopped;
            state.warned_fd_buses.clear();
            let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
        }
        Command::Pause => {
            if state.run_state == RunState::Running {
                state.run_state = RunState::Paused;
                state.paused_at = Some(Instant::now());
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            }
        }
        Command::Resume => {
            if state.run_state == RunState::Paused {
                if let (Some(hw), Some(at)) = (state.hw.as_mut(), state.paused_at.take()) {
                    hw.shift_epoch(at.elapsed());
                }
                state.run_state = RunState::Running;
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            }
        }
        Command::SetSpeed(speed) => {
            if state.realtime_only && speed != 1.0 {
                let _ = ev_tx.try_send(EngineEvent::Log(REALTIME_MSG.into()));
            } else {
                state.speed = speed.max(0.0);
            }
        }
        Command::SendOnce(node, bus, frame) => {
            if let Some(sim) = state.sim.as_mut() {
                sim.send_once(node, bus, frame);
                for line in sim.drain_logs() {
                    let _ = ev_tx.try_send(EngineEvent::Log(line));
                }
            }
        }
        Command::GenSend { gen_id, bus, frame } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.gen_send(gen_id, bus, frame);
            }
        }
        Command::GenSetCyclic {
            gen_id,
            row,
            bus,
            frame,
            period_ns,
        } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.gen_set_cyclic(gen_id, row, bus, frame, period_ns);
            }
        }
        Command::GenUpdateFrame { gen_id, row, frame } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.gen_update_frame(gen_id, row, frame);
            }
        }
        Command::GenStopAll { gen_id } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.gen_stop_all(gen_id);
            }
        }
        Command::InjectErrors(spec) => {
            if let Some(sim) = state.sim.as_mut() {
                sim.inject_errors(spec);
            }
        }
        Command::ClearInjections => {
            if let Some(sim) = state.sim.as_mut() {
                sim.clear_injections();
            }
        }
        Command::ForceBusOff(node, bus) => {
            if let Some(sim) = state.sim.as_mut() {
                sim.force_bus_off(node, bus);
            }
        }
        Command::RecoverBusOff { node, bus } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.recover_bus_off(node, bus);
            }
        }
        Command::SetSeed(seed) => {
            if let Some(sim) = state.sim.as_mut() {
                sim.set_seed(seed);
            }
        }
        Command::SetNodeOnline { node, bus, online } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.set_node_online(node, bus, online);
            }
        }
        Command::SetMsgControl { node, id, control } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.set_msg_control(node, id, control);
            }
        }
        Command::Ecu(node, cmd) => {
            if let Some(sim) = state.sim.as_mut() {
                sim.command(node, cmd);
                for line in sim.drain_logs() {
                    let _ = ev_tx.try_send(EngineEvent::Log(line));
                }
            }
        }
        Command::DiagRequest {
            tester_bus,
            req_id,
            resp_id,
            extended,
            fd,
            payload,
            functional,
        } => {
            let fail = |msg: &str, payload: Vec<u8>| {
                let _ = ev_tx.try_send(EngineEvent::DiagResponse {
                    req: payload,
                    resp: Err(msg.to_string()),
                    elapsed_ms: 0.0,
                });
            };
            match state.sim.as_mut() {
                Some(sim) if state.run_state == RunState::Running => {
                    let spec = DiagRequestSpec {
                        bus: tester_bus,
                        req_id,
                        resp_id,
                        extended,
                        fd,
                        payload: payload.clone(),
                        functional,
                    };
                    if let Err(e) = sim.diag_request(spec) {
                        fail(&e, payload);
                    }
                }
                _ => fail("simulation is not running", payload),
            }
        }
        Command::TesterPresent {
            enable,
            tester_bus,
            req_id,
            functional_id,
            functional,
            extended,
            fd,
            period_ms,
        } => {
            if let Some(sim) = state.sim.as_mut() {
                sim.set_tester_present(TesterPresentSpec {
                    enable,
                    bus: tester_bus,
                    req_id,
                    functional_id,
                    functional,
                    extended,
                    fd,
                    period_ms,
                });
            }
        }
        Command::Shutdown => {
            state.hw = None;
            return false;
        }
    }
    true
}
