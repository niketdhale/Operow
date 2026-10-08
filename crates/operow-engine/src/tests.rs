use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use operow_core::{
    BusEvent, BusId, CanBusConfig, CanErrorKind, CanFrame, Direction, EcuConfig, IdFilter, Link,
    NodeErrorState, NodeId, NodeKind, RouteRule, SendType, Timestamp, Topology, TxMessage,
};

use crate::ecu::{Ecu, EcuCommand, EcuCtx};
use crate::runner::{Command, Engine, EngineEvent};
use crate::sim::{
    GENERATOR_NODE_BASE, GeneratorId, InjectMode, InjectSpec, MAX_HOPS, MsgControl, Simulation,
};
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
                    send_type: Default::default(),
                }],
                kind: Default::default(),
                pos: (0.0, 0.0),
                script: None,
                diag: None,
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
                    send_type: Default::default(),
                }],
                kind: Default::default(),
                pos: (0.0, 0.0),
                script: None,
                diag: None,
            },
        ],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            hardware: None,
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
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
                send_type: Default::default(),
            }],
            kind: Default::default(),
            pos: (0.0, 0.0),
            script: None,
            diag: None,
        }],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            hardware: None,
        }],
        links: vec![Link {
            node: NodeId(1),
            bus: BusId(1),
        }],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
                    send_type: Default::default(),
                }],
                kind: Default::default(),
                pos: (0.0, 0.0),
                script: None,
                diag: None,
            },
            EcuConfig {
                id: NodeId(2),
                name: "Receiver".into(),
                tx: vec![],
                kind: Default::default(),
                pos: (0.0, 0.0),
                script: None,
                diag: None,
            },
        ],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            hardware: None,
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
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
            script: None,
            diag: None,
        }],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            hardware: None,
        }],
        links: vec![Link {
            node: NodeId(1),
            bus: BusId(1),
        }],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
        simulate_ack: false,
        hardware: None,
    }
}

fn node(id: u32, tx: Vec<TxMessage>, kind: NodeKind) -> EcuConfig {
    EcuConfig {
        id: NodeId(id),
        name: format!("N{id}"),
        tx,
        kind,
        pos: (0.0, 0.0),
        script: None,
        diag: None,
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
        send_type: Default::default(),
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
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
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

// ---- send types ----

const MS: u64 = 1_000_000;

fn send_type_sim(send_type: SendType, enabled: bool) -> Simulation {
    let mut msg = periodic(0x200, 10, Some(1));
    msg.send_type = send_type;
    msg.enabled = enabled;
    let topo = Topology {
        nodes: vec![node(1, vec![msg], NodeKind::Ecu)],
        buses: vec![bus(1, "A")],
        links: vec![link(1, 1)],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
    };
    Simulation::new(&topo).unwrap()
}

/// Run to `ms` (plus slack for the last frame to finish) and return events.
fn run_to(sim: &mut Simulation, ms: u64) -> Vec<operow_core::BusEvent> {
    let mut out = Vec::new();
    sim.run_until(Timestamp(ms * MS + MS / 2), &mut out);
    out
}

fn set(sim: &mut Simulation, byte: u8) {
    sim.command(
        NodeId(1),
        EcuCommand::SetPayload {
            msg: 0,
            data: vec![byte],
        },
    );
}

#[test]
fn send_type_cyclic_counts() {
    let mut sim = send_type_sim(SendType::Cyclic, true);
    assert_eq!(run_to(&mut sim, 100).len(), 11);
    // Triggers do nothing for cyclic.
    sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
    assert_eq!(run_to(&mut sim, 100).len(), 0);
}

#[test]
fn send_type_disabled_suppresses_everything() {
    for st in [
        SendType::Cyclic,
        SendType::Event,
        SendType::OnChange { min_gap_ms: 0 },
        SendType::CyclicIfActive,
        SendType::CyclicAndEvent,
    ] {
        let mut sim = send_type_sim(st, false);
        sim.command(
            NodeId(1),
            EcuCommand::SetActive {
                msg: 0,
                active: true,
            },
        );
        sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
        set(&mut sim, 9);
        assert_eq!(run_to(&mut sim, 50).len(), 0, "{st:?}");
    }
}

#[test]
fn send_type_event_only_on_trigger() {
    let mut sim = send_type_sim(SendType::Event, true);
    assert_eq!(run_to(&mut sim, 100).len(), 0);
    // Setting the payload alone does not send.
    set(&mut sim, 5);
    assert_eq!(run_to(&mut sim, 110).len(), 0);
    sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
    let out = run_to(&mut sim, 120);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frame.payload(), [5]);
    assert_eq!(run_to(&mut sim, 300).len(), 0);
}

#[test]
fn send_type_on_change() {
    let mut sim = send_type_sim(SendType::OnChange { min_gap_ms: 50 }, true);
    assert_eq!(run_to(&mut sim, 100).len(), 0);
    // Same payload (initial is [1]): no send.
    set(&mut sim, 1);
    assert_eq!(run_to(&mut sim, 101).len(), 0);
    // Change: immediate send.
    set(&mut sim, 2);
    let out = run_to(&mut sim, 102);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frame.payload(), [2]);
    let first = out[0].time.0;
    // Two changes within the gap coalesce into one deferred send of the
    // latest payload, 50ms after the first.
    set(&mut sim, 3);
    set(&mut sim, 4);
    assert_eq!(run_to(&mut sim, 140).len(), 0);
    let out = run_to(&mut sim, 200);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frame.payload(), [4]);
    assert_eq!(out[0].time.0 - first, 50 * MS);
    // After the gap has elapsed a change sends immediately again.
    assert_eq!(run_to(&mut sim, 210).len(), 0);
    set(&mut sim, 5);
    assert_eq!(run_to(&mut sim, 211).len(), 1);
}

#[test]
fn send_type_cyclic_if_active_toggle_no_duplicates() {
    let mut sim = send_type_sim(SendType::CyclicIfActive, true);
    assert_eq!(run_to(&mut sim, 100).len(), 0);
    let active = |a| EcuCommand::SetActive { msg: 0, active: a };
    sim.command(NodeId(1), active(true));
    // Sends at 100.5, 110.5, ..., 150.5.
    assert_eq!(run_to(&mut sim, 155).len(), 6);
    sim.command(NodeId(1), active(false));
    assert_eq!(run_to(&mut sim, 250).len(), 0);
    // Toggle off/on at the same instant while a timer is pending, then on
    // again later: exactly one chain at 10ms period.
    sim.command(NodeId(1), active(true));
    sim.command(NodeId(1), active(false));
    sim.command(NodeId(1), active(true));
    // One chain only (no doubled sends).
    assert_eq!(run_to(&mut sim, 300).len(), 5);
    assert_eq!(run_to(&mut sim, 400).len(), 10);
}

#[test]
fn send_type_cyclic_and_event() {
    let mut sim = send_type_sim(SendType::CyclicAndEvent, true);
    // 0, 10, ..., 100.
    assert_eq!(run_to(&mut sim, 100).len(), 11);
    sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
    set(&mut sim, 7);
    let out = run_to(&mut sim, 102);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].frame.payload(), [7]);
    // The cycle is unaffected and now uses the new payload.
    let out = run_to(&mut sim, 110);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frame.payload(), [7]);
}

#[test]
fn set_payload_keeps_id_and_ignores_extra_bytes() {
    let mut msg = periodic(0x321, 10, Some(1));
    msg.send_type = SendType::Event;
    msg.frame = CanFrame::new(0x321, false, &[1, 2, 3]).unwrap();
    let topo = Topology {
        nodes: vec![node(1, vec![msg], NodeKind::Ecu)],
        buses: vec![bus(1, "A")],
        links: vec![link(1, 1)],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
    };
    let mut sim = Simulation::new(&topo).unwrap();
    // Shorter keeps the rest; longer is truncated to the frame length.
    sim.command(
        NodeId(1),
        EcuCommand::SetPayload {
            msg: 0,
            data: vec![9],
        },
    );
    sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
    let out = run_to(&mut sim, 1);
    assert_eq!(out[0].frame.id, 0x321);
    assert_eq!(out[0].frame.payload(), [9, 2, 3]);
    sim.command(
        NodeId(1),
        EcuCommand::SetPayload {
            msg: 0,
            data: vec![4, 5, 6, 7, 8],
        },
    );
    sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
    let out = run_to(&mut sim, 2);
    assert_eq!(out[0].frame.payload(), [4, 5, 6]);
}

#[test]
fn gateway_delegates_commands_to_own_tx() {
    let mut msg = periodic(0x400, 10, None);
    msg.send_type = SendType::Event;
    let topo = Topology {
        nodes: vec![node(1, vec![msg], NodeKind::Gateway { routes: Vec::new() })],
        buses: vec![bus(1, "A")],
        links: vec![link(1, 1)],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
    };
    let mut sim = Simulation::new(&topo).unwrap();
    assert_eq!(run_to(&mut sim, 50).len(), 0);
    sim.command(NodeId(1), EcuCommand::Trigger { msg: 0 });
    assert_eq!(run_to(&mut sim, 51).len(), 1);
}

// ---- scripting ----

fn script_topo(script: &str, tx: Vec<TxMessage>) -> Topology {
    let mut s = node(1, tx, NodeKind::Ecu);
    s.script = Some(script.into());
    Topology {
        // Node 2 sends 0x100 once at t=0 on bus 1.
        nodes: vec![
            s,
            node(2, vec![periodic(0x100, 1000, Some(1))], NodeKind::Ecu),
        ],
        buses: vec![bus(1, "A"), bus(2, "B")],
        links: vec![link(1, 1), link(1, 2), link(2, 1)],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
    }
}

fn ids(events: &[operow_core::BusEvent]) -> Vec<(u32, u32)> {
    events.iter().map(|e| (e.bus.0, e.frame.id)).collect()
}

#[test]
fn script_echoes_id_plus_one() {
    let script = r#"
        fn on_message(msg) {
            if msg.id == 0x100 {
                output(#{ id: msg.id + 1, data: msg.data, bus: msg.bus });
            }
        }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    let out = run_to(&mut sim, 10);
    assert_eq!(ids(&out), vec![(1, 0x100), (1, 0x101)]);
    assert_eq!(out[1].frame.payload(), [1]);
}

#[test]
fn script_timer_periodic_send() {
    let script = r#"
        fn on_start() { set_timer(1, 10); }
        fn on_timer(id) {
            output(#{ id: 0x300 + id, data: [now_ms()] , bus: 2});
            set_timer(id, 10);
        }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    let out: Vec<_> = run_to(&mut sim, 35)
        .into_iter()
        .filter(|e| e.frame.id == 0x301)
        .collect();
    assert_eq!(out.len(), 3);
    assert_eq!(out[2].frame.payload(), [30]);
}

#[test]
fn script_state_persists_across_calls() {
    let script = r#"
        fn on_start() { this.count = 0; set_timer(1, 10); }
        fn on_timer(id) {
            this.count += 1;
            output(#{ id: 0x400, data: [this.count], bus: 2 });
            set_timer(1, 10);
        }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    let out: Vec<_> = run_to(&mut sim, 35)
        .into_iter()
        .filter(|e| e.frame.id == 0x400)
        .collect();
    let counts: Vec<u8> = out.iter().map(|e| e.frame.payload()[0]).collect();
    assert_eq!(counts, [1, 2, 3]);
}

#[test]
fn script_output_bus_field_selects_bus() {
    let script = r#"
        fn on_start() {
            output(#{ id: 0x500, bus: 2 });
            output(#{ id: 0x501 });
        }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    let out = run_to(&mut sim, 10);
    let mut got = ids(&out);
    got.retain(|(_, id)| *id >= 0x500);
    got.sort();
    assert_eq!(got, vec![(1, 0x501), (2, 0x500), (2, 0x501)]);
}

#[test]
fn script_trigger_sends_event_message() {
    let mut msg = periodic(0x600, 10, Some(1));
    msg.send_type = SendType::Event;
    let script = r#"
        fn on_message(msg) { trigger(0); }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![msg])).unwrap();
    let out = run_to(&mut sim, 10);
    assert_eq!(ids(&out), vec![(1, 0x100), (1, 0x600)]);
}

#[test]
fn script_set_payload_applies() {
    let mut msg = periodic(0x600, 10, Some(1));
    msg.send_type = SendType::CyclicAndEvent;
    let script = "fn on_message(msg) { set_payload(0, [9]); }";
    let mut sim = Simulation::new(&script_topo(script, vec![msg])).unwrap();
    let out = run_to(&mut sim, 5);
    let last = out.iter().rfind(|e| e.frame.id == 0x600).unwrap();
    assert_eq!(last.frame.payload(), [9]);
}

#[test]
fn script_compile_error_surfaces() {
    let err = Simulation::new(&script_topo("fn on_start( {", vec![]))
        .err()
        .unwrap();
    assert!(matches!(
        err,
        crate::SimError::Script {
            node: NodeId(1),
            ..
        }
    ));
}

#[test]
fn script_runtime_error_is_logged() {
    let script = r#"
        fn on_message(msg) { let x = 1 / 0; output(#{ id: 0x700 }); }
        fn on_start() { print("hello"); }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    let out = run_to(&mut sim, 10);
    assert_eq!(ids(&out), vec![(1, 0x100)]);
    let logs = sim.drain_logs();
    assert!(logs.iter().any(|l| l.contains("hello") && l.contains("N1")));
    assert!(logs.iter().any(|l| l.contains("script error")));
    assert!(sim.drain_logs().is_empty());
}

#[test]
fn script_infinite_loop_is_stopped() {
    let script = "fn on_message(msg) { loop {} }";
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    let out = run_to(&mut sim, 10);
    assert_eq!(out.len(), 1);
    assert!(sim.drain_logs().iter().any(|l| l.contains("script error")));
}

fn gen_events(out: &[operow_core::BusEvent]) -> Vec<&operow_core::BusEvent> {
    out.iter().filter(|e| e.frame.id >= 0x500).collect()
}

fn gen_frame(id: u32, b: u8) -> CanFrame {
    CanFrame::new(id, false, &[b]).unwrap()
}

#[test]
fn generator_frame_hits_chosen_bus_only_and_gets_forwarded() {
    let filter = IdFilter::Exact {
        id: 0x500,
        extended: false,
    };
    let topo = gateway_topo(vec![route(1, 2, filter, None, 0)]);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.gen_send(GeneratorId(1), Some(BusId(1)), gen_frame(0x500, 1));
    sim.gen_send(GeneratorId(1), Some(BusId(2)), gen_frame(0x501, 2));
    sim.run_until(Timestamp::from_ms(5), &mut out);
    let g = gen_events(&out);
    // 0x500: Tx on bus 1 + forwarded to bus 2; 0x501: Tx on bus 2 only.
    let ids: Vec<(u32, u32, Direction)> = g.iter().map(|e| (e.frame.id, e.bus.0, e.dir)).collect();
    assert!(ids.contains(&(0x500, 1, Direction::Tx)));
    assert!(ids.contains(&(0x500, 2, Direction::Rx)));
    assert!(ids.contains(&(0x501, 2, Direction::Tx)));
    assert_eq!(g.len(), 3);
    let tx = g
        .iter()
        .find(|e| e.frame.id == 0x500 && e.hop == 0)
        .unwrap();
    assert_eq!(tx.sender, GeneratorId(1).node());
    assert!(tx.sender.0 >= GENERATOR_NODE_BASE);
    let fwd = g
        .iter()
        .find(|e| e.frame.id == 0x500 && e.hop == 1)
        .unwrap();
    assert_eq!(fwd.origin, GeneratorId(1).node());
    assert_eq!(fwd.sender, NodeId(3));
}

#[test]
fn generators_have_distinct_sender_ids() {
    let topo = gateway_topo(vec![]);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.gen_send(GeneratorId(1), None, gen_frame(0x510, 0));
    sim.gen_send(GeneratorId(2), None, gen_frame(0x511, 0));
    sim.run_until(Timestamp::from_ms(5), &mut out);
    let a = out.iter().find(|e| e.frame.id == 0x510).unwrap().sender;
    let b = out.iter().find(|e| e.frame.id == 0x511).unwrap().sender;
    assert_ne!(a, b);
    assert_eq!(GeneratorId::from_node(a), Some(GeneratorId(1)));
    assert_eq!(GeneratorId::from_node(b), Some(GeneratorId(2)));
    assert_eq!(GeneratorId::from_node(NodeId(3)), None);
    // `None` fans out to both buses.
    assert_eq!(out.iter().filter(|e| e.frame.id == 0x510).count(), 2);
}

#[test]
fn generator_cyclic_period_is_exact_and_stops() {
    let topo = gateway_topo(vec![]);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    let g = GeneratorId(1);
    sim.gen_set_cyclic(g, 0, Some(BusId(1)), gen_frame(0x520, 0), Some(10_000_000));
    sim.run_until(Timestamp::from_ms(55), &mut out);
    // Completion times are exactly one period apart.
    let ends: Vec<u64> = gen_events(&out).iter().map(|e| e.time.0).collect();
    assert_eq!(ends.len(), 6);
    // (The first frame waits behind ECU 1's own t=0 frame.)
    assert!(ends[1..].windows(2).all(|w| w[1] - w[0] == 10_000_000));

    // Restarting does not double the rate.
    sim.gen_set_cyclic(g, 0, Some(BusId(1)), gen_frame(0x520, 0), Some(10_000_000));
    out.clear();
    sim.run_until(Timestamp::from_ms(106), &mut out);
    assert_eq!(gen_events(&out).len(), 6);

    sim.gen_set_cyclic(g, 0, Some(BusId(1)), gen_frame(0x520, 0), None);
    out.clear();
    sim.run_until(Timestamp::from_ms(300), &mut out);
    assert!(gen_events(&out).is_empty());
}

#[test]
fn generator_update_frame_and_stop_all() {
    let topo = gateway_topo(vec![]);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    let g = GeneratorId(1);
    sim.gen_set_cyclic(g, 0, Some(BusId(1)), gen_frame(0x530, 1), Some(10_000_000));
    sim.gen_set_cyclic(g, 1, Some(BusId(1)), gen_frame(0x531, 1), Some(10_000_000));
    sim.run_until(Timestamp::from_ms(15), &mut out);
    sim.gen_update_frame(g, 0, gen_frame(0x530, 9));
    out.clear();
    sim.run_until(Timestamp::from_ms(35), &mut out);
    let v: Vec<u8> = out
        .iter()
        .filter(|e| e.frame.id == 0x530)
        .map(|e| e.frame.data[0])
        .collect();
    assert_eq!(v, vec![9, 9]);
    sim.gen_stop_all(g);
    out.clear();
    sim.run_until(Timestamp::from_ms(100), &mut out);
    assert!(gen_events(&out).is_empty());
}

#[test]
fn generator_commands_through_runner() {
    let handle = Engine::spawn();
    handle
        .cmd
        .send(Command::Load(gateway_topo(vec![])))
        .unwrap();
    handle.cmd.send(Command::SetSpeed(0.0)).unwrap();
    handle.cmd.send(Command::Start).unwrap();
    handle
        .cmd
        .send(Command::GenSetCyclic {
            gen_id: GeneratorId(1),
            row: 0,
            bus: Some(BusId(1)),
            frame: gen_frame(0x540, 0),
            period_ns: Some(1_000_000),
        })
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut n = 0;
    while n < 5 && std::time::Instant::now() < deadline {
        if let Ok(EngineEvent::Frames(f)) = handle.events.recv_timeout(Duration::from_millis(100)) {
            n += f.iter().filter(|e| e.frame.id == 0x540).count();
        }
    }
    assert!(n >= 5);
    handle.shutdown();
}

// ---- replay node ----

/// Write an ASC log of `(ms, channel, id)` records (one data byte) to a
/// unique temp file and return its path.
fn write_log(records: &[(u64, u8, u32)], extra: &str) -> String {
    use operow_log::{AscDate, AscWriter, LogRecord, LogWriter, RecordKind};
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "operow-replay-{}-{}.asc",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let mut w = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
    for &(ms, channel, id) in records {
        let r = LogRecord {
            time: Timestamp::from_ms(ms),
            channel,
            dir: Direction::Tx,
            kind: RecordKind::Frame(CanFrame::new(id, false, &[ms as u8]).unwrap()),
        };
        w.write(&r).unwrap();
    }
    let mut text = String::from_utf8(w.into_inner()).unwrap();
    text.push_str(extra);
    text.push_str("End TriggerBlock\n");
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

fn replay_kind(
    path: &str,
    map: &[(u8, u32)],
    looped: bool,
    offset_ms: i64,
    filter: Option<&str>,
) -> NodeKind {
    NodeKind::Replay {
        path: path.into(),
        channel_map: map.iter().map(|&(c, b)| (c, BusId(b))).collect(),
        looped,
        time_offset_ms: offset_ms,
        id_filter: filter.map(Into::into),
    }
}

/// Replay node 1 on bus 1 (and bus 2 when `two`).
fn replay_topo(kind: NodeKind, two: bool) -> Topology {
    let mut topo = Topology {
        nodes: vec![node(1, vec![], kind)],
        buses: vec![bus(1, "A")],
        links: vec![link(1, 1)],
        ..Default::default()
    };
    if two {
        topo.buses.push(bus(2, "B"));
        topo.links.push(link(1, 2));
    }
    topo
}

fn times_ms(out: &[operow_core::BusEvent]) -> Vec<u64> {
    // Frames take ~0.2 ms on the wire; round down to the send time.
    out.iter().map(|e| e.time.0 / 1_000_000).collect()
}

#[test]
fn replay_frames_appear_on_mapped_bus_at_log_times() {
    let path = write_log(&[(10, 1, 0x100), (20, 2, 0x200), (35, 1, 0x101)], "");
    let topo = replay_topo(replay_kind(&path, &[(1, 1), (2, 2)], false, 0, None), true);
    let out = run_ms(&topo, 100);
    assert_eq!(times_ms(&out), [10, 20, 35]);
    assert_eq!(
        out.iter().map(|e| (e.bus, e.frame.id)).collect::<Vec<_>>(),
        [(BusId(1), 0x100), (BusId(2), 0x200), (BusId(1), 0x101)]
    );
    assert!(out.iter().all(|e| e.sender == NodeId(1) && e.hop == 0));
    assert!(out.iter().all(|e| e.dir == Direction::Tx));
    // On the wire right after the log time (frame duration < 1 ms).
    assert!(out[0].time.0 > 10_000_000);
}

#[test]
fn replay_skips_unmapped_channels_error_frames_and_filtered_ids() {
    let path = write_log(
        &[
            (1, 1, 0x100),
            (2, 3, 0x100),
            (3, 1, 0x150),
            (4, 1, 0x7DF),
            (5, 1, 0x1FF),
        ],
        "   0.006000 1  ErrorFrame\n",
    );
    let topo = replay_topo(
        replay_kind(&path, &[(1, 1)], false, 0, Some("100-1FF, !150")),
        false,
    );
    let out = run_ms(&topo, 50);
    assert_eq!(
        out.iter().map(|e| e.frame.id).collect::<Vec<_>>(),
        [0x100, 0x1FF]
    );
}

#[test]
fn replay_offset_shifts_and_drops_negative_times() {
    let path = write_log(&[(5, 1, 0x1), (20, 1, 0x2)], "");
    let later = replay_topo(replay_kind(&path, &[(1, 1)], false, 30, None), false);
    assert_eq!(times_ms(&run_ms(&later, 100)), [35, 50]);
    let earlier = replay_topo(replay_kind(&path, &[(1, 1)], false, -10, None), false);
    let out = run_ms(&earlier, 100);
    assert_eq!(
        times_ms(&out),
        [10],
        "the record shifted before t=0 is dropped"
    );
}

#[test]
fn replay_loop_restarts_after_the_last_record() {
    let path = write_log(&[(10, 1, 0x1), (20, 1, 0x2)], "");
    let topo = replay_topo(replay_kind(&path, &[(1, 1)], true, 0, None), false);
    let out = run_ms(&topo, 85);
    // Cycle length is the last record's time (20 ms).
    assert_eq!(times_ms(&out), [10, 20, 30, 40, 50, 60, 70, 80]);
    let unlooped = replay_topo(replay_kind(&path, &[(1, 1)], false, 0, None), false);
    assert_eq!(run_ms(&unlooped, 85).len(), 2);
}

#[test]
fn replay_loop_with_nothing_to_send_terminates() {
    let path = write_log(&[(10, 2, 0x1)], "");
    let topo = replay_topo(replay_kind(&path, &[(1, 1)], true, 0, None), false);
    assert!(run_ms(&topo, 100).is_empty());
}

#[test]
fn replayed_frames_are_forwarded_by_a_gateway() {
    let path = write_log(&[(10, 1, 0x123)], "");
    let topo = Topology {
        nodes: vec![
            node(1, vec![], replay_kind(&path, &[(1, 1)], false, 0, None)),
            node(
                3,
                vec![],
                NodeKind::Gateway {
                    routes: vec![route(1, 2, IdFilter::Any, None, 0)],
                },
            ),
        ],
        buses: vec![bus(1, "A"), bus(2, "B")],
        links: vec![link(1, 1), link(3, 1), link(3, 2)],
        ..Default::default()
    };
    let out = run_ms(&topo, 50);
    assert_eq!(out.len(), 2);
    assert_eq!((out[0].bus, out[0].hop), (BusId(1), 0));
    assert_eq!(
        (out[1].bus, out[1].hop, out[1].sender),
        (BusId(2), 1, NodeId(3))
    );
    assert_eq!(out[1].origin, NodeId(1));
}

#[test]
fn replay_load_errors_name_the_node() {
    let missing = replay_topo(
        replay_kind("/nonexistent/none.asc", &[(1, 1)], false, 0, None),
        false,
    );
    match Simulation::new(&missing) {
        Err(crate::SimError::Replay { node, msg }) => {
            assert_eq!(node, "N1");
            assert!(msg.contains("none.asc"), "{msg}");
        }
        other => panic!("expected a replay error, got {:?}", other.err()),
    }
    let empty = replay_topo(replay_kind("", &[], false, 0, None), false);
    assert!(matches!(
        Simulation::new(&empty),
        Err(crate::SimError::Replay { .. })
    ));
    let path = write_log(&[(1, 1, 0x1)], "");
    let bad_filter = replay_topo(replay_kind(&path, &[(1, 1)], false, 0, Some("zz")), false);
    assert!(matches!(
        Simulation::new(&bad_filter),
        Err(crate::SimError::Replay { .. })
    ));
    let broken = write_log(&[(1, 1, 0x1)], "   0.5 1  12 Tx   d 9 00\n");
    let bad_file = replay_topo(replay_kind(&broken, &[(1, 1)], false, 0, None), false);
    assert!(matches!(
        Simulation::new(&bad_file),
        Err(crate::SimError::Replay { .. })
    ));
}

#[test]
fn replay_streams_logs_larger_than_one_chunk() {
    let records: Vec<(u64, u8, u32)> = (0..5000).map(|i| (i, 1, 0x100 + (i % 16) as u32)).collect();
    let path = write_log(&records, "");
    let topo = replay_topo(replay_kind(&path, &[(1, 1)], false, 0, None), false);
    let out = run_ms(&topo, 6000);
    assert_eq!(out.len(), 5000);
    assert!(out.windows(2).all(|w| w[0].time <= w[1].time));
}

// --- CAN error model --------------------------------------------------------

/// One bus (id 1) with a node per `(node id, frame id, period ms)`; each node
/// cyclically sends its frame (first one at t = 0).
fn err_topo(nodes: &[(u32, u32, u32)], bitrate: u32, simulate_ack: bool) -> Topology {
    Topology {
        nodes: nodes
            .iter()
            .map(|&(id, frame_id, period_ms)| EcuConfig {
                id: NodeId(id),
                name: format!("N{id}"),
                tx: vec![TxMessage {
                    name: "Msg".into(),
                    frame: CanFrame::new(frame_id, false, &[1, 2]).unwrap(),
                    period_ms,
                    enabled: true,
                    bus: None,
                    send_type: Default::default(),
                }],
                kind: Default::default(),
                pos: (0.0, 0.0),
                script: None,
                diag: None,
            })
            .collect(),
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "CAN0".into(),
            bitrate,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack,
            hardware: None,
        }],
        links: nodes
            .iter()
            .map(|&(id, ..)| Link {
                node: NodeId(id),
                bus: BusId(1),
            })
            .collect(),
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
    }
}

fn inject_spec(node: u32, kind: CanErrorKind, mode: InjectMode) -> InjectSpec {
    InjectSpec {
        bus: BusId(1),
        node: Some(NodeId(node)),
        id: None,
        kind,
        mode,
        remaining: None,
    }
}

fn error_events(out: &[BusEvent]) -> Vec<&BusEvent> {
    out.iter().filter(|e| e.is_error()).collect()
}

fn frame_events(out: &[BusEvent]) -> Vec<&BusEvent> {
    out.iter().filter(|e| !e.is_error()).collect()
}

/// Step the simulation until `n` error events have been produced.
fn run_to_error(sim: &mut Simulation, out: &mut Vec<BusEvent>, n: usize) {
    let mut t = sim.now().0;
    while error_events(out).len() < n {
        t += 10_000;
        assert!(t < 1_000_000_000, "never reached {n} errors");
        sim.run_until(Timestamp(t), out);
    }
}

const N1: NodeId = NodeId(1);
const N2: NodeId = NodeId(2);
const BUS1: BusId = BusId(1);

#[test]
fn error_event_carries_the_failed_frame() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Crc, InjectMode::Count(1)));
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(5), &mut out);
    let err = out[0];
    assert!(err.is_error());
    assert_eq!(err.error_kind(), Some(CanErrorKind::Crc));
    assert_eq!(
        err.kind,
        operow_core::BusEventKind::Error {
            error: CanErrorKind::Crc,
            node: N1
        }
    );
    assert_eq!(
        (err.sender, err.frame.id, err.dir),
        (N1, 0x100, Direction::Tx)
    );
    // The frame is retransmitted with the same uid after the error frame.
    let ok = frame_events(&out)
        .into_iter()
        .find(|e| e.frame.id == 0x100)
        .unwrap();
    assert_eq!(ok.frame_uid, err.frame_uid);
    assert!(ok.time > err.time);
    assert_eq!(sim.stats()[&BUS1].can_errors.crc, 1);
    assert_eq!(sim.stats()[&BUS1].can_errors.total(), 1);
}

#[test]
fn errors_drive_the_transmitter_error_passive() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Bit, InjectMode::Count(100)));
    let mut out = Vec::new();
    run_to_error(&mut sim, &mut out, 15);
    assert_eq!(
        sim.node_state(N1, BUS1),
        (NodeErrorState::ErrorActive, 120, 0)
    );
    // The receiver counted every error frame.
    assert_eq!(
        sim.node_state(N2, BUS1),
        (NodeErrorState::ErrorActive, 0, 15)
    );
    run_to_error(&mut sim, &mut out, 16);
    assert_eq!(
        sim.node_state(N1, BUS1),
        (NodeErrorState::ErrorPassive, 128, 0)
    );
}

#[test]
fn passive_node_returns_to_active_when_counters_drop() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Bit, InjectMode::Count(16)));
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(5), &mut out);
    assert_eq!(error_events(&out).len(), 16);
    // The retransmission succeeded: TEC 128 - 1 = 127 is no longer passive,
    // and the receivers' REC (16) dropped by one per good frame.
    let (state, tec, _) = sim.node_state(N1, BUS1);
    assert_eq!((state, tec), (NodeErrorState::ErrorActive, 127));
    let (_, _, rec) = sim.node_state(N2, BUS1);
    assert!(rec < 16, "rec {rec}");
}

#[test]
fn errors_drive_the_transmitter_bus_off() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Stuff, InjectMode::Count(1000)));
    let mut out = Vec::new();
    // 32 errors: +8 each reaches TEC 256 (> 255), then node 1 goes silent.
    run_to_error(&mut sim, &mut out, 32);
    assert_eq!(sim.node_state(N1, BUS1), (NodeErrorState::BusOff, 256, 0));
    let t = sim.now().0;
    sim.run_until(Timestamp(t + 100_000), &mut out);
    assert_eq!(error_events(&out).len(), 32);
    assert!(frame_events(&out).iter().all(|e| e.sender != N1));
    assert_eq!(sim.stats()[&BUS1].dropped_bus_off, 1, "the abandoned frame");
}

#[test]
fn bus_off_recovers_after_1408_bit_times() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Bit, InjectMode::Count(1000)));
    let mut out = Vec::new();
    run_to_error(&mut sim, &mut out, 32);
    let off_at = error_events(&out)[31].time.0;
    let recover_ns = 1408 * 2000;
    sim.run_until(Timestamp(off_at + recover_ns - 1), &mut out);
    assert_eq!(sim.node_state(N1, BUS1).0, NodeErrorState::BusOff);
    sim.run_until(Timestamp(off_at + recover_ns), &mut out);
    assert_eq!(
        sim.node_state(N1, BUS1),
        (NodeErrorState::ErrorActive, 0, 0)
    );
}

#[test]
fn error_passive_transmitter_waits_the_suspend_time() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Bit, InjectMode::Count(20)));
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(10), &mut out);
    let errs = error_events(&out);
    assert_eq!(errs.len(), 20);
    let bit = 2000;
    let frame = CanFrame::new(0x100, false, &[1, 2]).unwrap();
    let half = frame_duration_ns(&frame, 500_000) / 2;
    // Active: 20-bit error frame + 3 intermission, then retransmit.
    assert_eq!(errs[1].time.0 - errs[0].time.0, 23 * bit + half);
    // Passive: 14-bit error frame + 3 intermission + 8 suspend.
    assert_eq!(errs[18].time.0 - errs[17].time.0, (14 + 3 + 8) * bit + half);
}

#[test]
fn lone_node_with_simulate_ack_goes_passive_but_never_bus_off() {
    let topo = err_topo(&[(1, 0x100, 1000)], 500_000, true);
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(200), &mut out);
    assert!(frame_events(&out).is_empty());
    let errs = error_events(&out);
    assert!(errs.len() > 100, "{} ack errors", errs.len());
    assert!(
        errs.iter()
            .all(|e| e.error_kind() == Some(CanErrorKind::Ack))
    );
    // 16 errors reach TEC 128; further ACK errors leave it unchanged.
    assert_eq!(
        sim.node_state(N1, BUS1),
        (NodeErrorState::ErrorPassive, 128, 0)
    );
    assert_eq!(sim.stats()[&BUS1].can_errors.ack, errs.len() as u64);
}

#[test]
fn no_ack_error_when_another_node_is_online() {
    let topo = err_topo(&[(1, 0x100, 10), (2, 0x200, 10)], 500_000, true);
    let out = run_ms(&topo, 100);
    assert!(error_events(&out).is_empty());
    assert_eq!(frame_events(&out).len(), 20);
}

#[test]
fn no_ack_error_without_simulate_ack() {
    let topo = err_topo(&[(1, 0x100, 10)], 500_000, false);
    let out = run_ms(&topo, 100);
    assert!(error_events(&out).is_empty());
    assert_eq!(frame_events(&out).len(), 10);
}

#[test]
fn ack_error_returns_once_the_only_other_node_is_bus_off() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 125_000, true);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.force_bus_off(N2, BUS1);
    let mut out = Vec::new();
    // Node 2 is offline until ~11.3 ms; node 1 sees no ACK meanwhile.
    sim.run_until(Timestamp::from_ms(5), &mut out);
    assert!(
        error_events(&out)
            .iter()
            .all(|e| e.error_kind() == Some(CanErrorKind::Ack))
    );
    assert!(!error_events(&out).is_empty());
    assert!(frame_events(&out).is_empty());
}

#[test]
fn bus_off_node_drops_transmissions_and_receives_nothing() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 125_000, false);
    let rx1 = Arc::new(AtomicU32::new(0));
    let mut sim = Simulation::new(&topo).unwrap();
    sim.set_ecu(
        N1,
        Box::new(CountingEcu {
            counter: rx1.clone(),
        }),
    );
    sim.set_ecu(
        N2,
        Box::new(CountingEcu {
            counter: Arc::new(AtomicU32::new(0)),
        }),
    );
    let frame = CanFrame::new(0x123, false, &[7]).unwrap();
    sim.force_bus_off_with(N1, BUS1, crate::sim::BusOffRecovery::Auto);
    assert_eq!(sim.node_state(N1, BUS1).0, NodeErrorState::BusOff);
    let mut out = Vec::new();
    sim.send_once(N1, None, frame);
    sim.send_once(N2, None, frame);
    sim.run_until(Timestamp::from_ms(5), &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].sender, N2);
    assert_eq!(
        rx1.load(Ordering::SeqCst),
        0,
        "bus-off node must not receive"
    );
    assert_eq!(sim.stats()[&BUS1].dropped_bus_off, 1);

    // Recovery takes 1408 bits = 11.264 ms at 125 kbit/s.
    sim.run_until(Timestamp::from_ms(12), &mut out);
    assert_eq!(
        sim.node_state(N1, BUS1),
        (NodeErrorState::ErrorActive, 0, 0)
    );
    sim.send_once(N2, None, frame);
    sim.send_once(N1, None, frame);
    sim.run_until(Timestamp::from_ms(20), &mut out);
    assert_eq!(out.len(), 3);
    assert_eq!(rx1.load(Ordering::SeqCst), 1);
}

#[test]
fn force_bus_off_ignores_unlinked_pairs() {
    let topo = err_topo(&[(1, 0x100, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.force_bus_off(NodeId(9), BUS1);
    sim.force_bus_off(N1, BusId(7));
    assert_eq!(sim.node_state(N1, BUS1).0, NodeErrorState::ErrorActive);
    assert_eq!(
        sim.node_state(NodeId(9), BUS1).0,
        NodeErrorState::ErrorActive
    );
    assert_eq!(sim.node_states().len(), 1);
}

#[test]
fn injection_count_filters_and_retransmission() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    // Filters: another node, another id and another bus never match.
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(2, CanErrorKind::Form, InjectMode::Count(5)));
    sim.inject_errors(InjectSpec {
        id: Some((0x100, true)),
        ..inject_spec(1, CanErrorKind::Form, InjectMode::Count(5))
    });
    sim.inject_errors(InjectSpec {
        bus: BusId(2),
        ..inject_spec(1, CanErrorKind::Form, InjectMode::Count(5))
    });
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(10), &mut out);
    assert_eq!(
        error_events(&out).iter().filter(|e| e.sender == N1).count(),
        0
    );
    // Node 2's frame (0x200) was hit five times, then got through.
    let n2_errs = error_events(&out).iter().filter(|e| e.sender == N2).count();
    assert_eq!(n2_errs, 5);
    assert_eq!(frame_events(&out).len(), 2);
    assert!(
        error_events(&out)
            .iter()
            .all(|e| e.error_kind() == Some(CanErrorKind::Form))
    );
}

#[test]
fn injection_every_nth_and_remaining_cap() {
    let topo = err_topo(&[(1, 0x100, 10), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(InjectSpec {
        id: Some((0x100, false)),
        ..inject_spec(1, CanErrorKind::Bit, InjectMode::EveryNth(3))
    });
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(200), &mut out);
    let errs = error_events(&out).len();
    let ok = frame_events(&out).iter().filter(|e| e.sender == N1).count();
    assert_eq!(ok, 20, "every cyclic frame gets through eventually");
    // Every third attempt (retransmissions included) is corrupted.
    assert_eq!(errs, (errs + ok) / 3);
    assert!(errs >= 9);

    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(InjectSpec {
        remaining: Some(5),
        ..inject_spec(1, CanErrorKind::Bit, InjectMode::EveryNth(1))
    });
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(200), &mut out);
    assert_eq!(error_events(&out).len(), 5);
}

fn probability_run(seed: u64, p: f64) -> (usize, usize) {
    let topo = err_topo(&[(1, 0x100, 1), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.set_seed(seed);
    sim.inject_errors(inject_spec(
        1,
        CanErrorKind::Crc,
        InjectMode::Probability(p),
    ));
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(1500), &mut out);
    let errs = error_events(&out).len();
    let ok = frame_events(&out).iter().filter(|e| e.sender == N1).count();
    (errs, errs + ok)
}

#[test]
fn injection_probability_is_seeded_and_close_to_p() {
    let (errs, attempts) = probability_run(42, 0.1);
    assert!(attempts > 1000, "{attempts} attempts");
    let rate = errs as f64 / attempts as f64;
    assert!((0.07..0.13).contains(&rate), "rate {rate}");
    assert_eq!(probability_run(42, 0.1), (errs, attempts), "same seed");
    assert_ne!(probability_run(7, 0.1), (errs, attempts), "other seed");
    assert_eq!(probability_run(1, 0.0).0, 0);
}

#[test]
fn clear_injections_stops_errors() {
    let topo = err_topo(&[(1, 0x100, 10), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(
        1,
        CanErrorKind::Bit,
        InjectMode::Probability(1.0),
    ));
    sim.clear_injections();
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(50), &mut out);
    assert!(error_events(&out).is_empty());
}

#[test]
fn error_frames_occupy_the_bus() {
    let topo = err_topo(&[(1, 0x100, 1000), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Bit, InjectMode::Count(2)));
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(10), &mut out);
    let stats = sim.stats()[&BUS1];
    let frame = CanFrame::new(0x100, false, &[1, 2]).unwrap();
    let dur = frame_duration_ns(&frame, 500_000);
    let other = CanFrame::new(0x200, false, &[1, 2]).unwrap();
    let expected = 2 * (dur / 2 + 23 * 2000) + dur + frame_duration_ns(&other, 500_000);
    assert_eq!(stats.busy_ns, expected);
    assert_eq!(stats.frames, 2, "error frames are not counted as frames");
}

#[test]
fn runner_reports_node_states_and_accepts_error_commands() {
    let topo = err_topo(&[(1, 0x100, 10), (2, 0x200, 10)], 500_000, false);
    let h = Engine::spawn();
    h.cmd.send(Command::Load(topo)).unwrap();
    h.cmd
        .send(Command::InjectErrors(inject_spec(
            1,
            CanErrorKind::Crc,
            InjectMode::Count(3),
        )))
        .unwrap();
    h.cmd.send(Command::ForceBusOff(N2, BUS1)).unwrap();
    h.cmd.send(Command::SetSpeed(0.0)).unwrap();
    h.cmd.send(Command::Start).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_bus_off = false;
    while std::time::Instant::now() < deadline && !saw_bus_off {
        if let Ok(EngineEvent::NodeStates { nodes, .. }) =
            h.events.recv_timeout(Duration::from_millis(100))
        {
            saw_bus_off = nodes.iter().any(|n| n.node == N2);
            assert_eq!(nodes.len(), 2);
        }
    }
    assert!(saw_bus_off, "no NodeStates event");
    h.shutdown();
}

// ---- runtime node / message controls ----

/// Node 1 sends 0x100 every 10 ms on bus 1; node 2 listens on bus 1.
fn ctl_topo() -> Topology {
    Topology {
        nodes: vec![
            node(1, vec![periodic(0x100, 10, None)], NodeKind::Ecu),
            node(2, vec![], NodeKind::Ecu),
        ],
        buses: vec![bus(1, "A")],
        links: vec![link(1, 1), link(2, 1)],
        databases: vec![],
        user_signals: vec![],
        tests: vec![],
        workspace: None,
        ..Default::default()
    }
}

/// Events produced up to `to_ms` since the previous call.
fn run_span(sim: &mut Simulation, to_ms: u64) -> Vec<BusEvent> {
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(to_ms), &mut out);
    out
}

fn counting_sim(topo: &Topology) -> (Simulation, Arc<AtomicU32>) {
    let mut sim = Simulation::new(topo).unwrap();
    let counter = Arc::new(AtomicU32::new(0));
    sim.set_ecu(
        NodeId(2),
        Box::new(CountingEcu {
            counter: counter.clone(),
        }),
    );
    (sim, counter)
}

#[test]
fn offline_transmitter_stops_and_resumes() {
    let mut sim = Simulation::new(&ctl_topo()).unwrap();
    assert_eq!(run_span(&mut sim, 100).len(), 10);
    sim.set_node_online(NodeId(1), None, false);
    assert!(!sim.node_online(NodeId(1), BusId(1)));
    assert!(run_span(&mut sim, 200).is_empty());
    assert!(sim.stats()[&BusId(1)].dropped_offline >= 9);
    sim.set_node_online(NodeId(1), Some(BusId(1)), true);
    assert!(run_span(&mut sim, 300).len() >= 9);
}

#[test]
fn offline_receiver_gets_nothing_and_resumes() {
    let (mut sim, counter) = counting_sim(&ctl_topo());
    run_span(&mut sim, 50);
    let before = counter.load(Ordering::SeqCst);
    assert!(before >= 4);
    sim.set_node_online(NodeId(2), None, false);
    run_span(&mut sim, 150);
    assert_eq!(counter.load(Ordering::SeqCst), before);
    sim.set_node_online(NodeId(2), None, true);
    run_span(&mut sim, 250);
    assert!(counter.load(Ordering::SeqCst) >= before + 9);
}

#[test]
fn offline_node_does_not_ack() {
    let mut topo = ctl_topo();
    topo.buses[0].simulate_ack = true;
    let mut sim = Simulation::new(&topo).unwrap();
    assert_eq!(sim.stats().get(&BusId(1)).unwrap().can_errors.ack, 0);
    sim.set_node_online(NodeId(2), None, false);
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(5), &mut out);
    assert!(sim.stats()[&BusId(1)].can_errors.ack > 0, "no ACK expected");
}

#[test]
fn offline_gateway_does_not_forward_then_resumes() {
    let topo = gateway_topo(vec![route(1, 2, IdFilter::Any, None, 0)]);
    let mut topo = topo;
    topo.nodes[0].tx[0].period_ms = 10;
    let mut sim = Simulation::new(&topo).unwrap();
    let on_b = |ev: &[BusEvent]| ev.iter().filter(|e| e.bus == BusId(2)).count();
    assert!(on_b(&run_span(&mut sim, 50)) >= 4);
    sim.set_node_online(NodeId(3), None, false);
    let ev = run_span(&mut sim, 150);
    assert_eq!(on_b(&ev), 0);
    assert!(ev.iter().any(|e| e.bus == BusId(1)), "source keeps sending");
    sim.set_node_online(NodeId(3), None, true);
    assert!(on_b(&run_span(&mut sim, 250)) >= 8);
}

#[test]
fn offline_on_one_bus_keeps_the_other() {
    let topo = gateway_topo(vec![route(1, 2, IdFilter::Any, None, 0)]);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.set_node_online(NodeId(3), Some(BusId(2)), false);
    assert!(sim.node_online(NodeId(3), BusId(1)));
    assert!(!sim.node_online(NodeId(3), BusId(2)));
}

#[test]
fn msg_control_pause_and_resume() {
    let mut sim = Simulation::new(&ctl_topo()).unwrap();
    let ctl = MsgControl {
        paused: true,
        ..Default::default()
    };
    sim.set_msg_control(NodeId(1), (0x100, false), ctl);
    assert!(run_span(&mut sim, 100).is_empty());
    assert!(sim.stats()[&BusId(1)].dropped_msg_control >= 9);
    sim.set_msg_control(NodeId(1), (0x100, false), MsgControl::default());
    assert!(run_span(&mut sim, 200).len() >= 9);
}

fn drop_count(seed: u64, pct: f32) -> usize {
    let mut topo = ctl_topo();
    topo.nodes[0].tx[0].period_ms = 1;
    let mut sim = Simulation::new(&topo).unwrap();
    sim.set_seed(seed);
    sim.set_msg_control(
        NodeId(1),
        (0x100, false),
        MsgControl {
            drop_pct: pct,
            ..Default::default()
        },
    );
    run_span(&mut sim, 2000).len()
}

#[test]
fn msg_control_drop_pct_is_seeded_and_bounded() {
    let sent = drop_count(7, 0.0) as f64;
    let kept = drop_count(7, 30.0) as f64;
    let frac = 1.0 - kept / sent;
    assert!((0.25..0.35).contains(&frac), "dropped {frac}");
    assert_eq!(drop_count(7, 30.0) as f64, kept, "same seed, same result");
    assert_eq!(drop_count(7, 100.0), 0);
}

#[test]
fn msg_control_delay_and_jitter_bounds() {
    let first_time = |ctl: MsgControl| {
        let mut sim = Simulation::new(&ctl_topo()).unwrap();
        sim.set_msg_control(NodeId(1), (0x100, false), ctl);
        let ev = run_span(&mut sim, 5);
        ev.first().map(|e| e.time.0)
    };
    let dur = frame_duration_ns(&CanFrame::new(0x100, false, &[1]).unwrap(), 500_000);
    assert_eq!(first_time(MsgControl::default()), Some(dur));
    // 3 ms delay: nothing before 3 ms, the first frame completes at 3 ms + dur.
    let d = first_time(MsgControl {
        delay_ms: 3.0,
        ..Default::default()
    });
    assert_eq!(d, Some(3_000_000 + dur));
    // Jitter only: queued within +-2 ms (negative clamps to now).
    for seed in 1..30u64 {
        let mut sim = Simulation::new(&ctl_topo()).unwrap();
        sim.set_seed(seed);
        sim.set_msg_control(
            NodeId(1),
            (0x100, false),
            MsgControl {
                delay_ms: 1.0,
                jitter_ms: 2.0,
                ..Default::default()
            },
        );
        let ev = run_span(&mut sim, 5);
        let t = ev.first().expect("frame within 5ms").time.0;
        assert!(t >= dur && t <= 3_000_000 + dur, "t = {t}");
    }
}

#[test]
fn msg_control_applies_to_script_output_by_id() {
    let script = r#"
        fn on_message(msg) {
            if msg.id == 0x100 {
                output(#{ id: 0x200, data: msg.data, bus: msg.bus });
            }
        }
    "#;
    let mut sim = Simulation::new(&script_topo(script, vec![])).unwrap();
    sim.set_msg_control(
        NodeId(1),
        (0x200, false),
        MsgControl {
            paused: true,
            ..Default::default()
        },
    );
    let ev = run_span(&mut sim, 20);
    assert!(ev.iter().any(|e| e.frame.id == 0x100));
    assert!(!ev.iter().any(|e| e.frame.id == 0x200));
    assert!(sim.stats()[&BusId(1)].dropped_msg_control >= 1);
}

#[test]
fn msg_control_sanitizes_values() {
    let c = MsgControl {
        paused: false,
        drop_pct: 250.0,
        delay_ms: -4.0,
        jitter_ms: f32::NAN,
    }
    .sanitized();
    assert_eq!(c.drop_pct, 100.0);
    assert_eq!(c.delay_ms, 0.0);
    assert_eq!(c.jitter_ms, 0.0);
    assert!(MsgControl::default().is_noop());
}

#[test]
fn runtime_controls_reset_on_stop() {
    let h = Engine::spawn();
    h.cmd.send(Command::Load(ctl_topo())).unwrap();
    h.cmd
        .send(Command::SetNodeOnline {
            node: NodeId(1),
            bus: None,
            online: false,
        })
        .unwrap();
    h.cmd.send(Command::Stop).unwrap();
    h.cmd.send(Command::SetSpeed(0.0)).unwrap();
    h.cmd.send(Command::Start).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw = false;
    while std::time::Instant::now() < deadline && !saw {
        if let Ok(EngineEvent::Frames(f)) = h.events.recv_timeout(Duration::from_millis(100)) {
            saw = !f.is_empty();
        }
    }
    assert!(saw, "node should be online again after Stop");
    h.shutdown();
}

#[test]
fn manual_bus_off_holds_until_recovered_and_counts_events() {
    let topo = err_topo(&[(1, 0x100, 10), (2, 0x200, 10)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.force_bus_off(N1, BUS1);
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(50), &mut out);
    let info = |sim: &Simulation| {
        sim.node_states()
            .into_iter()
            .find(|i| i.node == N1)
            .unwrap()
    };
    assert_eq!(info(&sim).state, NodeErrorState::BusOff, "no auto recovery");
    assert_eq!(info(&sim).bus_off_events, 1);
    assert!(out.iter().all(|e| e.sender != N1));
    sim.recover_bus_off(N1, BUS1);
    assert_eq!(info(&sim).state, NodeErrorState::ErrorActive);
    assert_eq!(info(&sim).bus_off_events, 1, "counter survives recovery");
    out.clear();
    sim.run_until(Timestamp::from_ms(100), &mut out);
    assert!(out.iter().any(|e| e.sender == N1));
}

#[test]
fn injected_bus_off_recovers_automatically_and_is_counted() {
    let topo = err_topo(&[(1, 0x100, 10), (2, 0x200, 1000)], 500_000, false);
    let mut sim = Simulation::new(&topo).unwrap();
    sim.inject_errors(inject_spec(1, CanErrorKind::Bit, InjectMode::Count(40)));
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(200), &mut out);
    let i = sim
        .node_states()
        .into_iter()
        .find(|i| i.node == N1)
        .unwrap();
    assert!(i.bus_off_events >= 1);
    assert!(i.last_bus_off_ns > 0);
    assert_ne!(i.state, NodeErrorState::BusOff, "auto recovery");
}
