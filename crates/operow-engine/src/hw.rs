//! Real-time bridge between a [`Simulation`] and the hardware channels of the
//! buses bound to an adapter (see `CanBusConfig::hardware`).
//!
//! Each hardware bus gets a worker thread that owns the opened channel: it
//! reads frames (stamped with the arrival `Instant`), transmits the frames
//! the simulation queued and polls the controller state. The owner of the
//! bridge calls [`HwBridge::step`] regularly; virtual time is the wall clock
//! since the bridge was opened.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use operow_core::{BusEvent, BusId, CanFrame, NodeErrorState, Timestamp, Topology};
use operow_hw::{CanChannel, ChannelConfig, HwBusState, RxFrame, open_channel};

use crate::sim::Simulation;

/// How long a worker waits for a frame before servicing its transmit queue;
/// this bounds the transmit latency.
const WORKER_POLL: Duration = Duration::from_millis(1);
/// How often a worker reads the controller state.
const STATE_INTERVAL: Duration = Duration::from_millis(500);

/// Something the owner of a bridge should report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HwNotice {
    Log(String),
    Error(String),
}

/// Whether a hardware bus's channel is usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HwLink {
    /// Opened and serviced by its worker.
    Open,
    /// Not open (the measurement is stopped).
    Closed,
    /// Could not be opened, or failed while running.
    Error(String),
}

/// Live state of one hardware bus, reported with every statistics update
/// (see `EngineEvent::HwStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwBusStatus {
    pub bus: BusId,
    pub interface: String,
    pub listen_only: bool,
    pub link: HwLink,
    /// Controller fault-confinement state and counters, as last polled.
    pub controller: HwBusState,
    /// Frames received from the real bus (echoes excluded).
    pub rx_frames: u64,
    /// Frames handed to the adapter for transmission.
    pub tx_frames: u64,
}

enum WorkerMsg {
    Rx(BusId, RxFrame, Instant),
    State(BusId, HwBusState),
    TxFailed(BusId, String),
    Failed(BusId, String),
}

struct Worker {
    bus: BusId,
    label: String,
    tx: Sender<CanFrame>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    state: NodeErrorState,
    dead: bool,
    interface: String,
    listen_only: bool,
    controller: HwBusState,
    error: Option<String>,
    rx_frames: u64,
    tx_frames: u64,
}

/// Opened hardware channels of a topology and their worker threads. Dropping
/// the bridge stops the workers and closes the channels.
pub struct HwBridge {
    epoch: Instant,
    workers: Vec<Worker>,
    rx: Receiver<WorkerMsg>,
}

impl HwBridge {
    /// Open the channel of every hardware bus of `topology`. `Ok(None)` when
    /// no bus is bound to hardware. On error (naming bus and interface)
    /// nothing stays open.
    pub fn open(topology: &Topology) -> Result<Option<HwBridge>, String> {
        let (msg_tx, msg_rx) = unbounded();
        let mut workers = Vec::new();
        for bus in &topology.buses {
            let Some(hw) = &bus.hardware else { continue };
            let cfg = ChannelConfig {
                interface: hw.interface.clone(),
                bitrate: bus.bitrate,
                fd: bus.fd_enabled,
                data_bitrate: bus.data_bitrate,
                listen_only: hw.listen_only,
                receive_own: hw.receive_own,
            };
            let channel = open_channel(&cfg)
                .map_err(|e| format!("bus {}: cannot open {}: {e}", bus.name, hw.interface))?;
            let (tx, tx_rx) = unbounded();
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let (stop, out, id) = (stop.clone(), msg_tx.clone(), bus.id);
                thread::Builder::new()
                    .name(format!("hw-{}", bus.name))
                    .spawn(move || worker(id, channel, tx_rx, out, stop))
                    .map_err(|e| format!("bus {}: cannot start reader thread: {e}", bus.name))?
            };
            workers.push(Worker {
                bus: bus.id,
                label: format!("bus {} ({})", bus.name, hw.interface),
                tx,
                stop,
                thread: Some(thread),
                state: NodeErrorState::ErrorActive,
                dead: false,
                interface: hw.interface.clone(),
                listen_only: hw.listen_only,
                controller: HwBusState::default(),
                error: None,
                rx_frames: 0,
                tx_frames: 0,
            });
        }
        if workers.is_empty() {
            return Ok(None);
        }
        Ok(Some(HwBridge {
            epoch: Instant::now(),
            workers,
            rx: msg_rx,
        }))
    }

    /// Virtual nanoseconds: wall time since the bridge was opened (minus
    /// paused time, see [`HwBridge::shift_epoch`]).
    pub fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    /// Exclude `paused` from virtual time (call on resume).
    pub fn shift_epoch(&mut self, paused: Duration) {
        self.epoch += paused;
    }

    /// Drop everything received so far (used while paused).
    pub fn discard_pending(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }

    fn worker_mut(&mut self, bus: BusId) -> Option<&mut Worker> {
        self.workers.iter_mut().find(|w| w.bus == bus)
    }

    /// Hand received frames to `sim`, advance it to the wall clock, then
    /// forward the frames it wants on real buses. Returns the new virtual
    /// time and what to report.
    pub fn step(&mut self, sim: &mut Simulation, out: &mut Vec<BusEvent>) -> (u64, Vec<HwNotice>) {
        let now = self.now_ns();
        let mut notices = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Rx(bus, rx, at) => {
                    if rx.is_echo {
                        continue; // recorded when it was sent
                    }
                    if let Some(w) = self.worker_mut(bus) {
                        w.rx_frames += 1;
                    }
                    let at_ns = at.saturating_duration_since(self.epoch).as_nanos() as u64;
                    sim.hw_rx(bus, rx.frame, at_ns.min(now), rx.error);
                }
                WorkerMsg::State(bus, st) => {
                    if let Some(w) = self.worker_mut(bus) {
                        w.controller = st;
                    }
                    if let Some(w) = self.worker_mut(bus)
                        && w.state != st.state
                    {
                        w.state = st.state;
                        notices.push(HwNotice::Log(format!(
                            "{}: controller is now {} (TEC {}, REC {})",
                            w.label,
                            st.state.label(),
                            st.tec,
                            st.rec
                        )));
                    }
                }
                WorkerMsg::TxFailed(bus, e) => {
                    if let Some(w) = self.worker_mut(bus) {
                        notices.push(HwNotice::Log(format!("{}: transmit failed: {e}", w.label)));
                    }
                }
                WorkerMsg::Failed(bus, e) => {
                    if let Some(w) = self.worker_mut(bus) {
                        w.dead = true;
                        w.error = Some(e.clone());
                        notices.push(HwNotice::Error(format!("{}: {e}", w.label)));
                    }
                }
            }
        }
        sim.run_until(Timestamp(now), out);
        for (bus, frame) in sim.take_hw_tx() {
            if let Some(w) = self.worker_mut(bus)
                && !w.dead
            {
                w.tx_frames += 1;
                let _ = w.tx.send(frame);
            }
        }
        (now, notices)
    }

    /// Live state of every hardware bus.
    pub fn status(&self) -> Vec<HwBusStatus> {
        self.workers
            .iter()
            .map(|w| HwBusStatus {
                bus: w.bus,
                interface: w.interface.clone(),
                listen_only: w.listen_only,
                link: match &w.error {
                    Some(e) => HwLink::Error(e.clone()),
                    None => HwLink::Open,
                },
                controller: w.controller,
                rx_frames: w.rx_frames,
                tx_frames: w.tx_frames,
            })
            .collect()
    }

    /// Stop the workers and close the channels.
    pub fn close(self) {}
}

impl Drop for HwBridge {
    fn drop(&mut self) {
        for w in &self.workers {
            w.stop.store(true, Ordering::Relaxed);
        }
        for w in &mut self.workers {
            if let Some(t) = w.thread.take() {
                let _ = t.join();
            }
        }
    }
}

fn worker(
    bus: BusId,
    mut ch: Box<dyn CanChannel>,
    tx_rx: Receiver<CanFrame>,
    out: Sender<WorkerMsg>,
    stop: Arc<AtomicBool>,
) {
    let mut last_poll = Instant::now();
    let mut tx_failing = false;
    while !stop.load(Ordering::Relaxed) {
        while let Ok(frame) = tx_rx.try_recv() {
            match ch.send(&frame) {
                Ok(()) => tx_failing = false,
                Err(e) => {
                    if !tx_failing {
                        let _ = out.send(WorkerMsg::TxFailed(bus, e.to_string()));
                    }
                    tx_failing = true;
                }
            }
        }
        match ch.recv(WORKER_POLL) {
            Ok(Some(rx)) => {
                let _ = out.send(WorkerMsg::Rx(bus, rx, Instant::now()));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = out.send(WorkerMsg::Failed(bus, e.to_string()));
                break;
            }
        }
        if last_poll.elapsed() >= STATE_INTERVAL {
            last_poll = Instant::now();
            if let Ok(st) = ch.bus_state() {
                let _ = out.send(WorkerMsg::State(bus, st));
            }
        }
    }
    ch.close();
}
