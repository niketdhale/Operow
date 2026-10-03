//! Interactive Generator windows (CANoe style): a table of frames that are
//! sent once, cyclically or on a key press. Generator frames are separate
//! from the ECUs' own TX messages; the engine sends them from a virtual
//! sender per window (`GeneratorId`).
//!
//! Cyclic rows are timed by the engine in virtual time. Payload changes
//! (edits and auto-change) reach a running row through `GenUpdateFrame`;
//! auto-change on a running cyclic row is applied by the UI at about 20 Hz
//! rather than at every transmission, which is an approximation (a counter
//! steps per UI tick, not per frame sent).

use std::time::{Duration, Instant};

use operow_core::{BusId, CanFrame, is_valid_fd_len};
use operow_dbc::{Database, MessageDef, Mux, SignalDef};
use operow_engine::{Command, GeneratorId};
use serde::{Deserialize, Serialize};

use crate::trace::NameLookup;
use crate::workspace::WindowId;

const ERROR_RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x30, 0x30);
const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x9c, 0x3a);
const SENT_BLUE: egui::Color32 = egui::Color32::from_rgb(0x30, 0x80, 0xe0);
/// UI rate of auto-change on running cyclic rows.
const AUTO_TICK: Duration = Duration::from_millis(50);
/// How long the status dot stays highlighted after a one-off send.
const FLASH: Duration = Duration::from_millis(250);
const FD_LENS: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];

// ---- pure helpers ----

/// Whether `len` bytes is a legal payload length for a classic or FD frame.
pub fn valid_len(len: usize, fd: bool) -> bool {
    if fd { is_valid_fd_len(len) } else { len <= 8 }
}

/// Smallest legal payload length that is at least `len`.
pub fn round_up_len(len: usize, fd: bool) -> usize {
    if !fd {
        return len.min(8);
    }
    FD_LENS
        .iter()
        .map(|l| *l as usize)
        .find(|l| *l >= len)
        .unwrap_or(64)
}

/// `00 11 22` style hex.
pub fn hex_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a hexadecimal CAN id (optional `0x`), checking the 11/29-bit range.
pub fn parse_id(text: &str, extended: bool) -> Result<u32, String> {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let id = u32::from_str_radix(t, 16).map_err(|_| format!("invalid hex id '{text}'"))?;
    let max = if extended { 0x1FFF_FFFF } else { 0x7FF };
    if id > max {
        return Err(format!(
            "id 0x{id:X} exceeds 0x{max:X} ({}-bit)",
            if extended { 29 } else { 11 }
        ));
    }
    Ok(id)
}

/// Parse hex payload text for a frame of `dlc` bytes. Bytes may be spaced
/// (`11 22`) or run together (`1122`); a shorter payload is zero-padded to
/// `dlc`, a longer one is an error. `dlc` must be legal for `fd`.
pub fn parse_data(text: &str, dlc: usize, fd: bool) -> Result<Vec<u8>, String> {
    if !valid_len(dlc, fd) {
        return Err(if fd {
            format!("DLC {dlc} is not a CAN FD length (0-8, 12, 16, 20, 24, 32, 48, 64)")
        } else {
            format!("DLC {dlc} exceeds 8 for a classic frame")
        });
    }
    let mut bytes = Vec::new();
    for tok in text.split(|c: char| c.is_whitespace() || c == ',') {
        if tok.is_empty() {
            continue;
        }
        if !tok.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("'{tok}' is not hex"));
        }
        if tok.len() <= 2 {
            bytes.push(u8::from_str_radix(tok, 16).unwrap_or(0));
        } else if tok.len() % 2 == 0 {
            for pair in tok.as_bytes().chunks(2) {
                let s = std::str::from_utf8(pair).unwrap_or("0");
                bytes.push(u8::from_str_radix(s, 16).unwrap_or(0));
            }
        } else {
            return Err(format!("'{tok}' has an odd number of hex digits"));
        }
    }
    if bytes.len() > dlc {
        return Err(format!("{} bytes exceed DLC {dlc}", bytes.len()));
    }
    bytes.resize(dlc, 0);
    Ok(bytes)
}

/// Value range a signal can take: `[min, max]` from the DBC, or the range of
/// its raw bit width when the DBC leaves it unspecified (`min == max`).
pub fn sig_range(sig: &SignalDef) -> (f64, f64) {
    if sig.max > sig.min {
        return (sig.min, sig.max);
    }
    let n = sig.size.clamp(1, 63) as u32;
    let (lo, hi) = match sig.value_type {
        operow_dbc::ValueType::Unsigned => (0.0, ((1u64 << n) - 1) as f64),
        operow_dbc::ValueType::Signed => {
            (-((1u64 << (n - 1)) as f64), ((1u64 << (n - 1)) - 1) as f64)
        }
    };
    let (a, b) = (lo * sig.factor + sig.offset, hi * sig.factor + sig.offset);
    (a.min(b), a.max(b))
}

/// Starting physical value of a signal: its `GenSigStartValue`, else zero
/// clamped into range.
pub fn default_value(sig: &SignalDef) -> f64 {
    let (lo, hi) = sig_range(sig);
    match sig.initial_raw {
        Some(raw) => (raw as f64 * sig.factor + sig.offset).clamp(lo, hi),
        None => 0.0f64.clamp(lo, hi),
    }
}

/// What changes a signal's value each time its row sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum AutoChange {
    #[default]
    None,
    /// Add `step`, wrapping to the minimum past the maximum.
    Counter { step: f64 },
    /// Triangle between min and max; each leg lasts `period_s` seconds.
    Ramp { period_s: f64 },
    /// Swap between min and max.
    Toggle,
    /// Uniform random value in range.
    Random,
}

impl AutoChange {
    pub fn label(&self) -> &'static str {
        match self {
            AutoChange::None => "None",
            AutoChange::Counter { .. } => "Counter",
            AutoChange::Ramp { .. } => "Ramp",
            AutoChange::Toggle => "Toggle",
            AutoChange::Random => "Random",
        }
    }
}

/// Next counter value: `cur + step`, wrapping to `min` once it passes `max`.
pub fn counter_next(cur: f64, step: f64, min: f64, max: f64) -> f64 {
    let next = cur + step;
    if next > max + 1e-9 || next < min - 1e-9 {
        min
    } else {
        next
    }
}

/// Triangle wave: `min` at `t = 0`, `max` after `period_s`, back to `min`
/// after `2 * period_s`, and so on.
pub fn ramp_value(min: f64, max: f64, period_s: f64, t_s: f64) -> f64 {
    let phase = (t_s / period_s.max(1e-3)).rem_euclid(2.0);
    let f = if phase <= 1.0 { phase } else { 2.0 - phase };
    min + (max - min) * f
}

/// The other end of the range: the one farther from `cur`.
pub fn toggle_next(cur: f64, min: f64, max: f64) -> f64 {
    if (cur - min).abs() <= (cur - max).abs() {
        max
    } else {
        min
    }
}

/// Small xorshift generator for the Random auto-change.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A signal's edited physical value and its auto-change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigEdit {
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub auto: AutoChange,
}

impl SigEdit {
    fn new(sig: &SignalDef) -> Self {
        SigEdit {
            name: sig.name.clone(),
            value: default_value(sig),
            auto: AutoChange::None,
        }
    }

    /// Apply one auto-change step.
    pub fn advance(&mut self, sig: &SignalDef, t_s: f64, rng: &mut Rng) {
        let (lo, hi) = sig_range(sig);
        self.value = match self.auto {
            AutoChange::None => return,
            AutoChange::Counter { step } => counter_next(self.value, step, lo, hi),
            AutoChange::Ramp { period_s } => ramp_value(lo, hi, period_s, t_s),
            AutoChange::Toggle => toggle_next(self.value, lo, hi),
            AutoChange::Random => lo + rng.next_f64() * (hi - lo),
        };
    }
}

/// Edits for every signal of `msg`, keeping values already present in `old`.
fn sync_edits(msg: &MessageDef, old: &[SigEdit]) -> Vec<SigEdit> {
    msg.signals
        .iter()
        .map(|s| {
            old.iter()
                .find(|e| e.name == s.name)
                .cloned()
                .unwrap_or_else(|| SigEdit::new(s))
        })
        .collect()
}

fn edit_value(edits: &[SigEdit], sig: &SignalDef) -> f64 {
    edits
        .iter()
        .find(|e| e.name == sig.name)
        .map_or_else(|| default_value(sig), |e| e.value)
}

/// Raw selector value of the message's multiplexor, if it has one.
fn selector_raw(msg: &MessageDef, edits: &[SigEdit]) -> Option<u64> {
    let sel = msg
        .signals
        .iter()
        .find(|s| s.multiplexer == Some(Mux::Multiplexor))?;
    let mut scratch = [0u8; 64];
    sel.encode(&mut scratch, edit_value(edits, sel));
    Some(sel.decode_raw(&scratch))
}

/// Whether `sig` is part of the frame given the current selector value.
pub fn signal_active(msg: &MessageDef, edits: &[SigEdit], sig: &SignalDef) -> bool {
    match sig.multiplexer {
        Some(Mux::Multiplexed(n)) => selector_raw(msg, edits) == Some(n),
        _ => true,
    }
}

/// Encode every active signal of `msg` into a `len`-byte payload.
pub fn encode_message(msg: &MessageDef, edits: &[SigEdit], len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    for s in msg.signals.iter().filter(|s| signal_active(msg, edits, s)) {
        s.encode(&mut data, edit_value(edits, s));
    }
    data
}

// ---- model ----

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SendMode {
    #[default]
    Once,
    Cyclic,
    Key,
}

impl SendMode {
    fn label(self) -> &'static str {
        match self {
            SendMode::Once => "Once",
            SendMode::Cyclic => "Cyclic",
            SendMode::Key => "Key",
        }
    }
}

/// DBC mode of a row: the chosen message and its signal edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbcRow {
    pub msg: String,
    pub signals: Vec<SigEdit>,
}

/// Keys a row can be bound to.
fn key_choices() -> Vec<String> {
    (1..=12)
        .map(|n| format!("F{n}"))
        .chain(('A'..='Z').map(|c| c.to_string()))
        .collect()
}

/// Run-time state of a row; not saved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowRt {
    /// The cyclic row is running in the engine.
    active: bool,
    last_sent: Option<Instant>,
    /// What the engine was last told for this running row.
    synced: Option<(Option<BusId>, u64, CanFrame)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenRow {
    /// Stable per-window id, used as the engine's row number.
    pub uid: u32,
    pub bus: Option<BusId>,
    pub id_text: String,
    pub extended: bool,
    pub fd: bool,
    pub brs: bool,
    /// Payload length in bytes.
    pub dlc: u8,
    pub data_text: String,
    pub mode: SendMode,
    pub period_ms: u32,
    /// Key name ("F5", "A"), for `SendMode::Key`.
    pub key: Option<String>,
    pub dbc: Option<DbcRow>,
    #[serde(skip)]
    pub rt: RowRt,
}

impl Default for GenRow {
    fn default() -> Self {
        GenRow {
            uid: 0,
            bus: None,
            id_text: "100".into(),
            extended: false,
            fd: false,
            brs: false,
            dlc: 8,
            data_text: "00 00 00 00 00 00 00 00".into(),
            mode: SendMode::Once,
            period_ms: 100,
            key: None,
            dbc: None,
            rt: RowRt::default(),
        }
    }
}

impl GenRow {
    pub fn is_active(&self) -> bool {
        self.rt.active
    }

    fn has_auto(&self) -> bool {
        self.dbc
            .as_ref()
            .is_some_and(|d| d.signals.iter().any(|s| s.auto != AutoChange::None))
    }

    fn period_ns(&self) -> u64 {
        self.period_ms.max(1) as u64 * 1_000_000
    }

    /// Validate and build the frame from the row's fields.
    pub fn build_frame(&self) -> Result<CanFrame, String> {
        let id = parse_id(&self.id_text, self.extended)?;
        let data = parse_data(&self.data_text, self.dlc as usize, self.fd)?;
        let frame = if self.fd {
            CanFrame::new_fd(id, self.extended, self.brs, &data)
        } else {
            CanFrame::new(id, self.extended, &data)
        };
        frame.map_err(|e| e.to_string())
    }

    /// In DBC mode, re-derive id, DLC and bytes from the signal edits.
    fn refresh_dbc(&mut self, db: Option<&Database>) {
        let (Some(dbc), Some(db)) = (self.dbc.as_mut(), db) else {
            return;
        };
        let Some(msg) = db.messages.iter().find(|m| m.name == dbc.msg) else {
            return;
        };
        dbc.signals = sync_edits(msg, &dbc.signals);
        let len = round_up_len(msg.dlc as usize, self.fd);
        self.id_text = format!("{:X}", msg.id);
        self.extended = msg.extended;
        self.dlc = len as u8;
        self.data_text = hex_string(&encode_message(msg, &dbc.signals, len));
    }

    /// Step every auto-change signal.
    fn apply_auto(&mut self, db: Option<&Database>, t_s: f64, rng: &mut Rng) {
        let (Some(dbc), Some(db)) = (self.dbc.as_mut(), db) else {
            return;
        };
        let Some(msg) = db.messages.iter().find(|m| m.name == dbc.msg) else {
            return;
        };
        for e in &mut dbc.signals {
            if let Some(sig) = msg.signals.iter().find(|s| s.name == e.name) {
                e.advance(sig, t_s, rng);
            }
        }
    }

    /// The frame for a one-off send: auto-change applies first.
    fn next_send(&mut self, db: Option<&Database>, t_s: f64, rng: &mut Rng) -> Option<CanFrame> {
        self.apply_auto(db, t_s, rng);
        self.refresh_dbc(db);
        self.rt.last_sent = Some(Instant::now());
        self.build_frame().ok()
    }

    fn stop_local(&mut self) {
        self.rt.active = false;
        self.rt.synced = None;
    }

    /// A row copied from a bus event.
    fn from_frame(uid: u32, bus: BusId, frame: &CanFrame) -> Self {
        GenRow {
            uid,
            bus: Some(bus),
            id_text: format!("{:X}", frame.id),
            extended: frame.extended,
            fd: frame.fd,
            brs: frame.brs,
            dlc: frame.dlc,
            data_text: hex_string(frame.payload()),
            ..Default::default()
        }
    }
}

/// Saved settings of a generator window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneratorView {
    pub title: Option<String>,
    pub rows: Vec<GenRow>,
}

pub struct GeneratorWindow {
    pub title: Option<String>,
    pub rows: Vec<GenRow>,
    next_uid: u32,
    pub renaming: bool,
    rename_buf: String,
    epoch: Instant,
    last_auto: Instant,
    rng: Rng,
}

impl Default for GeneratorWindow {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9, |d| d.as_nanos() as u64);
        GeneratorWindow {
            title: None,
            rows: Vec::new(),
            next_uid: 1,
            renaming: false,
            rename_buf: String::new(),
            epoch: Instant::now(),
            last_auto: Instant::now(),
            rng: Rng::new(seed),
        }
    }
}

enum RowAction {
    Send,
    Start,
    Stop,
    Delete,
}

impl GeneratorWindow {
    pub fn view(&self) -> GeneratorView {
        GeneratorView {
            title: self.title.clone(),
            rows: self.rows.clone(),
        }
    }

    /// Apply saved settings. Rows never start running on load.
    pub fn apply_view(&mut self, v: GeneratorView) {
        self.title = v.title;
        self.rows = v.rows;
        for r in &mut self.rows {
            r.stop_local();
        }
        // Rows saved without a uid (or with duplicates) get fresh ones.
        let mut seen = std::collections::HashSet::new();
        self.next_uid = self.rows.iter().map(|r| r.uid).max().unwrap_or(0) + 1;
        for r in &mut self.rows {
            if r.uid == 0 || !seen.insert(r.uid) {
                r.uid = self.next_uid;
                self.next_uid += 1;
                seen.insert(r.uid);
            }
        }
    }

    pub fn begin_rename(&mut self) {
        self.renaming = true;
        self.rename_buf = self.title.clone().unwrap_or_default();
    }

    /// The sender name traces show for this window's frames.
    pub fn sender_name(&self, id: WindowId) -> String {
        self.title.clone().unwrap_or_else(|| id.title())
    }

    fn new_uid(&mut self) -> u32 {
        let u = self.next_uid;
        self.next_uid += 1;
        u
    }

    pub fn add_row(&mut self) {
        let uid = self.new_uid();
        self.rows.push(GenRow {
            uid,
            ..Default::default()
        });
    }

    /// Append a row built from a bus event ("Copy as generator frame").
    pub fn add_frame_row(&mut self, bus: BusId, frame: &CanFrame) {
        let uid = self.new_uid();
        self.rows.push(GenRow::from_frame(uid, bus, frame));
    }

    /// Forget which rows run (the measurement stopped; the engine dropped
    /// its timers).
    pub fn stop_local(&mut self) {
        for r in &mut self.rows {
            r.stop_local();
        }
    }

    /// Mark a row as running; the next `update` tells the engine.
    pub fn start_row(&mut self, idx: usize) {
        if let Some(r) = self.rows.get_mut(idx) {
            r.rt.active = true;
            r.rt.synced = None;
        }
    }

    fn start_all(&mut self) {
        for i in 0..self.rows.len() {
            if self.rows[i].mode == SendMode::Cyclic && self.rows[i].build_frame().is_ok() {
                self.start_row(i);
            }
        }
    }

    fn stop_all(&mut self, gen_id: GeneratorId) -> Command {
        self.stop_local();
        Command::GenStopAll { gen_id }
    }

    fn send_cmd(&mut self, i: usize, names: &NameLookup, gen_id: GeneratorId) -> Option<Command> {
        let t_s = self.epoch.elapsed().as_secs_f64();
        let row = &mut self.rows[i];
        let db = row_db(names, row);
        let frame = row.next_send(db, t_s, &mut self.rng)?;
        Some(Command::GenSend {
            gen_id,
            bus: row.bus,
            frame,
        })
    }

    /// Per-frame work independent of the window being visible: key presses,
    /// auto-change ticks and keeping the engine's running rows in sync.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        id: WindowId,
        names: &NameLookup,
        running: bool,
    ) -> Vec<Command> {
        let gen_id = GeneratorId(id.n);
        let mut cmds = Vec::new();
        let now = Instant::now();

        if running && !ctx.wants_keyboard_input() {
            for i in 0..self.rows.len() {
                let row = &self.rows[i];
                let key = (row.mode == SendMode::Key)
                    .then(|| row.key.as_deref().and_then(egui::Key::from_name))
                    .flatten();
                let pressed = key
                    .is_some_and(|k| ctx.input(|inp| inp.key_pressed(k) && !inp.modifiers.any()));
                if pressed {
                    cmds.extend(self.send_cmd(i, names, gen_id));
                }
            }
        }

        let tick = now.duration_since(self.last_auto) >= AUTO_TICK;
        if tick {
            self.last_auto = now;
        }
        let t_s = self.epoch.elapsed().as_secs_f64();
        let mut repaint = false;
        for row in &mut self.rows {
            if row.rt.active && (row.mode != SendMode::Cyclic || !running) {
                row.stop_local();
                cmds.push(Command::GenSetCyclic {
                    gen_id,
                    row: row.uid,
                    bus: row.bus,
                    frame: empty_frame(),
                    period_ns: None,
                });
                continue;
            }
            let db = row_db(names, row);
            if row.rt.active && tick && row.has_auto() {
                row.apply_auto(db, t_s, &mut self.rng);
            }
            row.refresh_dbc(db);
            if row.rt.active {
                repaint |= row.has_auto();
                if let Ok(frame) = row.build_frame() {
                    let key = (row.bus, row.period_ns());
                    match row.rt.synced {
                        Some((b, p, f)) if (b, p) == key => {
                            if f != frame {
                                cmds.push(Command::GenUpdateFrame {
                                    gen_id,
                                    row: row.uid,
                                    frame,
                                });
                                row.rt.synced = Some((b, p, frame));
                            }
                        }
                        _ => {
                            cmds.push(Command::GenSetCyclic {
                                gen_id,
                                row: row.uid,
                                bus: row.bus,
                                frame,
                                period_ns: Some(key.1),
                            });
                            row.rt.synced = Some((key.0, key.1, frame));
                        }
                    }
                }
            }
            if row
                .rt
                .last_sent
                .is_some_and(|t| now.duration_since(t) < FLASH)
            {
                repaint = true;
            }
        }
        if repaint {
            ctx.request_repaint_after(AUTO_TICK);
        }
        cmds
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        id: WindowId,
        names: &NameLookup,
        running: bool,
    ) -> Vec<Command> {
        let gen_id = GeneratorId(id.n);
        let mut cmds = Vec::new();

        ui.horizontal(|ui| {
            if self.renaming {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.rename_buf)
                        .desired_width(160.0)
                        .hint_text("Window title"),
                );
                if !resp.has_focus() && !resp.lost_focus() {
                    resp.request_focus();
                }
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if enter || escape || resp.lost_focus() {
                    if !escape {
                        let t = self.rename_buf.trim();
                        self.title = (!t.is_empty()).then(|| t.to_string());
                    }
                    self.renaming = false;
                }
                ui.separator();
            }
            if ui.button("+ Add row").clicked() {
                self.add_row();
            }
            let any_cyclic = self.rows.iter().any(|r| r.mode == SendMode::Cyclic);
            if ui
                .add_enabled(running && any_cyclic, egui::Button::new("Start all"))
                .on_disabled_hover_text(if running {
                    "No cyclic rows"
                } else {
                    "Start the measurement first"
                })
                .clicked()
            {
                self.start_all();
            }
            if ui.button("Stop all").clicked() {
                cmds.push(self.stop_all(gen_id));
            }
            ui.separator();
            ui.weak(format!("Sender: {}", self.sender_name(id)));
            if !running {
                ui.weak("Sending is disabled until the measurement runs.");
            }
        });
        ui.separator();

        let mut buses: Vec<(BusId, String)> = names
            .bus_names
            .iter()
            .map(|(b, n)| (*b, n.clone()))
            .collect();
        buses.sort_by(|a, b| a.1.cmp(&b.1));

        let mut actions: Vec<(usize, RowAction)> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if self.rows.is_empty() {
                    ui.weak(
                        "No rows. Use \"+ Add row\" or \"Copy as generator frame\" in a trace.",
                    );
                }
                let frame = egui::Frame::group(ui.style()).inner_margin(6.0);
                for (i, row) in self.rows.iter_mut().enumerate() {
                    let db = row_db(names, row);
                    frame.show(ui, |ui| {
                        ui.set_width(ui.available_width() - 14.0);
                        ui.push_id(row.uid, |ui| {
                            if let Some(a) = row_ui(ui, row, db, &buses, running) {
                                actions.push((i, a));
                            }
                        });
                    });
                }
            });

        let mut delete = Vec::new();
        for (i, action) in actions {
            match action {
                RowAction::Send => cmds.extend(self.send_cmd(i, names, gen_id)),
                RowAction::Start => self.start_row(i),
                RowAction::Stop => {
                    let row = &mut self.rows[i];
                    row.stop_local();
                    cmds.push(Command::GenSetCyclic {
                        gen_id,
                        row: row.uid,
                        bus: row.bus,
                        frame: empty_frame(),
                        period_ns: None,
                    });
                }
                RowAction::Delete => delete.push(i),
            }
        }
        for i in delete.into_iter().rev() {
            let row = self.rows.remove(i);
            if row.rt.active {
                cmds.push(Command::GenSetCyclic {
                    gen_id,
                    row: row.uid,
                    bus: row.bus,
                    frame: empty_frame(),
                    period_ns: None,
                });
            }
        }
        cmds
    }
}

/// Placeholder frame for a stop command (ignored by the engine).
fn empty_frame() -> CanFrame {
    CanFrame::new(0, false, &[]).expect("empty frame is valid")
}

/// The database of the row's bus, if it is a specific bus with one.
fn row_db<'a>(names: &'a NameLookup, row: &GenRow) -> Option<&'a Database> {
    let bus = row.bus?;
    names.dbcs.by_bus.get(&bus).map(|d| &**d)
}

fn resize_data_text(text: &str, len: usize) -> String {
    let mut bytes = parse_data(text, 64, true).unwrap_or_default();
    bytes.resize(len, 0);
    hex_string(&bytes)
}

fn mono_edit<'a>(text: &'a mut String, width: f32, bad: bool) -> egui::TextEdit<'a> {
    let mut e = egui::TextEdit::singleline(text)
        .desired_width(width)
        .font(egui::TextStyle::Monospace);
    if bad {
        e = e.text_color(ERROR_RED);
    }
    e
}

fn row_ui(
    ui: &mut egui::Ui,
    row: &mut GenRow,
    db: Option<&Database>,
    buses: &[(BusId, String)],
    running: bool,
) -> Option<RowAction> {
    let mut action = None;
    let dbc_mode = row.dbc.is_some() && db.is_some();
    let built = row.build_frame();

    ui.horizontal(|ui| {
        // Status dot.
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        let flashing = row.rt.last_sent.is_some_and(|t| t.elapsed() < FLASH);
        let (color, tip) = if built.is_err() {
            (ERROR_RED, "Invalid frame")
        } else if row.is_active() {
            (OK_GREEN, "Sending cyclically")
        } else if flashing {
            (SENT_BLUE, "Sent")
        } else {
            (egui::Color32::GRAY, "Idle")
        };
        ui.painter().circle_filled(rect.center(), 5.0, color);
        resp.on_hover_text(tip);

        let bus_text = match row.bus {
            None => "All".to_string(),
            Some(b) => buses
                .iter()
                .find(|(id, _)| *id == b)
                .map_or_else(|| format!("Bus{}", b.0), |(_, n)| n.clone()),
        };
        egui::ComboBox::from_id_salt("gen_bus")
            .selected_text(bus_text)
            .width(84.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut row.bus, None, "All");
                for (b, n) in buses {
                    ui.selectable_value(&mut row.bus, Some(*b), n);
                }
            });

        ui.label("ID");
        let id_bad = parse_id(&row.id_text, row.extended).is_err();
        ui.add_enabled_ui(!dbc_mode, |ui| {
            ui.add(mono_edit(&mut row.id_text, 64.0, id_bad));
            ui.checkbox(&mut row.extended, "Ext");
        });
        if ui.checkbox(&mut row.fd, "FD").changed() {
            if !row.fd {
                row.brs = false;
            }
            let len = round_up_len(row.dlc as usize, row.fd);
            if len != row.dlc as usize {
                row.dlc = len as u8;
                row.data_text = resize_data_text(&row.data_text, len);
            }
        }
        ui.add_enabled(row.fd, egui::Checkbox::new(&mut row.brs, "BRS"));

        ui.label("DLC");
        ui.add_enabled_ui(!dbc_mode, |ui| {
            let prev = row.dlc;
            egui::ComboBox::from_id_salt("gen_dlc")
                .selected_text(row.dlc.to_string())
                .width(46.0)
                .show_ui(ui, |ui| {
                    for l in FD_LENS.iter().filter(|l| row.fd || **l <= 8) {
                        ui.selectable_value(&mut row.dlc, *l, l.to_string());
                    }
                });
            if row.dlc != prev {
                row.data_text = resize_data_text(&row.data_text, row.dlc as usize);
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        egui::ComboBox::from_id_salt("gen_mode")
            .selected_text(row.mode.label())
            .width(64.0)
            .show_ui(ui, |ui| {
                for m in [SendMode::Once, SendMode::Cyclic, SendMode::Key] {
                    ui.selectable_value(&mut row.mode, m, m.label());
                }
            });
        match row.mode {
            SendMode::Once => {}
            SendMode::Cyclic => {
                ui.add(
                    egui::DragValue::new(&mut row.period_ms)
                        .range(1..=600_000)
                        .suffix(" ms"),
                );
            }
            SendMode::Key => {
                egui::ComboBox::from_id_salt("gen_key")
                    .selected_text(row.key.as_deref().unwrap_or("none"))
                    .width(56.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut row.key, None, "none");
                        for k in key_choices() {
                            ui.selectable_value(&mut row.key, Some(k.clone()), k);
                        }
                    });
            }
        }

        let ok = built.is_ok();
        if row.mode == SendMode::Cyclic && row.is_active() {
            if ui.button("Stop").clicked() {
                action = Some(RowAction::Stop);
            }
        } else {
            let label = if row.mode == SendMode::Cyclic {
                "Start"
            } else {
                "Send"
            };
            let resp = ui.add_enabled(running && ok, egui::Button::new(label));
            let resp = if !running {
                resp.on_disabled_hover_text("Start the measurement first")
            } else {
                resp.on_disabled_hover_text("Fix the frame first")
            };
            if resp.clicked() {
                action = Some(if row.mode == SendMode::Cyclic {
                    RowAction::Start
                } else {
                    RowAction::Send
                });
            }
        }

        // Raw <-> DBC.
        if db.is_some() {
            let mut want_dbc = row.dbc.is_some();
            if ui
                .selectable_label(!want_dbc, "Raw")
                .on_hover_text("Edit the id and bytes directly")
                .clicked()
            {
                want_dbc = false;
            }
            if ui
                .selectable_label(want_dbc, "DBC")
                .on_hover_text("Edit signals of a DBC message")
                .clicked()
                && let Some(db) = db
            {
                want_dbc = true;
                if row.dbc.is_none() {
                    let id = parse_id(&row.id_text, row.extended).ok();
                    let msg = id
                        .and_then(|i| db.message(i, row.extended))
                        .or_else(|| db.messages.first());
                    if let Some(m) = msg {
                        row.dbc = Some(DbcRow {
                            msg: m.name.clone(),
                            signals: sync_edits(m, &[]),
                        });
                    }
                }
            }
            if !want_dbc {
                row.dbc = None;
            }
        } else if row.bus.is_none() {
            ui.add_enabled(false, egui::Button::new("DBC"))
                .on_disabled_hover_text("Choose a bus with a DBC to edit signals");
        }

        if ui.button("\u{1f5d1}").on_hover_text("Delete row").clicked() {
            action = Some(RowAction::Delete);
        }
    });

    ui.add_space(2.0);
    if dbc_mode {
        if let Some(db) = db {
            dbc_section(ui, row, db);
        }
    } else {
        if row.dbc.is_some() {
            ui.colored_label(
                ERROR_RED,
                "DBC not available on this bus; sending the stored bytes.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Data");
            let bad = built.is_err();
            let w = (ui.available_width() - 12.0).clamp(120.0, 520.0);
            ui.add(mono_edit(&mut row.data_text, w, bad));
            if let Err(e) = &built
                && !e.starts_with("invalid hex id")
                && !e.starts_with("id ")
            {
                ui.colored_label(ERROR_RED, e);
            }
        });
        if let Err(e) = &built
            && (e.starts_with("invalid hex id") || e.starts_with("id "))
        {
            ui.colored_label(ERROR_RED, e);
        }
    }
    action
}

fn dbc_section(ui: &mut egui::Ui, row: &mut GenRow, db: &Database) {
    let Some(dbc) = row.dbc.as_mut() else {
        return;
    };
    let Some(msg) = db.messages.iter().find(|m| m.name == dbc.msg) else {
        ui.colored_label(ERROR_RED, format!("Message {} not in this DBC", dbc.msg));
        return;
    };
    ui.horizontal(|ui| {
        ui.label("Message");
        let mut chosen = None;
        egui::ComboBox::from_id_salt("gen_msg")
            .selected_text(format!("{} (0x{:X})", msg.name, msg.id))
            .show_ui(ui, |ui| {
                for m in &db.messages {
                    if ui
                        .selectable_label(m.name == dbc.msg, format!("{} (0x{:X})", m.name, m.id))
                        .clicked()
                    {
                        chosen = Some(m);
                    }
                }
            });
        if let Some(m) = chosen {
            dbc.msg = m.name.clone();
            dbc.signals = sync_edits(m, &[]);
        }
        ui.weak("Bytes");
        ui.label(egui::RichText::new(&row.data_text).monospace());
    });

    let edits_snapshot = dbc.signals.clone();
    egui::Grid::new("gen_signals")
        .num_columns(4)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            for sig in &msg.signals {
                if !signal_active(msg, &edits_snapshot, sig) {
                    continue;
                }
                let Some(edit) = dbc.signals.iter_mut().find(|e| e.name == sig.name) else {
                    continue;
                };
                ui.label(&sig.name);
                signal_control(ui, sig, &mut edit.value);
                ui.label(if sig.unit.is_empty() { "" } else { &sig.unit });
                auto_change_ui(ui, sig, &mut edit.auto);
                ui.end_row();
            }
        });
}

fn signal_control(ui: &mut egui::Ui, sig: &SignalDef, value: &mut f64) {
    let (lo, hi) = (sig.min, sig.max);
    if sig.size == 1 {
        let mut on = ((*value - sig.offset) / sig.factor.max(1e-12)).round() != 0.0;
        if ui.checkbox(&mut on, "").changed() {
            *value = if on {
                sig.offset + sig.factor
            } else {
                sig.offset
            };
        }
    } else if !sig.value_descriptions.is_empty() {
        let raw = ((*value - sig.offset) / sig.factor.max(1e-12)).round() as i64;
        let text = sig
            .value_descriptions
            .iter()
            .find(|(v, _)| *v == raw)
            .map_or_else(|| raw.to_string(), |(v, d)| format!("{v} = {d}"));
        egui::ComboBox::from_id_salt(("gen_enum", &sig.name))
            .selected_text(text)
            .width(150.0)
            .show_ui(ui, |ui| {
                for (v, d) in &sig.value_descriptions {
                    if ui
                        .selectable_label(*v == raw, format!("{v} = {d}"))
                        .clicked()
                    {
                        *value = *v as f64 * sig.factor + sig.offset;
                    }
                }
            });
    } else if hi > lo {
        ui.spacing_mut().slider_width = 140.0;
        let mut s = egui::Slider::new(value, lo..=hi).max_decimals(3);
        if sig.factor > 0.0 {
            s = s.step_by(sig.factor);
        }
        ui.add(s);
    } else {
        ui.add(egui::DragValue::new(value).speed(sig.factor.max(0.01)));
    }
}

fn auto_change_ui(ui: &mut egui::Ui, sig: &SignalDef, auto: &mut AutoChange) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("gen_auto", &sig.name))
            .selected_text(auto.label())
            .width(76.0)
            .show_ui(ui, |ui| {
                let options = [
                    AutoChange::None,
                    AutoChange::Counter {
                        step: sig.factor.max(1e-6),
                    },
                    AutoChange::Ramp { period_s: 5.0 },
                    AutoChange::Toggle,
                    AutoChange::Random,
                ];
                for o in options {
                    let same = std::mem::discriminant(auto) == std::mem::discriminant(&o);
                    if ui.selectable_label(same, o.label()).clicked() && !same {
                        *auto = o;
                    }
                }
            });
        match auto {
            AutoChange::Counter { step } => {
                ui.add(
                    egui::DragValue::new(step)
                        .speed(sig.factor.max(0.01))
                        .prefix("step "),
                );
            }
            AutoChange::Ramp { period_s } => {
                ui.add(
                    egui::DragValue::new(period_s)
                        .range(0.1..=3600.0)
                        .speed(0.1)
                        .suffix(" s"),
                );
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DBC: &str = r#"VERSION ""
BU_: Engine Dash
BO_ 256 EngineData: 8 Engine
 SG_ EngineSpeed : 7|16@0+ (0.25,0) [0|16383.75] "rpm" Dash
 SG_ CoolantTemp : 16|8@1- (1,-40) [-40|215] "degC" Dash
 SG_ Running : 40|1@1+ (1,0) [0|1] "" Dash
BO_ 2147484928 GatewayStatus: 8 Engine
 SG_ Mode M : 0|4@1+ (1,0) [0|15] "" Dash
 SG_ Voltage m0 : 8|12@1+ (0.01,0) [0|40.95] "V" Dash
 SG_ Current m1 : 8|12@1- (0.1,0) [-204.8|204.7] "A" Dash
VAL_ 256 Running 0 "Off" 1 "On" ;
"#;

    fn db() -> Database {
        Database::parse(DBC).unwrap()
    }

    #[test]
    fn data_validation_classic_and_fd() {
        assert_eq!(
            parse_data("11 22", 4, false).unwrap(),
            vec![0x11, 0x22, 0, 0]
        );
        assert_eq!(
            parse_data("1122,33", 3, false).unwrap(),
            vec![0x11, 0x22, 0x33]
        );
        assert_eq!(parse_data("", 0, false).unwrap(), Vec::<u8>::new());
        assert!(parse_data("11 22 33", 2, false).is_err());
        assert!(parse_data("GG", 2, false).is_err());
        assert!(parse_data("123", 2, false).unwrap_err().contains("odd"));
        // DLC legality: classic caps at 8, FD allows 12..64 in steps.
        assert!(parse_data("", 9, false).is_err());
        assert!(parse_data("", 12, true).is_ok());
        assert!(parse_data("", 13, true).is_err());
        assert_eq!(parse_data("AA", 64, true).unwrap().len(), 64);
        assert_eq!(round_up_len(9, true), 12);
        assert_eq!(round_up_len(9, false), 8);
        assert_eq!(round_up_len(65, true), 64);
    }

    #[test]
    fn id_validation_and_frame_build() {
        assert_eq!(parse_id("0x1A", false), Ok(0x1A));
        assert!(parse_id("800", false).is_err());
        assert_eq!(parse_id("800", true), Ok(0x800));
        assert!(parse_id("20000000", true).is_err());
        assert!(parse_id("zz", false).is_err());
        let mut row = GenRow {
            id_text: "123".into(),
            dlc: 2,
            data_text: "AB CD".into(),
            ..Default::default()
        };
        let f = row.build_frame().unwrap();
        assert_eq!((f.id, f.dlc, f.payload()), (0x123, 2, &[0xAB, 0xCD][..]));
        assert!(!f.fd);
        row.fd = true;
        row.brs = true;
        let f = row.build_frame().unwrap();
        assert!(f.fd && f.brs);
        row.dlc = 13;
        assert!(row.build_frame().is_err());
    }

    #[test]
    fn dbc_edits_encode_into_bytes() {
        let db = db();
        let msg = db.message(256, false).unwrap();
        let mut edits = sync_edits(msg, &[]);
        for e in &mut edits {
            match e.name.as_str() {
                "EngineSpeed" => e.value = 3000.0,
                "CoolantTemp" => e.value = 25.0,
                "Running" => e.value = 1.0,
                _ => {}
            }
        }
        let data = encode_message(msg, &edits, 8);
        assert_eq!(data.len(), 8);
        let get = |n: &str| {
            let s = msg.signals.iter().find(|s| s.name == n).unwrap();
            s.decode(&data)
        };
        assert_eq!(get("EngineSpeed"), 3000.0);
        assert_eq!(get("CoolantTemp"), 25.0);
        assert_eq!(get("Running"), 1.0);
        // Raw: 3000 / 0.25 = 12000 = 0x2EE0, Motorola at byte 0..1.
        assert_eq!(&data[..2], &[0x2E, 0xE0]);
    }

    #[test]
    fn dbc_multiplexed_signals_follow_selector() {
        let db = db();
        let msg = db.message(0x500, true).unwrap();
        let mut edits = sync_edits(msg, &[]);
        let set = |edits: &mut Vec<SigEdit>, n: &str, v: f64| {
            edits.iter_mut().find(|e| e.name == n).unwrap().value = v;
        };
        set(&mut edits, "Voltage", 12.0);
        set(&mut edits, "Current", -5.0);
        set(&mut edits, "Mode", 0.0);
        let cur = msg.signals.iter().find(|s| s.name == "Current").unwrap();
        let vol = msg.signals.iter().find(|s| s.name == "Voltage").unwrap();
        assert!(signal_active(msg, &edits, vol));
        assert!(!signal_active(msg, &edits, cur));
        let d = encode_message(msg, &edits, 8);
        assert_eq!(vol.decode(&d), 12.0);
        set(&mut edits, "Mode", 1.0);
        assert!(signal_active(msg, &edits, cur));
        let d = encode_message(msg, &edits, 8);
        assert!((cur.decode(&d) + 5.0).abs() < 1e-9);
    }

    #[test]
    fn row_refresh_takes_id_dlc_and_bytes_from_dbc() {
        let db = db();
        let mut row = GenRow {
            fd: false,
            dbc: Some(DbcRow {
                msg: "EngineData".into(),
                signals: Vec::new(),
            }),
            ..Default::default()
        };
        row.refresh_dbc(Some(&db));
        assert_eq!(row.id_text, "100");
        assert!(!row.extended);
        assert_eq!(row.dlc, 8);
        let f = row.build_frame().unwrap();
        assert_eq!(f.dlc, 8);
    }

    #[test]
    fn counter_wraps_at_max() {
        assert_eq!(counter_next(0.0, 1.0, 0.0, 3.0), 1.0);
        assert_eq!(counter_next(2.0, 1.0, 0.0, 3.0), 3.0);
        assert_eq!(counter_next(3.0, 1.0, 0.0, 3.0), 0.0);
        assert_eq!(counter_next(14.0, 1.0, 0.0, 15.0), 15.0);
        assert_eq!(counter_next(15.0, 1.0, 0.0, 15.0), 0.0);
    }

    #[test]
    fn ramp_is_a_triangle() {
        let r = |t| ramp_value(10.0, 20.0, 2.0, t);
        assert_eq!(r(0.0), 10.0);
        assert!((r(1.0) - 15.0).abs() < 1e-9);
        assert!((r(2.0) - 20.0).abs() < 1e-9);
        assert!((r(3.0) - 15.0).abs() < 1e-9);
        assert!((r(4.0) - 10.0).abs() < 1e-9);
        assert!((r(5.0) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn toggle_and_random_stay_in_range() {
        assert_eq!(toggle_next(0.0, 0.0, 1.0), 1.0);
        assert_eq!(toggle_next(1.0, 0.0, 1.0), 0.0);
        let mut rng = Rng::new(42);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v));
            seen.insert((v * 10.0) as u32);
        }
        assert!(seen.len() > 5);
    }

    #[test]
    fn sig_edit_advance_uses_signal_range() {
        let db = db();
        let msg = db.message(256, false).unwrap();
        let sig = msg.signals.iter().find(|s| s.name == "Running").unwrap();
        let mut e = SigEdit::new(sig);
        e.auto = AutoChange::Toggle;
        let mut rng = Rng::new(1);
        e.advance(sig, 0.0, &mut rng);
        assert_eq!(e.value, 1.0);
        e.advance(sig, 0.0, &mut rng);
        assert_eq!(e.value, 0.0);
        let sig = msg
            .signals
            .iter()
            .find(|s| s.name == "EngineSpeed")
            .unwrap();
        let mut e = SigEdit::new(sig);
        e.auto = AutoChange::Ramp { period_s: 4.0 };
        e.advance(sig, 2.0, &mut rng);
        assert!((e.value - sig.max / 2.0).abs() < 1e-6);
    }

    #[test]
    fn unspecified_range_falls_back_to_raw_width() {
        let db = db();
        let msg = db.message(256, false).unwrap();
        let mut sig = msg
            .signals
            .iter()
            .find(|s| s.name == "EngineSpeed")
            .unwrap()
            .clone();
        sig.min = 0.0;
        sig.max = 0.0;
        assert_eq!(sig_range(&sig), (0.0, 65535.0 * 0.25));
    }

    #[test]
    fn view_round_trips_and_rows_never_run() {
        let mut w = GeneratorWindow {
            title: Some("Body".into()),
            ..Default::default()
        };
        w.add_row();
        w.add_row();
        w.rows[0].mode = SendMode::Cyclic;
        w.rows[0].period_ms = 20;
        w.rows[0].bus = Some(BusId(2));
        w.rows[0].rt.active = true;
        w.rows[1].mode = SendMode::Key;
        w.rows[1].key = Some("F5".into());
        w.rows[1].dbc = Some(DbcRow {
            msg: "EngineData".into(),
            signals: vec![
                SigEdit {
                    name: "EngineSpeed".into(),
                    value: 1234.5,
                    auto: AutoChange::Ramp { period_s: 3.0 },
                },
                SigEdit {
                    name: "Running".into(),
                    value: 1.0,
                    auto: AutoChange::Counter { step: 1.0 },
                },
            ],
        });
        let json = serde_json::to_string(&w.view()).unwrap();
        let view: GeneratorView = serde_json::from_str(&json).unwrap();
        let mut back = GeneratorWindow::default();
        back.apply_view(view);
        assert_eq!(back.title.as_deref(), Some("Body"));
        assert_eq!(back.rows.len(), 2);
        assert!(back.rows.iter().all(|r| !r.is_active()));
        assert_eq!(back.rows[0].period_ms, 20);
        assert_eq!(back.rows[1].key.as_deref(), Some("F5"));
        assert_eq!(back.rows[1].dbc, w.rows[1].dbc);
        // New rows get fresh uids.
        back.add_row();
        assert_eq!(back.rows[2].uid, 3);
    }

    #[test]
    fn copied_frame_row_matches_event() {
        let frame =
            CanFrame::new_fd(0x1234, true, true, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]).unwrap();
        let mut w = GeneratorWindow::default();
        w.add_frame_row(BusId(3), &frame);
        let row = &w.rows[0];
        assert_eq!(row.bus, Some(BusId(3)));
        assert_eq!(row.build_frame().unwrap(), frame);
    }
}
