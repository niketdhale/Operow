//! Offline replay: stream an ASC log into the shared frame store by
//! virtual time, without running the simulation.
//!
//! A background thread parses the file and sends chunks of already-mapped
//! frames through a bounded channel, so a huge log is never loaded as a
//! whole. [`ReplaySource::advance`] moves a virtual clock and hands out the
//! events that became due. Seeking re-opens the file and fast-skips records
//! before the target time (parsing them, but keeping none), which is simple
//! and needs no index; the cost is linear in the distance from the start.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use operow_core::{BusEvent, BusId, CanFrame, Direction, NodeId, Timestamp};
use operow_log::{AscReader, RecordKind};

/// First [`NodeId`] of the virtual "Log" senders; channel `n` sends as
/// `LOG_NODE_BASE + n`. Below the generator range, above any topology node.
pub const LOG_NODE_BASE: u32 = 0xE000_0000;

/// The virtual sender of frames read from ASC channel `channel`.
pub fn log_sender(channel: u8) -> NodeId {
    NodeId(LOG_NODE_BASE + u32::from(channel))
}

/// The channel behind `node`, if it is a virtual log sender.
pub fn log_channel(node: NodeId) -> Option<u8> {
    node.0
        .checked_sub(LOG_NODE_BASE)
        .and_then(|c| u8::try_from(c).ok())
}

/// Records inspected to find the channels of a log.
const PROBE_RECORDS: usize = 20_000;
/// Frames per chunk sent by the reader thread.
const CHUNK: usize = 1024;
/// Chunks the reader may run ahead of playback.
const AHEAD_CHUNKS: usize = 8;
/// Most events one `advance` hands out, so fast speeds cannot stall the UI.
const MAX_EVENTS_PER_ADVANCE: usize = 50_000;
/// How much of the end of a file is read to find the last timestamp.
const TAIL_BYTES: u64 = 64 * 1024;

/// What opening a log needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogInfo {
    /// Channels seen in the first records, ascending.
    pub channels: Vec<u8>,
    pub first: Timestamp,
    pub last: Timestamp,
}

/// Channels used by the first records of `path` (cheap; for mapping UIs).
pub fn probe_channels(path: &Path) -> Result<Vec<u8>, String> {
    let mut channels = Vec::new();
    for rec in reader(path)?.take(PROBE_RECORDS) {
        let rec = rec.map_err(|e| e.to_string())?;
        if !channels.contains(&rec.channel) {
            channels.push(rec.channel);
        }
    }
    channels.sort_unstable();
    Ok(channels)
}

/// Channels plus the time span of `path`. The last time comes from the end
/// of the file; a log with relative timestamps needs a full pass instead.
pub fn probe(path: &Path) -> Result<LogInfo, String> {
    let mut first = None;
    let mut channels = Vec::new();
    for rec in reader(path)?.take(PROBE_RECORDS) {
        let rec = rec.map_err(|e| e.to_string())?;
        first.get_or_insert(rec.time);
        if !channels.contains(&rec.channel) {
            channels.push(rec.channel);
        }
    }
    channels.sort_unstable();
    let Some(first) = first else {
        return Err("the log has no records".into());
    };
    let last = if has_relative_timestamps(path) {
        let mut last = first;
        for rec in reader(path)?.flatten() {
            last = rec.time;
        }
        last
    } else {
        tail_last_time(path).unwrap_or(first).max(first)
    };
    Ok(LogInfo {
        channels,
        first,
        last,
    })
}

fn reader(path: &Path) -> Result<AscReader<BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(AscReader::new(BufReader::new(file)))
}

fn has_relative_timestamps(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut head = String::new();
    let _ = BufReader::new(file).take(2048).read_to_string(&mut head);
    head.lines()
        .take(20)
        .any(|l| l.contains("timestamps") && l.contains("relative"))
}

/// Time of the last parsable record in the final `TAIL_BYTES` of the file.
fn tail_last_time(path: &Path) -> Option<Timestamp> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // The first line is cut off unless the read began at the file start.
    let text = if start > 0 {
        text.split_once('\n')?.1
    } else {
        &text
    };
    AscReader::new(text.as_bytes())
        .flatten()
        .last()
        .map(|r| r.time)
}

/// A mapped frame as sent by the reader thread.
struct Rec {
    time: Timestamp,
    bus: BusId,
    channel: u8,
    dir: Direction,
    frame: CanFrame,
}

enum Msg {
    Chunk(Vec<Rec>),
    Done,
    Failed(String),
}

/// A running reader thread; dropping it stops the thread.
struct Reader {
    rx: mpsc::Receiver<Msg>,
    stop: Arc<AtomicBool>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        // The thread notices the flag or the closed channel, whichever
        // comes first (it may be blocked on a full channel).
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Reader {
    /// Read `path` from the start, skipping records before `from`.
    fn spawn(path: PathBuf, map: HashMap<u8, BusId>, from: Timestamp) -> Reader {
        let (tx, rx) = mpsc::sync_channel(AHEAD_CHUNKS);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        thread::spawn(move || {
            let records = match reader(&path) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e));
                    return;
                }
            };
            let mut chunk = Vec::with_capacity(CHUNK);
            for rec in records {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                let rec = match rec {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send(Msg::Chunk(std::mem::take(&mut chunk)));
                        let _ = tx.send(Msg::Failed(e.to_string()));
                        return;
                    }
                };
                if rec.time < from {
                    continue;
                }
                let RecordKind::Frame(frame) = rec.kind else {
                    continue;
                };
                let Some(&bus) = map.get(&rec.channel) else {
                    continue;
                };
                chunk.push(Rec {
                    time: rec.time,
                    bus,
                    channel: rec.channel,
                    dir: rec.dir,
                    frame,
                });
                if chunk.len() >= CHUNK
                    && tx
                        .send(Msg::Chunk(std::mem::replace(
                            &mut chunk,
                            Vec::with_capacity(CHUNK),
                        )))
                        .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(Msg::Chunk(chunk));
            let _ = tx.send(Msg::Done);
        });
        Reader { rx, stop }
    }
}

/// Result of one [`ReplaySource::advance`].
#[derive(Debug, Default)]
pub struct Advance {
    pub events: Vec<BusEvent>,
    /// A looped replay went back to the start: clear what was shown.
    pub restarted: bool,
}

/// Playback speed; `None` is "as fast as possible".
pub type Speed = Option<f64>;

/// Streams an ASC log as [`BusEvent`]s on a virtual clock.
pub struct ReplaySource {
    path: PathBuf,
    map: HashMap<u8, BusId>,
    reader: Option<Reader>,
    staged: VecDeque<Rec>,
    /// The reader sent everything (or failed).
    eof: bool,
    pub error: Option<String>,
    /// Virtual time: events with a time `<= pos` have been handed out.
    pos: Timestamp,
    pub first: Timestamp,
    pub last: Timestamp,
    pub playing: bool,
    pub speed: Speed,
    pub looped: bool,
    next_uid: u64,
    last_tick: Option<Instant>,
    /// How long `advance` waits for a slow reader before giving up for now.
    starve_wait: Duration,
}

impl ReplaySource {
    /// Start reading `path`; records of channels missing from `map` are
    /// skipped. Playback begins paused at the first record.
    pub fn new(path: &Path, info: &LogInfo, map: HashMap<u8, BusId>) -> Self {
        let mut s = ReplaySource {
            path: path.to_path_buf(),
            map,
            reader: None,
            staged: VecDeque::new(),
            eof: false,
            error: None,
            pos: info.first,
            first: info.first,
            last: info.last,
            playing: false,
            speed: Some(1.0),
            looped: false,
            next_uid: 0,
            last_tick: None,
            starve_wait: Duration::from_millis(20),
        };
        s.restart_reader(info.first);
        s
    }

    /// Current virtual time.
    pub fn position(&self) -> Timestamp {
        self.pos
    }

    fn restart_reader(&mut self, from: Timestamp) {
        self.reader = Some(Reader::spawn(self.path.clone(), self.map.clone(), from));
        self.staged.clear();
        self.eof = false;
        self.error = None;
    }

    /// Jump to `time` (clamped to the log): the file is re-opened and
    /// records before `time` are skipped. The caller clears the store.
    pub fn seek(&mut self, time: Timestamp) {
        let time = Timestamp(time.0.clamp(self.first.0, self.last.0));
        self.pos = time;
        self.restart_reader(time);
    }

    /// Pull staged events from the reader; `false` when nothing more is
    /// available right now (the reader is slower than playback).
    fn stage(&mut self) -> bool {
        while self.staged.is_empty() && !self.eof {
            let Some(reader) = &self.reader else {
                self.eof = true;
                break;
            };
            match reader.rx.recv_timeout(self.starve_wait) {
                Ok(Msg::Chunk(c)) => self.staged.extend(c),
                Ok(Msg::Done) | Err(mpsc::RecvTimeoutError::Disconnected) => self.eof = true,
                Ok(Msg::Failed(e)) => {
                    self.error = Some(e);
                    self.eof = true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return false,
            }
        }
        true
    }

    fn event(&mut self, r: Rec) -> BusEvent {
        let uid = self.next_uid;
        self.next_uid += 1;
        BusEvent {
            time: r.time,
            bus: r.bus,
            sender: log_sender(r.channel),
            origin: log_sender(r.channel),
            dir: r.dir,
            frame_uid: uid,
            hop: 0,
            frame: r.frame,
        }
    }

    /// Wall-clock driven [`ReplaySource::advance`]; call once per UI frame.
    pub fn tick(&mut self, now: Instant) -> Advance {
        let dt = self
            .last_tick
            .replace(now)
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        self.advance(dt)
    }

    /// Move the virtual clock by `dt` of real time at the current speed
    /// (does nothing while paused) and return the events that became due.
    pub fn advance(&mut self, dt: Duration) -> Advance {
        let mut out = Advance::default();
        if !self.playing {
            return out;
        }
        let target = self
            .speed
            .map(|s| self.pos.0.saturating_add((dt.as_nanos() as f64 * s) as u64));
        loop {
            if !self.stage() {
                // Reader behind: wait at the last delivered event.
                break;
            }
            let Some(front) = self.staged.front() else {
                break; // end of the log
            };
            if target.is_some_and(|t| front.time.0 > t)
                || out.events.len() >= MAX_EVENTS_PER_ADVANCE
            {
                break;
            }
            let rec = self.staged.pop_front().expect("front exists");
            self.pos = Timestamp(self.pos.0.max(rec.time.0));
            let ev = self.event(rec);
            out.events.push(ev);
        }
        let done = self.eof && self.staged.is_empty();
        let next_is_later = self
            .staged
            .front()
            .is_some_and(|f| target.is_some_and(|t| f.time.0 > t));
        if done || next_is_later {
            // Everything due was delivered: the clock reaches the target,
            // but not beyond the end of the log.
            self.pos = match target {
                Some(t) => Timestamp(t.min(self.last.0).max(self.pos.0)),
                None => self.last.max(self.pos),
            };
        }
        if done && self.pos >= self.last {
            if self.looped && self.last > self.first {
                self.seek(self.first);
                out.restarted = true;
            } else {
                self.playing = false;
                self.last_tick = None;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    use operow_log::{AscDate, AscWriter, LogRecord, LogWriter};

    /// Write an ASC log of `(ms, channel, id)` records to a unique temp file.
    fn write_log(records: &[(u64, u8, u32)]) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "operow-app-replay-{}-{}.asc",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let mut w = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
        for &(ms, channel, id) in records {
            let r = LogRecord {
                time: Timestamp::from_ms(ms),
                channel,
                dir: Direction::Tx,
                kind: RecordKind::Frame(CanFrame::new(id, false, &[1]).unwrap()),
            };
            w.write(&r).unwrap();
        }
        let mut text = String::from_utf8(w.into_inner()).unwrap();
        text.push_str("End TriggerBlock\n");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn source(records: &[(u64, u8, u32)], map: &[(u8, u32)]) -> ReplaySource {
        let path = write_log(records);
        let info = probe(&path).unwrap();
        let map = map.iter().map(|&(c, b)| (c, BusId(b))).collect();
        let mut s = ReplaySource::new(&path, &info, map);
        s.starve_wait = Duration::from_secs(5);
        s.playing = true;
        s
    }

    fn ms(e: &Advance) -> Vec<u64> {
        e.events.iter().map(|e| e.time.0 / 1_000_000).collect()
    }

    const LOG: [(u64, u8, u32); 4] = [(100, 1, 0x1), (200, 1, 0x2), (300, 1, 0x3), (1000, 1, 0x4)];

    #[test]
    fn probe_finds_channels_and_span() {
        let path = write_log(&[(100, 2, 1), (150, 1, 2), (900, 2, 3)]);
        let info = probe(&path).unwrap();
        assert_eq!(info.channels, [1, 2]);
        assert_eq!(info.first, Timestamp::from_ms(100));
        assert_eq!(info.last, Timestamp::from_ms(900));
        assert!(probe(Path::new("/nonexistent.asc")).is_err());
    }

    #[test]
    fn events_arrive_at_their_virtual_times() {
        let mut s = source(&LOG, &[(1, 1)]);
        // The clock starts at the first record.
        assert_eq!(s.position(), Timestamp::from_ms(100));
        assert_eq!(ms(&s.advance(Duration::ZERO)), [100]);
        assert_eq!(ms(&s.advance(Duration::from_millis(50))), Vec::<u64>::new());
        assert_eq!(s.position(), Timestamp::from_ms(150));
        assert_eq!(ms(&s.advance(Duration::from_millis(50))), [200]);
        assert_eq!(ms(&s.advance(Duration::from_millis(100))), [300]);
        assert_eq!(s.position(), Timestamp::from_ms(300));
        // Nothing before 1000 ms.
        assert!(s.advance(Duration::from_millis(600)).events.is_empty());
        assert_eq!(ms(&s.advance(Duration::from_millis(100))), [1000]);
        assert!(!s.playing, "stops at the end");
        assert_eq!(s.position(), Timestamp::from_ms(1000));
    }

    #[test]
    fn speed_scales_virtual_time() {
        let mut s = source(&LOG, &[(1, 1)]);
        s.speed = Some(10.0);
        // 50 ms real = 500 ms virtual: up to t = 600 ms.
        assert_eq!(ms(&s.advance(Duration::from_millis(50))), [100, 200, 300]);
        assert_eq!(s.position(), Timestamp::from_ms(600));
        let mut slow = source(&LOG, &[(1, 1)]);
        slow.speed = Some(0.1);
        // 1 s real = 100 ms virtual: reaches only t = 200 ms.
        assert_eq!(ms(&slow.advance(Duration::from_secs(1))), [100, 200]);
    }

    #[test]
    fn max_speed_drains_everything() {
        let mut s = source(&LOG, &[(1, 1)]);
        s.speed = None;
        assert_eq!(
            ms(&s.advance(Duration::from_millis(1))),
            [100, 200, 300, 1000]
        );
        assert!(!s.playing);
    }

    #[test]
    fn paused_source_delivers_nothing() {
        let mut s = source(&LOG, &[(1, 1)]);
        s.playing = false;
        assert!(s.advance(Duration::from_secs(10)).events.is_empty());
        assert_eq!(s.position(), Timestamp::from_ms(100));
    }

    #[test]
    fn seek_skips_earlier_records() {
        let mut s = source(&LOG, &[(1, 1)]);
        s.advance(Duration::from_millis(500)); // plays 100, 200, 300 (and ahead)
        s.seek(Timestamp::from_ms(250));
        assert_eq!(s.position(), Timestamp::from_ms(250));
        let a = s.advance(Duration::from_millis(100));
        assert_eq!(ms(&a), [300]);
        // Seeking backwards replays; a seek exactly on a record keeps it.
        s.seek(Timestamp::from_ms(200));
        assert_eq!(ms(&s.advance(Duration::ZERO)), [200]);
        // Out of range seeks clamp.
        s.seek(Timestamp::from_ms(99_999));
        assert_eq!(s.position(), Timestamp::from_ms(1000));
        s.seek(Timestamp::ZERO);
        assert_eq!(s.position(), Timestamp::from_ms(100));
    }

    #[test]
    fn loop_wraps_to_the_start() {
        let mut s = source(&LOG, &[(1, 1)]);
        s.looped = true;
        s.speed = None;
        let a = s.advance(Duration::from_millis(1));
        assert_eq!(ms(&a), [100, 200, 300, 1000]);
        assert!(a.restarted && s.playing);
        assert_eq!(s.position(), Timestamp::from_ms(100));
        let b = s.advance(Duration::from_millis(1));
        assert_eq!(ms(&b), [100, 200, 300, 1000], "plays again");
        assert!(b.restarted);
    }

    #[test]
    fn channel_mapping_sets_bus_sender_and_uids() {
        let mut s = source(
            &[(10, 1, 0x1), (20, 2, 0x2), (30, 3, 0x3), (40, 2, 0x4)],
            &[(2, 7)],
        );
        s.speed = None;
        let a = s.advance(Duration::from_millis(1));
        let got: Vec<_> = a
            .events
            .iter()
            .map(|e| {
                (
                    e.bus,
                    e.sender,
                    e.origin,
                    e.frame.id,
                    e.hop,
                    e.dir,
                    e.frame_uid,
                )
            })
            .collect();
        let sender = NodeId(LOG_NODE_BASE + 2);
        assert_eq!(
            got,
            [
                (BusId(7), sender, sender, 0x2, 0, Direction::Tx, 0),
                (BusId(7), sender, sender, 0x4, 0, Direction::Tx, 1),
            ]
        );
        assert_eq!(log_channel(sender), Some(2));
        assert_eq!(log_channel(NodeId(5)), None);
    }

    #[test]
    fn streams_more_records_than_the_read_ahead_holds() {
        let n = (CHUNK * (AHEAD_CHUNKS + 3)) as u64;
        let records: Vec<_> = (0..n).map(|i| (i, 1u8, 0x100)).collect();
        let mut s = source(&records, &[(1, 1)]);
        s.speed = None;
        let mut total = 0;
        while s.playing {
            total += s.advance(Duration::from_millis(1)).events.len();
        }
        assert_eq!(total as u64, n);
    }

    #[test]
    fn tail_probe_matches_a_full_read_for_long_logs() {
        let records: Vec<_> = (0..6000).map(|i| (i, 1u8, 0x100)).collect();
        let path = write_log(&records);
        assert!(std::fs::metadata(&path).unwrap().len() > TAIL_BYTES);
        assert_eq!(probe(&path).unwrap().last, Timestamp::from_ms(5999));
    }
}
