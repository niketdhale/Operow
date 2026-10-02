use operow_core::{BusId, CanFrame, EcuConfig, Link, NodeId, SendType, Topology};
use operow_dbc::{BusTarget, ByteOrder, Database, DbcError, MergeError, Mux, SignalDef, ValueType};

const SAMPLE: &str = include_str!("fixtures/sample.dbc");

fn sig(start: u16, size: u16, order: ByteOrder, vt: ValueType) -> SignalDef {
    SignalDef {
        name: "S".into(),
        start_bit: start,
        size,
        byte_order: order,
        value_type: vt,
        factor: 1.0,
        offset: 0.0,
        min: 0.0,
        max: 0.0,
        unit: String::new(),
        receivers: vec![],
        multiplexer: None,
        initial_raw: None,
        value_descriptions: vec![],
        comment: None,
    }
}

#[test]
fn parses_sample() {
    let db = Database::parse(SAMPLE).unwrap();
    assert_eq!(db.version, "1.0");
    assert_eq!(db.nodes, ["Engine", "Gateway", "Dash"]);
    assert_eq!(db.messages.len(), 3);

    let e = &db.messages[0];
    assert_eq!((e.id, e.extended, e.dlc), (256, false, 8));
    assert_eq!(e.transmitter, "Engine");
    assert_eq!(e.cycle_time_ms, Some(10));
    assert_eq!(e.send_type.as_deref(), Some("Cyclic"));
    assert_eq!(e.comment.as_deref(), Some("Engine data,\nsent every 10 ms"));
    let rpm = &e.signals[0];
    assert_eq!(rpm.byte_order, ByteOrder::Motorola);
    assert_eq!(rpm.factor, 0.25);
    assert_eq!(rpm.receivers, ["Gateway", "Dash"]);
    assert_eq!(rpm.comment.as_deref(), Some("Crankshaft speed"));
    let temp = &e.signals[1];
    assert_eq!(temp.value_type, ValueType::Signed);
    assert_eq!(temp.offset, -40.0);
    assert_eq!(temp.min, -40.0);
    assert_eq!(
        e.signals[3].value_descriptions,
        [(0, "Off".into()), (1, "On".into())]
    );

    let g = &db.messages[1];
    assert_eq!((g.id, g.extended), (0x500, true));
    assert_eq!(g.signals[0].multiplexer, Some(Mux::Multiplexor));
    assert_eq!(g.signals[1].multiplexer, Some(Mux::Multiplexed(0)));

    let d = &db.messages[2];
    assert_eq!(d.cycle_time_ms, None);
    assert_eq!(d.send_type.as_deref(), Some("Event"));
    assert_eq!(d.signals[0].initial_raw, Some(128));
}

#[test]
fn handles_crlf() {
    let crlf = SAMPLE.replace('\n', "\r\n");
    let a = Database::parse(&crlf).unwrap();
    let b = Database::parse(SAMPLE).unwrap();
    assert_eq!(a.messages.len(), b.messages.len());
    assert_eq!(a.messages[0].signals, b.messages[0].signals);
    assert_eq!(a.messages[0].comment, b.messages[0].comment);
}

#[test]
fn syntax_error_has_line() {
    let err = Database::parse("VERSION \"1\"\nBO_ abc Foo: 8 X\n").unwrap_err();
    assert!(matches!(err, DbcError::Syntax { line: 2, .. }));
}

#[test]
fn intel_layout() {
    let s = sig(0, 16, ByteOrder::Intel, ValueType::Unsigned);
    let mut d = [0u8; 8];
    s.encode_raw(&mut d, 0x1234);
    assert_eq!(&d[..2], [0x34, 0x12]);
    assert_eq!(s.decode_raw(&d), 0x1234);
}

#[test]
fn motorola_layout() {
    let s = sig(7, 16, ByteOrder::Motorola, ValueType::Unsigned);
    let mut d = [0u8; 8];
    s.encode_raw(&mut d, 0x1234);
    assert_eq!(&d[..2], [0x12, 0x34]);
    assert_eq!(s.decode_raw(&d), 0x1234);
}

#[test]
fn crossing_byte_boundaries() {
    // Intel: 12 bits from bit 4 -> spans bytes 0 and 1.
    let s = sig(4, 12, ByteOrder::Intel, ValueType::Unsigned);
    let mut d = [0u8; 8];
    s.encode_raw(&mut d, 0xABC);
    assert_eq!(&d[..2], [0xC0, 0xAB]);
    assert_eq!(s.decode_raw(&d), 0xABC);
    // Motorola: 12 bits MSB at bit 3 -> byte0 low nibble, byte1 all.
    let s = sig(3, 12, ByteOrder::Motorola, ValueType::Unsigned);
    let mut d = [0u8; 8];
    s.encode_raw(&mut d, 0xABC);
    assert_eq!(&d[..2], [0x0A, 0xBC]);
    assert_eq!(s.decode_raw(&d), 0xABC);
    // Encoding must not disturb neighbouring bits.
    let mut d = [0xFFu8; 8];
    s.encode_raw(&mut d, 0);
    assert_eq!(&d[..3], [0xF0, 0x00, 0xFF]);
}

#[test]
fn signed_factor_offset_and_clamp() {
    let mut s = sig(0, 8, ByteOrder::Intel, ValueType::Signed);
    let mut d = [0u8; 8];
    s.encode(&mut d, -5.0);
    assert_eq!(d[0], 0xFB);
    assert_eq!(s.decode(&d), -5.0);
    s.encode(&mut d, 1000.0);
    assert_eq!(s.decode(&d), 127.0);
    s.encode(&mut d, -1000.0);
    assert_eq!(s.decode(&d), -128.0);

    s.factor = 0.5;
    s.offset = -40.0;
    s.value_type = ValueType::Unsigned;
    s.encode(&mut d, 25.0);
    assert_eq!(d[0], 130);
    assert_eq!(s.decode(&d), 25.0);
    s.encode(&mut d, -100.0);
    assert_eq!(d[0], 0);
}

#[test]
fn full_width_64() {
    let s = sig(0, 64, ByteOrder::Intel, ValueType::Unsigned);
    let mut d = [0u8; 8];
    s.encode_raw(&mut d, u64::MAX - 1);
    assert_eq!(s.decode_raw(&d), u64::MAX - 1);
}

#[test]
fn multiplexed_decode() {
    let db = Database::parse(SAMPLE).unwrap();
    let m = &db.messages[1];
    let mut data = [0u8; 8];
    m.signals[0].encode_raw(&mut data, 1);
    m.signals[2].encode(&mut data, -12.5);
    let frame = CanFrame::new(m.id, m.extended, &data).unwrap();
    let out = m.decode(&frame);
    assert_eq!(
        out,
        [("Mode".to_string(), 1.0), ("Current".to_string(), -12.5)]
    );

    m.signals[0].encode_raw(&mut data, 0);
    m.signals[1].encode(&mut data, 12.34);
    let frame = CanFrame::new(m.id, m.extended, &data).unwrap();
    let out = m.decode(&frame);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].0, "Voltage");
    assert!((out[1].1 - 12.34).abs() < 1e-9);
}

#[test]
fn message_decode_motorola_signal() {
    let db = Database::parse(SAMPLE).unwrap();
    let m = &db.messages[0];
    let mut data = [0u8; 8];
    m.signals[0].encode(&mut data, 3000.0);
    m.signals[1].encode(&mut data, 50.0);
    let frame = CanFrame::new(m.id, m.extended, &data).unwrap();
    let out = m.decode(&frame);
    assert_eq!(out[0], ("EngineSpeed".to_string(), 3000.0));
    assert_eq!(out[1], ("CoolantTemp".to_string(), 50.0));
}

#[test]
fn to_topology_validates() {
    let db = Database::parse(SAMPLE).unwrap();
    let topo = db.to_topology(BusId(1), "Powertrain", 500_000);
    topo.validate().unwrap();
    assert_eq!(topo.nodes.len(), 3);
    assert_eq!(topo.nodes[0].id.0, 1);
    assert_eq!(topo.links.len(), 3);
    assert_eq!(topo.buses[0].bitrate, 500_000);
    assert!(!topo.buses[0].fd_enabled);

    let engine = &topo.nodes[0];
    assert_eq!(engine.tx.len(), 1);
    assert_eq!(engine.tx[0].period_ms, 10);
    assert!(engine.tx[0].enabled);
    assert_eq!(engine.tx[0].frame.dlc, 8);

    let dash = &topo.nodes[2];
    assert_eq!(dash.tx[0].period_ms, 100);
    assert!(dash.tx[0].enabled);
    assert_eq!(dash.tx[0].send_type, SendType::Event);
    assert_eq!(engine.tx[0].send_type, SendType::Cyclic);
    assert_eq!(dash.tx[0].frame.payload(), [128, 0]);

    assert!(topo.nodes[1].tx[0].frame.extended);
}

#[test]
fn to_topology_fd_frame() {
    let db = Database::parse(
        "BU_: A\nBO_ 5 Big: 12 A\n SG_ X : 0|8@1+ (1,0) [0|255] \"\" A\nBA_ \"GenMsgCycleTime\" BO_ 5 20;\n",
    )
    .unwrap();
    let topo = db.to_topology(BusId(1), "FD", 500_000);
    topo.validate().unwrap();
    let f = &topo.nodes[0].tx[0].frame;
    assert!(f.fd && !f.brs);
    assert_eq!(f.dlc, 12);
    assert!(topo.buses[0].fd_enabled);
}

#[test]
fn send_type_mapping() {
    let cases = [
        (Some("Cyclic"), Some(10), SendType::Cyclic),
        (Some("cyclic"), None, SendType::Cyclic),
        (Some("Spontaneous"), Some(10), SendType::Event),
        (Some("NoMsgSendType"), None, SendType::Event),
        (Some("IfActive"), Some(10), SendType::CyclicIfActive),
        (Some("CyclicIfActive"), None, SendType::CyclicIfActive),
        (
            Some("CyclicAndSpontanX"),
            Some(10),
            SendType::CyclicAndEvent,
        ),
        (
            Some("CyclicIfActiveAndSpontanWithDelay"),
            Some(10),
            SendType::CyclicAndEvent,
        ),
        (Some("Bogus"), Some(10), SendType::Cyclic),
        (Some("Bogus"), None, SendType::Event),
        (None, Some(10), SendType::Cyclic),
        (None, None, SendType::Event),
    ];
    for (name, cycle, want) in cases {
        let mut db = Database::parse(SAMPLE).unwrap();
        let m = db
            .messages
            .iter_mut()
            .find(|m| m.transmitter == "Engine")
            .unwrap();
        m.send_type = name.map(String::from);
        m.cycle_time_ms = cycle;
        let topo = db.to_topology(BusId(1), "P", 500_000);
        let tx = &topo.nodes[0].tx[0];
        assert_eq!(tx.send_type, want, "{name:?} {cycle:?}");
        assert!(tx.enabled);
    }
}

fn existing_ecu(id: u32, name: &str) -> EcuConfig {
    EcuConfig {
        id: NodeId(id),
        name: name.into(),
        tx: vec![],
        kind: Default::default(),
        pos: (60.0, 60.0),
        script: None,
    }
}

fn base_topology() -> Topology {
    let mut t = Database::default().to_topology(BusId(1), "Old", 250_000);
    t.nodes.push(existing_ecu(7, "Engine"));
    t.links.push(Link {
        node: NodeId(7),
        bus: BusId(1),
    });
    t
}

#[test]
fn merge_new_bus_allocates_fresh_ids_and_links() {
    let db = Database::parse(SAMPLE).unwrap();
    let base = base_topology();
    let target = BusTarget::New {
        name: "PT".into(),
        bitrate: 500_000,
    };
    let (topo, bus) = db.merge_into(&base, target, true).unwrap();
    assert_eq!(bus, BusId(2));
    assert_eq!(topo.buses.len(), 2);
    // Engine reused (id 7); Gateway and Dash get 8 and 9.
    let ids: Vec<_> = topo
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.id.0))
        .collect();
    assert_eq!(ids, [("Engine", 7), ("Gateway", 8), ("Dash", 9)]);
    for n in &topo.nodes {
        assert!(topo.links.contains(&Link { node: n.id, bus }), "{}", n.name);
    }
    topo.validate().unwrap();
    let mut seen = std::collections::HashSet::new();
    assert!(topo.nodes.iter().all(|n| seen.insert(n.id)));
    // New nodes are placed in distinct spots.
    assert_ne!(topo.nodes[1].pos, topo.nodes[2].pos);
}

#[test]
fn merge_reuses_node_by_name_without_duplicating_tx() {
    let db = Database::parse(SAMPLE).unwrap();
    let base = base_topology();
    let target = BusTarget::Existing(BusId(1));
    let (once, _) = db.merge_into(&base, target.clone(), true).unwrap();
    let engine = once.nodes.iter().find(|n| n.name == "Engine").unwrap();
    assert_eq!(engine.id, NodeId(7));
    assert_eq!(engine.tx.len(), 1);
    assert_eq!(engine.tx[0].name, "EngineData");
    assert_eq!(engine.tx[0].bus, Some(BusId(1)));
    // Merging again changes nothing: no duplicate nodes, tx or links.
    let (twice, _) = db.merge_into(&once, target, true).unwrap();
    assert_eq!(twice, once);
    // Case-sensitive: "engine" is a different node.
    let mut lower = base.clone();
    lower.nodes[0].name = "engine".into();
    let (t, _) = db
        .merge_into(&lower, BusTarget::Existing(BusId(1)), true)
        .unwrap();
    assert_eq!(
        t.nodes
            .iter()
            .filter(|n| n.name.eq_ignore_ascii_case("engine"))
            .count(),
        2
    );
}

#[test]
fn merge_without_nodes_and_unknown_bus() {
    let db = Database::parse(SAMPLE).unwrap();
    let base = base_topology();
    let (t, bus) = db
        .merge_into(
            &base,
            BusTarget::New {
                name: "X".into(),
                bitrate: 125_000,
            },
            false,
        )
        .unwrap();
    assert_eq!((t.nodes.len(), t.links.len(), t.buses.len()), (1, 1, 2));
    assert_eq!(bus, BusId(2));
    assert_eq!(
        db.merge_into(&base, BusTarget::Existing(BusId(9)), true),
        Err(MergeError::UnknownBus(BusId(9)))
    );
}

#[test]
fn message_lookup_by_id_and_format() {
    let db = Database::parse(SAMPLE).unwrap();
    assert_eq!(db.message(256, false).unwrap().name, "EngineData");
    assert!(db.message(256, true).is_none());
    assert_eq!(
        db.message(0x500, true).map(|m| m.name.as_str()),
        Some("GatewayStatus")
    );
}
