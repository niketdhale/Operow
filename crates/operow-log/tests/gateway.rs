//! Run the engine on the gateway example, log to ASC and read it back.

use std::collections::HashMap;
use std::io::{BufReader, Cursor};
use std::time::{Duration, Instant};

use operow_core::{BusEvent, BusId, Topology};
use operow_engine::{Command, Engine, EngineEvent};
use operow_log::{
    AscDate, AscReader, AscWriter, BlfReader, BlfWriter, LogRecord, LogWriter, RecordKind,
};

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
            kind: RecordKind::Frame(*ev.frame.as_can().unwrap()),
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
    let want: Vec<u32> = events
        .iter()
        .map(|e| e.frame.as_can().unwrap().id)
        .collect();
    assert_eq!(ids, want);
}

#[test]
fn gateway_run_converts_asc_to_blf_and_back() {
    let (topo, events) = run_gateway(1.0);
    assert!(events.len() > 10, "engine produced {} frames", events.len());

    let channel: HashMap<BusId, u8> = topo
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i as u8 + 1))
        .collect();
    let mut asc = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
    for ev in &events {
        asc.write(&LogRecord {
            time: ev.time,
            channel: channel[&ev.bus],
            dir: ev.dir,
            kind: RecordKind::Frame(*ev.frame.as_can().unwrap()),
        })
        .unwrap();
    }
    let mut asc_bytes = asc.into_inner();
    asc_bytes.extend_from_slice(b"End TriggerBlock\n");
    let from_asc: Vec<LogRecord> = AscReader::new(BufReader::new(asc_bytes.as_slice()))
        .collect::<Result<_, _>>()
        .unwrap();

    let mut blf = BlfWriter::new(Cursor::new(Vec::new()), AscDate::from_unix_ms(0)).unwrap();
    for r in &from_asc {
        blf.write(r).unwrap();
    }
    let blf_bytes = blf.finish_into_inner().unwrap().into_inner();
    let from_blf: Vec<LogRecord> = BlfReader::new(blf_bytes.as_slice())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(from_blf, from_asc);

    let mut asc2 = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
    for r in &from_blf {
        asc2.write(r).unwrap();
    }
    let mut asc2_bytes = asc2.into_inner();
    asc2_bytes.extend_from_slice(b"End TriggerBlock\n");
    let again: Vec<LogRecord> = AscReader::new(BufReader::new(asc2_bytes.as_slice()))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(again, from_asc);
}
