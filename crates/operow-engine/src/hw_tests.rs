use std::time::{Duration, Instant};

use operow_core::{
    BusEvent, BusId, CanBusConfig, CanFrame, Direction, EcuConfig, HwBinding, IdFilter, Link,
    NodeId, NodeKind, RouteRule, Topology, TxMessage,
};
use operow_hw::{ChannelConfig, open_channel};

use crate::runner::{Command, Engine, EngineEvent, EngineHandle};
use crate::sim::{HW_NODE_BASE, hw_node};

const WAIT: Duration = Duration::from_secs(3);

fn bus(id: u32, hw: Option<HwBinding>) -> CanBusConfig {
    CanBusConfig {
        id: BusId(id),
        name: format!("B{id}"),
        bitrate: 500_000,
        fd_enabled: false,
        data_bitrate: 2_000_000,
        simulate_ack: false,
        hardware: hw,
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

fn link(n: u32, b: u32) -> Link {
    Link {
        node: NodeId(n),
        bus: BusId(b),
    }
}

fn binding(name: &str, listen_only: bool) -> HwBinding {
    HwBinding {
        interface: format!("virtual:{name}"),
        listen_only,
        receive_own: false,
    }
}

fn periodic(id: u32, bus: u32) -> TxMessage {
    TxMessage {
        name: "M".into(),
        frame: CanFrame::new(id, false, &[1]).unwrap(),
        period_ms: 20,
        enabled: true,
        bus: Some(BusId(bus)),
        send_type: Default::default(),
    }
}

/// Run `f` on events until it returns true or the wait expires.
fn wait_for(h: &EngineHandle, mut f: impl FnMut(&EngineEvent) -> bool) -> bool {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if let Ok(ev) = h.events.recv_timeout(Duration::from_millis(50))
            && f(&ev)
        {
            return true;
        }
    }
    false
}

fn start(h: &EngineHandle, topo: Topology) {
    h.cmd.send(Command::Load(topo)).unwrap();
    h.cmd.send(Command::Start).unwrap();
}

fn running(ev: &EngineEvent) -> bool {
    matches!(ev, EngineEvent::State(crate::RunState::Running))
}

fn frames_of(ev: &EngineEvent) -> &[BusEvent] {
    match ev {
        EngineEvent::Frames(f) => f,
        _ => &[],
    }
}

#[test]
fn external_frame_is_received_and_forwarded_by_gateway() {
    let gw = node(
        1,
        vec![],
        NodeKind::Gateway {
            routes: vec![RouteRule {
                from_bus: BusId(1),
                to_buses: vec![BusId(2)],
                filter: IdFilter::Any,
                remap_id: None,
                delay_us: 0,
            }],
        },
    );
    let topo = Topology {
        nodes: vec![gw],
        buses: vec![bus(1, Some(binding("eng_rx", true))), bus(2, None)],
        links: vec![link(1, 1), link(1, 2)],
        ..Default::default()
    };
    let mut ext = open_channel(&ChannelConfig::new("virtual:eng_rx")).unwrap();
    let h = Engine::spawn();
    start(&h, topo);
    assert!(wait_for(&h, running));
    let f = CanFrame::new(0x321, false, &[9, 8]).unwrap();
    ext.send(&f).unwrap();
    let (mut rx, mut fwd) = (None, None);
    wait_for(&h, |ev| {
        for e in frames_of(ev) {
            if e.bus == BusId(1) && e.frame == f {
                rx = Some(*e);
            }
            if e.bus == BusId(2) && e.frame == f {
                fwd = Some(*e);
            }
        }
        rx.is_some() && fwd.is_some()
    });
    let rx = rx.expect("rx event on hardware bus");
    assert_eq!(rx.dir, Direction::Rx);
    assert_eq!(rx.hop, 0);
    assert_eq!(rx.sender, hw_node(BusId(1)));
    assert!(rx.sender.0 >= HW_NODE_BASE);
    let fwd = fwd.expect("forwarded to simulated bus");
    assert_eq!(fwd.hop, 1);
    assert_eq!(fwd.frame_uid, rx.frame_uid);
    h.shutdown();
}

#[test]
fn simulated_frames_are_transmitted_to_hardware() {
    let topo = Topology {
        nodes: vec![node(1, vec![periodic(0x123, 1)], NodeKind::Ecu)],
        buses: vec![bus(1, Some(binding("eng_tx", false)))],
        links: vec![link(1, 1)],
        ..Default::default()
    };
    let mut ext = open_channel(&ChannelConfig::new("virtual:eng_tx")).unwrap();
    let h = Engine::spawn();
    start(&h, topo);
    let got = ext.recv(WAIT).unwrap().expect("frame on the channel");
    assert_eq!(got.frame.id, 0x123);
    let mut tx_event = false;
    wait_for(&h, |ev| {
        tx_event |= frames_of(ev)
            .iter()
            .any(|e| e.bus == BusId(1) && e.dir == Direction::Tx && e.frame.id == 0x123);
        tx_event
    });
    assert!(tx_event, "Tx event recorded");
    h.shutdown();
}

#[test]
fn listen_only_drops_and_counts() {
    let topo = Topology {
        nodes: vec![node(1, vec![periodic(0x123, 1)], NodeKind::Ecu)],
        buses: vec![bus(1, Some(binding("eng_lo", true)))],
        links: vec![link(1, 1)],
        ..Default::default()
    };
    let mut ext = open_channel(&ChannelConfig::new("virtual:eng_lo")).unwrap();
    let h = Engine::spawn();
    start(&h, topo);
    let (mut logged, mut dropped) = (false, false);
    wait_for(&h, |ev| {
        match ev {
            EngineEvent::Log(l) if l.contains("listen-only") => logged = true,
            EngineEvent::Stats { buses, .. } => {
                dropped |= buses.iter().any(|(_, s)| s.dropped_listen_only > 0)
            }
            _ => {}
        }
        logged && dropped
    });
    assert!(logged, "logged once");
    assert!(dropped, "counted");
    assert!(ext.recv(Duration::from_millis(100)).unwrap().is_none());
    h.shutdown();
}

#[test]
fn failed_open_reports_error_and_does_not_start() {
    let topo = Topology {
        nodes: vec![node(1, vec![], NodeKind::Ecu)],
        buses: vec![bus(1, Some(HwBinding::new("nosuchdriver:x")))],
        links: vec![link(1, 1)],
        ..Default::default()
    };
    let h = Engine::spawn();
    start(&h, topo);
    let mut err = None;
    let mut started = false;
    wait_for(&h, |ev| {
        match ev {
            EngineEvent::Error(e) => err = Some(e.clone()),
            e if running(e) => started = true,
            _ => {}
        }
        err.is_some()
    });
    let err = err.expect("error event");
    assert!(
        err.contains("B1") && err.contains("nosuchdriver:x"),
        "{err}"
    );
    assert!(!started);
    h.shutdown();
}

#[test]
fn non_realtime_speed_is_rejected_with_hardware() {
    let topo = Topology {
        nodes: vec![node(1, vec![], NodeKind::Ecu)],
        buses: vec![bus(1, Some(binding("eng_speed", true)))],
        links: vec![link(1, 1)],
        ..Default::default()
    };
    let h = Engine::spawn();
    h.cmd.send(Command::Load(topo)).unwrap();
    h.cmd.send(Command::SetSpeed(2.0)).unwrap();
    assert!(wait_for(&h, |ev| matches!(
        ev,
        EngineEvent::Log(l) if l.contains("real time")
    )));
    h.shutdown();
}

#[test]
fn hw_status_reports_open_counts_and_closed_after_stop() {
    let topo = Topology {
        nodes: vec![],
        buses: vec![bus(1, Some(binding("eng_status", true)))],
        ..Default::default()
    };
    let mut ext = open_channel(&ChannelConfig::new("virtual:eng_status")).unwrap();
    let h = Engine::spawn();
    start(&h, topo);
    assert!(wait_for(&h, running));
    ext.send(&CanFrame::new(0x10, false, &[1]).unwrap())
        .unwrap();
    let mut seen = None;
    wait_for(&h, |ev| match ev {
        EngineEvent::HwStatus(s) if s.iter().any(|b| b.rx_frames > 0) => {
            seen = Some(s.clone());
            true
        }
        _ => false,
    });
    let s = seen.expect("status with rx count");
    assert_eq!(s[0].interface, "virtual:eng_status");
    assert_eq!(s[0].link, crate::HwLink::Open);
    h.cmd.send(Command::Stop).unwrap();
    let mut closed = false;
    wait_for(&h, |ev| match ev {
        EngineEvent::HwStatus(s) => {
            closed = s.iter().all(|b| b.link == crate::HwLink::Closed);
            closed
        }
        _ => false,
    });
    assert!(closed);
    h.shutdown();
}
