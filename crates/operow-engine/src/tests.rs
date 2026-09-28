use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use operow_core::{
    BusId, CanBusConfig, CanFrame, EcuConfig, Link, NodeId, Timestamp, Topology, TxMessage,
};

use crate::ecu::{Ecu, EcuCtx};
use crate::runner::{Command, Engine, EngineEvent};
use crate::sim::Simulation;
use crate::timing::{frame_bits, frame_duration_ns};

fn topo_with_two_senders() -> Topology {
    Topology {
        nodes: vec![
            EcuConfig {
                id: NodeId(1),
                name: "A".into(),
                tx: vec![TxMessage {
                    name: "MsgHigh".into(),
                    frame: CanFrame::new(0x200, false, &[0]).unwrap(),
                    period_ms: 1000,
                    enabled: true,
                }],
                pos: (0.0, 0.0),
            },
            EcuConfig {
                id: NodeId(2),
                name: "B".into(),
                tx: vec![TxMessage {
                    name: "MsgLow".into(),
                    frame: CanFrame::new(0x100, false, &[0]).unwrap(),
                    period_ms: 1000,
                    enabled: true,
                }],
                pos: (0.0, 0.0),
            },
        ],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
        }],
        links: vec![
            Link {
                node: NodeId(1),
                bus: BusId(1),
            },
            Link {
                node: NodeId(2),
                bus: BusId(1),
            },
        ],
    }
}

#[test]
fn arbitration_lowest_id_wins() {
    let topo = topo_with_two_senders();
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(1), &mut out);

    assert_eq!(out.len(), 2, "both frames should have gone out by t=1ms");
    assert_eq!(
        out[0].frame.id, 0x100,
        "the lower arbitration id (0x100) must win and be transmitted first"
    );
    assert_eq!(out[1].frame.id, 0x200);
}

#[test]
fn frame_timing_matches_formula() {
    let frame = CanFrame::new(0x100, false, &[0u8; 8]).unwrap();
    let bits = frame_bits(&frame);
    assert_eq!(bits, 135);
    assert_eq!(frame_duration_ns(&frame, 500_000), bits as u64 * 2000);
}

#[test]
fn periodic_message_produces_expected_frame_count() {
    let topo = Topology {
        nodes: vec![EcuConfig {
            id: NodeId(1),
            name: "A".into(),
            tx: vec![TxMessage {
                name: "Heartbeat".into(),
                frame: CanFrame::new(0x123, false, &[]).unwrap(),
                period_ms: 10,
                enabled: true,
            }],
            pos: (0.0, 0.0),
        }],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
        }],
        links: vec![Link {
            node: NodeId(1),
            bus: BusId(1),
        }],
    };

    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    // Messages are *started* at t=0, 10, ..., 1000ms (101 sends), but each
    // occupies the bus for a nonzero `frame_duration_ns` before the
    // completion (BusEvent) is emitted. To observe the completion of the
    // message started at exactly t=1000ms we must run slightly past it.
    let frame = CanFrame::new(0x123, false, &[]).unwrap();
    let duration_ns = frame_duration_ns(&frame, 500_000);
    sim.run_until(Timestamp(1_000_000_000 + duration_ns), &mut out);

    // 0, 10, 20, ..., 1000ms -> 101 frames.
    assert_eq!(out.len(), 101);
}

/// Test ECU that counts how many times it receives a frame from the bus.
struct CountingEcu {
    counter: Arc<AtomicU32>,
}

impl Ecu for CountingEcu {
    fn on_frame(&mut self, _bus: BusId, _frame: &CanFrame, _ctx: &mut EcuCtx) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn sender_does_not_receive_its_own_frame() {
    let topo = Topology {
        nodes: vec![
            EcuConfig {
                id: NodeId(1),
                name: "Sender".into(),
                tx: vec![TxMessage {
                    name: "Msg".into(),
                    frame: CanFrame::new(0x10, false, &[1]).unwrap(),
                    period_ms: 10,
                    enabled: true,
                }],
                pos: (0.0, 0.0),
            },
            EcuConfig {
                id: NodeId(2),
                name: "Receiver".into(),
                tx: vec![],
                pos: (0.0, 0.0),
            },
        ],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
        }],
        links: vec![
            Link {
                node: NodeId(1),
                bus: BusId(1),
            },
            Link {
                node: NodeId(2),
                bus: BusId(1),
            },
        ],
    };

    let sender_count = Arc::new(AtomicU32::new(0));
    let receiver_count = Arc::new(AtomicU32::new(0));

    let mut sim = Simulation::new(&topo).unwrap();
    sim.set_ecu(
        NodeId(1),
        Box::new(CountingEcu {
            counter: sender_count.clone(),
        }),
    );
    // Node 1's PeriodicEcu behavior is replaced by CountingEcu, which never
    // sends on its own, so drive its transmissions manually instead.
    sim.set_ecu(
        NodeId(2),
        Box::new(CountingEcu {
            counter: receiver_count.clone(),
        }),
    );

    let mut out = Vec::new();
    for _ in 0..5 {
        sim.send_once(NodeId(1), CanFrame::new(0x10, false, &[1]).unwrap());
    }
    sim.run_until(Timestamp::from_ms(10), &mut out);

    assert!(out.len() >= 5);
    assert_eq!(
        sender_count.load(Ordering::SeqCst),
        0,
        "sender must never observe its own frame via on_frame"
    );
    assert_eq!(receiver_count.load(Ordering::SeqCst), out.len() as u32);
}

#[test]
fn runner_smoke_test() {
    let topo = topo_with_two_senders();
    let handle = Engine::spawn();

    handle.cmd.send(Command::Load(topo)).unwrap();
    handle.cmd.send(Command::SetSpeed(0.0)).unwrap();
    handle.cmd.send(Command::Start).unwrap();

    let mut got_frames = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if let Ok(ev) = handle.events.recv_timeout(Duration::from_millis(100)) {
            match ev {
                EngineEvent::Frames(frames) if !frames.is_empty() => {
                    got_frames = true;
                    break;
                }
                EngineEvent::Error(e) => panic!("engine reported error: {e}"),
                _ => {}
            }
        }
    }
    assert!(got_frames, "expected at least one BusEvent batch within 1s");

    handle.cmd.send(Command::Shutdown).unwrap();
    handle.join();
}
