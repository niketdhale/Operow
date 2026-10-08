use std::time::Duration;

use operow_core::{
    BusEvent, BusId, CanBusConfig, DiagConfig, DidEntry, DtcEntry, EcuConfig, KeyAlgo, Link,
    NodeId, NodeKind, SecurityConfig, Timestamp, Topology,
};

use crate::runner::{Command, Engine, EngineEvent};
use crate::sim::Simulation;
use crate::tester::DiagRequestSpec;

const VIN: &[u8] = b"WAUZZZ8K9BA123456";
const SEED: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
const KEY: [u8; 4] = [0xED, 0xCB, 0xA9, 0x87];
const BUS: BusId = BusId(1);

fn diag_cfg(security: bool) -> DiagConfig {
    DiagConfig {
        dids: vec![
            DidEntry {
                did: 0xF190,
                name: "VIN".into(),
                data: VIN.to_vec(),
                writable: false,
            },
            DidEntry {
                did: 0x0100,
                name: "Config".into(),
                data: vec![1, 2, 3, 4],
                writable: true,
            },
            DidEntry {
                did: 0x0200,
                name: "Blob".into(),
                data: (0..40).collect(),
                writable: false,
            },
        ],
        dtcs: vec![
            DtcEntry {
                code: 0x012300,
                status: 0x09,
            },
            DtcEntry {
                code: 0xC10001,
                status: 0x2F,
            },
        ],
        security: security.then(|| SecurityConfig {
            level: 1,
            seed: SEED.to_vec(),
            key_algo: KeyAlgo::XorConst(vec![0xFF]),
        }),
        ..DiagConfig::default()
    }
}

fn topo(diag: DiagConfig, script: Option<&str>) -> Topology {
    Topology {
        nodes: vec![EcuConfig {
            id: NodeId(1),
            name: "Engine".into(),
            tx: vec![],
            kind: NodeKind::Ecu,
            pos: (0.0, 0.0),
            script: script.map(Into::into),
            diag: Some(diag),
        }],
        buses: vec![CanBusConfig {
            id: BUS,
            name: "CAN0".into(),
            bitrate: 500_000,
            fd_enabled: true,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            kind: Default::default(),
            hardware: None,
        }],
        links: vec![Link {
            node: NodeId(1),
            bus: BUS,
        }],
        ..Topology::default()
    }
}

struct Rig {
    sim: Simulation,
    frames: Vec<BusEvent>,
}

impl Rig {
    fn new(diag: DiagConfig, script: Option<&str>) -> Rig {
        Rig {
            sim: Simulation::new(&topo(diag, script)).unwrap(),
            frames: Vec::new(),
        }
    }

    fn advance_ms(&mut self, ms: u64) {
        let t = self.sim.now().0 + ms * 1_000_000;
        self.sim.run_until(Timestamp(t), &mut self.frames);
    }

    /// Send `payload` from a tester and run until its outcome is known.
    fn ask_on(
        &mut self,
        req_id: u32,
        functional: bool,
        payload: &[u8],
    ) -> (Result<Vec<u8>, String>, f64) {
        self.sim
            .diag_request(DiagRequestSpec {
                bus: BUS,
                req_id,
                resp_id: 0x7E8,
                extended: false,
                fd: false,
                payload: payload.to_vec(),
                functional,
            })
            .unwrap();
        for _ in 0..800 {
            self.advance_ms(10);
            if let Some(r) = self.sim.take_diag_results().pop() {
                assert_eq!(r.req, payload);
                return (r.resp, r.elapsed_ms);
            }
        }
        panic!("no outcome for request {payload:02X?}");
    }

    fn ask(&mut self, payload: &[u8]) -> Vec<u8> {
        self.ask_on(0x7E0, false, payload).0.unwrap()
    }
}

fn neg(sid: u8, nrc: u8) -> Vec<u8> {
    vec![0x7F, sid, nrc]
}

#[test]
fn read_did_returns_vin() {
    let mut rig = Rig::new(diag_cfg(false), None);
    let mut expect = vec![0x62, 0xF1, 0x90];
    expect.extend_from_slice(VIN);
    // 20 bytes: a multi-frame response, so the tester also drove ISO-TP.
    assert_eq!(rig.ask(&[0x22, 0xF1, 0x90]), expect);
    assert_eq!(rig.ask(&[0x22, 0x12, 0x34]), neg(0x22, 0x31));
}

#[test]
fn multi_frame_response_uses_flow_control() {
    let mut rig = Rig::new(diag_cfg(false), None);
    let resp = rig.ask(&[0x22, 0x02, 0x00]);
    let mut expect = vec![0x62, 0x02, 0x00];
    expect.extend(0..40u8);
    assert_eq!(resp, expect);
    let from_ecu: Vec<_> = rig
        .frames
        .iter()
        .filter(|e| e.frame.as_can().unwrap().id == 0x7E8)
        .collect();
    assert!(from_ecu.len() >= 7, "FF + CFs, got {}", from_ecu.len());
    assert_eq!(from_ecu[0].frame.payload()[0] >> 4, 1);
    assert!(
        rig.frames
            .iter()
            .any(|e| e.frame.as_can().unwrap().id == 0x7E0 && e.frame.payload()[0] >> 4 == 3),
        "tester sent a flow control frame"
    );
}

#[test]
fn multiple_dids_in_one_request() {
    let mut rig = Rig::new(diag_cfg(false), None);
    let resp = rig.ask(&[0x22, 0x01, 0x00, 0x01, 0x00]);
    assert_eq!(resp, [0x62, 0x01, 0x00, 1, 2, 3, 4, 0x01, 0x00, 1, 2, 3, 4]);
}

#[test]
fn write_needs_extended_session() {
    let mut rig = Rig::new(diag_cfg(false), None);
    let write = [0x2E, 0x01, 0x00, 9, 8, 7, 6];
    assert_eq!(rig.ask(&write), neg(0x2E, 0x7F));
    assert_eq!(rig.ask(&[0x10, 0x03]), [0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]);
    assert_eq!(rig.ask(&write), [0x6E, 0x01, 0x00]);
    assert_eq!(rig.ask(&[0x22, 0x01, 0x00]), [0x62, 0x01, 0x00, 9, 8, 7, 6]);
    // Read-only DID, unknown DID and wrong length.
    assert_eq!(rig.ask(&[0x2E, 0xF1, 0x90, 1]), neg(0x2E, 0x31));
    assert_eq!(rig.ask(&[0x2E, 0x01, 0x00, 1]), neg(0x2E, 0x13));
    assert_eq!(rig.ask(&[0x10, 0x09]), neg(0x10, 0x12));
}

#[test]
fn security_unlock_and_failures() {
    let mut rig = Rig::new(diag_cfg(true), None);
    let write = [0x2E, 0x01, 0x00, 9, 8, 7, 6];
    assert_eq!(rig.ask(&[0x10, 0x03]).len(), 6);
    assert_eq!(rig.ask(&write), neg(0x2E, 0x33));
    // Key without a seed.
    assert_eq!(rig.ask(&[0x27, 0x02, 0, 0, 0, 0]), neg(0x27, 0x24));
    // Wrong level.
    assert_eq!(rig.ask(&[0x27, 0x03]), neg(0x27, 0x12));
    let mut seed = vec![0x67, 0x01];
    seed.extend_from_slice(&SEED);
    assert_eq!(rig.ask(&[0x27, 0x01]), seed);
    let mut send = vec![0x27, 0x02];
    send.extend_from_slice(&KEY);
    assert_eq!(rig.ask(&send), [0x67, 0x02]);
    assert_eq!(rig.ask(&write), [0x6E, 0x01, 0x00]);
    // Already unlocked: zero seed.
    assert_eq!(rig.ask(&[0x27, 0x01]), [0x67, 0x01, 0, 0, 0, 0]);
    // A session change locks again.
    rig.ask(&[0x10, 0x02]);
    rig.ask(&[0x10, 0x03]);
    assert_eq!(rig.ask(&write), neg(0x2E, 0x33));
}

#[test]
fn security_invalid_key_and_lockout() {
    let mut rig = Rig::new(diag_cfg(true), None);
    let bad = [0x27, 0x02, 0, 0, 0, 0];
    for expected in [0x35, 0x35, 0x36] {
        assert_eq!(rig.ask(&[0x27, 0x01]).len(), 6);
        assert_eq!(rig.ask(&bad), neg(0x27, expected));
    }
    assert_eq!(rig.ask(&[0x27, 0x01]), neg(0x27, 0x37));
    rig.advance_ms(10_500);
    assert_eq!(rig.ask(&[0x27, 0x01]).len(), 6);
    let mut send = vec![0x27, 0x02];
    send.extend_from_slice(&KEY);
    assert_eq!(rig.ask(&send), [0x67, 0x02]);
}

#[test]
fn dtc_read_and_clear() {
    let mut rig = Rig::new(diag_cfg(false), None);
    assert_eq!(
        rig.ask(&[0x19, 0x02, 0xFF]),
        [
            0x59, 0x02, 0xFF, 0x01, 0x23, 0x00, 0x09, 0xC1, 0x00, 0x01, 0x2F
        ]
    );
    // Mask 0x08 (confirmed) matches both; 0x20 only the second.
    assert_eq!(
        rig.ask(&[0x19, 0x02, 0x20]),
        [0x59, 0x02, 0xFF, 0xC1, 0x00, 0x01, 0x2F]
    );
    assert_eq!(
        rig.ask(&[0x19, 0x01, 0xFF]),
        [0x59, 0x01, 0xFF, 0x01, 0x00, 0x02]
    );
    assert_eq!(rig.ask(&[0x19, 0x0A]).len(), 3 + 8);
    assert_eq!(rig.ask(&[0x19, 0x55, 0xFF]), neg(0x19, 0x12));
    assert_eq!(rig.ask(&[0x14, 0xFF, 0xFF, 0xFF]), [0x54]);
    assert_eq!(rig.ask(&[0x19, 0x02, 0xFF]), [0x59, 0x02, 0xFF]);
    assert_eq!(rig.ask(&[0x19, 0x01, 0xFF]), [0x59, 0x01, 0xFF, 0x01, 0, 0]);
}

#[test]
fn s3_timeout_returns_to_default_session() {
    let mut rig = Rig::new(diag_cfg(false), None);
    let write = [0x2E, 0x01, 0x00, 9, 8, 7, 6];
    rig.ask(&[0x10, 0x03]);
    rig.advance_ms(3000);
    assert_eq!(rig.ask(&[0x3E, 0x00]), [0x7E, 0x00]);
    rig.advance_ms(3000);
    assert_eq!(rig.ask(&write), [0x6E, 0x01, 0x00], "kept alive by 3E");
    rig.advance_ms(5500);
    assert_eq!(rig.ask(&write), neg(0x2E, 0x7F), "S3 expired");
}

#[test]
fn periodic_tester_present_keeps_session() {
    let mut rig = Rig::new(diag_cfg(false), None);
    rig.ask(&[0x10, 0x03]);
    rig.sim.set_tester_present(tp_spec(true, false, false));
    rig.advance_ms(12_000);
    let tp = rig
        .frames
        .iter()
        .filter(|e| e.frame.as_can().unwrap().id == 0x7E0)
        .count();
    assert!(tp >= 11, "{tp} tester present frames");
    assert_eq!(rig.ask(&[0x2E, 0x01, 0x00, 9, 8, 7, 6]), [0x6E, 0x01, 0x00]);
    assert!(
        rig.frames
            .iter()
            .filter(
                |e| e.frame.as_can().unwrap().id == 0x7E0 && e.frame.payload() == [2, 0x3E, 0x80]
            )
            .all(|e| e.sender == crate::sim::TESTER_PRESENT_NODE),
        "sent by the tester identity"
    );
    rig.sim.set_tester_present(tp_spec(false, false, false));
    let before = rig.frames.len();
    rig.advance_ms(3000);
    assert_eq!(rig.frames.len(), before);
}

fn tp_spec(enable: bool, functional: bool, fd: bool) -> crate::TesterPresentSpec {
    crate::TesterPresentSpec {
        enable,
        bus: BUS,
        req_id: 0x7E0,
        functional_id: 0x7DF,
        functional,
        extended: false,
        fd,
        period_ms: 1000,
    }
}

#[test]
fn tester_present_honours_functional_and_fd() {
    let mut rig = Rig::new(diag_cfg(false), None);
    rig.sim.set_tester_present(tp_spec(true, true, true));
    rig.advance_ms(2500);
    let tp: Vec<_> = rig
        .frames
        .iter()
        .filter(|e| e.frame.as_can().unwrap().id == 0x7DF)
        .collect();
    assert!(tp.len() >= 3, "{} functional frames", tp.len());
    assert!(tp.iter().all(|e| e.frame.as_can().unwrap().fd));
    assert!(
        rig.frames
            .iter()
            .all(|e| e.frame.as_can().unwrap().id != 0x7E0)
    );
}

#[test]
fn functional_tester_present_and_silence() {
    let mut rig = Rig::new(diag_cfg(false), None);
    assert_eq!(
        rig.ask_on(0x7DF, true, &[0x3E, 0x00]).0.unwrap(),
        [0x7E, 0x00]
    );
    // Suppressed positive response: nothing comes back.
    assert!(rig.ask_on(0x7DF, true, &[0x3E, 0x80]).0.is_err());
    // Unsupported service functionally addressed: no negative response.
    assert!(rig.ask_on(0x7DF, true, &[0x85, 0x01]).0.is_err());
    // ... but physically addressed it is answered.
    assert_eq!(rig.ask(&[0x85, 0x01]), neg(0x85, 0x11));
    // Suppress bit works on physical requests too.
    assert!(rig.ask_on(0x7E0, false, &[0x3E, 0x80]).0.is_err());
}

#[test]
fn misc_services() {
    let mut rig = Rig::new(diag_cfg(false), None);
    assert_eq!(
        rig.ask(&[0x31, 0x01, 0xFF, 0x00]),
        [0x71, 0x01, 0xFF, 0x00, 0x00]
    );
    assert_eq!(rig.ask(&[0x31, 0x01, 0x12, 0x34]), neg(0x31, 0x31));
    assert_eq!(rig.ask(&[0x22, 0x01]), neg(0x22, 0x13));
    assert_eq!(rig.ask(&[0x11, 0x01]), [0x51, 0x01]);
    assert_eq!(rig.ask(&[0x11, 0x55]), neg(0x11, 0x12));
    // ECU reset drops the extended session.
    rig.ask(&[0x10, 0x03]);
    rig.ask(&[0x11, 0x01]);
    assert_eq!(rig.ask(&[0x2E, 0x01, 0x00, 9, 8, 7, 6]), neg(0x2E, 0x7F));
    // Responses honour the processing delay (p2_ms / 5).
    let (_, ms) = rig.ask_on(0x7E0, false, &[0x3E, 0x00]);
    assert!((10.0..20.0).contains(&ms), "{ms}");
}

#[test]
fn script_on_diag_overrides_and_falls_back() {
    let script = r#"
        fn on_diag(req) {
            if req[0] == 0x22 && req[1] == 0xF1 && req[2] == 0x90 {
                return [0x62, 0xF1, 0x90, 0x58];
            }
            if req[0] == 0x85 { return []; }
        }
    "#;
    let mut rig = Rig::new(diag_cfg(false), Some(script));
    assert_eq!(rig.ask(&[0x22, 0xF1, 0x90]), [0x62, 0xF1, 0x90, 0x58]);
    assert_eq!(rig.ask(&[0x3E, 0x00]), [0x7E, 0x00]);
    assert!(rig.ask_on(0x7E0, false, &[0x85, 0x01]).0.is_err());
}

#[test]
fn script_security_key_algorithm() {
    let script = r#"
        fn on_security_key(seed) {
            let k = [];
            for b in seed { k.push(b ^ 0x55); }
            k
        }
    "#;
    let mut cfg = diag_cfg(true);
    cfg.security.as_mut().unwrap().key_algo = KeyAlgo::Script;
    let mut rig = Rig::new(cfg, Some(script));
    rig.ask(&[0x27, 0x01]);
    let mut send = vec![0x27, 0x02];
    send.extend(SEED.iter().map(|b| b ^ 0x55));
    assert_eq!(rig.ask(&send), [0x67, 0x02]);
    // Without the hook no key is ever valid.
    let mut cfg = diag_cfg(true);
    cfg.security.as_mut().unwrap().key_algo = KeyAlgo::Script;
    let mut rig = Rig::new(cfg, None);
    rig.ask(&[0x27, 0x01]);
    assert_eq!(rig.ask(&send), neg(0x27, 0x35));
}

#[test]
fn add_const_key_algorithm() {
    let mut cfg = diag_cfg(true);
    cfg.security.as_mut().unwrap().seed = vec![0x00, 0xFF, 0xFF];
    cfg.security.as_mut().unwrap().key_algo = KeyAlgo::AddConst(2);
    let mut rig = Rig::new(cfg, None);
    rig.ask(&[0x27, 0x01]);
    assert_eq!(rig.ask(&[0x27, 0x02, 0x01, 0x00, 0x01]), [0x67, 0x02]);
}

#[test]
fn response_pending_is_followed_to_final_response() {
    let script = r#"
        fn on_start() { this.n = 0; }
        fn on_diag(req) {
            if req[0] == 0x22 {
                this.n += 1;
                if this.n == 1 { return [0x7F, 0x22, 0x78]; }
                return [0x62, 0xF1, 0x90, 0x41];
            }
        }
    "#;
    let mut rig = Rig::new(diag_cfg(false), Some(script));
    let (resp, ms) = rig.ask_on(0x7E0, false, &[0x22, 0xF1, 0x90]);
    assert_eq!(resp.unwrap(), [0x62, 0xF1, 0x90, 0x41]);
    assert!(ms >= 100.0, "{ms}");
    // The pending frame really went out first.
    let pending = rig
        .frames
        .iter()
        .filter(|e| {
            e.frame.as_can().unwrap().id == 0x7E8 && e.frame.payload()[1..] == [0x7F, 0x22, 0x78]
        })
        .count();
    assert_eq!(pending, 1);
}

#[test]
fn diag_state_resets_on_new_simulation() {
    let mut rig = Rig::new(diag_cfg(false), None);
    rig.ask(&[0x14, 0xFF, 0xFF, 0xFF]);
    let mut fresh = Rig::new(diag_cfg(false), None);
    assert_eq!(
        fresh.ask(&[0x19, 0x01, 0xFF]),
        [0x59, 0x01, 0xFF, 0x01, 0, 2]
    );
}

#[test]
fn bad_config_is_rejected() {
    let mut cfg = diag_cfg(false);
    cfg.req_id = 0x8000;
    assert!(Simulation::new(&topo(cfg, None)).is_err());
    let mut cfg = diag_cfg(false);
    cfg.bus = Some(BusId(9));
    assert!(Simulation::new(&topo(cfg, None)).is_err());
}

#[test]
fn runner_diag_request_event() {
    let handle = Engine::spawn();
    let (tx, rx) = (&handle.cmd, &handle.events);
    let req = |payload: Vec<u8>| Command::DiagRequest {
        tester_bus: BUS,
        req_id: 0x7E0,
        resp_id: 0x7E8,
        extended: false,
        fd: false,
        payload,
        functional: false,
    };
    tx.send(Command::Load(topo(diag_cfg(false), None))).unwrap();
    tx.send(Command::SetSpeed(0.0)).unwrap();
    // Not running: immediate error.
    tx.send(req(vec![0x3E, 0x00])).unwrap();
    tx.send(Command::Start).unwrap();
    tx.send(req(vec![0x22, 0xF1, 0x90])).unwrap();
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while got.len() < 2 && std::time::Instant::now() < deadline {
        if let Ok(EngineEvent::DiagResponse { req, resp, .. }) =
            rx.recv_timeout(Duration::from_millis(50))
        {
            got.push((req, resp));
        }
    }
    assert_eq!(got.len(), 2);
    assert!(got[0].1.is_err());
    assert_eq!(got[1].0, [0x22, 0xF1, 0x90]);
    assert_eq!(&got[1].1.as_ref().unwrap()[..3], [0x62, 0xF1, 0x90]);
    handle.shutdown();
}

#[test]
fn example_project_answers_vin_request() {
    let json = include_str!("../../../examples/diag_demo.operow.json");
    let topo = Topology::from_json(json).unwrap();
    let mut sim = Simulation::new(&topo).unwrap();
    let mut frames = Vec::new();
    sim.diag_request(DiagRequestSpec {
        bus: BUS,
        req_id: 0x7E0,
        resp_id: 0x7E8,
        extended: false,
        fd: false,
        payload: vec![0x22, 0xF1, 0x90],
        functional: false,
    })
    .unwrap();
    sim.run_until(Timestamp::from_ms(200), &mut frames);
    let r = sim.take_diag_results().pop().unwrap();
    assert_eq!(&r.resp.unwrap()[3..], VIN);
}
