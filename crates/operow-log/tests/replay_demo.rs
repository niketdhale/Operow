//! The replay example: the Replay node feeds Powertrain and the gateway
//! forwards the replayed frames to Body.

use operow_core::{BusId, Timestamp, Topology};
use operow_engine::Simulation;

#[test]
fn replay_demo_feeds_powertrain_and_gateway_forwards_to_body() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/replay_demo.operow.json"
    );
    let mut topo = Topology::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    // The app resolves relative log paths against the project folder.
    for n in &mut topo.nodes {
        if let operow_core::NodeKind::Replay { path, .. } = &mut n.kind {
            *path = format!("{}/../../examples/{path}", env!("CARGO_MANIFEST_DIR"));
        }
    }
    let mut sim = Simulation::new(&topo).unwrap();
    let mut out = Vec::new();
    sim.run_until(Timestamp::from_ms(500), &mut out);
    let on = |bus: u32, id: u32| {
        out.iter()
            .filter(|e| e.bus == BusId(bus) && e.frame.as_can().unwrap().id == id)
            .count()
    };
    assert!(on(1, 0x100) >= 45, "replayed EngineData on Powertrain");
    assert!(on(2, 0x100) >= 45, "forwarded to Body");
    assert!(on(1, 0x1A0) >= 20 && on(2, 0x1A0) >= 20);
    assert!(on(1, 0x300) > 0, "replayed");
    assert_eq!(on(2, 0x300), 0, "outside the gateway route");
}
