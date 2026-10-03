//! Run the engine on the gateway example, log to ASC and read it back.

use std::collections::HashMap;
use std::io::BufReader;
use std::time::{Duration, Instant};

use operow_core::{BusEvent, BusId, Topology};
use operow_engine::{Command, Engine, EngineEvent};
use operow_log::{AscDate, AscReader, AscWriter, LogRecord, LogWriter, RecordKind};

fn run_gateway(secs: f64) -> (Topology, Vec<BusEvent>) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/gateway.operow.json"
    );
    let topo = Topology::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let engine = Engine::spawn();
    engine.cmd.send(Command::Load(topo.clone())).unwrap();
    engine.cmd.send(Command::SetSpeed(1.0)).unwrap();
    engine.cmd.send(Command::Start).unwrap();
    let mut events = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if let Ok(EngineEvent::Frames(f)) = engine.events.recv_timeout(Duration::from_millis(20)) {
            events.extend(f);
        }
    }
    engine.cmd.send(Command::Stop).unwrap();
    engine.shutdown();
    (topo, events)
}

#[test]
fn gateway_run_round_trips_through_asc() {
    let (topo, events) = run_gateway(1.0);
    assert!(events.len() > 10, "engine produced {} frames", events.len());

    let channel: HashMap<BusId, u8> = topo
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i as u8 + 1))
        .collect();
    let mut w = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
    let mut expected = Vec::new();
    for ev in &events {
        let r = LogRecord {
            // ASC keeps microseconds.
            time: operow_core::Timestamp(ev.time.0 / 1000 * 1000),
            channel: channel[&ev.bus],
            dir: ev.dir,
            kind: RecordKind::Frame(ev.frame),
        };
        w.write(&r).unwrap();
        expected.push(r);
    }
    let mut bytes = w.into_inner();
    bytes.extend_from_slice(b"End TriggerBlock\n");

    let back: Vec<LogRecord> = AscReader::new(BufReader::new(bytes.as_slice()))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(back.len(), events.len());
    assert_eq!(back, expected);
    let ids: Vec<u32> = back
        .iter()
        .map(|r| match r.kind {
            RecordKind::Frame(f) => f.id,
            RecordKind::ErrorFrame => unreachable!(),
        })
        .collect();
    let want: Vec<u32> = events.iter().map(|e| e.frame.id).collect();
    assert_eq!(ids, want);
}
