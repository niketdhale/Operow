//! [`ReplayEcu`]: injects the records of a log (ASC or BLF) onto the simulated
//! buses at their log times.

use std::collections::VecDeque;
use std::path::Path;

use operow_core::{BusId, CanFrame, EcuConfig, IdExpr, NodeKind};
use operow_log::{LogReader, RecordKind, open_log};

use crate::ecu::{Ecu, EcuCtx};

/// Records held in memory ahead of the playback position. The log is read
/// lazily in chunks of this size, so memory stays flat for huge files.
const CHUNK: usize = 2048;
/// Logs with more records than this get a warning in the node's log.
pub const LARGE_LOG_RECORDS: u64 = 1_000_000;
/// The single timer a replay node uses: "the next record is due".
const PUMP_TIMER: u32 = 0;

type Reader = Box<dyn LogReader + Send>;

/// Totals found by the validating pass over the file.
struct Scan {
    records: u64,
    /// Time of the last record that survives mapping and filtering.
    last_ns: u64,
}

/// A frame waiting for its send time (absolute virtual nanoseconds).
struct Due {
    at_ns: u64,
    bus: BusId,
    frame: CanFrame,
}

/// Replays an ASC file on the buses of a [`NodeKind::Replay`] node. Records
/// are sent with [`EcuCtx::send_on`] at `record time + offset`; only mapped
/// channels and ids passing the filter are sent, error frames are skipped.
/// A looped replay restarts after its last record: cycle `k` is shifted by
/// `k` times the time of that last record.
pub struct ReplayEcu {
    path: String,
    map: Vec<(u8, BusId)>,
    looped: bool,
    offset_ns: i64,
    filter: IdExpr,
    reader: Option<Reader>,
    /// Time of the last sent-able record, the length of one loop cycle.
    cycle_ns: u64,
    cycle: u64,
    /// Records of the current cycle that passed mapping and filter.
    cycle_sent: u64,
    queue: VecDeque<Due>,
    eof: bool,
    scan: Scan,
    logs: Vec<String>,
}

impl ReplayEcu {
    /// Open `config`'s log and validate it with one streaming pass (parse
    /// errors surface here, so the simulation fails to build instead of
    /// replaying half a file). Errors are plain messages.
    pub fn new(config: &EcuConfig) -> Result<Self, String> {
        let NodeKind::Replay {
            path,
            channel_map,
            looped,
            time_offset_ms,
            id_filter,
        } = &config.kind
        else {
            return Err("not a replay node".into());
        };
        if path.trim().is_empty() {
            return Err("no log file selected".into());
        }
        let filter =
            IdExpr::parse(id_filter.as_deref().unwrap_or("")).map_err(|e| e.to_string())?;
        let mut ecu = ReplayEcu {
            path: path.clone(),
            map: channel_map.clone(),
            looped: *looped,
            offset_ns: time_offset_ms.saturating_mul(1_000_000),
            filter,
            reader: None,
            cycle_ns: 0,
            cycle: 0,
            cycle_sent: 0,
            queue: VecDeque::new(),
            eof: false,
            scan: Scan {
                records: 0,
                last_ns: 0,
            },
            logs: Vec::new(),
        };
        let mut reader = ecu.open()?;
        let mut total = 0;
        for rec in &mut reader {
            let rec = rec.map_err(|e| format!("{path}: {e}"))?;
            total += 1;
            if ecu.pick(&rec).is_some() {
                ecu.scan.last_ns = rec.time.0;
            }
        }
        ecu.scan.records = total;
        ecu.cycle_ns = ecu.scan.last_ns;
        if total > LARGE_LOG_RECORDS {
            ecu.logs.push(format!(
                "replay {}: {total} records (large log, played lazily)",
                config.name
            ));
        }
        Ok(ecu)
    }

    fn open(&self) -> Result<Reader, String> {
        open_log(Path::new(&self.path)).map_err(|e| format!("{}: {e}", self.path))
    }

    /// The bus and frame of `rec` when it is mapped, not an error frame and
    /// passes the id filter.
    fn pick(&self, rec: &operow_log::LogRecord) -> Option<(BusId, CanFrame)> {
        let RecordKind::Frame(frame) = rec.kind else {
            return None;
        };
        let bus = self.map.iter().find(|(c, _)| *c == rec.channel)?.1;
        self.filter.matches(frame.id).then_some((bus, frame))
    }

    /// Read until `CHUNK` frames are queued or the log is over.
    fn refill(&mut self) {
        while self.queue.len() < CHUNK && !self.eof {
            let Some(reader) = self.reader.as_mut() else {
                self.eof = true;
                return;
            };
            match reader.next() {
                Some(Ok(rec)) => {
                    let Some((bus, frame)) = self.pick(&rec) else {
                        continue;
                    };
                    self.cycle_sent += 1;
                    let at = i128::from(rec.time.0)
                        + i128::from(self.cycle) * i128::from(self.cycle_ns)
                        + i128::from(self.offset_ns);
                    // Records shifted before t = 0 are dropped.
                    if let Ok(at_ns) = u64::try_from(at) {
                        self.queue.push_back(Due { at_ns, bus, frame });
                    }
                }
                Some(Err(e)) => {
                    self.logs.push(format!("replay {}: {e}", self.path));
                    self.eof = true;
                }
                None => {
                    if self.looped && self.cycle_ns > 0 && self.cycle_sent > 0 {
                        match self.open() {
                            Ok(r) => {
                                self.reader = Some(r);
                                self.cycle += 1;
                                self.cycle_sent = 0;
                            }
                            Err(e) => {
                                self.logs.push(format!("replay: {e}"));
                                self.eof = true;
                            }
                        }
                    } else {
                        self.eof = true;
                    }
                }
            }
        }
    }

    fn arm(&mut self, ctx: &mut EcuCtx) {
        if let Some(next) = self.queue.front() {
            ctx.set_timer(PUMP_TIMER, next.at_ns.saturating_sub(ctx.now().0));
        }
    }
}

impl Ecu for ReplayEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        self.reader = self.open().ok();
        self.refill();
        self.arm(ctx);
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if timer != PUMP_TIMER {
            return;
        }
        let now = ctx.now().0;
        while self.queue.front().is_some_and(|d| d.at_ns <= now) {
            let d = self.queue.pop_front().expect("front exists");
            ctx.send_on(d.bus, d.frame);
        }
        if self.queue.len() < CHUNK / 2 {
            self.refill();
        }
        self.arm(ctx);
    }

    fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }
}
