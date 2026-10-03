//! Trace windows: a virtualized log of bus events read from the shared
//! [`FrameStore`], either chronological (a filtered index of store
//! sequence numbers) or fixed-position (one row per frame key).

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui_extras::{Column, TableBuilder};
use operow_core::{BusEvent, BusId, CanFrame, Direction, NodeId};
use serde::{Deserialize, Serialize};

use crate::dbcs::{self, DbcStore};
use crate::filters::{CompiledFilters, RowFields, TextFilter, TraceFilters};
use crate::icons;
use crate::signals::{RawKind, SignalRef};
use crate::store::FrameStore;

/// How long a changed data byte stays highlighted in fixed-position mode.
const HIGHLIGHT: Duration = Duration::from_secs(1);
/// Height of the column-title part of the header; the filter row follows.
const TITLE_H: f32 = 18.0;
const FILTER_H: f32 = 24.0;

fn amber(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgba_unmultiplied(0xff, 0xb0, 0x20, 70)
    } else {
        egui::Color32::from_rgba_unmultiplied(0xff, 0xb0, 0x00, 110)
    }
}

/// A frame resolved to display strings.
#[derive(Clone)]
pub struct TraceRow {
    pub time_s: f64,
    pub bus: BusId,
    pub bus_name: String,
    pub sender_name: String,
    /// The ECU that first created the frame.
    pub origin_name: String,
    pub dir: Direction,
    pub hop: u8,
    pub id: u32,
    pub extended: bool,
    pub fd: bool,
    pub brs: bool,
    pub dlc: u8,
    pub data: [u8; 64],
    pub msg_name: String,
}

impl TraceRow {
    pub fn from_event(ev: &BusEvent, names: &NameLookup) -> TraceRow {
        TraceRow {
            time_s: ev.time.as_secs_f64(),
            bus: ev.bus,
            bus_name: names.bus_name(ev.bus),
            sender_name: names.node_name(ev.sender),
            origin_name: names.node_name(ev.origin),
            dir: ev.dir,
            hop: ev.hop,
            id: ev.frame.id,
            extended: ev.frame.extended,
            fd: ev.frame.fd,
            brs: ev.frame.brs,
            dlc: ev.frame.dlc,
            data: ev.frame.data,
            msg_name: names.msg_name(ev.bus, ev.origin, ev.frame.id, ev.frame.extended),
        }
    }

    pub fn frame_type(&self) -> &'static str {
        match (self.fd, self.brs) {
            (false, _) => "CAN",
            (true, false) => "CAN FD",
            (true, true) => "CAN FD BRS",
        }
    }

    fn dir_label(&self) -> &'static str {
        match self.dir {
            Direction::Tx => "Tx",
            Direction::Rx => "Rx",
        }
    }

    fn id_text(&self) -> String {
        format!("{:03X}{}", self.id, if self.extended { "x" } else { "" })
    }

    fn dlc_code(&self) -> u8 {
        operow_core::len_to_dlc(self.dlc as usize).unwrap_or(0)
    }

    fn sender_text(&self) -> String {
        if self.origin_name != self.sender_name {
            format!("{} (from {})", self.sender_name, self.origin_name)
        } else {
            self.sender_name.clone()
        }
    }

    fn data_hex(&self) -> String {
        self.data[..self.dlc as usize]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn frame(&self) -> CanFrame {
        CanFrame {
            id: self.id,
            extended: self.extended,
            fd: self.fd,
            brs: self.brs,
            dlc: self.dlc,
            data: self.data,
        }
    }
}

/// A table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Col {
    Time,
    Chn,
    Id,
    Name,
    Dir,
    Hop,
    Type,
    Count,
    Dt,
    Dlc,
    Len,
    Data,
    Sender,
}

impl Col {
    pub const ALL: [Col; 13] = [
        Col::Time,
        Col::Chn,
        Col::Id,
        Col::Name,
        Col::Dir,
        Col::Hop,
        Col::Type,
        Col::Count,
        Col::Dt,
        Col::Dlc,
        Col::Len,
        Col::Data,
        Col::Sender,
    ];

    fn label(self) -> &'static str {
        match self {
            Col::Time => "Time (s)",
            Col::Chn => "Chn",
            Col::Id => "ID",
            Col::Name => "Name",
            Col::Dir => "Dir",
            Col::Hop => "Hop",
            Col::Type => "Type",
            Col::Count => "Count",
            Col::Dt => "\u{394}t (ms)",
            Col::Dlc => "DLC",
            Col::Len => "Len",
            Col::Data => "Data",
            Col::Sender => "Sender",
        }
    }

    fn fixed_only(self) -> bool {
        matches!(self, Col::Count | Col::Dt)
    }

    fn width(self) -> f32 {
        match self {
            Col::Time => 140.0,
            Col::Chn => 90.0,
            Col::Id => 110.0,
            Col::Name => 120.0,
            Col::Dir => 66.0,
            Col::Hop => 60.0,
            Col::Type => 80.0,
            Col::Count => 70.0,
            Col::Dt => 80.0,
            Col::Dlc => 60.0,
            Col::Len => 40.0,
            Col::Data => 220.0,
            Col::Sender => 170.0,
        }
    }
}

/// Text of a cell, for CSV export and clipboard. `agg` is the fixed-mode
/// `(count, dt_ms)`.
fn cell_text(col: Col, r: &TraceRow, agg: Option<(u64, Option<f64>)>) -> String {
    match col {
        Col::Time => format!("{:.6}", r.time_s),
        Col::Chn => r.bus_name.clone(),
        Col::Id => r.id_text(),
        Col::Name => r.msg_name.clone(),
        Col::Dir => r.dir_label().to_string(),
        Col::Hop => r.hop.to_string(),
        Col::Type => r.frame_type().to_string(),
        Col::Count => agg.map_or(String::new(), |a| a.0.to_string()),
        Col::Dt => agg
            .and_then(|a| a.1)
            .map_or(String::new(), |d| format!("{d:.3}")),
        Col::Dlc => r.dlc_code().to_string(),
        Col::Len => r.dlc.to_string(),
        Col::Data => r.data_hex(),
        Col::Sender => r.sender_text(),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// CSV with a header line and one line per row, restricted to `cols`.
pub fn to_csv<'a>(
    rows: impl Iterator<Item = (TraceRow, Option<(u64, Option<f64>)>)> + 'a,
    cols: &[Col],
) -> String {
    let mut out = cols
        .iter()
        .map(|c| csv_escape(c.label()))
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for (row, agg) in rows {
        let line = cols
            .iter()
            .map(|c| csv_escape(&cell_text(*c, &row, agg)))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TraceMode {
    #[default]
    Chronological,
    Fixed,
}

/// Identity of a row in fixed-position mode.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FixedKey {
    bus: BusId,
    id: u32,
    extended: bool,
    /// `true` for Rx (so Tx sorts first).
    rx: bool,
}

/// One fixed-position row: the latest frame for a key plus aggregates.
pub struct FixedRow {
    seq: u64,
    ev: BusEvent,
    pub count: u64,
    /// Milliseconds since the previous frame with the same key.
    pub dt_ms: Option<f64>,
    /// When each data byte last changed value.
    changed_at: [Option<Instant>; 64],
}

/// Bitmask (bit i = byte i) of data bytes that differ between two frames.
/// Bytes beyond the previous length count as changed.
pub fn changed_mask(prev: &CanFrame, new: &CanFrame) -> u64 {
    let mut mask = 0u64;
    for i in 0..new.dlc as usize {
        if i >= prev.dlc as usize || prev.data[i] != new.data[i] {
            mask |= 1 << i;
        }
    }
    mask
}

impl FixedRow {
    fn is_changed(&self, i: usize, now: Instant) -> bool {
        self.changed_at[i].is_some_and(|t| now.saturating_duration_since(t) < HIGHLIGHT)
    }

    fn any_highlight(&self, now: Instant) -> bool {
        (0..self.ev.frame.dlc as usize).any(|i| self.is_changed(i, now))
    }
}

fn apply_fixed(fixed: &mut HashMap<FixedKey, FixedRow>, seq: u64, ev: &BusEvent, now: Instant) {
    let key = FixedKey {
        bus: ev.bus,
        id: ev.frame.id,
        extended: ev.frame.extended,
        rx: ev.dir == Direction::Rx,
    };
    match fixed.get_mut(&key) {
        Some(f) => {
            let mask = changed_mask(&f.ev.frame, &ev.frame);
            for i in 0..ev.frame.dlc as usize {
                if mask & (1 << i) != 0 {
                    f.changed_at[i] = Some(now);
                }
            }
            f.dt_ms = Some(ev.time.0.saturating_sub(f.ev.time.0) as f64 / 1e6);
            f.count += 1;
            f.seq = seq;
            f.ev = *ev;
        }
        None => {
            fixed.insert(
                key,
                FixedRow {
                    seq,
                    ev: *ev,
                    count: 1,
                    dt_ms: None,
                    changed_at: [None; 64],
                },
            );
        }
    }
}

/// Whether `ev` passes `c`; resolves names only for the filters that need
/// them.
fn event_matches(c: &CompiledFilters, ev: &BusEvent, names: &NameLookup) -> bool {
    let name = if c.needs_name() {
        names.msg_name(ev.bus, ev.origin, ev.frame.id, ev.frame.extended)
    } else {
        String::new()
    };
    let sender = if c.needs_sender() {
        names.node_name(ev.sender)
    } else {
        String::new()
    };
    c.matches(&RowFields {
        time_s: ev.time.as_secs_f64(),
        bus: &names.bus_name_cow(ev.bus),
        dir: ev.dir,
        id: ev.frame.id,
        name: &name,
        sender: &sender,
        data: ev.frame.payload(),
        dlc: ev.frame.dlc_code(),
        hop: ev.hop,
    })
}

/// Fixed-mode rows passing the filters, ordered by bus name, ID, format
/// and direction (Tx first).
fn fixed_items<'a>(
    fixed: &'a HashMap<FixedKey, FixedRow>,
    c: &CompiledFilters,
    names: &NameLookup,
) -> Vec<(&'a FixedKey, &'a FixedRow)> {
    let mut v: Vec<_> = fixed
        .iter()
        .filter(|(_, f)| event_matches(c, &f.ev, names))
        .collect();
    v.sort_by_cached_key(|(k, _)| (names.bus_name(k.bus), k.id, k.extended, k.rx));
    v
}

/// One open signal list under a frame row.
struct Expansion {
    /// Index of the frame among the frame rows.
    pos: usize,
    lines: Vec<String>,
}

/// What a display row shows.
#[derive(Debug, PartialEq, Eq)]
enum Loc {
    Frame(usize),
    Signal { exp: usize, line: usize },
}

/// Map display row `r` to a frame or a signal line, given the expansions
/// as `(frame index, line count)` sorted by frame index.
fn locate(r: usize, exp: &[(usize, usize)]) -> Loc {
    let mut extra = 0;
    for (k, &(pos, lines)) in exp.iter().enumerate() {
        let start = pos + extra;
        if r < start {
            break;
        }
        if r == start {
            return Loc::Frame(pos);
        }
        if r <= start + lines {
            return Loc::Signal {
                exp: k,
                line: r - start - 1,
            };
        }
        extra += lines;
    }
    Loc::Frame(r - extra)
}

/// Settings of a trace window that are saved in the project workspace.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TraceView {
    pub title: Option<String>,
    pub mode: TraceMode,
    pub hidden: BTreeSet<Col>,
    pub filters: TraceFilters,
}

/// Something a trace asks the application to do.
pub enum TraceAction {
    /// Hook for step 4 (graph wiring).
    AddSignalToGraph(SignalRef),
    /// Add the frame as a row of a Generator window; the frame JSON is also
    /// put on the clipboard.
    CopyAsGenerator(BusId, CanFrame),
    Log(String),
}

pub struct Trace {
    /// User title shown after the window name.
    pub title: Option<String>,
    pub paused: bool,
    pub filters: TraceFilters,
    pub autoscroll: bool,
    pub mode: TraceMode,
    pub hidden: BTreeSet<Col>,
    /// Show the signals of every fixed-mode row.
    pub expand_all: bool,
    /// The title is being edited.
    pub renaming: bool,
    rename_buf: String,

    /// Filters the current index was built with.
    applied: TraceFilters,
    compiled: CompiledFilters,
    /// This window shows nothing older than this seq (set by Clear).
    start_seq: u64,
    /// Next store seq to add to the chronological index.
    cursor: u64,
    /// Seqs of the frames passing the filters, ascending.
    index: VecDeque<u64>,
    fixed: HashMap<FixedKey, FixedRow>,
    fixed_cursor: u64,
    /// Store seq at which the view froze while paused.
    paused_at: Option<u64>,
    epoch: u64,
    /// Chronological rows (by seq) whose signals are shown.
    expanded: HashSet<u64>,
    expanded_fixed: HashSet<FixedKey>,
}

impl Default for Trace {
    fn default() -> Self {
        let filters = TraceFilters::default();
        Trace {
            title: None,
            paused: false,
            applied: filters.clone(),
            compiled: filters.compile(),
            filters,
            autoscroll: true,
            mode: TraceMode::default(),
            hidden: BTreeSet::new(),
            expand_all: false,
            renaming: false,
            rename_buf: String::new(),
            start_seq: 0,
            cursor: 0,
            index: VecDeque::new(),
            fixed: HashMap::new(),
            fixed_cursor: 0,
            paused_at: None,
            epoch: 0,
            expanded: HashSet::new(),
            expanded_fixed: HashSet::new(),
        }
    }
}

enum CtxAction {
    FilterId(u32),
    FilterBus(String),
    Act(TraceAction),
}

impl Trace {
    pub fn view(&self) -> TraceView {
        TraceView {
            title: self.title.clone(),
            mode: self.mode,
            hidden: self.hidden.clone(),
            filters: self.filters.clone(),
        }
    }

    pub fn apply_view(&mut self, v: TraceView) {
        self.title = v.title;
        self.mode = v.mode;
        self.hidden = v.hidden;
        self.filters = v.filters;
    }

    /// Start editing the window title.
    pub fn begin_rename(&mut self) {
        self.renaming = true;
        self.rename_buf = self.title.clone().unwrap_or_default();
    }

    /// Rows of the chronological view.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    fn visible_cols(&self, fixed_mode: bool) -> Vec<Col> {
        let mut v: Vec<Col> = Col::ALL
            .into_iter()
            .filter(|c| !self.hidden.contains(c) && (fixed_mode || !c.fixed_only()))
            .collect();
        if v.is_empty() {
            v.push(Col::Id);
        }
        v
    }

    fn reset(&mut self, seq: u64) {
        self.start_seq = seq;
        self.cursor = seq;
        self.fixed_cursor = seq;
        self.index.clear();
        self.fixed.clear();
        self.expanded.clear();
        self.expanded_fixed.clear();
    }

    /// Clear this window's view only: it shows nothing older than now. The
    /// shared store is untouched.
    pub fn clear(&mut self, store: &FrameStore) {
        self.reset(store.next_seq());
        self.paused_at = self.paused.then(|| store.next_seq());
    }

    /// Bring the index and fixed rows up to date with the store.
    pub fn update(&mut self, store: &FrameStore, names: &NameLookup, now: Instant) {
        if self.epoch != store.epoch() {
            self.epoch = store.epoch();
            self.reset(store.first_seq());
        }
        if self.filters != self.applied {
            self.applied = self.filters.clone();
            self.compiled = self.applied.compile();
            self.index.clear();
            self.cursor = self.start_seq;
        }
        if self.paused {
            self.paused_at.get_or_insert(store.next_seq());
        } else {
            self.paused_at = None;
        }
        let end = self.paused_at.unwrap_or(u64::MAX);

        let mut last = None;
        for (seq, ev) in store.iter_from(self.cursor.max(self.start_seq)) {
            if seq >= end {
                break;
            }
            if event_matches(&self.compiled, ev, names) {
                self.index.push_back(seq);
            }
            last = Some(seq);
        }
        if let Some(s) = last {
            self.cursor = s + 1;
        }
        let mut last = None;
        for (seq, ev) in store.iter_from(self.fixed_cursor.max(self.start_seq)) {
            if seq >= end {
                break;
            }
            apply_fixed(&mut self.fixed, seq, ev, now);
            last = Some(seq);
        }
        if let Some(s) = last {
            self.fixed_cursor = s + 1;
        }

        let first = store.first_seq();
        while self.index.front().is_some_and(|s| *s < first) {
            self.index.pop_front();
        }
        if !self.expanded.is_empty() {
            self.expanded.retain(|s| *s >= first);
        }
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        store: &FrameStore,
        names: &NameLookup,
        actions: &mut Vec<TraceAction>,
    ) {
        let fixed_mode = self.mode == TraceMode::Fixed;
        let dark = ui.visuals().dark_mode;
        ui.horizontal_wrapped(|ui| {
            if self.renaming {
                ui.label("Title:");
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
            ui.checkbox(&mut self.paused, "Pause");
            if icons::icon_button(ui, icons::clear(), "Clear this window's view").clicked() {
                self.clear(store);
            }
            let (icon, tip) = if fixed_mode {
                (
                    icons::trace_fixed(),
                    "Mode: fixed position (click for chronological)",
                )
            } else {
                (
                    icons::trace_chronological(),
                    "Mode: chronological (click for fixed position)",
                )
            };
            if icons::icon_button(ui, icon, tip).clicked() {
                self.mode = if fixed_mode {
                    TraceMode::Chronological
                } else {
                    TraceMode::Fixed
                };
            }
            ui.add_enabled(
                !fixed_mode,
                egui::Checkbox::new(&mut self.autoscroll, "Autoscroll"),
            );
            ui.add_enabled(
                fixed_mode,
                egui::Checkbox::new(&mut self.expand_all, "Expand signals"),
            )
            .on_hover_text("Fixed mode: show decoded DBC signals for every row");
            ui.separator();

            let n = self.filters.active_count();
            let on = self.filters.global;
            let label = if on {
                format!("Filters ON \u{b7} {n}")
            } else {
                "Filters OFF".to_string()
            };
            let mut btn = egui::Button::image_and_text(
                egui::Image::new(icons::filter())
                    .fit_to_exact_size(egui::vec2(14.0, 14.0))
                    .tint(ui.visuals().text_color()),
                label,
            );
            if on && n > 0 {
                btn = btn.fill(amber(dark));
            }
            if ui
                .add(btn)
                .on_hover_text("Turn all column filters on or off without clearing them")
                .clicked()
            {
                self.filters.global = !on;
            }
            if ui
                .add_enabled(!self.filters.is_clear(), egui::Button::new("Clear filters"))
                .clicked()
            {
                self.filters.clear();
            }
            ui.separator();

            ui.menu_button("Columns \u{23F7}", |ui| {
                for col in Col::ALL {
                    let mut shown = !self.hidden.contains(&col);
                    let text = if col.fixed_only() {
                        format!("{} (fixed mode)", col.label())
                    } else {
                        col.label().to_string()
                    };
                    if ui.checkbox(&mut shown, text).changed() {
                        if shown {
                            self.hidden.remove(&col);
                        } else {
                            self.hidden.insert(col);
                        }
                    }
                }
            });
            if ui
                .button("Export CSV")
                .on_hover_text("Save the rows passing the filters")
                .clicked()
            {
                self.export_csv(store, names, actions);
            }
            ui.menu_button("\u{2026}", |ui| {
                if ui.button("Rename window\u{2026}").clicked() {
                    self.begin_rename();
                    ui.close();
                }
            });
            ui.separator();
            if fixed_mode {
                ui.label(format!("{} ids", self.fixed.len()));
            } else {
                ui.label(format!("{} rows", self.len()));
            }
        });
    }

    fn export_csv(&self, store: &FrameStore, names: &NameLookup, actions: &mut Vec<TraceAction>) {
        let fixed_mode = self.mode == TraceMode::Fixed;
        let cols = self.visible_cols(fixed_mode);
        let csv = if fixed_mode {
            let items = fixed_items(&self.fixed, &self.compiled, names);
            to_csv(
                items
                    .into_iter()
                    .map(|(_, f)| (TraceRow::from_event(&f.ev, names), Some((f.count, f.dt_ms)))),
                &cols,
            )
        } else {
            to_csv(
                self.index
                    .iter()
                    .filter_map(|s| Some((TraceRow::from_event(store.get(*s)?, names), None))),
                &cols,
            )
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("trace.csv")
            .save_file()
        else {
            return;
        };
        actions.push(TraceAction::Log(match std::fs::write(&path, csv) {
            Ok(()) => format!("exported trace to {}", path.display()),
            Err(e) => format!("error: export {}: {e}", path.display()),
        }));
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        store: &FrameStore,
        names: &NameLookup,
    ) -> Vec<TraceAction> {
        let mut actions = Vec::new();
        self.update(store, names, Instant::now());
        self.toolbar(ui, store, names, &mut actions);
        ui.add_space(2.0);
        ui.separator();

        let rx_color = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(0x7f, 0xb4, 0xff)
        } else {
            egui::Color32::from_rgb(0x1f, 0x5f, 0xc0)
        };
        let fixed_mode = self.mode == TraceMode::Fixed;
        let cols = self.visible_cols(fixed_mode);
        let now = Instant::now();
        let text_height = ui.text_style_height(&egui::TextStyle::Body);
        let bus_names: Vec<String> = {
            let mut v: Vec<String> = names.bus_names.values().cloned().collect();
            v.sort();
            v
        };

        let items = if fixed_mode {
            fixed_items(&self.fixed, &self.compiled, names)
        } else {
            Vec::new()
        };
        if items.iter().any(|(_, f)| f.any_highlight(now)) {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        let n_frames = if fixed_mode {
            items.len()
        } else {
            self.index.len()
        };

        // Open signal lists. Decoding happens only for expanded rows.
        let mut exps: Vec<Expansion> = Vec::new();
        if fixed_mode {
            for (i, (k, f)) in items.iter().enumerate() {
                if (self.expand_all || self.expanded_fixed.contains(k))
                    && let Some(def) = message_def(names, &f.ev)
                {
                    exps.push(Expansion {
                        pos: i,
                        lines: dbcs::decode_lines(def, &f.ev.frame),
                    });
                }
            }
        } else {
            for seq in &self.expanded {
                if let (Ok(pos), Some(ev)) = (self.index.binary_search(seq), store.get(*seq))
                    && let Some(def) = message_def(names, ev)
                {
                    exps.push(Expansion {
                        pos,
                        lines: dbcs::decode_lines(def, &ev.frame),
                    });
                }
            }
            exps.sort_by_key(|e| e.pos);
        }
        let exp_pos: Vec<(usize, usize)> = exps.iter().map(|e| (e.pos, e.lines.len())).collect();
        let total_rows = n_frames + exp_pos.iter().map(|e| e.1).sum::<usize>();

        let filters = &mut self.filters;
        let mut toggled: Option<(Option<FixedKey>, u64)> = None;
        let mut ctx_action: Option<CtxAction> = None;
        let (expanded, expanded_fixed, expand_all) =
            (&self.expanded, &self.expanded_fixed, self.expand_all);
        let index = &self.index;
        let sig_col = if cols.contains(&Col::Name) {
            Col::Name
        } else {
            cols[0]
        };

        egui::ScrollArea::horizontal()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let mut table = TableBuilder::new(ui)
                    .id_salt(("trace_table", fixed_mode, cols.len()))
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                for col in &cols {
                    table = table.column(match col {
                        Col::Name => Column::remainder().at_least(col.width()),
                        c => Column::exact(c.width()),
                    });
                }
                if self.autoscroll && !fixed_mode {
                    table = table.scroll_to_row(total_rows, Some(egui::Align::BOTTOM));
                }

                table
                    .header(TITLE_H + FILTER_H + 6.0, |mut header| {
                        for col in &cols {
                            header.col(|ui| {
                                ui.vertical(|ui| {
                                    ui.strong(col.label());
                                    filter_cell(ui, *col, filters, &bus_names);
                                });
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(text_height, total_rows, |mut row| {
                            let frame_idx = match locate(row.index(), &exp_pos) {
                                Loc::Frame(i) => i,
                                Loc::Signal { exp, line } => {
                                    for col in &cols {
                                        row.col(|ui| {
                                            if *col == sig_col {
                                                ui.add_space(22.0);
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(&exps[exp].lines[line])
                                                            .monospace(),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                        });
                                    }
                                    return;
                                }
                            };
                            let (seq, ev, fixed, key) = if fixed_mode {
                                let Some((k, f)) = items.get(frame_idx) else {
                                    return;
                                };
                                (f.seq, f.ev, Some(*f), Some(**k))
                            } else {
                                let Some(seq) = index.get(frame_idx).copied() else {
                                    return;
                                };
                                let Some(ev) = store.get(seq) else { return };
                                (seq, *ev, None, None)
                            };
                            let r = TraceRow::from_event(&ev, names);
                            let decodable = message_def(names, &ev).is_some();
                            let open = match key {
                                Some(k) => expand_all || expanded_fixed.contains(&k),
                                None => expanded.contains(&seq),
                            };
                            let tint = (r.dir == Direction::Rx).then_some(rx_color);
                            for col in &cols {
                                row.col(|ui| {
                                    if let Some(c) = tint {
                                        ui.visuals_mut().override_text_color = Some(c);
                                    }
                                    match col {
                                        Col::Time => {
                                            ui.monospace(cell_text(*col, &r, None));
                                        }
                                        Col::Id => {
                                            ui.monospace(r.id_text());
                                        }
                                        Col::Count => {
                                            ui.monospace(fixed.map_or(0, |f| f.count).to_string());
                                        }
                                        Col::Dt => {
                                            match fixed.and_then(|f| f.dt_ms) {
                                                Some(dt) => ui.monospace(format!("{dt:.3}")),
                                                None => ui.monospace("-"),
                                            };
                                        }
                                        Col::Name => {
                                            if decodable {
                                                if arrow(ui, open).clicked() {
                                                    toggled = Some((key, seq));
                                                }
                                            } else {
                                                ui.add_space(14.0 + ui.spacing().item_spacing.x);
                                            }
                                            ui.label(&r.msg_name);
                                        }
                                        Col::Data => data_cell(ui, &r, fixed, now),
                                        c => {
                                            ui.label(cell_text(*c, &r, None));
                                        }
                                    }
                                });
                            }
                            row.response().context_menu(|ui| {
                                ui.set_min_width(190.0);
                                if ui.button("Filter on this ID").clicked() {
                                    ctx_action = Some(CtxAction::FilterId(r.id));
                                    ui.close();
                                }
                                if ui.button("Filter on this bus").clicked() {
                                    ctx_action = Some(CtxAction::FilterBus(r.bus_name.clone()));
                                    ui.close();
                                }
                                if ui.button("Copy row").clicked() {
                                    let text = cols
                                        .iter()
                                        .map(|c| {
                                            cell_text(*c, &r, fixed.map(|f| (f.count, f.dt_ms)))
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\t");
                                    ui.ctx().copy_text(text);
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Add signal to graph\u{2026}").clicked() {
                                    ctx_action = Some(CtxAction::Act(
                                        TraceAction::AddSignalToGraph(SignalRef::Raw {
                                            bus: r.bus,
                                            id: r.id,
                                            extended: r.extended,
                                            kind: RawKind::Byte(0),
                                        }),
                                    ));
                                    ui.close();
                                }
                                if ui.button("Copy as generator frame").clicked() {
                                    ctx_action = Some(CtxAction::Act(
                                        TraceAction::CopyAsGenerator(r.bus, r.frame()),
                                    ));
                                    ui.close();
                                }
                            });
                        });
                    });
            });

        match ctx_action {
            Some(CtxAction::FilterId(id)) => {
                self.filters.global = true;
                self.filters.id = TextFilter {
                    enabled: true,
                    text: format!("{id:X}"),
                };
            }
            Some(CtxAction::FilterBus(name)) => {
                self.filters.global = true;
                self.filters.bus.enabled = true;
                self.filters.bus.selected = BTreeSet::from([name]);
            }
            Some(CtxAction::Act(TraceAction::CopyAsGenerator(bus, frame))) => {
                if let Ok(json) = serde_json::to_string(&frame) {
                    ui.ctx().copy_text(json);
                }
                actions.push(TraceAction::CopyAsGenerator(bus, frame));
            }
            Some(CtxAction::Act(a)) => actions.push(a),
            None => {}
        }
        if self.filters != self.applied {
            ui.ctx().request_repaint();
        }

        if let Some((key, seq)) = toggled {
            match key {
                Some(k) => {
                    if self.expand_all {
                        // Leaving "expand all": keep the other rows open.
                        self.expand_all = false;
                        self.expanded_fixed.extend(self.fixed.keys().copied());
                    }
                    if !self.expanded_fixed.insert(k) {
                        self.expanded_fixed.remove(&k);
                    }
                }
                None => {
                    if !self.expanded.insert(seq) {
                        self.expanded.remove(&seq);
                    }
                }
            }
        }
        actions
    }
}

/// The expand/collapse triangle in front of a decodable message name.
fn arrow(ui: &mut egui::Ui, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
    let c = rect.center();
    let pts = if open {
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(4.0, -2.0),
            c + egui::vec2(0.0, 3.0),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -4.0),
            c + egui::vec2(-2.0, 4.0),
            c + egui::vec2(3.0, 0.0),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        pts,
        ui.visuals().text_color(),
        egui::Stroke::NONE,
    ));
    resp.on_hover_text("Show decoded signals")
}

/// A one-line text field for a filter expression: amber when active, red
/// when the expression is invalid. Typing enables the filter.
fn expr_field(
    ui: &mut egui::Ui,
    f: &mut TextFilter,
    width: f32,
    hint: &str,
    tip: &str,
    error: Option<String>,
) {
    let dark = ui.visuals().dark_mode;
    let active = f.enabled && !f.text.trim().is_empty() && error.is_none();
    let mut edit = egui::TextEdit::singleline(&mut f.text)
        .desired_width(width)
        .hint_text(hint)
        .font(egui::TextStyle::Small);
    if active {
        edit = edit.background_color(amber(dark));
    }
    if error.is_some() {
        edit = edit.text_color(egui::Color32::from_rgb(0xe0, 0x50, 0x50));
    }
    let resp = ui.add(edit);
    if resp.changed() {
        f.enabled = !f.text.trim().is_empty();
    }
    match error {
        Some(e) => resp.on_hover_text(format!("{e}\n\n{tip}")),
        None => resp.on_hover_text(tip),
    };
}

/// Tints menu buttons while a filter is active.
fn menu_tint(ui: &mut egui::Ui, active: bool) {
    if active {
        let c = amber(ui.visuals().dark_mode);
        ui.visuals_mut().widgets.inactive.weak_bg_fill = c;
    }
}

/// The filter widget under a column title (nothing for columns without one).
fn filter_cell(ui: &mut egui::Ui, col: Col, f: &mut TraceFilters, bus_names: &[String]) {
    if matches!(col, Col::Type | Col::Count | Col::Dt | Col::Len) {
        return;
    }
    if !f.global {
        ui.set_opacity(0.45);
    }
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.spacing_mut().interact_size.y = 18.0;
    ui.horizontal(|ui| {
        let (enabled, label): (&mut bool, &str) = match col {
            Col::Time => (&mut f.time.enabled, "Filter on time range"),
            Col::Chn => (&mut f.bus.enabled, "Filter on bus"),
            Col::Dir => (&mut f.dir.enabled, "Filter on direction"),
            Col::Id => (&mut f.id.enabled, "Filter on ID"),
            Col::Name => (&mut f.name.enabled, "Filter on message name"),
            Col::Sender => (&mut f.sender.enabled, "Filter on sender"),
            Col::Data => (&mut f.data.enabled, "Filter on data bytes"),
            Col::Dlc => (&mut f.dlc.enabled, "Filter on DLC"),
            Col::Hop => (&mut f.hop.enabled, "Filter on hop count"),
            _ => return,
        };
        ui.checkbox(enabled, "").on_hover_text(label);
        let w = (ui.available_width() - 2.0).max(20.0);
        match col {
            Col::Time => {
                let err = f.time_error();
                let dark = ui.visuals().dark_mode;
                let active = f.time.enabled && err.is_none() && (!f.time.from.trim().is_empty() || !f.time.to.trim().is_empty());
                let half = ((w - ui.spacing().item_spacing.x) / 2.0).max(16.0);
                let mut changed = false;
                for (text, hint) in [(&mut f.time.from, "from"), (&mut f.time.to, "to")] {
                    let mut edit = egui::TextEdit::singleline(text)
                        .desired_width(half)
                        .hint_text(hint)
                        .font(egui::TextStyle::Small);
                    if active {
                        edit = edit.background_color(amber(dark));
                    }
                    if err.is_some() {
                        edit = edit.text_color(egui::Color32::from_rgb(0xe0, 0x50, 0x50));
                    }
                    let r = ui.add(edit).on_hover_text(
                        "Seconds; leave blank for open-ended. Example: from 1.5, to 3",
                    );
                    changed |= r.changed();
                }
                if changed {
                    f.time.enabled = !f.time.from.trim().is_empty() || !f.time.to.trim().is_empty();
                }
            }
            Col::Chn => {
                let active = f.bus.enabled && !f.bus.selected.is_empty();
                let text = match f.bus.selected.len() {
                    0 => "All".to_string(),
                    1 => f.bus.selected.iter().next().cloned().unwrap_or_default(),
                    n => format!("{n} buses"),
                };
                ui.scope(|ui| {
                    menu_tint(ui, active);
                    ui.menu_button(egui::RichText::new(text).small(), |ui| {
                        if bus_names.is_empty() {
                            ui.weak("No buses");
                        }
                        for name in bus_names {
                            let mut on = f.bus.selected.contains(name);
                            if ui.checkbox(&mut on, name).changed() {
                                if on {
                                    f.bus.selected.insert(name.clone());
                                } else {
                                    f.bus.selected.remove(name);
                                }
                                f.bus.enabled = !f.bus.selected.is_empty();
                            }
                        }
                    });
                });
            }
            Col::Dir => {
                let active = f.dir.enabled && !(f.dir.tx && f.dir.rx);
                let text = match (f.dir.tx, f.dir.rx) {
                    (true, true) => "All",
                    (true, false) => "Tx",
                    (false, true) => "Rx",
                    (false, false) => "None",
                };
                ui.scope(|ui| {
                    menu_tint(ui, active);
                    ui.menu_button(egui::RichText::new(text).small(), |ui| {
                        let a = ui.checkbox(&mut f.dir.tx, "Tx").changed();
                        let b = ui.checkbox(&mut f.dir.rx, "Rx").changed();
                        if a || b {
                            f.dir.enabled = true;
                        }
                    });
                });
            }
            Col::Id => {
                let err = f.id_error();
                expr_field(
                    ui,
                    &mut f.id,
                    w,
                    "100-1FF,!7DF",
                    "Hex IDs: ranges, singles and ! to exclude.\nExample: 100-1FF, 3A0, !7DF",
                    err,
                );
            }
            Col::Name => expr_field(
                ui,
                &mut f.name,
                w,
                "contains\u{2026}",
                "Case-insensitive substring of the message name",
                None,
            ),
            Col::Sender => expr_field(
                ui,
                &mut f.sender,
                w,
                "contains\u{2026}",
                "Case-insensitive substring of the sender",
                None,
            ),
            Col::Data => {
                let err = f.data_error();
                expr_field(
                    ui,
                    &mut f.data,
                    w,
                    "xx 01 ?? FF",
                    "Hex byte pattern matched against the start of the payload.\nxx or ?? match any byte.",
                    err,
                );
            }
            Col::Dlc => {
                let err = f.dlc_error();
                expr_field(
                    ui,
                    &mut f.dlc,
                    w,
                    ">4",
                    "Compare the DLC: 8, >4, <=2, !=0",
                    err,
                );
            }
            Col::Hop => {
                let err = f.hop_error();
                expr_field(
                    ui,
                    &mut f.hop,
                    w,
                    ">0",
                    "Compare the gateway hop count: 0, >0, <=1",
                    err,
                );
            }
            _ => {}
        }
    });
}

/// DBC definition of the message in `ev`, via the DBC attached to its bus.
fn message_def<'a>(names: &'a NameLookup, ev: &BusEvent) -> Option<&'a operow_dbc::MessageDef> {
    names
        .dbcs
        .by_bus
        .get(&ev.bus)?
        .message(ev.frame.id, ev.frame.extended)
}

/// Data bytes, truncated to 8 with a hover for the full payload. Bytes that
/// recently changed (fixed mode) get a highlighted background.
fn data_cell(ui: &mut egui::Ui, r: &TraceRow, fixed: Option<&FixedRow>, now: Instant) {
    const TRUNCATE_AT: usize = 8;
    let len = r.dlc as usize;
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().text_color();
    let hl = if ui.visuals().dark_mode {
        egui::Color32::from_rgba_unmultiplied(0xff, 0xc0, 0x30, 90)
    } else {
        egui::Color32::from_rgba_unmultiplied(0xff, 0xb0, 0x00, 120)
    };
    let mut job = LayoutJob::default();
    for i in 0..len.min(TRUNCATE_AT) {
        let changed = fixed.is_some_and(|f| f.is_changed(i, now));
        let fmt = TextFormat {
            font_id: font.clone(),
            color,
            background: if changed {
                hl
            } else {
                egui::Color32::TRANSPARENT
            },
            ..Default::default()
        };
        job.append(&format!("{:02X}", r.data[i]), 0.0, fmt.clone());
        job.append(
            " ",
            0.0,
            TextFormat {
                background: egui::Color32::TRANSPARENT,
                ..fmt
            },
        );
    }
    if len > TRUNCATE_AT {
        job.append(
            "\u{2026}",
            0.0,
            TextFormat {
                font_id: font,
                color,
                ..Default::default()
            },
        );
    }
    ui.label(job).on_hover_text(r.data_hex());
}

/// Helper lookups shared by the trace and inspector: node/bus names, and
/// tx-message names by (sender, id).
#[derive(Default)]
pub struct NameLookup {
    pub node_names: HashMap<NodeId, String>,
    pub bus_names: HashMap<BusId, String>,
    pub msg_names: HashMap<(NodeId, u32), String>,
    /// Virtual sender ids of Generator windows; kept across `rebuild`.
    pub generator_names: HashMap<NodeId, String>,
    /// DBC databases per bus; their message names take precedence.
    pub dbcs: DbcStore,
}

impl NameLookup {
    pub fn rebuild(&mut self, topo: &operow_core::Topology) {
        self.node_names.clear();
        self.bus_names.clear();
        self.msg_names.clear();
        for n in &topo.nodes {
            self.node_names.insert(n.id, n.name.clone());
            for tx in &n.tx {
                self.msg_names.insert((n.id, tx.frame.id), tx.name.clone());
            }
        }
        for b in &topo.buses {
            self.bus_names.insert(b.id, b.name.clone());
        }
    }

    pub fn node_name(&self, id: NodeId) -> String {
        self.generator_names
            .get(&id)
            .or_else(|| self.node_names.get(&id))
            .cloned()
            .unwrap_or_else(|| format!("Node{}", id.0))
    }

    /// Like [`NameLookup::bus_name`], borrowing when the bus is known.
    pub fn bus_name_cow(&self, id: BusId) -> Cow<'_, str> {
        match self.bus_names.get(&id) {
            Some(n) => Cow::Borrowed(n),
            None => Cow::Owned(format!("Bus{}", id.0)),
        }
    }

    pub fn bus_name(&self, id: BusId) -> String {
        self.bus_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Bus{}", id.0))
    }

    /// The database attached to the bus called `name`.
    #[cfg(test)]
    pub fn dbc_for_bus_name(&self, name: &str) -> Option<&operow_dbc::Database> {
        let (bus, _) = self.bus_names.iter().find(|(_, n)| n.as_str() == name)?;
        self.dbcs.by_bus.get(bus).map(|d| &**d)
    }

    /// Message name: from the bus's DBC when it knows the id, else the
    /// sending node's own tx message name, else empty.
    pub fn msg_name(&self, bus: BusId, sender: NodeId, id: u32, extended: bool) -> String {
        if let Some(m) = self
            .dbcs
            .by_bus
            .get(&bus)
            .and_then(|d| d.message(id, extended))
        {
            return m.name.clone();
        }
        self.msg_names
            .get(&(sender, id))
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::Timestamp;

    fn ev(bus: u32, id: u32, dir: Direction, t_ms: u64, data: &[u8]) -> BusEvent {
        BusEvent {
            time: Timestamp(t_ms * 1_000_000),
            bus: BusId(bus),
            sender: NodeId(1),
            origin: NodeId(1),
            dir,
            frame_uid: 0,
            hop: u8::from(dir == Direction::Rx),
            frame: CanFrame::new(id, false, data).unwrap(),
        }
    }

    fn names() -> NameLookup {
        let mut n = NameLookup::default();
        n.bus_names.insert(BusId(1), "CAN1".into());
        n.bus_names.insert(BusId(2), "CAN2".into());
        n.node_names.insert(NodeId(1), "ECU".into());
        n
    }

    fn setup(events: &[BusEvent]) -> (FrameStore, NameLookup, Trace) {
        let mut store = FrameStore::new(1000);
        store.push_batch(events);
        (store, names(), Trace::default())
    }

    #[test]
    fn fixed_mode_aggregates_in_place() {
        let (store, n, mut t) = setup(&[
            ev(1, 0x100, Direction::Tx, 10, &[1, 2, 3]),
            ev(1, 0x100, Direction::Tx, 30, &[1, 9, 3]),
            ev(1, 0x100, Direction::Rx, 31, &[1, 9, 3]),
        ]);
        let now = Instant::now();
        t.update(&store, &n, now);
        assert_eq!(t.fixed.len(), 2, "dir is part of the key");
        assert_eq!(t.len(), 3, "chronological keeps every frame");
        let tx = t
            .fixed
            .values()
            .find(|f| f.ev.dir == Direction::Tx)
            .unwrap();
        assert_eq!(tx.count, 2);
        assert!((tx.dt_ms.unwrap() - 20.0).abs() < 1e-9);
        assert!(tx.changed_at[1].is_some());
        assert!(tx.changed_at[0].is_none() && tx.changed_at[2].is_none());
        assert!(tx.is_changed(1, now));
        assert!(!tx.is_changed(1, now + HIGHLIGHT * 2));
    }

    #[test]
    fn fixed_mode_sorted_by_bus_then_id() {
        let (store, n, mut t) = setup(&[
            ev(2, 0x001, Direction::Tx, 1, &[0]),
            ev(1, 0x200, Direction::Tx, 2, &[0]),
            ev(1, 0x100, Direction::Tx, 3, &[0]),
        ]);
        t.update(&store, &n, Instant::now());
        let order: Vec<_> = fixed_items(&t.fixed, &t.compiled, &n)
            .iter()
            .map(|(k, _)| (n.bus_name(k.bus), k.id))
            .collect();
        assert_eq!(
            order,
            [
                ("CAN1".into(), 0x100),
                ("CAN1".into(), 0x200),
                ("CAN2".into(), 1)
            ]
        );
    }

    #[test]
    fn changed_mask_marks_differing_and_new_bytes() {
        let a = CanFrame::new(1, false, &[1, 2]).unwrap();
        let b = CanFrame::new(1, false, &[1, 3, 4]).unwrap();
        assert_eq!(changed_mask(&a, &b), 0b110);
        assert_eq!(changed_mask(&b, &b), 0);
    }

    #[test]
    fn traces_read_incrementally_through_their_cursor() {
        let (mut store, n, mut t) = setup(&[ev(1, 1, Direction::Tx, 0, &[1])]);
        t.update(&store, &n, Instant::now());
        assert_eq!(t.len(), 1);
        store.push_batch(&[
            ev(1, 2, Direction::Tx, 1, &[1]),
            ev(1, 3, Direction::Tx, 2, &[1]),
        ]);
        t.update(&store, &n, Instant::now());
        t.update(&store, &n, Instant::now());
        assert_eq!(t.index, [0, 1, 2]);
    }

    #[test]
    fn clear_only_affects_this_view_and_pause_freezes() {
        let (mut store, n, mut t) = setup(&[ev(1, 1, Direction::Tx, 0, &[1])]);
        let mut other = Trace::default();
        t.update(&store, &n, Instant::now());
        other.update(&store, &n, Instant::now());
        t.clear(&store);
        assert_eq!((t.len(), t.fixed.len()), (0, 0));
        assert_eq!(store.len(), 1, "shared store keeps its frames");
        assert_eq!(other.len(), 1);
        t.update(&store, &n, Instant::now());
        assert_eq!(t.len(), 0, "cleared frames do not come back");
        store.push_batch(&[ev(1, 2, Direction::Tx, 1, &[1])]);
        t.paused = true;
        t.update(&store, &n, Instant::now());
        store.push_batch(&[ev(1, 3, Direction::Tx, 2, &[1])]);
        t.update(&store, &n, Instant::now());
        assert_eq!(t.index, [1], "frames after the pause are not shown");
        t.paused = false;
        t.update(&store, &n, Instant::now());
        assert_eq!(t.index, [1, 2], "resuming catches up");
    }

    #[test]
    fn store_clear_resets_traces() {
        let (mut store, n, mut t) = setup(&[ev(1, 1, Direction::Tx, 0, &[1])]);
        t.update(&store, &n, Instant::now());
        store.clear();
        store.push_batch(&[ev(1, 2, Direction::Tx, 1, &[1])]);
        t.update(&store, &n, Instant::now());
        assert_eq!(t.index, [1]);
        assert_eq!(t.fixed.len(), 1);
    }

    #[test]
    fn dropped_frames_leave_the_index() {
        let mut store = FrameStore::new(3);
        let n = names();
        let mut t = Trace::default();
        for i in 0..5 {
            store.push_batch(&[ev(1, i, Direction::Tx, i as u64, &[1])]);
            t.update(&store, &n, Instant::now());
        }
        assert_eq!(t.index, [2, 3, 4]);
    }

    #[test]
    fn filters_rebuild_the_index() {
        let (store, n, mut t) = setup(&[
            ev(1, 0x1A0, Direction::Tx, 0, &[1]),
            ev(2, 0x1A0, Direction::Rx, 1, &[1]),
            ev(2, 0x300, Direction::Rx, 2, &[1]),
        ]);
        t.update(&store, &n, Instant::now());
        assert_eq!(t.len(), 3);
        t.filters.bus.enabled = true;
        t.filters.bus.selected.insert("CAN2".into());
        t.filters.id.enabled = true;
        t.filters.id.text = "100-1FF".into();
        t.update(&store, &n, Instant::now());
        assert_eq!(t.index, [1]);
        t.filters.global = false;
        t.update(&store, &n, Instant::now());
        assert_eq!(t.len(), 3, "global off filters nothing");
        t.filters.global = true;
        t.filters.id.text = "!1A0".into();
        t.update(&store, &n, Instant::now());
        assert_eq!(t.index, [2]);
        // Name and sender filters resolve names.
        t.filters.clear();
        t.filters.sender.enabled = true;
        t.filters.sender.text = "ec".into();
        t.update(&store, &n, Instant::now());
        assert_eq!(t.len(), 3);
        // Fixed rows are filtered at display time.
        t.filters.clear();
        t.filters.bus.enabled = true;
        t.filters.bus.selected.insert("CAN1".into());
        t.update(&store, &n, Instant::now());
        assert_eq!(fixed_items(&t.fixed, &t.compiled, &n).len(), 1);
    }

    #[test]
    fn locate_maps_display_rows_with_expansions() {
        // Frame 1 has 2 signal lines, frame 3 has 1.
        let exp = [(1, 2), (3, 1)];
        let rows: Vec<Loc> = (0..8).map(|r| locate(r, &exp)).collect();
        assert_eq!(
            rows,
            [
                Loc::Frame(0),
                Loc::Frame(1),
                Loc::Signal { exp: 0, line: 0 },
                Loc::Signal { exp: 0, line: 1 },
                Loc::Frame(2),
                Loc::Frame(3),
                Loc::Signal { exp: 1, line: 0 },
                Loc::Frame(4),
            ]
        );
        assert_eq!(locate(5, &[]), Loc::Frame(5));
    }

    #[test]
    fn csv_export_escapes_and_follows_columns() {
        let n = names();
        let mut r = TraceRow::from_event(&ev(1, 0x100, Direction::Tx, 1500, &[1, 0xAB]), &n);
        r.msg_name = "Say \"hi\", ok".into();
        let csv = to_csv(
            [(r, None)].into_iter(),
            &[Col::Time, Col::Chn, Col::Id, Col::Name, Col::Data],
        );
        assert_eq!(
            csv,
            "Time (s),Chn,ID,Name,Data\n1.500000,CAN1,100,\"Say \"\"hi\"\", ok\",01 AB\n"
        );
    }

    #[test]
    fn view_round_trips_through_json() {
        let mut t = Trace {
            title: Some("Body debug".into()),
            mode: TraceMode::Fixed,
            ..Default::default()
        };
        t.hidden.insert(Col::Len);
        t.filters.id.enabled = true;
        t.filters.id.text = "100-2FF".into();
        let json = serde_json::to_value(t.view()).unwrap();
        let mut back = Trace::default();
        back.apply_view(serde_json::from_value(json).unwrap());
        assert_eq!(back.title.as_deref(), Some("Body debug"));
        assert!(back.mode == TraceMode::Fixed);
        assert!(back.hidden.contains(&Col::Len));
        assert_eq!(back.filters, t.filters);
        assert!(!back.visible_cols(true).contains(&Col::Len));
        assert!(!back.visible_cols(false).contains(&Col::Count));
    }

    #[test]
    fn million_frames_filter_quickly() {
        const N: u64 = 1_000_000;
        let mut store = FrameStore::new(N as usize);
        let base = ev(1, 0, Direction::Tx, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let batch: Vec<BusEvent> = (0..N)
            .map(|i| {
                let mut e = base;
                e.time = Timestamp(i * 1000);
                e.frame.id = (i % 0x400) as u32;
                e
            })
            .collect();
        for chunk in batch.chunks(256) {
            store.push_batch(chunk);
        }
        assert_eq!(store.len(), N as usize);
        let n = names();
        let mut t = Trace::default();
        t.filters.id.enabled = true;
        t.filters.id.text = "100-1FF, !150".into();
        t.filters.data.enabled = true;
        t.filters.data.text = "01 xx 03".into();
        let started = Instant::now();
        t.update(&store, &n, started);
        let took = started.elapsed();
        assert!(t.len() > 200_000 && t.len() < 300_000, "{}", t.len());
        assert!(took < Duration::from_secs(2), "indexing took {took:?}");
    }
}
