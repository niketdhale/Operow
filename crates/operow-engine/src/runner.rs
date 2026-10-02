use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};

use operow_core::{BusEvent, BusId, CanFrame, NodeId, Timestamp, Topology};

use crate::sim::{BusStats, Simulation};

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
    Shutdown,
}

/// Events emitted by the engine thread.
pub enum EngineEvent {
    Frames(Vec<BusEvent>),
    Stats {
        time: Timestamp,
        buses: Vec<(BusId, BusStats)>,
    },
    State(RunState),
    Log(String),
    Error(String),
}

const EVENT_CHANNEL_CAPACITY: usize = 1024;
const TICK: Duration = Duration::from_millis(5);
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
}

fn engine_loop(cmd_rx: Receiver<Command>, ev_tx: Sender<EngineEvent>) {
    let mut state = EngineState {
        topology: None,
        sim: None,
        run_state: RunState::Stopped,
        speed: 1.0,
        virtual_ns: 0,
        warned_fd_buses: std::collections::HashSet::new(),
    };
    let mut last_tick = Instant::now();
    let mut last_stats = Instant::now();
    let mut frames_buf = Vec::new();
    let mut dropped_batches: u64 = 0;

    loop {
        match cmd_rx.recv_timeout(TICK) {
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
            last_tick = Instant::now();
            continue;
        }

        let Some(sim) = state.sim.as_mut() else {
            last_tick = Instant::now();
            continue;
        };

        let elapsed = last_tick.elapsed();
        last_tick = Instant::now();
        let advance_ns = if state.speed == 0.0 {
            FAST_FORWARD_STEP_NS
        } else {
            (elapsed.as_secs_f64() * state.speed * 1_000_000_000.0).round() as u64
        };
        state.virtual_ns = state.virtual_ns.saturating_add(advance_ns);

        sim.run_until(Timestamp(state.virtual_ns), &mut frames_buf);
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
                state.sim = Some(sim);
                state.topology = Some(topology);
                state.virtual_ns = 0;
                state.run_state = RunState::Stopped;
                state.warned_fd_buses.clear();
                let _ = ev_tx.try_send(EngineEvent::Log("topology loaded".into()));
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            }
            Err(e) => {
                let _ = ev_tx.try_send(EngineEvent::Error(format!("invalid topology: {e}")));
            }
        },
        Command::Start => {
            if state.sim.is_some() {
                state.run_state = RunState::Running;
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            } else {
                let _ = ev_tx.try_send(EngineEvent::Error("no topology loaded".into()));
            }
        }
        Command::Stop => {
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
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            }
        }
        Command::Resume => {
            if state.run_state == RunState::Paused {
                state.run_state = RunState::Running;
                let _ = ev_tx.try_send(EngineEvent::State(state.run_state));
            }
        }
        Command::SetSpeed(speed) => state.speed = speed.max(0.0),
        Command::SendOnce(node, bus, frame) => {
            if let Some(sim) = state.sim.as_mut() {
                sim.send_once(node, bus, frame);
            }
        }
        Command::Shutdown => return false,
    }
    true
}
