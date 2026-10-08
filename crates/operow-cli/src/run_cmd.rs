//! `operow-cli run`.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use operow_core::{BusEvent, BusId, Timestamp};
use operow_engine::{HwBridge, HwNotice, Simulation};
use operow_log::{LogRecord, RecordKind};
use operow_test::Project;

use crate::RunArgs;
use crate::log_cmd::AnyWriter;

/// Virtual time simulated per call to `run_until`.
const CHUNK_NS: u64 = 10_000_000;

/// Parse `500ms`, `10s`, `1.5s`, `2m`; a bare number is seconds.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, scale) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1e6)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1e9)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60e9)
    } else {
        (s, 1e9)
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("bad duration {s:?}: use e.g. 500ms, 10s or 2m"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("bad duration {s:?}"));
    }
    Ok((v * scale).round() as u64)
}

pub fn run(a: &RunArgs) -> Result<ExitCode, String> {
    let total_ns = parse_duration(&a.duration)?;
    let project = Project::load(&a.project).map_err(|e| e.to_string())?;
    let topo = &project.topology;
    let mut sim = Simulation::new(topo).map_err(|e| e.to_string())?;
    sim.set_seed(a.seed);
    let channels: HashMap<BusId, u8> = topo
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, (i + 1).min(255) as u8))
        .collect();
    let mut writer = match &a.log {
        Some(p) => Some(AnyWriter::create(p)?),
        None => None,
    };

    // Buses bound to hardware force real time.
    let mut bridge = HwBridge::open(topo).map_err(|e| e.to_string())?;
    if bridge.is_some() {
        println!("Hardware buses present: running in real time (speed 1.0)");
    }
    let wall = Instant::now();
    let mut events: Vec<BusEvent> = Vec::new();
    let mut written = 0u64;
    let mut now = 0u64;
    while now < total_ns {
        events.clear();
        if let Some(b) = bridge.as_mut() {
            std::thread::sleep(Duration::from_millis(1));
            let (t, notices) = b.step(&mut sim, &mut events);
            now = t.min(total_ns);
            for n in notices {
                match n {
                    HwNotice::Log(l) => println!("[{:9.3} s] {l}", now as f64 / 1e9),
                    HwNotice::Error(e) => return Err(e),
                }
            }
        } else {
            now = (now + CHUNK_NS).min(total_ns);
            sim.run_until(Timestamp(now), &mut events);
        }
        if let Some(w) = &mut writer {
            for ev in &events {
                let Some(&channel) = channels.get(&ev.bus) else {
                    continue;
                };
                // ponytail: Ethernet not logged yet
                let Some(&frame) = ev.frame.as_can() else {
                    continue;
                };
                w.write(&LogRecord {
                    time: ev.time,
                    channel,
                    dir: ev.dir,
                    kind: if ev.is_error() {
                        RecordKind::ErrorFrame
                    } else {
                        RecordKind::Frame(frame)
                    },
                })
                .map_err(|e| format!("cannot write the log: {e}"))?;
                written += 1;
            }
        }
        for line in sim.drain_logs() {
            println!("[{:9.3} s] {line}", now as f64 / 1e9);
        }
        if a.realtime && bridge.is_none() {
            let due = Duration::from_nanos(now);
            if let Some(wait) = due.checked_sub(wall.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    }
    drop(bridge);
    if let Some(w) = writer {
        w.finish()
            .map_err(|e| format!("cannot write the log: {e}"))?;
    }

    println!(
        "Simulated {:.3} s of virtual time in {:.2} s wall time (seed {})",
        total_ns as f64 / 1e9,
        wall.elapsed().as_secs_f64(),
        a.seed
    );
    println!(
        "{:<16} {:>9} {:>10} {:>7} {:>7}",
        "Bus", "Bitrate", "Frames", "Load %", "Errors"
    );
    for b in &topo.buses {
        let s = sim.stats().get(&b.id).copied().unwrap_or_default();
        println!(
            "{:<16} {:>9} {:>10} {:>7.2} {:>7}",
            b.name,
            b.bitrate,
            s.frames,
            s.load(total_ns) * 100.0,
            s.can_errors.total() + s.error_frames
        );
    }
    if let Some(p) = &a.log {
        println!("Log: {} ({written} records)", p.display());
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms"), Ok(500_000_000));
        assert_eq!(parse_duration("10s"), Ok(10_000_000_000));
        assert_eq!(parse_duration("1.5s"), Ok(1_500_000_000));
        assert_eq!(parse_duration("2m"), Ok(120_000_000_000));
        assert_eq!(parse_duration("3"), Ok(3_000_000_000));
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("-1s").is_err());
    }
}
