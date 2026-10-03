//! ASC/BLF logging: configuration, the trigger state machine and the task that
//! feeds a background writer thread from the shared frame store.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use operow_core::{BusEvent, BusId, Timestamp, UserSignalDef};
use operow_log::{AscDate, AscWriter, BlfWriter, LogRecord, LogWriter, RecordKind};
use serde::{Deserialize, Serialize};

use crate::dbcs::DbcStore;
use crate::signals::SignalRef;
use crate::store::FrameStore;

pub const DEFAULT_PATTERN: &str = "{project}_{date}_{n}.asc";

/// On-disk format of a recording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogFormat {
    #[default]
    Asc,
    Blf,
}

impl LogFormat {
    pub const ALL: [LogFormat; 2] = [LogFormat::Asc, LogFormat::Blf];

    pub fn label(self) -> &'static str {
        match self {
            LogFormat::Asc => "ASC",
            LogFormat::Blf => "BLF",
        }
    }

    /// File extension including the dot.
    pub fn ext(self) -> &'static str {
        match self {
            LogFormat::Asc => ".asc",
            LogFormat::Blf => ".blf",
        }
    }
}

/// Comparison operator of a signal condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    pub const ALL: [CmpOp; 6] = [
        CmpOp::Eq,
        CmpOp::Ne,
        CmpOp::Lt,
        CmpOp::Le,
        CmpOp::Gt,
        CmpOp::Ge,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

/// `value <op> threshold` on a decoded signal value. Unlike
/// `filters::Cmp` (integers, for trace filters) this compares physical
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SigCmp {
    pub op: CmpOp,
    pub value: f64,
}

impl SigCmp {
    pub fn matches(&self, v: f64) -> bool {
        let eq = (v - self.value).abs() <= 1e-9 * self.value.abs().max(1.0);
        match self.op {
            CmpOp::Eq => eq,
            CmpOp::Ne => !eq,
            CmpOp::Lt => v < self.value,
            CmpOp::Le => v <= self.value,
            CmpOp::Gt => v > self.value,
            CmpOp::Ge => v >= self.value,
        }
    }
}

/// Something that starts or stops a recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Condition {
    /// A frame with this id was seen, on `bus` or on any bus.
    IdSeen {
        bus: Option<BusId>,
        id: u32,
        ext: bool,
    },
    /// A decoded signal value satisfies the comparison.
    Signal { signal: SignalRef, cmp: SigCmp },
    /// The key was pressed (while no text field has focus).
    Key(char),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum StartTrigger {
    #[default]
    Immediate,
    OnCondition(Condition),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum StopTrigger {
    #[default]
    OnMeasurementStop,
    OnCondition(Condition),
    /// This long after the recording started, in seconds.
    AfterSeconds(f64),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    pub start: StartTrigger,
    pub stop: StopTrigger,
    /// Seconds of history written before a start condition fires.
    pub pre_trigger_s: f64,
    /// Seconds still written after a stop condition fires.
    pub post_trigger_s: f64,
}

/// Logging settings, saved with the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub enabled: bool,
    pub format: LogFormat,
    /// File name with `{project}`, `{date}`, `{time}` and `{n}` tokens.
    pub pattern: String,
    /// Output folder; `None` is the project folder, else the home folder.
    pub folder: Option<PathBuf>,
    /// Buses to log; `None` logs all of them.
    pub buses: Option<Vec<BusId>>,
    /// ASC channel per bus, overriding the default (bus order, 1..n).
    pub channels: Vec<(BusId, u8)>,
    pub split_mb: Option<u32>,
    pub split_minutes: Option<u32>,
    pub trigger: TriggerConfig,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            enabled: false,
            format: LogFormat::default(),
            pattern: DEFAULT_PATTERN.into(),
            folder: None,
            buses: None,
            channels: Vec::new(),
            split_mb: None,
            split_minutes: None,
            trigger: TriggerConfig::default(),
        }
    }
}

impl LoggingConfig {
    pub fn includes(&self, bus: BusId) -> bool {
        self.buses.as_ref().is_none_or(|b| b.contains(&bus))
    }
}

/// ASC channel (1-based) of every bus: its position in `buses`, unless
/// overridden; limited to `include` when given.
pub fn channel_map(
    buses: &[BusId],
    overrides: &[(BusId, u8)],
    include: Option<&[BusId]>,
) -> HashMap<BusId, u8> {
    buses
        .iter()
        .enumerate()
        .filter(|(_, b)| include.is_none_or(|inc| inc.contains(b)))
        .map(|(i, b)| {
            let ch = overrides
                .iter()
                .find(|(ob, c)| ob == b && *c >= 1)
                .map_or((i + 1).min(255) as u8, |(_, c)| *c);
            (*b, ch)
        })
        .collect()
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Expand `{project}`, `{date}` (`2026-10-03`), `{time}` (`14-15-30`) and
/// `{n}` in `pattern`; the result always ends in the extension of `fmt`
/// (replacing the other format's) and is a valid file name.
pub fn expand_pattern(
    pattern: &str,
    project: &str,
    date: &AscDate,
    n: u32,
    fmt: LogFormat,
) -> String {
    let name = pattern
        .replace("{project}", project)
        .replace("{date}", &date.date_string())
        .replace("{time}", &date.time_string())
        .replace("{n}", &n.to_string());
    let mut name = sanitize(name.trim());
    if name.is_empty() {
        name = "log".into();
    }
    let ext = fmt.ext();
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(ext) {
        let other = LogFormat::ALL.iter().find(|f| lower.ends_with(f.ext()));
        if let Some(o) = other {
            name.truncate(name.len() - o.ext().len());
        }
        name.push_str(ext);
    }
    name
}

/// The first free path for a file of the recording: `{n}` counts up from
/// `start_n`. A pattern without `{n}` gets a `_<n>` suffix from the second
/// file on (split files, or when the name is taken).
pub fn next_path(
    dir: &Path,
    pattern: &str,
    project: &str,
    date: &AscDate,
    start_n: u32,
    fmt: LogFormat,
    exists: &dyn Fn(&Path) -> bool,
) -> (PathBuf, u32) {
    let has_n = pattern.contains("{n}");
    let mut n = start_n.max(1);
    loop {
        let mut name = expand_pattern(pattern, project, date, n, fmt);
        if !has_n && n > 1 {
            let stem = name.strip_suffix(fmt.ext()).unwrap_or(&name).to_string();
            let ext = &name[stem.len()..];
            name = format!("{stem}_{n}{ext}");
        }
        let path = dir.join(name);
        if !exists(&path) {
            return (path, n);
        }
        n += 1;
    }
}

fn secs_ns(s: f64) -> u64 {
    if s.is_finite() {
        (s.max(0.0) * 1e9).round() as u64
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogState {
    Idle,
    /// Waiting for the start condition.
    Armed,
    Recording,
    /// The stop condition fired; still writing the post-trigger window.
    PostTrigger,
}

impl LogState {
    pub fn label(self) -> &'static str {
        match self {
            LogState::Idle => "Idle",
            LogState::Armed => "Armed",
            LogState::Recording => "Recording",
            LogState::PostTrigger => "Post-trigger",
        }
    }
}

/// What the trigger machine asks its owner to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// Start a file; `trigger_time` is when the start condition fired.
    Open {
        trigger_time: Timestamp,
    },
    Record(BusEvent),
    /// Finish the file.
    Close,
}

/// Start/stop/pre/post logic, independent of files and the UI. A recording
/// is single-shot: after it closes the machine is idle until `start`.
pub struct TriggerMachine {
    cfg: TriggerConfig,
    state: LogState,
    /// Target time of an `AfterSeconds` stop.
    stop_at: Option<Timestamp>,
    stop_time: Timestamp,
}

type Eval<'a> = &'a mut dyn FnMut(&Condition, &BusEvent) -> bool;

fn key_is(c: &Condition, key: char) -> bool {
    matches!(c, Condition::Key(k) if k.eq_ignore_ascii_case(&key))
}

impl TriggerMachine {
    pub fn new(cfg: TriggerConfig) -> Self {
        TriggerMachine {
            cfg,
            state: LogState::Idle,
            stop_at: None,
            stop_time: Timestamp::ZERO,
        }
    }

    pub fn state(&self) -> LogState {
        self.state
    }

    /// Arm (or, for an immediate start, open) at measurement time `now`.
    pub fn start(&mut self, now: Timestamp, out: &mut Vec<Out>) {
        match self.cfg.start {
            StartTrigger::Immediate => self.open(now, out),
            StartTrigger::OnCondition(_) => self.state = LogState::Armed,
        }
    }

    fn open(&mut self, t: Timestamp, out: &mut Vec<Out>) {
        self.state = LogState::Recording;
        self.stop_at = match self.cfg.stop {
            StopTrigger::AfterSeconds(s) => Some(Timestamp(t.0 + secs_ns(s))),
            _ => None,
        };
        out.push(Out::Open { trigger_time: t });
    }

    fn close(&mut self, out: &mut Vec<Out>) {
        self.state = LogState::Idle;
        out.push(Out::Close);
    }

    /// Frames before the trigger: events of the store below `up_to` that
    /// are at most `pre_trigger_s` older than `t`.
    fn pre(&self, store: &FrameStore, up_to: u64, t: Timestamp, out: &mut Vec<Out>) {
        let from = t.0.saturating_sub(secs_ns(self.cfg.pre_trigger_s));
        let mut evs = Vec::new();
        let mut seq = up_to.min(store.next_seq());
        while seq > store.first_seq() {
            seq -= 1;
            match store.get(seq) {
                Some(e) if e.time.0 >= from => evs.push(*e),
                _ => break,
            }
        }
        out.extend(evs.into_iter().rev().map(Out::Record));
    }

    /// A key was pressed at measurement time `now`; `consumed` is the
    /// store seq up to which events were already processed.
    pub fn key(
        &mut self,
        c: char,
        now: Timestamp,
        store: &FrameStore,
        consumed: u64,
        out: &mut Vec<Out>,
    ) {
        match (&self.state, &self.cfg.start, &self.cfg.stop) {
            (LogState::Armed, StartTrigger::OnCondition(cond), _) if key_is(cond, c) => {
                self.open(now, out);
                self.pre(store, consumed, now, out);
            }
            (LogState::Recording, _, StopTrigger::OnCondition(cond)) if key_is(cond, c) => {
                self.stop_time = now;
                self.state = LogState::PostTrigger;
                if self.cfg.post_trigger_s <= 0.0 {
                    self.close(out);
                }
            }
            _ => {}
        }
    }

    /// Process every event from `*cursor` on, advancing the cursor.
    pub fn feed(
        &mut self,
        store: &FrameStore,
        cursor: &mut u64,
        eval: Eval<'_>,
        out: &mut Vec<Out>,
    ) {
        let mut seq = (*cursor).max(store.first_seq());
        while seq < store.next_seq() {
            if self.state == LogState::Idle {
                seq = store.next_seq();
                break;
            }
            let Some(&ev) = store.get(seq) else { break };
            if self.state == LogState::Armed {
                let hit = matches!(&self.cfg.start, StartTrigger::OnCondition(c) if eval(c, &ev));
                if hit {
                    self.open(ev.time, out);
                    self.pre(store, seq, ev.time, out);
                    out.push(Out::Record(ev));
                }
            } else {
                self.recording(ev, eval, out);
            }
            seq += 1;
        }
        *cursor = seq;
    }

    fn recording(&mut self, ev: BusEvent, eval: Eval<'_>, out: &mut Vec<Out>) {
        if self.state == LogState::Recording {
            let hit = match &self.cfg.stop {
                StopTrigger::AfterSeconds(_) => match self.stop_at {
                    Some(t) if ev.time >= t => {
                        self.stop_time = t;
                        true
                    }
                    _ => false,
                },
                StopTrigger::OnCondition(Condition::Key(_)) => false,
                StopTrigger::OnCondition(c) => {
                    self.stop_time = ev.time;
                    eval(c, &ev)
                }
                StopTrigger::OnMeasurementStop => false,
            };
            if hit {
                self.state = LogState::PostTrigger;
            }
        }
        if self.state == LogState::PostTrigger {
            let limit = Timestamp(self.stop_time.0 + secs_ns(self.cfg.post_trigger_s));
            if ev.time > limit {
                self.close(out);
                return;
            }
            out.push(Out::Record(ev));
            if ev.time >= limit {
                self.close(out);
            }
        } else {
            out.push(Out::Record(ev));
        }
    }

    /// The measurement stopped (or logging was switched off): close an open
    /// file whatever the stop trigger; an armed machine just goes idle.
    pub fn finish(&mut self, out: &mut Vec<Out>) {
        if matches!(self.state, LogState::Recording | LogState::PostTrigger) {
            self.close(out);
        }
        self.state = LogState::Idle;
    }
}

/// Whether `ev` satisfies `c`. Keys never match an event.
pub fn eval_condition(
    c: &Condition,
    ev: &BusEvent,
    dbcs: &DbcStore,
    users: &[UserSignalDef],
) -> bool {
    match c {
        Condition::IdSeen { bus, id, ext } => {
            bus.is_none_or(|b| b == ev.bus) && ev.frame.id == *id && ev.frame.extended == *ext
        }
        Condition::Signal { signal, cmp } => signal
            .sample_with_prev(ev, None, dbcs, users)
            .is_some_and(|v| cmp.matches(v)),
        Condition::Key(_) => false,
    }
}

/// A file written (or being written) this session.
#[derive(Clone)]
pub struct LogFile {
    pub path: PathBuf,
    pub bytes: Arc<AtomicU64>,
    pub frames: u64,
}

impl LogFile {
    pub fn size(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LogStatus {
    pub state: LogState,
    pub elapsed_s: f64,
    pub bytes: u64,
    pub frames: u64,
}

enum Msg {
    Open {
        path: PathBuf,
        bytes: Arc<AtomicU64>,
        date: AscDate,
        format: LogFormat,
    },
    Records(Vec<LogRecord>),
    Finish,
}

struct CountingWriter {
    inner: BufWriter<File>,
    bytes: Arc<AtomicU64>,
}

impl Seek for CountingWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The open file of the writer thread, in either format.
enum OutFile {
    Asc(AscWriter<CountingWriter>),
    Blf(BlfWriter<CountingWriter>),
}

impl OutFile {
    fn write(&mut self, r: &LogRecord) -> io::Result<()> {
        match self {
            OutFile::Asc(w) => w.write(r),
            OutFile::Blf(w) => w.write(r),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            OutFile::Asc(w) => w.flush(),
            OutFile::Blf(w) => w.flush(),
        }
    }

    fn finish(self) -> io::Result<()> {
        match self {
            OutFile::Asc(w) => w.finish(),
            OutFile::Blf(w) => w.finish(),
        }
    }
}

type Errors = Arc<Mutex<Vec<String>>>;

fn report(errors: &Errors, e: impl std::fmt::Display) {
    if let Ok(mut v) = errors.lock() {
        v.push(e.to_string());
    }
}

/// The writer thread: owns the file, so file IO never blocks the UI.
fn writer_thread(rx: Receiver<Msg>, errors: Errors) {
    let mut w: Option<OutFile> = None;
    for msg in rx {
        match msg {
            Msg::Open {
                path,
                bytes,
                date,
                format,
            } => {
                if let Some(old) = w.take()
                    && let Err(e) = old.finish()
                {
                    report(&errors, e);
                }
                let opened = File::create(&path).and_then(|f| {
                    let out = CountingWriter {
                        inner: BufWriter::new(f),
                        bytes,
                    };
                    match format {
                        LogFormat::Asc => AscWriter::new(out, date).map(OutFile::Asc),
                        LogFormat::Blf => BlfWriter::new(out, date).map(OutFile::Blf),
                    }
                });
                match opened {
                    Ok(x) => w = Some(x),
                    Err(e) => report(&errors, format!("{}: {e}", path.display())),
                }
            }
            Msg::Records(rs) => {
                if let Some(x) = w.as_mut() {
                    let res = rs
                        .iter()
                        .try_for_each(|r| x.write(r))
                        .and_then(|()| x.flush());
                    if let Err(e) = res {
                        report(&errors, format!("write error: {e}"));
                        w = None;
                    }
                }
            }
            Msg::Finish => break,
        }
    }
    if let Some(x) = w.take()
        && let Err(e) = x.finish()
    {
        report(&errors, e);
    }
}

/// What the runtime needs from the app each frame.
pub struct LogContext<'a> {
    pub store: &'a FrameStore,
    /// Every bus, in channel order.
    pub buses: &'a [BusId],
    pub project: &'a str,
    pub project_dir: Option<&'a Path>,
    pub dbcs: &'a DbcStore,
    pub users: &'a [UserSignalDef],
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

pub fn output_dir(cfg: &LoggingConfig, project_dir: Option<&Path>) -> PathBuf {
    cfg.folder
        .clone()
        .or_else(|| project_dir.map(Path::to_path_buf))
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Consumes the frame store through its own cursor and records according
/// to the config; `update` runs once per UI frame.
pub struct LogRuntime {
    machine: TriggerMachine,
    active: bool,
    cursor: u64,
    tx: Option<Sender<Msg>>,
    current_thread: Option<JoinHandle<()>>,
    finished_threads: Vec<JoinHandle<()>>,
    errors: Errors,
    /// Files written this session, oldest first.
    pub files: Vec<LogFile>,
    file_n: u32,
    channels: HashMap<BusId, u8>,
    record_start: Timestamp,
    file_start: Timestamp,
    last_time: Timestamp,
    session_frames: u64,
}

impl Default for LogRuntime {
    fn default() -> Self {
        LogRuntime {
            machine: TriggerMachine::new(TriggerConfig::default()),
            active: false,
            cursor: 0,
            tx: None,
            current_thread: None,
            finished_threads: Vec::new(),
            errors: Arc::default(),
            files: Vec::new(),
            file_n: 0,
            channels: HashMap::new(),
            record_start: Timestamp::ZERO,
            file_start: Timestamp::ZERO,
            last_time: Timestamp::ZERO,
            session_frames: 0,
        }
    }
}

impl Drop for LogRuntime {
    fn drop(&mut self) {
        self.close_thread();
        for t in self.finished_threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl LogRuntime {
    pub fn status(&self) -> LogStatus {
        let state = if self.active {
            self.machine.state()
        } else {
            LogState::Idle
        };
        let recording = matches!(state, LogState::Recording | LogState::PostTrigger);
        LogStatus {
            state,
            elapsed_s: if recording {
                self.last_time.0.saturating_sub(self.record_start.0) as f64 / 1e9
            } else {
                0.0
            },
            bytes: self.files.last().map_or(0, LogFile::size),
            frames: self.session_frames,
        }
    }

    /// One UI frame: start/stop with the measurement, feed new events
    /// through the trigger machine and hand records to the writer. Returns
    /// lines for the status log.
    pub fn update(
        &mut self,
        cfg: &LoggingConfig,
        running: bool,
        keys: &[char],
        ctx: &LogContext<'_>,
    ) -> Vec<String> {
        let mut msgs = Vec::new();
        let want = cfg.enabled && running;
        let now = ctx
            .store
            .find_latest(1, |_| true)
            .map_or(self.last_time, |e| e.time);
        let mut out = Vec::new();
        if want && !self.active {
            self.active = true;
            self.cursor = ctx.store.next_seq();
            self.file_n = 0;
            self.session_frames = 0;
            self.channels = channel_map(ctx.buses, &cfg.channels, cfg.buses.as_deref());
            self.machine = TriggerMachine::new(cfg.trigger.clone());
            self.machine.start(now, &mut out);
        }
        if self.active {
            for &k in keys {
                self.machine.key(k, now, ctx.store, self.cursor, &mut out);
            }
            let mut eval =
                |c: &Condition, ev: &BusEvent| eval_condition(c, ev, ctx.dbcs, ctx.users);
            self.machine
                .feed(ctx.store, &mut self.cursor, &mut eval, &mut out);
        }
        if self.active && !want {
            let armed = self.machine.state() == LogState::Armed;
            self.machine.finish(&mut out);
            self.active = false;
            if armed {
                msgs.push("logging: trigger never fired, no file written".into());
            }
        }
        self.apply(out, cfg, ctx, &mut msgs);
        if let Ok(mut e) = self.errors.lock() {
            msgs.extend(e.drain(..).map(|m| format!("error: logging: {m}")));
        }
        self.finished_threads.retain(|t| !t.is_finished());
        msgs
    }

    fn apply(
        &mut self,
        out: Vec<Out>,
        cfg: &LoggingConfig,
        ctx: &LogContext<'_>,
        msgs: &mut Vec<String>,
    ) {
        let mut batch: Vec<LogRecord> = Vec::new();
        for o in out {
            match o {
                Out::Open { trigger_time } => {
                    self.record_start = trigger_time;
                    self.open_file(cfg, ctx, trigger_time, msgs);
                }
                Out::Record(ev) => {
                    let Some(&channel) = self.channels.get(&ev.bus) else {
                        continue;
                    };
                    if self.tx.is_none() {
                        continue;
                    }
                    if self.should_rotate(cfg, ev.time) {
                        self.send(Msg::Records(std::mem::take(&mut batch)));
                        self.open_file(cfg, ctx, ev.time, msgs);
                    }
                    batch.push(LogRecord {
                        time: ev.time,
                        channel,
                        dir: ev.dir,
                        kind: RecordKind::Frame(ev.frame),
                    });
                    self.last_time = ev.time;
                    self.session_frames += 1;
                    if let Some(f) = self.files.last_mut() {
                        f.frames += 1;
                    }
                }
                Out::Close => {
                    self.send(Msg::Records(std::mem::take(&mut batch)));
                    self.close_thread();
                    if let Some(f) = self.files.last() {
                        msgs.push(format!("logging: saved {}", file_name(&f.path)));
                    }
                }
            }
        }
        self.send(Msg::Records(batch));
    }

    fn send(&self, msg: Msg) {
        if let Msg::Records(r) = &msg
            && r.is_empty()
        {
            return;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }

    fn should_rotate(&self, cfg: &LoggingConfig, t: Timestamp) -> bool {
        let by_size = cfg.split_mb.is_some_and(|mb| {
            self.files
                .last()
                .is_some_and(|f| f.size() >= u64::from(mb) * 1_000_000)
        });
        let by_time = cfg.split_minutes.is_some_and(|m| {
            t.0.saturating_sub(self.file_start.0) >= u64::from(m) * 60_000_000_000
        });
        by_size || by_time
    }

    /// Finish the writer thread (its file gets its footer).
    fn close_thread(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Finish);
        }
        if let Some(h) = self.current_thread.take() {
            self.finished_threads.push(h);
        }
    }

    fn open_file(
        &mut self,
        cfg: &LoggingConfig,
        ctx: &LogContext<'_>,
        t: Timestamp,
        msgs: &mut Vec<String>,
    ) {
        let dir = output_dir(cfg, ctx.project_dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            msgs.push(format!("error: logging: {}: {e}", dir.display()));
        }
        let date = AscDate::now();
        let (path, n) = next_path(
            &dir,
            &cfg.pattern,
            ctx.project,
            &date,
            self.file_n + 1,
            cfg.format,
            &|p| p.exists(),
        );
        self.file_n = n;
        self.file_start = t;
        let bytes = Arc::new(AtomicU64::new(0));
        self.files.push(LogFile {
            path: path.clone(),
            bytes: bytes.clone(),
            frames: 0,
        });
        if self.tx.is_none() {
            let (tx, rx) = channel();
            let errors = self.errors.clone();
            self.current_thread = Some(std::thread::spawn(move || writer_thread(rx, errors)));
            self.tx = Some(tx);
        }
        msgs.push(format!("logging: writing {}", file_name(&path)));
        self.send(Msg::Open {
            path,
            bytes,
            date,
            format: cfg.format,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::{CanFrame, Direction, NodeId};

    fn date() -> AscDate {
        AscDate::from_unix_ms(1_791_036_930_123)
    }

    fn ev_on(bus: u32, id: u32, t_ms: u64, b0: u8) -> BusEvent {
        BusEvent {
            time: Timestamp::from_ms(t_ms),
            bus: BusId(bus),
            sender: NodeId(1),
            origin: NodeId(1),
            dir: Direction::Tx,
            frame_uid: 0,
            hop: 0,
            frame: CanFrame::new(id, false, &[b0, 0]).unwrap(),
        }
    }

    fn ev(id: u32, t_ms: u64) -> BusEvent {
        ev_on(1, id, t_ms, 0)
    }

    fn store_of(evs: &[BusEvent]) -> FrameStore {
        let mut s = FrameStore::new(1000);
        s.push_batch(evs);
        s
    }

    /// Ids written, with `O`pen and `C`lose markers as `0` and `u32::MAX`.
    fn run(cfg: TriggerConfig, evs: &[BusEvent], finish: bool) -> Vec<Out> {
        let store = store_of(evs);
        let mut m = TriggerMachine::new(cfg);
        let mut out = Vec::new();
        let mut cur = 0;
        m.start(Timestamp::ZERO, &mut out);
        let mut eval =
            |c: &Condition, e: &BusEvent| eval_condition(c, e, &DbcStore::default(), &[]);
        m.feed(&store, &mut cur, &mut eval, &mut out);
        assert_eq!(cur, store.next_seq());
        if finish {
            m.finish(&mut out);
        }
        out
    }

    fn ids(out: &[Out]) -> Vec<i64> {
        out.iter()
            .map(|o| match o {
                Out::Open { .. } => -1,
                Out::Record(e) => i64::from(e.frame.id),
                Out::Close => -2,
            })
            .collect()
    }

    fn id_cond(id: u32) -> Condition {
        Condition::IdSeen {
            bus: None,
            id,
            ext: false,
        }
    }

    #[test]
    fn pattern_expansion() {
        let d = date();
        assert_eq!(
            expand_pattern(DEFAULT_PATTERN, "gateway", &d, 3, LogFormat::Asc),
            "gateway_2026-10-03_3.asc"
        );
        assert_eq!(
            expand_pattern("{project}-{time}", "p", &d, 1, LogFormat::Asc),
            "p-14-15-30.asc"
        );
        assert_eq!(
            expand_pattern("x/{project}:{n}.ASC", "a", &d, 2, LogFormat::Asc),
            "x_a_2.ASC"
        );
        assert_eq!(expand_pattern("", "a", &d, 1, LogFormat::Asc), "log.asc");
        assert_eq!(
            expand_pattern(DEFAULT_PATTERN, "g", &d, 2, LogFormat::Blf),
            "g_2026-10-03_2.blf"
        );
        assert_eq!(expand_pattern("a.BLF", "g", &d, 2, LogFormat::Asc), "a.asc");
        assert_eq!(
            expand_pattern("{foo}", "a", &d, 1, LogFormat::Asc),
            "{foo}.asc"
        );
    }

    #[test]
    fn split_rotation_naming() {
        let d = date();
        let dir = Path::new("/logs");
        let taken: Vec<PathBuf> = vec![dir.join("p_2026-10-03_1.asc")];
        let exists = |p: &Path| taken.contains(&p.to_path_buf());
        // {n} skips existing files and counts up for each split file.
        let (p1, n1) = next_path(dir, DEFAULT_PATTERN, "p", &d, 1, LogFormat::Asc, &exists);
        assert_eq!(
            (p1.file_name().unwrap().to_str().unwrap(), n1),
            ("p_2026-10-03_2.asc", 2)
        );
        let (p2, n2) = next_path(
            dir,
            DEFAULT_PATTERN,
            "p",
            &d,
            n1 + 1,
            LogFormat::Asc,
            &exists,
        );
        assert_eq!(
            (p2.file_name().unwrap().to_str().unwrap(), n2),
            ("p_2026-10-03_3.asc", 3)
        );
        // Without {n}: plain first name, `_<n>` for later parts.
        let none = |_: &Path| false;
        let (a, n) = next_path(dir, "run.asc", "p", &d, 1, LogFormat::Asc, &none);
        assert_eq!((a, n), (dir.join("run.asc"), 1));
        let (b, n) = next_path(dir, "run.asc", "p", &d, 2, LogFormat::Asc, &none);
        assert_eq!((b, n), (dir.join("run_2.asc"), 2));
        // ... and when the plain name is taken.
        let taken_plain = |p: &Path| p == dir.join("run.asc");
        let (c, _) = next_path(dir, "run", "p", &d, 1, LogFormat::Asc, &taken_plain);
        assert_eq!(c, dir.join("run_2.asc"));
    }

    #[test]
    fn channel_mapping() {
        let buses = [BusId(7), BusId(3), BusId(9)];
        let m = channel_map(&buses, &[], None);
        assert_eq!((m[&BusId(7)], m[&BusId(3)], m[&BusId(9)]), (1, 2, 3));
        let m = channel_map(&buses, &[(BusId(3), 5), (BusId(9), 0)], None);
        assert_eq!((m[&BusId(7)], m[&BusId(3)], m[&BusId(9)]), (1, 5, 3));
        let m = channel_map(&buses, &[], Some(&[BusId(9)]));
        assert_eq!(m.len(), 1);
        assert_eq!(m[&BusId(9)], 3);
        let cfg = LoggingConfig {
            buses: Some(vec![BusId(3)]),
            ..Default::default()
        };
        assert!(cfg.includes(BusId(3)) && !cfg.includes(BusId(7)));
    }

    #[test]
    fn immediate_start_stops_with_measurement() {
        let evs = [ev(1, 0), ev(2, 10), ev(3, 20)];
        let out = run(TriggerConfig::default(), &evs, false);
        assert_eq!(ids(&out), [-1, 1, 2, 3]);
        let out = run(TriggerConfig::default(), &evs, true);
        assert_eq!(ids(&out), [-1, 1, 2, 3, -2]);
    }

    #[test]
    fn condition_start_with_pre_trigger_window() {
        let evs: Vec<BusEvent> = (0..10).map(|i| ev(0x10 + i, u64::from(i) * 100)).collect();
        // Trigger on id 0x15 at 500 ms; 250 ms of pre-trigger history.
        let cfg = TriggerConfig {
            start: StartTrigger::OnCondition(id_cond(0x15)),
            pre_trigger_s: 0.25,
            ..Default::default()
        };
        let out = run(cfg, &evs, true);
        let Out::Open { trigger_time } = out[0] else {
            panic!("{out:?}")
        };
        assert_eq!(trigger_time, Timestamp::from_ms(500));
        // 300 ms is the oldest event within 250 ms of 500 ms: 0x13 at 300.
        assert_eq!(
            ids(&out),
            [-1, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, -2]
        );

        // No pre-trigger: starts at the trigger frame.
        let cfg = TriggerConfig {
            start: StartTrigger::OnCondition(id_cond(0x15)),
            ..Default::default()
        };
        assert_eq!(ids(&run(cfg, &evs, false))[..3], [-1, 0x15, 0x16]);

        // The trigger never fires: nothing is written.
        let cfg = TriggerConfig {
            start: StartTrigger::OnCondition(id_cond(0x99)),
            ..Default::default()
        };
        assert!(run(cfg, &evs, true).is_empty());
    }

    #[test]
    fn pre_trigger_is_limited_by_the_store() {
        let evs: Vec<BusEvent> = (0..5).map(|i| ev(i + 1, u64::from(i) * 10)).collect();
        let cfg = TriggerConfig {
            start: StartTrigger::OnCondition(id_cond(3)),
            pre_trigger_s: 60.0,
            ..Default::default()
        };
        assert_eq!(ids(&run(cfg, &evs, false)), [-1, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn stop_after_seconds_and_post_trigger() {
        let evs: Vec<BusEvent> = (0..10).map(|i| ev(i + 1, u64::from(i) * 100)).collect();
        // Immediate start at t=0, stop after 0.35 s: events at 0..300.
        let cfg = TriggerConfig {
            stop: StopTrigger::AfterSeconds(0.35),
            ..Default::default()
        };
        assert_eq!(ids(&run(cfg, &evs, false)), [-1, 1, 2, 3, 4, -2]);
        // With 0.2 s of post-trigger: up to 350 + 200 = 550 ms.
        let cfg = TriggerConfig {
            stop: StopTrigger::AfterSeconds(0.35),
            post_trigger_s: 0.2,
            ..Default::default()
        };
        assert_eq!(ids(&run(cfg, &evs, false)), [-1, 1, 2, 3, 4, 5, 6, -2]);
    }

    #[test]
    fn condition_stop_with_and_without_post_trigger() {
        let evs: Vec<BusEvent> = (0..10).map(|i| ev(i + 1, u64::from(i) * 100)).collect();
        let cfg = TriggerConfig {
            stop: StopTrigger::OnCondition(id_cond(4)),
            ..Default::default()
        };
        // The stop frame itself is the last one written.
        assert_eq!(ids(&run(cfg, &evs, false)), [-1, 1, 2, 3, 4, -2]);
        let cfg = TriggerConfig {
            stop: StopTrigger::OnCondition(id_cond(4)),
            post_trigger_s: 0.15,
            ..Default::default()
        };
        assert_eq!(ids(&run(cfg, &evs, false)), [-1, 1, 2, 3, 4, 5, -2]);
        // The measurement ends inside the post-trigger window.
        let short = &evs[..5];
        let cfg = TriggerConfig {
            stop: StopTrigger::OnCondition(id_cond(4)),
            post_trigger_s: 5.0,
            ..Default::default()
        };
        assert_eq!(ids(&run(cfg, short, true)), [-1, 1, 2, 3, 4, 5, -2]);
    }

    #[test]
    fn machine_reports_states_and_is_single_shot() {
        let store = store_of(&[ev(1, 0), ev(2, 10), ev(3, 20), ev(2, 30)]);
        let cfg = TriggerConfig {
            start: StartTrigger::OnCondition(id_cond(2)),
            stop: StopTrigger::OnCondition(id_cond(3)),
            post_trigger_s: 1.0,
            ..Default::default()
        };
        let mut m = TriggerMachine::new(cfg);
        assert_eq!(m.state(), LogState::Idle);
        let mut out = Vec::new();
        m.start(Timestamp::ZERO, &mut out);
        assert_eq!(m.state(), LogState::Armed);
        let mut eval =
            |c: &Condition, e: &BusEvent| eval_condition(c, e, &DbcStore::default(), &[]);
        let mut cur = 0;
        m.feed(&store, &mut cur, &mut eval, &mut out);
        assert_eq!(m.state(), LogState::PostTrigger);
        assert_eq!(ids(&out), [-1, 2, 3, 2]);
        m.finish(&mut out);
        assert_eq!(m.state(), LogState::Idle);
        assert_eq!(*out.last().unwrap(), Out::Close);
    }

    #[test]
    fn key_triggers() {
        let store = store_of(&[ev(1, 0), ev(2, 100), ev(3, 200)]);
        let cfg = TriggerConfig {
            start: StartTrigger::OnCondition(Condition::Key('s')),
            stop: StopTrigger::OnCondition(Condition::Key('e')),
            pre_trigger_s: 0.15,
            ..Default::default()
        };
        let mut m = TriggerMachine::new(cfg);
        let mut out = Vec::new();
        m.start(Timestamp::ZERO, &mut out);
        let mut cur = 0;
        let mut eval = |_: &Condition, _: &BusEvent| false;
        m.feed(&store, &mut cur, &mut eval, &mut out);
        assert!(out.is_empty());
        m.key('x', Timestamp::from_ms(200), &store, cur, &mut out);
        assert!(out.is_empty());
        m.key('S', Timestamp::from_ms(200), &store, cur, &mut out);
        assert_eq!(m.state(), LogState::Recording);
        // History within 150 ms of the key press: 100 ms and 200 ms.
        assert_eq!(ids(&out), [-1, 2, 3]);
        m.key('e', Timestamp::from_ms(300), &store, cur, &mut out);
        assert_eq!(m.state(), LogState::Idle);
        assert_eq!(*out.last().unwrap(), Out::Close);
    }

    #[test]
    fn signal_conditions() {
        let c = Condition::Signal {
            signal: SignalRef::Raw {
                bus: BusId(1),
                id: 0x100,
                extended: false,
                kind: crate::signals::RawKind::Byte(0),
            },
            cmp: SigCmp {
                op: CmpOp::Gt,
                value: 10.0,
            },
        };
        let d = DbcStore::default();
        assert!(!eval_condition(&c, &ev_on(1, 0x100, 0, 10), &d, &[]));
        assert!(eval_condition(&c, &ev_on(1, 0x100, 0, 11), &d, &[]));
        // Other id or bus never matches.
        assert!(!eval_condition(&c, &ev_on(1, 0x101, 0, 99), &d, &[]));
        assert!(!eval_condition(&c, &ev_on(2, 0x100, 0, 99), &d, &[]));
        let id = Condition::IdSeen {
            bus: Some(BusId(2)),
            id: 5,
            ext: false,
        };
        assert!(eval_condition(&id, &ev_on(2, 5, 0, 0), &d, &[]));
        assert!(!eval_condition(&id, &ev_on(1, 5, 0, 0), &d, &[]));
        assert!(
            SigCmp {
                op: CmpOp::Eq,
                value: 0.1
            }
            .matches(0.1)
        );
        assert!(
            SigCmp {
                op: CmpOp::Ne,
                value: 1.0
            }
            .matches(2.0)
        );
    }

    #[test]
    fn config_serializes_and_fills_defaults() {
        let cfg = LoggingConfig {
            enabled: true,
            folder: Some("/tmp/x".into()),
            buses: Some(vec![BusId(2)]),
            channels: vec![(BusId(2), 4)],
            split_mb: Some(10),
            trigger: TriggerConfig {
                start: StartTrigger::OnCondition(Condition::Key('t')),
                stop: StopTrigger::AfterSeconds(2.5),
                pre_trigger_s: 1.0,
                post_trigger_s: 0.5,
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<LoggingConfig>(&json).unwrap(), cfg);
        let back: LoggingConfig = serde_json::from_str(r#"{"enabled": true}"#).unwrap();
        assert!(back.enabled);
        assert_eq!(back.pattern, DEFAULT_PATTERN);
        assert_eq!(back.trigger, TriggerConfig::default());
    }

    #[test]
    fn runtime_writes_a_triggered_file() {
        let (ids, text) = run_triggered(LogFormat::Asc);
        assert_eq!(ids, [2, 3, 4]);
        assert!(text.trim_end().ends_with("End TriggerBlock"));
    }

    #[test]
    fn runtime_writes_a_triggered_blf_file() {
        let (ids, _) = run_triggered(LogFormat::Blf);
        assert_eq!(ids, [2, 3, 4]);
    }

    /// Log a triggered recording; the frame ids read back and the raw text.
    fn run_triggered(format: LogFormat) -> (Vec<u32>, String) {
        let dir = std::env::temp_dir().join(format!(
            "operow-log-test-{}-{}",
            format.label(),
            std::process::id()
        ));
        let cfg = LoggingConfig {
            enabled: true,
            format,
            pattern: "t_{n}.asc".into(),
            folder: Some(dir.clone()),
            trigger: TriggerConfig {
                start: StartTrigger::OnCondition(id_cond(3)),
                pre_trigger_s: 0.05,
                ..Default::default()
            },
            split_mb: None,
            ..Default::default()
        };
        let mut store = FrameStore::new(100);
        let dbcs = DbcStore::default();
        let mut rt = LogRuntime::default();
        let buses = [BusId(1)];
        let step = |rt: &mut LogRuntime, store: &FrameStore, running: bool| {
            let ctx = LogContext {
                store,
                buses: &buses,
                project: "p",
                project_dir: None,
                dbcs: &dbcs,
                users: &[],
            };
            rt.update(&cfg, running, &[], &ctx)
        };
        step(&mut rt, &store, true);
        assert_eq!(rt.status().state, LogState::Armed);
        store.push_batch(&[ev(1, 0), ev(2, 80), ev(3, 100), ev(4, 120)]);
        step(&mut rt, &store, true);
        assert_eq!(rt.status().state, LogState::Recording);
        step(&mut rt, &store, false);
        assert_eq!(rt.status().state, LogState::Idle);
        let path = rt.files[0].path.clone();
        drop(rt);
        assert_eq!(
            path.extension().unwrap(),
            format.ext().trim_start_matches('.')
        );
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let recs: Vec<LogRecord> = match format {
            LogFormat::Asc => operow_log::AscReader::new(bytes.as_slice())
                .collect::<Result<_, _>>()
                .unwrap(),
            LogFormat::Blf => operow_log::BlfReader::new(bytes.as_slice())
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap(),
        };
        let ids = recs
            .iter()
            .map(|r| match r.kind {
                RecordKind::Frame(f) => f.id,
                RecordKind::ErrorFrame => 0,
            })
            .collect();
        // 50 ms of history before id 3 (at 100 ms): 80 ms is in, 0 is out.
        (ids, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[test]
    fn runtime_rotates_by_time() {
        let dir = std::env::temp_dir().join(format!("operow-log-rot-{}", std::process::id()));
        let cfg = LoggingConfig {
            enabled: true,
            pattern: "r_{n}.asc".into(),
            folder: Some(dir.clone()),
            split_minutes: Some(1),
            ..Default::default()
        };
        let mut store = FrameStore::new(100);
        store.push_batch(&[ev(1, 0), ev(2, 30_000), ev(3, 61_000), ev(4, 70_000)]);
        let dbcs = DbcStore::default();
        let buses = [BusId(1)];
        let ctx = LogContext {
            store: &store,
            buses: &buses,
            project: "p",
            project_dir: None,
            dbcs: &dbcs,
            users: &[],
        };
        let mut rt = LogRuntime::default();
        // Events already in the store at start are not logged; new ones are.
        rt.update(&cfg, true, &[], &ctx);
        store.push_batch(&[ev(5, 71_000), ev(6, 100_000), ev(7, 132_000)]);
        let ctx = LogContext {
            store: &store,
            buses: &buses,
            project: "p",
            project_dir: None,
            dbcs: &dbcs,
            users: &[],
        };
        rt.update(&cfg, true, &[], &ctx);
        rt.update(&cfg, false, &[], &ctx);
        let names: Vec<String> = rt
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["r_1.asc", "r_2.asc"]);
        assert_eq!(rt.files[0].frames, 2);
        assert_eq!(rt.files[1].frames, 1);
        drop(rt);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
