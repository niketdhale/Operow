use crate::{BusId, CanBusConfig, CanFrame, EcuConfig, Link, NodeId, Topology, TxMessage};

#[test]
fn topology_json_roundtrip() {
    let topo = Topology {
        nodes: vec![EcuConfig {
            id: NodeId(1),
            name: "ECU_A".into(),
            tx: vec![TxMessage {
                name: "Msg1".into(),
                frame: CanFrame::new(0x100, false, &[1, 2, 3]).unwrap(),
                period_ms: 10,
                enabled: true,
            }],
            pos: (1.0, 2.0),
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

    let json = topo.to_json();
    let back = Topology::from_json(&json).unwrap();
    assert_eq!(topo, back);
    assert!(back.validate().is_ok());
}

#[test]
fn frame_new_validates_id_ranges() {
    assert!(CanFrame::new(0x7FF, false, &[]).is_ok());
    assert!(CanFrame::new(0x800, false, &[]).is_err());
    assert!(CanFrame::new(0x1FFF_FFFF, true, &[]).is_ok());
    assert!(CanFrame::new(0x2000_0000, true, &[]).is_err());
    assert!(CanFrame::new(0, false, &[0; 9]).is_err());
}

#[test]
fn frame_new_fd_validates_lengths() {
    assert!(CanFrame::new_fd(0x100, false, true, &[0; 64]).is_ok());
    assert!(CanFrame::new_fd(0x100, false, true, &[0; 48]).is_ok());
    assert!(CanFrame::new_fd(0x100, false, false, &[0; 8]).is_ok());
    // 9-11 are not valid FD lengths (next step is 12).
    assert!(CanFrame::new_fd(0x100, false, true, &[0; 9]).is_err());
    assert!(CanFrame::new_fd(0x100, false, true, &[0; 11]).is_err());
    assert!(CanFrame::new_fd(0x100, false, true, &[0; 13]).is_err());
    assert!(CanFrame::new_fd(0x100, false, true, &[0; 65]).is_err());
}

#[test]
fn frame_serde_roundtrip_fd_64_bytes() {
    let data: Vec<u8> = (0..64).collect();
    let frame = CanFrame::new_fd(0x7AA, true, true, &data).unwrap();
    let json = serde_json::to_string(&frame).unwrap();
    let back: CanFrame = serde_json::from_str(&json).unwrap();
    assert_eq!(frame, back);
    assert_eq!(back.payload(), &data[..]);
    assert!(back.fd);
    assert!(back.brs);
}

#[test]
fn old_classic_json_without_fd_fields_still_deserializes() {
    let json = r#"{"id":256,"extended":false,"dlc":8,"data":[0,0,0,0,0,0,0,0]}"#;
    let frame: CanFrame = serde_json::from_str(json).unwrap();
    assert_eq!(frame.id, 256);
    assert!(!frame.fd);
    assert!(!frame.brs);
    assert_eq!(frame.dlc, 8);
    assert_eq!(frame.payload(), &[0u8; 8]);
}

fn topology_json_with_frame(frame_json: &str) -> String {
    format!(
        r#"{{"nodes":[{{"id":1,"name":"A","tx":[{{"name":"M","frame":{frame},"period_ms":10,"enabled":true}}]}}],"buses":[],"links":[]}}"#,
        frame = frame_json
    )
}

#[test]
fn invalid_frame_json_rejected_via_topology_from_json() {
    // brs set without fd.
    let json = topology_json_with_frame(
        r#"{"id":1,"extended":false,"fd":false,"brs":true,"dlc":1,"data":[0]}"#,
    );
    assert!(
        Topology::from_json(&json).is_err(),
        "brs without fd must be rejected"
    );

    // classic dlc > 8.
    let json =
        topology_json_with_frame(r#"{"id":1,"extended":false,"dlc":9,"data":[0,0,0,0,0,0,0,0,0]}"#);
    assert!(
        Topology::from_json(&json).is_err(),
        "classic dlc > 8 must be rejected"
    );

    // FD dlc not in the valid FD length set (13).
    let json = topology_json_with_frame(&format!(
        r#"{{"id":1,"extended":false,"fd":true,"dlc":13,"data":[{}]}}"#,
        vec!["0"; 13].join(",")
    ));
    assert!(
        Topology::from_json(&json).is_err(),
        "FD dlc outside the valid length set must be rejected"
    );

    // id out of range for a standard frame.
    let json = topology_json_with_frame(r#"{"id":2048,"extended":false,"dlc":0,"data":[]}"#);
    assert!(
        Topology::from_json(&json).is_err(),
        "out-of-range standard id must be rejected"
    );

    // dlc greater than the provided data (previously silently zero-padded).
    let json = topology_json_with_frame(r#"{"id":1,"extended":false,"dlc":4,"data":[1,2]}"#);
    assert!(
        Topology::from_json(&json).is_err(),
        "dlc greater than data.len() must be rejected, not zero-padded"
    );
}

#[test]
fn old_classic_json_still_loads_via_topology_from_json() {
    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/basic.operow.json"
    ))
    .unwrap();
    let topo = Topology::from_json(&json).expect("legacy classic topology must still parse");
    assert!(!topo.nodes.is_empty());
    for node in &topo.nodes {
        for tx in &node.tx {
            assert!(!tx.frame.fd);
            assert!(!tx.frame.brs);
        }
    }
}
