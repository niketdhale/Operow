use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use operow_core::{
    BusId, CanBusConfig, CanFrame, Direction, EcuConfig, IdFilter, Link, NodeId, NodeKind,
    RouteRule, Timestamp, Topology, TxMessage,
};

use crate::ecu::{Ecu, EcuCtx};
use crate::runner::{Command, Engine, EngineEvent};
use crate::sim::{MAX_HOPS, Simulation};
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
                    bus: None,
                }],
                kind: Default::default(),
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
                    bus: None,
                }],
                kind: Default::default(),
                pos: (0.0, 0.0),
            },
        ],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
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
                bus: None,
            }],
            kind: Default::default(),
            pos: (0.0, 0.0),
        }],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
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
                    bus: None,
                }],
                kind: Default::default(),
                pos: (0.0, 0.0),
            },
            EcuConfig {
                id: NodeId(2),
                name: "Receiver".into(),
                tx: vec![],
                kind: Default::default(),
                pos: (0.0, 0.0),
            },
        ],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
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
        sim.send_once(NodeId(1), None, CanFrame::new(0x10, false, &[1]).unwrap());
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

fn topo_single_node(fd_enabled: bool) -> Topology {
    Topology {
        nodes: vec![EcuConfig {
            id: NodeId(1),
            name: "A".into(),
            tx: vec![],
            kind: Default::default(),
            pos: (0.0, 0.0),
        }],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled,
            data_bitrate: 2_000_000,
        }],
        links: vec![Link {
            node: NodeId(1),
            bus: BusId(1),
        }],
    }
}

#[test]
fn fd_frame_rejected_on_non_fd_bus_increments_error_frames() {
    let topo = topo_single_node(false);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();

    let fd_frame = CanFrame::new_fd(0x123, false, true, &[0u8; 64]).unwrap();
    sim.send_once(NodeId(1), None, fd_frame);
    sim.run_until(Timestamp::from_ms(10), &mut out);

    assert!(out.is_empty(), "FD frame must not be transmitted");
    let stats = sim.stats().get(&BusId(1)).unwrap();
    assert_eq!(stats.error_frames, 1);
    assert_eq!(stats.frames, 0);
}

#[test]
fn fd_frame_transmitted_on_fd_enabled_bus() {
    let topo = topo_single_node(true);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();

    let fd_frame = CanFrame::new_fd(0x123, false, true, &[0u8; 64]).unwrap();
    sim.send_once(NodeId(1), None, fd_frame);
    sim.run_until(Timestamp::from_ms(10), &mut out);

    assert_eq!(out.len(), 1);
    assert!(out[0].frame.fd);
    let stats = sim.stats().get(&BusId(1)).unwrap();
    assert_eq!(stats.error_frames, 0);
    assert_eq!(stats.frames, 1);
    // 34 nominal bits @500k + 678 data bits @2M = 407_000 ns.
    assert_eq!(stats.busy_ns, 407_000);
}

fn bus(id: u32, name: &str) -> CanBusConfig {
    CanBusConfig {
        id: BusId(id),
        name: name.into(),
        bitrate: 500_000,
        fd_enabled: false,
        data_bitrate: 2_000_000,
    }
}

fn node(id: u32, tx: Vec<TxMessage>, kind: NodeKind) -> EcuConfig {
    EcuConfig {
        id: NodeId(id),
        name: format!("N{id}"),
        tx,
        kind,
        pos: (0.0, 0.0),
    }
}

fn link(node: u32, bus: u32) -> Link {
    Link {
        node: NodeId(node),
        bus: BusId(bus),
    }
}

fn route(from: u32, to: u32, filter: IdFilter, remap_id: Option<u32>, delay_us: u32) -> RouteRule {
    RouteRule {
        from_bus: BusId(from),
        to_buses: vec![BusId(to)],
        filter,
        remap_id,
        delay_us,
    }
}

fn periodic(id: u32, period_ms: u32, bus: Option<u32>) -> TxMessage {
    TxMessage {
        name: "M".into(),
        frame: CanFrame::new(id, false, &[1]).unwrap(),
        period_ms,
        enabled: true,
        bus: bus.map(BusId),
    }
}

/// ECU 1 on bus 1 (sends 0x100 once), ECU 2 on bus 2, gateway 3 on both.
fn gateway_topo(routes: Vec<RouteRule>) -> Topology {
    Topology {
        nodes: vec![
            node(1, vec![periodic(0x100, 1000, None)], NodeKind::Ecu),
            node(2, vec![], NodeKind::Ecu),
            node(3, vec![], NodeKind::Gateway { routes }),
        ],
        buses: vec![bus(1, "A"), bus(2, "B")],
        links: vec![link(1, 1), link(2, 2), link(3, 1), link(3, 2)],
    }
}

fn run_ms(topo: &Topology, ms: u64) -> Vec<operow_core::BusEvent> {
    let mut sim = Simulation::new(topo).unwrap();
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(ms), &mut out);
    out
}

#[test]
fn send_on_only_hits_chosen_bus() {
    let topo = Topology {
        nodes: vec![node(1, vec![periodic(0x100, 1000, Some(2))], NodeKind::Ecu)],
        buses: vec![bus(1, "A"), bus(2, "B")],
        links: vec![link(1, 1), link(1, 2)],
    };
    let out = run_ms(&topo, 5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].bus, BusId(2));

    // send_on to a bus the node is not linked to is silently dropped.
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.send_once(
        NodeId(1),
        Some(BusId(9)),
        CanFrame::new(0x10, false, &[]).unwrap(),
    );
    sim.run_until(Timestamp::from_ms(1), &mut out);
    assert!(out.iter().all(|e| e.frame.id != 0x10));
}

#[test]
fn send_fans_out_with_shared_uid() {
    let topo = Topology {
        nodes: vec![node(1, vec![periodic(0x100, 1000, None)], NodeKind::Ecu)],
        buses: vec![bus(1, "A"), bus(2, "B")],
        links: vec![link(1, 1), link(1, 2)],
    };
    let out = run_ms(&topo, 5);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].frame_uid, out[1].frame_uid);
    assert!(out.iter().all(|e| e.hop == 0 && e.dir == Direction::Tx));
}

#[test]
fn gateway_forwards_exact_and_preserves_meta() {
    let filter = IdFilter::Exact {
        id: 0x100,
        extended: false,
    };
    let out = run_ms(&gateway_topo(vec![route(1, 2, filter, None, 0)]), 5);
    assert_eq!(out.len(), 2);
    let (tx, rx) = (&out[0], &out[1]);
    assert_eq!((tx.bus, tx.dir, tx.hop), (BusId(1), Direction::Tx, 0));
    assert_eq!(tx.sender, NodeId(1));
    assert_eq!((rx.bus, rx.dir, rx.hop), (BusId(2), Direction::Rx, 1));
    assert_eq!(rx.frame_uid, tx.frame_uid);
    assert_eq!(rx.origin, NodeId(1));
    assert_eq!(rx.sender, NodeId(3));
    assert_eq!(rx.frame.id, 0x100);
}

#[test]
fn gateway_mask_and_non_matching() {
    let hit = IdFilter::Mask {
        id: 0x100,
        mask: 0x700,
    };
    assert_eq!(
        run_ms(&gateway_topo(vec![route(1, 2, hit, None, 0)]), 5).len(),
        2
    );
    let miss = IdFilter::Mask {
        id: 0x200,
        mask: 0x700,
    };
    assert_eq!(
        run_ms(&gateway_topo(vec![route(1, 2, miss, None, 0)]), 5).len(),
        1
    );
}

#[test]
fn gateway_remaps_id() {
    let out = run_ms(
        &gateway_topo(vec![route(1, 2, IdFilter::Any, Some(0x555), 0)]),
        5,
    );
    assert_eq!(out[0].frame.id, 0x100);
    assert_eq!(out[1].frame.id, 0x555);
}

#[test]
fn gateway_delay_is_respected() {
    let now = run_ms(&gateway_topo(vec![route(1, 2, IdFilter::Any, None, 0)]), 5);
    let later = run_ms(
        &gateway_topo(vec![route(1, 2, IdFilter::Any, None, 500)]),
        5,
    );
    assert_eq!(later.len(), 2);
    assert_eq!(later[1].frame_uid, later[0].frame_uid);
    // The delay shifts the forwarded frame's completion by exactly 500us.
    assert_eq!(later[1].time.0 - now[1].time.0, 500_000);
}

#[test]
fn gateways_forwarding_to_each_other_stop_at_max_hops() {
    let topo = Topology {
        nodes: vec![
            node(1, vec![periodic(0x100, 100_000, Some(1))], NodeKind::Ecu),
            node(
                3,
                vec![],
                NodeKind::Gateway {
                    routes: vec![route(1, 2, IdFilter::Any, None, 0)],
                },
            ),
            node(
                4,
                vec![],
                NodeKind::Gateway {
                    routes: vec![route(2, 1, IdFilter::Any, None, 0)],
                },
            ),
        ],
        buses: vec![bus(1, "A"), bus(2, "B")],
        links: vec![link(1, 1), link(3, 1), link(3, 2), link(4, 1), link(4, 2)],
    };
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(50), &mut out);
    let hops: Vec<u8> = out.iter().map(|e| e.hop).collect();
    assert_eq!(hops, (0..=MAX_HOPS).collect::<Vec<_>>());
    assert!(out.iter().all(|e| e.frame_uid == out[0].frame_uid));
    let drops: u64 = sim.stats().values().map(|s| s.routing_drops).sum();
    assert_eq!(drops, 1);
}
