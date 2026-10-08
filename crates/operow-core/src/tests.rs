use crate::{
    BusId, CanBusConfig, CanFrame, DbcRef, DiagConfig, DidEntry, Domain, DtcEntry, EcuConfig,
    IdFilter, KeyAlgo, Link, NodeId, NodeKind, RouteRule, SecurityConfig, SendType,
    SignalByteOrder, Topology, TopologyError, TxMessage, UserSignalDef, UserSignalId, WireArrow,
    WireKind, WireLine, WireOverride, WireStyle,
};

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
                bus: None,
                send_type: Default::default(),
            }],
            kind: Default::default(),
            pos: (1.0, 2.0),
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
            kind: Default::default(),
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
    assert!(topo.validate().is_ok());
    for node in &topo.nodes {
        for tx in &node.tx {
            assert!(!tx.frame.fd);
            assert!(!tx.frame.brs);
        }
    }
}

#[test]
fn gateway_example_loads_and_validates() {
    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/gateway.operow.json"
    ))
    .unwrap();
    let topo = Topology::from_json(&json).unwrap();
    topo.validate().unwrap();
    assert!(matches!(topo.nodes[2].kind, NodeKind::Gateway { .. }));
}

#[test]
fn id_filter_matches() {
    let std = CanFrame::new(0x123, false, &[]).unwrap();
    let ext = CanFrame::new(0x123, true, &[]).unwrap();
    assert!(IdFilter::Any.matches(&std));
    let exact = IdFilter::Exact {
        id: 0x123,
        extended: false,
    };
    assert!(exact.matches(&std) && !exact.matches(&ext));
    assert!(
        IdFilter::Range {
            lo: 0x120,
            hi: 0x123
        }
        .matches(&std)
    );
    assert!(
        !IdFilter::Range {
            lo: 0x124,
            hi: 0x130
        }
        .matches(&std)
    );
    assert!(
        IdFilter::Mask {
            id: 0x120,
            mask: 0x7F0
        }
        .matches(&std)
    );
    assert!(
        !IdFilter::Mask {
            id: 0x130,
            mask: 0x7F0
        }
        .matches(&std)
    );
}

#[test]
fn validate_rejects_bad_bus_references() {
    let mut topo = Topology {
        nodes: vec![EcuConfig {
            id: NodeId(1),
            name: "G".into(),
            tx: vec![],
            kind: NodeKind::Gateway {
                routes: vec![RouteRule {
                    from_bus: BusId(1),
                    to_buses: vec![BusId(2)],
                    filter: IdFilter::Any,
                    remap_id: None,
                    delay_us: 0,
                }],
            },
            pos: (0.0, 0.0),
            script: None,
            diag: None,
        }],
        buses: ["A", "B"]
            .iter()
            .enumerate()
            .map(|(i, n)| CanBusConfig {
                id: BusId(i as u32 + 1),
                name: (*n).into(),
                bitrate: 500_000,
                fd_enabled: false,
                data_bitrate: 2_000_000,
                simulate_ack: false,
                kind: Default::default(),
                hardware: None,
            })
            .collect(),
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
    assert_eq!(
        topo.validate(),
        Err(TopologyError::RouteBusNotLinked {
            node: NodeId(1),
            bus: BusId(2)
        })
    );
    topo.links.push(Link {
        node: NodeId(1),
        bus: BusId(2),
    });
    assert!(topo.validate().is_ok());

    if let NodeKind::Gateway { routes } = &mut topo.nodes[0].kind {
        routes[0].to_buses = vec![BusId(1)];
    }
    assert_eq!(
        topo.validate(),
        Err(TopologyError::RouteToSameBus {
            node: NodeId(1),
            bus: BusId(1)
        })
    );

    topo.nodes[0].kind = NodeKind::Ecu;
    topo.nodes[0].tx.push(TxMessage {
        name: "M".into(),
        frame: CanFrame::new(1, false, &[]).unwrap(),
        period_ms: 10,
        enabled: true,
        bus: Some(BusId(3)),
        send_type: Default::default(),
    });
    assert_eq!(
        topo.validate(),
        Err(TopologyError::TxBusNotLinked {
            node: NodeId(1),
            bus: BusId(3)
        })
    );
}

#[test]
fn tx_message_without_send_type_loads_as_cyclic() {
    let msg = TxMessage {
        name: "M".into(),
        frame: CanFrame::new(1, false, &[1]).unwrap(),
        period_ms: 10,
        enabled: true,
        bus: None,
        send_type: SendType::OnChange { min_gap_ms: 5 },
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"OnChange""#));
    let back: TxMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(back, msg);

    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value.as_object_mut().unwrap().remove("send_type");
    let old: TxMessage = serde_json::from_value(value).unwrap();
    assert_eq!(old.send_type, SendType::Cyclic);
}

#[test]
fn old_send_type_names_deserialize() {
    use crate::SendType;
    let old: SendType = serde_json::from_str(r#"{"type":"Spontaneous"}"#).unwrap();
    assert_eq!(old, SendType::Event);
    let old: SendType = serde_json::from_str(r#"{"type":"CyclicAndSpontaneous"}"#).unwrap();
    assert_eq!(old, SendType::CyclicAndEvent);
    let new: SendType = serde_json::from_str(r#"{"type":"Event"}"#).unwrap();
    assert_eq!(new, SendType::Event);
}

#[test]
fn ecu_without_script_field_loads() {
    let json = r#"{"id":1,"name":"A","tx":[]}"#;
    let ecu: EcuConfig = serde_json::from_str(json).unwrap();
    assert_eq!(ecu.script, None);
}

#[test]
fn databases_default_empty_and_validated() {
    let mut topo =
        Topology::from_json(r#"{"buses":[{"id":1,"name":"A","bitrate":500000}]}"#).unwrap();
    assert!(topo.databases.is_empty());
    topo.databases.push(DbcRef {
        path: "a.dbc".into(),
        bus: BusId(1),
    });
    assert_eq!(Topology::from_json(&topo.to_json()).unwrap(), topo);
    assert_eq!(topo.validate(), Ok(()));
    topo.databases[0].bus = BusId(5);
    assert_eq!(
        topo.validate(),
        Err(TopologyError::DatabaseUnknownBus {
            path: "a.dbc".into(),
            bus: BusId(5)
        })
    );
}

#[test]
fn user_signals_round_trip_and_default_empty() {
    let mut topo = Topology::from_json("{}").unwrap();
    assert!(topo.user_signals.is_empty());
    assert!(!topo.to_json().contains("user_signals"));
    topo.user_signals.push(UserSignalDef {
        id: UserSignalId(3),
        name: "Speed".into(),
        bus: BusId(1),
        msg_id: 0x100,
        extended: false,
        start_bit: 8,
        size: 16,
        byte_order: SignalByteOrder::Motorola,
        signed: true,
        factor: 0.5,
        offset: -10.0,
        unit: "km/h".into(),
    });
    assert_eq!(Topology::from_json(&topo.to_json()).unwrap(), topo);
}

fn replay_topology(map: Vec<(u8, BusId)>) -> Topology {
    Topology {
        nodes: vec![EcuConfig {
            id: NodeId(1),
            name: "Replay".into(),
            tx: vec![],
            kind: NodeKind::Replay {
                path: "log.asc".into(),
                channel_map: map,
                looped: true,
                time_offset_ms: -5,
                id_filter: Some("100-1FF, !150".into()),
            },
            pos: (0.0, 0.0),
            script: None,
            diag: None,
        }],
        buses: vec![CanBusConfig {
            id: BusId(1),
            name: "A".into(),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            kind: Default::default(),
            hardware: None,
        }],
        links: vec![Link {
            node: NodeId(1),
            bus: BusId(1),
        }],
        ..Default::default()
    }
}

#[test]
fn replay_node_round_trips_and_validates() {
    let topo = replay_topology(vec![(1, BusId(1))]);
    let back = Topology::from_json(&topo.to_json()).unwrap();
    assert_eq!(back, topo);
    assert!(topo.to_json().contains("\"type\": \"Replay\""));
    assert_eq!(topo.validate(), Ok(()));
}

#[test]
fn replay_node_fields_default_when_missing() {
    let json = r#"{"nodes":[{"id":1,"name":"R","tx":[],"kind":{"type":"Replay"}}]}"#;
    let topo = Topology::from_json(json).unwrap();
    assert_eq!(topo.nodes[0].kind, NodeKind::new_replay());
}

#[test]
fn replay_node_must_be_linked_to_mapped_bus() {
    let topo = replay_topology(vec![(1, BusId(1)), (2, BusId(2))]);
    assert_eq!(
        topo.validate(),
        Err(TopologyError::ReplayBusNotLinked {
            node: NodeId(1),
            bus: BusId(2)
        })
    );
}

#[test]
fn bus_event_without_kind_deserializes_as_frame() {
    use crate::{BusEvent, BusEventKind, CanErrorKind, Direction, Timestamp};
    let ev = BusEvent {
        time: Timestamp(5),
        bus: BusId(1),
        sender: NodeId(1),
        origin: NodeId(1),
        dir: Direction::Tx,
        frame_uid: 0,
        hop: 0,
        frame: CanFrame::new(0x10, false, &[1]).unwrap().into(),
        kind: BusEventKind::Error {
            error: CanErrorKind::Ack,
            node: NodeId(1),
        },
    };
    let mut v = serde_json::to_value(ev.clone()).unwrap();
    assert!(v.get("kind").is_some());
    let back: BusEvent = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(back, ev);
    v.as_object_mut().unwrap().remove("kind");
    let old: BusEvent = serde_json::from_value(v).unwrap();
    assert_eq!(old.kind, BusEventKind::Frame);
    assert!(!old.is_error());
}

#[test]
fn bus_without_simulate_ack_defaults_to_false() {
    let b: CanBusConfig = serde_json::from_str(r#"{"id":1,"name":"A","bitrate":500000}"#).unwrap();
    assert!(!b.simulate_ack);
}

#[test]
fn diag_config_defaults_and_old_json() {
    // A node without `diag` (old project files) loads with none.
    let old = r#"{"nodes":[{"id":1,"name":"A","tx":[]}],"buses":[],"links":[]}"#;
    assert_eq!(Topology::from_json(old).unwrap().nodes[0].diag, None);

    let d: DiagConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(d, DiagConfig::default());
    assert_eq!((d.req_id, d.resp_id), (0x7E0, 0x7E8));
    assert_eq!(d.functional_id, Some(0x7DF));
    assert_eq!(d.sessions_supported, vec![1, 2, 3]);
    assert_eq!((d.p2_ms, d.p2_star_ms), (50, 5000));

    let mut topo = Topology::default();
    topo.nodes.push(EcuConfig {
        id: NodeId(1),
        name: "Engine".into(),
        tx: vec![],
        kind: NodeKind::Ecu,
        pos: (0.0, 0.0),
        script: None,
        diag: Some(DiagConfig {
            dids: vec![DidEntry {
                did: 0xF190,
                name: "VIN".into(),
                data: vec![1, 2],
                writable: true,
            }],
            dtcs: vec![DtcEntry {
                code: 0x012300,
                status: 9,
            }],
            security: Some(SecurityConfig {
                level: 1,
                seed: vec![1],
                key_algo: KeyAlgo::XorConst(vec![0xFF]),
            }),
            ..DiagConfig::default()
        }),
    });
    assert_eq!(Topology::from_json(&topo.to_json()).unwrap(), topo);
}

#[test]
fn diag_bus_must_be_linked() {
    let mut topo = Topology::default();
    topo.buses.push(CanBusConfig {
        id: BusId(1),
        name: "CAN0".into(),
        bitrate: 500_000,
        fd_enabled: false,
        data_bitrate: 2_000_000,
        simulate_ack: false,
        kind: Default::default(),
        hardware: None,
    });
    topo.nodes.push(EcuConfig {
        id: NodeId(1),
        name: "A".into(),
        tx: vec![],
        kind: NodeKind::Ecu,
        pos: (0.0, 0.0),
        script: None,
        diag: Some(DiagConfig {
            bus: Some(BusId(1)),
            ..DiagConfig::default()
        }),
    });
    assert_eq!(
        topo.validate(),
        Err(TopologyError::DiagBusNotLinked {
            node: NodeId(1),
            bus: BusId(1)
        })
    );
    topo.links.push(Link {
        node: NodeId(1),
        bus: BusId(1),
    });
    assert_eq!(topo.validate(), Ok(()));
}

#[test]
fn bus_hardware_binding_defaults_and_roundtrips() {
    let b: CanBusConfig = serde_json::from_str(r#"{"id":1,"name":"A","bitrate":500000}"#).unwrap();
    assert_eq!(b.hardware, None);
    let b: CanBusConfig = serde_json::from_str(
        r#"{"id":1,"name":"A","bitrate":500000,"hardware":{"interface":"socketcan:can0"}}"#,
    )
    .unwrap();
    let hw = b.hardware.clone().unwrap();
    assert!(hw.listen_only && !hw.receive_own);
    let back: CanBusConfig = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
    assert_eq!(back, b);
}

fn domain(id: u32, members: &[u32], parent: Option<u32>) -> Domain {
    Domain {
        id,
        name: format!("D{id}"),
        color: None,
        members: members.iter().map(|m| NodeId(*m)).collect(),
        bus_members: Vec::new(),
        collapsed: false,
        parent,
    }
}

fn ecu_topology(ids: &[u32]) -> Topology {
    let nodes: Vec<String> = ids
        .iter()
        .map(|i| format!(r#"{{"id":{i},"name":"E{i}","tx":[]}}"#))
        .collect();
    Topology::from_json(&format!(r#"{{"nodes":[{}]}}"#, nodes.join(","))).unwrap()
}

#[test]
fn domains_and_wire_styles_roundtrip_and_old_json_loads() {
    let old = Topology::from_json(r#"{"nodes":[],"buses":[]}"#).unwrap();
    assert!(old.domains.is_empty() && old.wire_default.is_none() && old.wires.is_empty());
    assert!(!old.to_json().contains("domains"));

    let mut topo = ecu_topology(&[1, 2]);
    topo.domains = vec![domain(1, &[1], None), domain(2, &[2], Some(1))];
    topo.wire_default = Some(WireStyle {
        line: Some(WireLine::Dotted),
        ..Default::default()
    });
    topo.wires = vec![WireOverride {
        node: NodeId(1),
        bus: BusId(1),
        style: WireStyle {
            kind: Some(WireKind::Step),
            arrow: Some(WireArrow::Diamond),
            label: Some("x".into()),
            ..Default::default()
        },
    }];
    assert_eq!(Topology::from_json(&topo.to_json()).unwrap(), topo);
    assert_eq!(topo.validate(), Ok(()));
}

#[test]
fn domain_validation_errors() {
    let mut topo = ecu_topology(&[1, 2]);
    topo.domains = vec![domain(1, &[9], None)];
    assert_eq!(
        topo.validate(),
        Err(TopologyError::DomainUnknownNode {
            domain: 1,
            node: NodeId(9)
        })
    );
    topo.domains = vec![domain(1, &[1], None), domain(2, &[1], None)];
    assert_eq!(
        topo.validate(),
        Err(TopologyError::NodeInTwoDomains(NodeId(1)))
    );
    topo.domains = vec![domain(1, &[1], Some(7))];
    assert_eq!(
        topo.validate(),
        Err(TopologyError::DomainUnknownParent {
            domain: 1,
            parent: 7
        })
    );
    topo.domains = vec![domain(1, &[], Some(2)), domain(2, &[], Some(1))];
    assert!(matches!(
        topo.validate(),
        Err(TopologyError::DomainCycle(_))
    ));
    topo.domains = vec![domain(1, &[], Some(1))];
    assert_eq!(topo.validate(), Err(TopologyError::DomainCycle(1)));
    topo.domains = vec![domain(1, &[], None), domain(1, &[], None)];
    assert_eq!(topo.validate(), Err(TopologyError::DuplicateDomain(1)));
}

#[test]
fn wire_style_unset_fields_fall_back() {
    let default = WireStyle {
        kind: Some(WireKind::Straight),
        width: Some(3.0),
        ..Default::default()
    };
    let link = WireStyle {
        kind: Some(WireKind::Step),
        ..Default::default()
    };
    let r = link.over(&default);
    assert_eq!(
        (r.kind, r.width, r.line),
        (Some(WireKind::Step), Some(3.0), None)
    );
    assert!(WireStyle::default().is_empty());
}

#[test]
fn bus_without_kind_loads_as_can_and_ethernet_rejects_can_options() {
    let b: CanBusConfig = serde_json::from_str(r#"{"id":1,"name":"A","bitrate":500000}"#).unwrap();
    assert_eq!(b.kind, crate::BusKind::Can);
    let mut topo = Topology::default();
    topo.buses.push(CanBusConfig {
        kind: crate::BusKind::Ethernet,
        ..b
    });
    assert_eq!(topo.validate(), Ok(()));
    topo.buses[0].fd_enabled = true;
    assert_eq!(topo.validate(), Err(TopologyError::CanOnlyOption(BusId(1))));
}
