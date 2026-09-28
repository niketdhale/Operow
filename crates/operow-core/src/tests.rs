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
