//! Graph windows: signals plotted against time, live or paused.
//!
//! Each window keeps its own sample cache (one series per plotted signal),
//! filled incrementally from the shared frame store. Drawing only passes the
//! visible slice of each series, decimated to about two points per pixel
//! column, to `egui_plot`.
//!
//! Two views are available. *Overlay* draws every signal in one plot; since
//! `egui_plot` has no real second Y axis, signals assigned to Y2 are
//! normalized into the Y1 range and the right-hand axis is labelled with a
//! formatter that maps tick positions back to Y2 values (its ticks sit on the
//! Y1 grid, so its labels are not round numbers). *Stacked* draws one plot per
//! signal with a linked time axis; there every signal has its own Y axis and
//! the Y1/Y2 assignment has no effect.

use std::collections::{BTreeMap, VecDeque};

use egui::{Align, Color32, Layout, RichText};
use egui_plot::{AxisHints, HPlacement, Line, Plot, PlotBounds, PlotPoints, Points, VLine};
use operow_core::{BusEvent, UserSignalDef};
use operow_dbc::SignalDef;
use serde::{Deserialize, Serialize};

use crate::dbcs::DbcStore;
use crate::signals::{RawKind, SignalRef};
use crate::store::FrameStore;
use crate::trace::NameLookup;
use crate::workspace::WindowId;

/// Most points kept per series.
const MAX_POINTS: usize = 400_000;
/// Store events scanned per series and frame, so backfilling a huge store
/// does not stall the UI.
const MAX_EVENTS_PER_UPDATE: u64 = 150_000;
/// Selectable live window lengths in seconds.
pub const WINDOW_LENGTHS: [u32; 5] = [1, 5, 10, 30, 60];

const PALETTE: [[u8; 3]; 8] = [
    [0x4e, 0x79, 0xa7],
    [0xf2, 0x8e, 0x2b],
    [0x59, 0xa1, 0x4f],
    [0xe1, 0x57, 0x59],
    [0xb0, 0x7a, 0xa1],
    [0x76, 0xb7, 0xb2],
    [0xed, 0xc9, 0x48],
    [0x9c, 0x75, 0x5f],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum YAxis {
    #[default]
    Y1,
    Y2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SeriesStyle {
    #[default]
    Line,
    Step,
    Points,
}

impl SeriesStyle {
    const ALL: [SeriesStyle; 3] = [SeriesStyle::Line, SeriesStyle::Step, SeriesStyle::Points];

    fn label(self) -> &'static str {
        match self {
            SeriesStyle::Line => "Line",
            SeriesStyle::Step => "Step",
            SeriesStyle::Points => "Points",
        }
    }
}

/// Follow the newest data, or let the user zoom and pan freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphMode {
    #[default]
    Live,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphLayout {
    #[default]
    Overlay,
    Stacked,
}

/// One plotted signal and how it is drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlotSignal {
    pub sig: SignalRef,
    pub color: [u8; 3],
    pub axis: YAxis,
    pub style: SeriesStyle,
    pub visible: bool,
}

impl PlotSignal {
    fn color32(&self) -> Color32 {
        Color32::from_rgb(self.color[0], self.color[1], self.color[2])
    }
}

/// Settings of a graph window that are saved in the project workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphView {
    pub title: Option<String>,
    pub signals: Vec<PlotSignal>,
    pub mode: GraphMode,
    /// Live window length in seconds.
    pub window_s: u32,
    pub layout: GraphLayout,
}

impl Default for GraphView {
    fn default() -> Self {
        GraphView {
            title: None,
            signals: Vec::new(),
            mode: GraphMode::Live,
            window_s: 10,
            layout: GraphLayout::Overlay,
        }
    }
}

/// Style a newly added signal gets: steps for enumerations and signals of at
/// most two bits, lines otherwise.
pub fn default_style(sig: &SignalRef, def: Option<&SignalDef>) -> SeriesStyle {
    match (sig, def) {
        (_, Some(d)) if !d.value_descriptions.is_empty() || d.size <= 2 => SeriesStyle::Step,
        (
            SignalRef::Raw {
                kind: RawKind::Dlc, ..
            },
            _,
        ) => SeriesStyle::Step,
        _ => SeriesStyle::Line,
    }
}

fn secs(ns: u64) -> f64 {
    ns as f64 / 1e9
}

/// The cached samples of one signal, as `[time in s, value]`, ascending in time.
#[derive(Default)]
struct Series {
    pts: VecDeque<[f64; 2]>,
    /// Next store seq to examine.
    cursor: u64,
    /// Previous frame of the signal's message, for rate and Δt kinds.
    prev: Option<BusEvent>,
}

impl Series {
    fn reset(&mut self) {
        self.pts.clear();
        self.prev = None;
    }

    /// Sample the events from the cursor on. Returns whether events are
    /// left over because the per-update budget ran out.
    fn advance(
        &mut self,
        sig: &SignalRef,
        store: &FrameStore,
        dbcs: &DbcStore,
        users: &[UserSignalDef],
        budget: u64,
    ) -> bool {
        let timing = match sig {
            SignalRef::Raw {
                bus,
                id,
                extended,
                kind: RawKind::Rate | RawKind::DeltaT,
            } => Some((*bus, *id, *extended)),
            _ => None,
        };
        let start = self.cursor.max(store.first_seq());
        let mut next = start;
        let mut n = 0;
        for (seq, ev) in store.iter_from(start) {
            if n >= budget {
                break;
            }
            n += 1;
            next = seq + 1;
            let prev = if timing.is_some() {
                self.prev.as_ref()
            } else {
                None
            };
            if let Some(v) = sig.sample_with_prev(ev, prev, dbcs, users)
                && v.is_finite()
            {
                // The store is in arrival order; keep time monotonic.
                let t = secs(ev.time.0).max(self.pts.back().map_or(f64::MIN, |p| p[0]));
                self.pts.push_back([t, v]);
            }
            if let Some((bus, id, ext)) = timing
                && !ev.is_error()
                && ev.bus == bus
                && ev
                    .frame
                    .as_can()
                    .is_some_and(|f| f.id == id && f.extended == ext)
            {
                self.prev = Some(ev.clone());
            }
        }
        if n < budget {
            self.cursor = store.next_seq();
            false
        } else {
            self.cursor = next;
            next < store.next_seq()
        }
    }
}

/// Drop points older than `t_min` and the oldest ones beyond `max_points`.
fn trim_series(pts: &mut VecDeque<[f64; 2]>, t_min: Option<f64>, max_points: usize) {
    if let Some(t) = t_min {
        let n = pts.partition_point(|p| p[0] < t);
        pts.drain(..n);
    }
    if pts.len() > max_points {
        let extra = pts.len() - max_points;
        pts.drain(..extra);
    }
}

/// The value of the newest sample at or before `t`.
fn value_at(pts: &VecDeque<[f64; 2]>, t: f64) -> Option<f64> {
    let i = pts.partition_point(|p| p[0] <= t);
    i.checked_sub(1).map(|i| pts[i][1])
}

/// The points in `x0..=x1` (plus one neighbour on each side so lines reach
/// the edges), reduced to at most two per bucket: the minimum and maximum of
/// each of `cols` equal-width buckets, in time order.
fn decimate(pts: &VecDeque<[f64; 2]>, x0: f64, x1: f64, cols: usize) -> Vec<[f64; 2]> {
    let lo = pts.partition_point(|p| p[0] < x0).saturating_sub(1);
    let hi = (pts.partition_point(|p| p[0] <= x1) + 1).min(pts.len());
    if lo >= hi {
        return Vec::new();
    }
    let cols = cols.max(1);
    let span = x1 - x0;
    if hi - lo <= cols * 2 || span <= 0.0 {
        return pts.range(lo..hi).copied().collect();
    }
    let mut out = Vec::with_capacity(cols * 2 + 2);
    // (bucket, index of min, index of max) of the bucket being scanned.
    let mut cur: Option<(usize, usize, usize)> = None;
    let flush = |out: &mut Vec<[f64; 2]>, lo_i: usize, hi_i: usize| {
        let (a, b) = if lo_i <= hi_i {
            (lo_i, hi_i)
        } else {
            (hi_i, lo_i)
        };
        out.push(pts[a]);
        if a != b {
            out.push(pts[b]);
        }
    };
    for i in lo..hi {
        let b = ((pts[i][0] - x0) / span * cols as f64).floor();
        let b = b.clamp(0.0, (cols - 1) as f64) as usize;
        match &mut cur {
            Some((cb, mn, mx)) if *cb == b => {
                if pts[i][1] < pts[*mn][1] {
                    *mn = i;
                }
                if pts[i][1] > pts[*mx][1] {
                    *mx = i;
                }
            }
            _ => {
                if let Some((_, mn, mx)) = cur {
                    flush(&mut out, mn, mx);
                }
                cur = Some((b, i, i));
            }
        }
    }
    if let Some((_, mn, mx)) = cur {
        flush(&mut out, mn, mx);
    }
    out
}

/// Turn samples into a staircase holding each value until the next sample
/// (and the last one until `hold_until`).
fn to_step(pts: &[[f64; 2]], hold_until: f64) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(pts.len() * 2 + 1);
    let mut prev: Option<[f64; 2]> = None;
    for p in pts {
        if let Some(q) = prev {
            out.push([p[0], q[1]]);
        }
        out.push(*p);
        prev = Some(*p);
    }
    if let Some(q) = prev
        && hold_until > q[0]
    {
        out.push([hold_until, q[1]]);
    }
    out
}

fn y_range(pts: &[[f64; 2]]) -> Option<(f64, f64)> {
    pts.iter().fold(None, |acc, p| match acc {
        None => Some((p[1], p[1])),
        Some((lo, hi)) => Some((lo.min(p[1]), hi.max(p[1]))),
    })
}

fn union(a: Option<(f64, f64)>, b: (f64, f64)) -> Option<(f64, f64)> {
    Some(match a {
        None => b,
        Some((lo, hi)) => (lo.min(b.0), hi.max(b.1)),
    })
}

/// A range with some air around it (and a minimum height).
fn padded((lo, hi): (f64, f64)) -> (f64, f64) {
    let span = hi - lo;
    if span < 1e-9 {
        (lo - 0.5, hi + 0.5)
    } else {
        (lo - 0.05 * span, hi + 0.05 * span)
    }
}

fn fmt_value(v: f64) -> String {
    if v != 0.0 && (v.abs() >= 1e7 || v.abs() < 1e-3) {
        return format!("{v:.3e}");
    }
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn fmt_tick(v: f64, step: f64) -> String {
    let decimals = if step > 0.0 && step.is_finite() {
        (-step.log10().floor()).clamp(0.0, 8.0) as usize
    } else {
        2
    };
    format!("{v:.decimals$}")
}

fn fmt_dt(dt: f64) -> String {
    if dt.abs() < 1.0 {
        format!("{:.1} ms", dt * 1000.0)
    } else {
        format!("{dt:.3} s")
    }
}

/// Plot-ready points of one signal and their value range.
type Prepared = (Vec<[f64; 2]>, (f64, f64));

/// A plot-ready series: points in plot coordinates (after step expansion and
/// Y2 normalization).
struct Drawn {
    idx: usize,
    pts: Vec<[f64; 2]>,
}

/// What the plot area computed for one frame.
struct PlotOut {
    clicked: Option<f64>,
    shift: bool,
    double_clicked: bool,
    bounds: PlotBounds,
}

pub struct GraphWindow {
    pub title: Option<String>,
    pub mode: GraphMode,
    pub window_s: u32,
    pub layout: GraphLayout,
    signals: Vec<PlotSignal>,
    /// One per entry of `signals`.
    series: Vec<Series>,
    epoch: u64,
    /// Time of the newest event in the store, in seconds.
    latest_t: f64,
    cursor: Option<f64>,
    cursor2: Option<f64>,
    /// The title is being edited.
    pub renaming: bool,
    rename_buf: String,
    /// Time range shown by the plot last frame (used while paused).
    view_x: Option<(f64, f64)>,
    /// Fit the view to all data on the next frame (paused mode).
    fit_pending: bool,
    max_points: usize,
    /// Display names of `signals`, refreshed every frame.
    labels: Vec<String>,
}

impl Default for GraphWindow {
    fn default() -> Self {
        let v = GraphView::default();
        GraphWindow {
            title: v.title,
            mode: v.mode,
            window_s: v.window_s,
            layout: v.layout,
            signals: Vec::new(),
            series: Vec::new(),
            epoch: 0,
            latest_t: 0.0,
            cursor: None,
            cursor2: None,
            renaming: false,
            rename_buf: String::new(),
            view_x: None,
            fit_pending: true,
            max_points: MAX_POINTS,
            labels: Vec::new(),
        }
    }
}

impl GraphWindow {
    pub fn view(&self) -> GraphView {
        GraphView {
            title: self.title.clone(),
            signals: self.signals.clone(),
            mode: self.mode,
            window_s: self.window_s,
            layout: self.layout,
        }
    }

    /// Apply saved settings; the sample cache is rebuilt from the store.
    pub fn apply_view(&mut self, v: GraphView) {
        self.title = v.title;
        self.mode = v.mode;
        self.window_s = v.window_s.max(1);
        self.layout = v.layout;
        self.series = v.signals.iter().map(|_| Series::default()).collect();
        self.signals = v.signals;
        self.fit_pending = true;
    }

    /// Start editing the window title.
    pub fn begin_rename(&mut self) {
        self.renaming = true;
        self.rename_buf = self.title.clone().unwrap_or_default();
    }

    fn next_color(&self) -> [u8; 3] {
        PALETTE
            .iter()
            .find(|c| !self.signals.iter().any(|s| s.color == **c))
            .copied()
            .unwrap_or(PALETTE[self.signals.len() % PALETTE.len()])
    }

    /// Plot `sig` (ignored when already plotted). Its history is backfilled
    /// from the store on the next update.
    pub fn add_signal(&mut self, sig: SignalRef, dbcs: &DbcStore, users: &[UserSignalDef]) {
        if self.signals.iter().any(|s| s.sig == sig) {
            return;
        }
        let style = default_style(&sig, sig.signal_def(dbcs, users).as_ref());
        self.signals.push(PlotSignal {
            color: self.next_color(),
            sig,
            axis: YAxis::Y1,
            style,
            visible: true,
        });
        self.series.push(Series::default());
        self.fit_pending = true;
    }

    pub fn set_axis(&mut self, i: usize, axis: YAxis) {
        if let Some(ps) = self.signals.get_mut(i) {
            ps.axis = axis;
        }
    }

    pub fn remove_signal(&mut self, i: usize) {
        if i < self.signals.len() {
            self.signals.remove(i);
            self.series.remove(i);
        }
    }

    /// Bring the sample cache up to date with the store. Returns whether
    /// more work is pending (call again next frame).
    pub fn update(&mut self, store: &FrameStore, dbcs: &DbcStore, users: &[UserSignalDef]) -> bool {
        if store.epoch() != self.epoch {
            // The store was cleared (a new run): start over.
            self.epoch = store.epoch();
            for s in &mut self.series {
                s.reset();
            }
            self.cursor = None;
            self.cursor2 = None;
            self.fit_pending = true;
        }
        let mut more = false;
        for (ps, s) in self.signals.iter().zip(&mut self.series) {
            more |= s.advance(&ps.sig, store, dbcs, users, MAX_EVENTS_PER_UPDATE);
        }
        // Forget what the store no longer holds.
        let oldest = store.get(store.first_seq()).map(|e| secs(e.time.0));
        for s in &mut self.series {
            trim_series(&mut s.pts, oldest, self.max_points);
        }
        self.latest_t = store
            .next_seq()
            .checked_sub(1)
            .and_then(|q| store.get(q))
            .map_or(0.0, |e| secs(e.time.0));
        more
    }

    /// Time range of the live view.
    fn live_range(&self) -> (f64, f64) {
        let w = self.window_s.max(1) as f64;
        let x1 = self.latest_t.max(w);
        (x1 - w, x1)
    }

    /// Time range of all cached data.
    fn data_range(&self) -> Option<(f64, f64)> {
        self.series
            .iter()
            .zip(&self.signals)
            .filter(|(_, ps)| ps.visible)
            .filter_map(|(s, _)| Some((s.pts.front()?[0], s.pts.back()?[0])))
            .fold(None, union)
    }

    /// Decimated, step-expanded points of signal `idx` for `x0..=x1`, with
    /// their value range.
    fn prepare_one(&self, idx: usize, x0: f64, x1: f64, cols: usize) -> Option<Prepared> {
        let pts = decimate(&self.series[idx].pts, x0, x1, cols);
        let pts = if self.signals[idx].style == SeriesStyle::Step {
            to_step(&pts, self.latest_t.min(x1.max(x0)))
        } else {
            pts
        };
        let r = y_range(&pts)?;
        Some((pts, r))
    }

    // ---- UI ----

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        id: WindowId,
        store: &FrameStore,
        names: &NameLookup,
        users: &[UserSignalDef],
    ) {
        self.labels = self
            .signals
            .iter()
            .map(|ps| ps.sig.label(names, users))
            .collect();
        self.toolbar(ui);
        ui.add_space(2.0);
        ui.separator();

        let mut remove = None;
        let mut add = None;
        egui::SidePanel::right(egui::Id::new(("graph_side", id)))
            .resizable(true)
            .default_width(300.0)
            .width_range(230.0..=520.0)
            .show_inside(ui, |ui| {
                add = self.side_panel(ui, store, names, users, &mut remove);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                let (_, dropped) = ui.dnd_drop_zone::<SignalRef, _>(egui::Frame::NONE, |ui| {
                    self.plot_area(ui, id);
                });
                if let Some(sig) = dropped {
                    add = Some((*sig).clone());
                }
            });
        if let Some(i) = remove {
            self.remove_signal(i);
        }
        if let Some(sig) = add {
            self.add_signal(sig, &names.dbcs, users);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
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
            ui.selectable_value(&mut self.mode, GraphMode::Live, "Live")
                .on_hover_text("Follow the newest data");
            if ui
                .selectable_value(&mut self.mode, GraphMode::Paused, "Paused")
                .on_hover_text("Zoom (ctrl+scroll or drag a box with right button) and pan (drag)")
                .clicked()
            {
                // Keep what is on screen.
                self.view_x = Some(self.live_range());
            }
            match self.mode {
                GraphMode::Live => {
                    egui::ComboBox::from_id_salt("graph_window_len")
                        .width(60.0)
                        .selected_text(format!("{} s", self.window_s))
                        .show_ui(ui, |ui| {
                            for w in WINDOW_LENGTHS {
                                ui.selectable_value(&mut self.window_s, w, format!("{w} s"));
                            }
                        })
                        .response
                        .on_hover_text("Window length");
                }
                GraphMode::Paused => {
                    if ui
                        .button("Fit")
                        .on_hover_text("Fit all data (or double-click the plot)")
                        .clicked()
                    {
                        self.fit_pending = true;
                    }
                }
            }
            ui.separator();
            let before = self.layout;
            ui.selectable_value(&mut self.layout, GraphLayout::Overlay, "Overlay");
            ui.selectable_value(&mut self.layout, GraphLayout::Stacked, "Stacked")
                .on_hover_text("One plot per signal with a shared time axis");
            if before != self.layout {
                self.fit_pending = true;
            }
            ui.separator();
            match (self.cursor, self.cursor2) {
                (Some(a), Some(b)) => {
                    ui.label(format!(
                        "t1 {a:.3} s   t2 {b:.3} s   \u{394}t {}",
                        fmt_dt(b - a)
                    ));
                }
                (Some(a), None) => {
                    ui.label(format!("t1 {a:.3} s"));
                }
                _ => {
                    ui.weak("Click to place a cursor, shift-click for a second");
                }
            }
            if (self.cursor.is_some() || self.cursor2.is_some())
                && ui.small_button("Clear cursors").clicked()
            {
                self.cursor = None;
                self.cursor2 = None;
            }
        });
    }

    fn side_panel(
        &mut self,
        ui: &mut egui::Ui,
        store: &FrameStore,
        names: &NameLookup,
        users: &[UserSignalDef],
        remove: &mut Option<usize>,
    ) -> Option<SignalRef> {
        let mut picked = None;
        ui.add_space(4.0);
        egui::containers::menu::MenuButton::new("+ Add signal")
            .config(
                egui::containers::menu::MenuConfig::new()
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
            )
            .ui(ui, |ui| {
                picked = picker_ui(ui, store, names, users);
                if picked.is_some() {
                    ui.close();
                }
            });
        ui.separator();
        if self.signals.is_empty() {
            ui.weak("No signals yet.");
            return picked;
        }
        let multi = self.layout == GraphLayout::Overlay;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for i in 0..self.signals.len() {
                    let name = self.signals[i].sig.label(names, users);
                    let unit = self.signals[i].sig.unit(&names.dbcs, users);
                    let pts = &self.series[i].pts;
                    let value = match (self.cursor, self.cursor2) {
                        (Some(a), Some(b)) => match (value_at(pts, a), value_at(pts, b)) {
                            (Some(x), Some(y)) => format!(
                                "{} \u{2192} {}  \u{394} {}",
                                fmt_value(x),
                                fmt_value(y),
                                fmt_value(y - x)
                            ),
                            _ => "\u{2014}".into(),
                        },
                        (Some(a), None) => value_at(pts, a).map_or("\u{2014}".into(), fmt_value),
                        _ => pts.back().map_or("\u{2014}".into(), |p| fmt_value(p[1])),
                    };
                    let ps = &mut self.signals[i];
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut ps.color)
                            .on_hover_text("Color");
                        ui.checkbox(&mut ps.visible, "").on_hover_text("Visible");
                        let w = (ui.available_width() - 26.0).max(30.0);
                        ui.scope(|ui| {
                            ui.set_max_width(w);
                            ui.add(egui::Label::new(RichText::new(&name).strong()).truncate())
                                .on_hover_text(&name);
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if egui_flow::icon_button(ui, egui_flow::Icon::Close, 9.0)
                                .on_hover_text("Remove")
                                .clicked()
                            {
                                *remove = Some(i);
                            }
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(ps.sig.source());
                        if !unit.is_empty() {
                            ui.weak(format!("[{unit}]"));
                        }
                        ui.label(RichText::new(value).monospace());
                    });
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(multi, |ui| {
                            egui::ComboBox::from_id_salt(("gaxis", i))
                                .width(48.0)
                                .selected_text(match ps.axis {
                                    YAxis::Y1 => "Y1",
                                    YAxis::Y2 => "Y2",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut ps.axis, YAxis::Y1, "Y1");
                                    ui.selectable_value(&mut ps.axis, YAxis::Y2, "Y2");
                                })
                                .response
                                .on_hover_text("Value axis (Overlay view)");
                        });
                        egui::ComboBox::from_id_salt(("gstyle", i))
                            .width(70.0)
                            .selected_text(ps.style.label())
                            .show_ui(ui, |ui| {
                                for s in SeriesStyle::ALL {
                                    ui.selectable_value(&mut ps.style, s, s.label());
                                }
                            })
                            .response
                            .on_hover_text("Drawing style");
                    });
                    ui.separator();
                }
            });
        picked
    }

    fn plot_area(&mut self, ui: &mut egui::Ui, id: WindowId) {
        if self.signals.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak("Add a signal with \u{201c}+ Add signal\u{201d} or drag one here from the project tree.");
            });
            return;
        }
        let live = self.mode == GraphMode::Live;
        let width = ui.available_width().clamp(100.0, 4000.0);
        let (x0, x1) = if live {
            self.live_range()
        } else {
            self.view_x
                .or_else(|| self.data_range())
                .unwrap_or((0.0, 1.0))
        };
        // Paused views are one frame behind while panning, so prepare data
        // for half a view on each side too.
        let (qx0, qx1, cols) = if live {
            (x0, x1, width as usize)
        } else {
            let m = (x1 - x0) / 2.0;
            (x0 - m, x1 + m, width as usize * 2)
        };
        let fit = !live && self.fit_pending;
        if fit {
            self.fit_pending = false;
        }
        let fit_x = if fit {
            self.data_range()
                .map(|(a, b)| if b > a { (a, b) } else { (a, a + 1.0) })
        } else {
            None
        };
        let out = match self.layout {
            GraphLayout::Overlay => self.overlay(ui, id, (qx0, qx1), cols, live, fit_x),
            GraphLayout::Stacked => self.stacked(ui, id, (qx0, qx1), cols, live, fit_x),
        };
        if out.double_clicked && !live {
            self.fit_pending = true;
        }
        if let Some(x) = out.clicked {
            if out.shift {
                self.cursor2 = Some(x);
            } else {
                self.cursor = Some(x);
            }
        }
        if !live {
            let r = out.bounds.range_x();
            self.view_x = Some((*r.start(), *r.end()));
        }
    }

    fn plot_flags<'a>(plot: Plot<'a>, live: bool) -> Plot<'a> {
        plot.allow_drag(!live)
            .allow_zoom(!live)
            .allow_scroll(!live)
            .allow_boxed_zoom(!live)
            .allow_double_click_reset(false)
            .show_grid(true)
            .x_axis_formatter(|m, _| format!("{:.2}", m.value))
    }

    fn overlay(
        &self,
        ui: &mut egui::Ui,
        id: WindowId,
        (qx0, qx1): (f64, f64),
        cols: usize,
        live: bool,
        fit_x: Option<(f64, f64)>,
    ) -> PlotOut {
        let mut items: Vec<(usize, Prepared)> = Vec::new();
        for (idx, ps) in self.signals.iter().enumerate() {
            if !ps.visible {
                continue;
            }
            if let Some((pts, r)) = self.prepare_one(idx, qx0, qx1, cols) {
                items.push((idx, (pts, r)));
            }
        }
        let r1 = items
            .iter()
            .filter(|(i, _)| self.signals[*i].axis == YAxis::Y1)
            .fold(None, |a, (_, (_, r))| union(a, *r));
        let r2 = items
            .iter()
            .filter(|(i, _)| self.signals[*i].axis == YAxis::Y2)
            .fold(None, |a, (_, (_, r))| union(a, *r));
        // Y2 is only separate when both axes have data; otherwise all
        // signals share one axis.
        let dual = match (r1, r2) {
            (Some(a), Some(b)) => Some((padded(a), padded(b))),
            _ => None,
        };
        let all = items.iter().fold(None, |a, (_, (_, r))| union(a, *r));
        let y_view = match dual {
            Some((a, _)) => Some(a),
            None => all.map(padded),
        };
        // Maps a Y2 value into the Y1 range and back.
        let norm = move |y: f64| match dual {
            Some(((l1, h1), (l2, h2))) => l1 + (y - l2) / (h2 - l2) * (h1 - l1),
            None => y,
        };
        let denorm = move |y: f64| match dual {
            Some(((l1, h1), (l2, h2))) => l2 + (y - l1) / (h1 - l1) * (h2 - l2),
            None => y,
        };
        let on_y2 = |idx: usize| dual.is_some() && self.signals[idx].axis == YAxis::Y2;
        let drawn: Vec<Drawn> = items
            .into_iter()
            .map(|(idx, (mut pts, _))| {
                if on_y2(idx) {
                    for p in &mut pts {
                        p[1] = norm(p[1]);
                    }
                }
                Drawn { idx, pts }
            })
            .collect();

        let mut plot = Self::plot_flags(
            Plot::new((id, "overlay"))
                .min_size(egui::vec2(60.0, 80.0))
                .y_axis_min_width(46.0)
                .label_formatter(|_, p| format!("t = {:.3} s", p.x)),
            live,
        )
        .link_cursor(egui::Id::new((id, "cursor")), [true, false]);
        if dual.is_some() {
            let right = AxisHints::new_y()
                .placement(HPlacement::Right)
                .min_thickness(46.0)
                .formatter(move |m, _| {
                    fmt_tick(
                        denorm(m.value),
                        m.step_size * (denorm(1.0) - denorm(0.0)).abs(),
                    )
                });
            plot = plot.custom_y_axes(vec![AxisHints::new_y().min_thickness(46.0), right]);
        }
        let (c1, c2) = (self.cursor, self.cursor2);
        let shift = ui.input(|i| i.modifiers.shift);
        let resp = plot.show(ui, |pui| {
            if live {
                pui.set_plot_bounds_x(qx0..=qx1);
            } else if let Some((a, b)) = fit_x {
                pui.set_plot_bounds_x(a..=b);
            }
            if (live || fit_x.is_some())
                && let Some((lo, hi)) = y_view
            {
                pui.set_plot_bounds_y(lo..=hi);
            }
            for d in &drawn {
                self.draw_series(pui, d);
                for t in [c1, c2].into_iter().flatten() {
                    if let Some(v) = value_at(&self.series[d.idx].pts, t) {
                        let v = if on_y2(d.idx) { norm(v) } else { v };
                        pui.points(
                            Points::new("", vec![[t, v]])
                                .color(self.signals[d.idx].color32())
                                .radius(4.0_f32),
                        );
                    }
                }
            }
            draw_cursors(pui, c1, c2);
            let r = pui.response();
            (
                r.clicked()
                    .then(|| pui.pointer_coordinate().map(|p| p.x))
                    .flatten(),
                r.double_clicked(),
            )
        });
        PlotOut {
            clicked: resp.inner.0,
            shift,
            double_clicked: resp.inner.1,
            bounds: *resp.transform.bounds(),
        }
    }

    fn stacked(
        &self,
        ui: &mut egui::Ui,
        id: WindowId,
        (qx0, qx1): (f64, f64),
        cols: usize,
        live: bool,
        fit_x: Option<(f64, f64)>,
    ) -> PlotOut {
        let visible: Vec<usize> = (0..self.signals.len())
            .filter(|i| self.signals[*i].visible)
            .collect();
        let mut out = PlotOut {
            clicked: None,
            shift: ui.input(|i| i.modifiers.shift),
            double_clicked: false,
            bounds: PlotBounds::from_min_max([qx0, 0.0], [qx1, 1.0]),
        };
        if visible.is_empty() {
            ui.weak("All signals are hidden.");
            return out;
        }
        let n = visible.len();
        let gap = ui.spacing().item_spacing.y;
        let h = ((ui.available_height() - gap * (n - 1) as f32) / n as f32).max(110.0);
        let (c1, c2) = (self.cursor, self.cursor2);
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (k, idx) in visible.iter().copied().enumerate() {
                    let ps = &self.signals[idx];
                    let prepared = self.prepare_one(idx, qx0, qx1, cols);
                    let plot = Self::plot_flags(
                        Plot::new((id, "stacked", idx))
                            .height(h)
                            .min_size(egui::vec2(60.0, 60.0))
                            .y_axis_min_width(60.0)
                            .show_axes([k + 1 == n, true])
                            .label_formatter(|_, p| {
                                format!("t = {:.3} s\n{}", p.x, fmt_value(p.y))
                            }),
                        live,
                    )
                    .link_axis(egui::Id::new((id, "xlink")), [true, false])
                    .link_cursor(egui::Id::new((id, "cursor")), [true, false]);
                    let resp = plot.show(ui, |pui| {
                        if live {
                            pui.set_plot_bounds_x(qx0..=qx1);
                        } else if let Some((a, b)) = fit_x {
                            pui.set_plot_bounds_x(a..=b);
                        }
                        if (live || fit_x.is_some())
                            && let Some((_, r)) = &prepared
                        {
                            let (lo, hi) = padded(*r);
                            pui.set_plot_bounds_y(lo..=hi);
                        }
                        if let Some((pts, _)) = prepared {
                            self.draw_series(pui, &Drawn { idx, pts });
                        }
                        for t in [c1, c2].into_iter().flatten() {
                            if let Some(v) = value_at(&self.series[idx].pts, t) {
                                pui.points(
                                    Points::new("", vec![[t, v]])
                                        .color(ps.color32())
                                        .radius(4.0_f32),
                                );
                            }
                        }
                        draw_cursors(pui, c1, c2);
                        let r = pui.response();
                        (
                            r.clicked()
                                .then(|| pui.pointer_coordinate().map(|p| p.x))
                                .flatten(),
                            r.double_clicked(),
                        )
                    });
                    ui.painter().text(
                        resp.transform.frame().left_top() + egui::vec2(8.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        self.labels.get(idx).map_or("", String::as_str),
                        egui::FontId::proportional(12.0),
                        ps.color32(),
                    );
                    out.clicked = out.clicked.or(resp.inner.0);
                    out.double_clicked |= resp.inner.1;
                    out.bounds = *resp.transform.bounds();
                }
            });
        out
    }

    fn draw_series(&self, pui: &mut egui_plot::PlotUi<'_>, d: &Drawn) {
        let ps = &self.signals[d.idx];
        let pts = PlotPoints::new(d.pts.clone());
        match ps.style {
            SeriesStyle::Line | SeriesStyle::Step => {
                pui.line(Line::new("", pts).color(ps.color32()).width(1.5_f32));
            }
            SeriesStyle::Points => {
                pui.points(Points::new("", pts).color(ps.color32()).radius(2.0_f32));
            }
        }
    }
}

fn draw_cursors(pui: &mut egui_plot::PlotUi<'_>, c1: Option<f64>, c2: Option<f64>) {
    if let Some(x) = c1 {
        pui.vline(
            VLine::new("", x)
                .color(Color32::from_rgb(0xe0, 0xa0, 0x30))
                .width(1.5_f32),
        );
    }
    if let Some(x) = c2 {
        pui.vline(
            VLine::new("", x)
                .color(Color32::from_rgb(0x30, 0xb8, 0xd0))
                .width(1.5_f32),
        );
    }
}

/// The "+ Add signal" menu: buses with their DBC messages and raw frames,
/// then user signals. Returns the signal clicked.
fn picker_ui(
    ui: &mut egui::Ui,
    store: &FrameStore,
    names: &NameLookup,
    users: &[UserSignalDef],
) -> Option<SignalRef> {
    use egui::CollapsingHeader;
    let mut picked = None;
    ui.set_min_width(280.0);
    egui::ScrollArea::vertical()
        .max_height(420.0)
        .show(ui, |ui| {
            let mut buses: Vec<_> = names.bus_names.iter().collect();
            buses.sort_by_key(|(b, _)| **b);
            for (bus, bus_name) in buses {
                let bus = *bus;
                CollapsingHeader::new(bus_name)
                    .icon(crate::icons::disclosure)
                    .id_salt(("gpick_bus", bus))
                    .show(ui, |ui| {
                        if let Some(db) = names.dbcs.by_bus.get(&bus) {
                            for m in &db.messages {
                                CollapsingHeader::new(format!("0x{:X}  {}", m.id, m.name))
                                    .icon(crate::icons::disclosure)
                                    .id_salt(("gpick_msg", bus, m.id, m.extended))
                                    .show(ui, |ui| {
                                        for s in &m.signals {
                                            let unit = if s.unit.is_empty() {
                                                String::new()
                                            } else {
                                                format!("  [{}]", s.unit)
                                            };
                                            if ui
                                                .selectable_label(
                                                    false,
                                                    format!("{}{unit}", s.name),
                                                )
                                                .clicked()
                                            {
                                                picked = Some(SignalRef::Dbc {
                                                    bus,
                                                    msg_id: m.id,
                                                    extended: m.extended,
                                                    signal_name: s.name.clone(),
                                                });
                                            }
                                        }
                                    });
                            }
                        }
                        CollapsingHeader::new("Raw frames")
                            .icon(crate::icons::disclosure)
                            .id_salt(("gpick_raw", bus))
                            .show(ui, |ui| {
                                let ids = raw_frames(store, names, bus);
                                if ids.is_empty() {
                                    ui.weak("No frames seen yet");
                                }
                                for ((id, extended), len) in ids {
                                    CollapsingHeader::new(format!(
                                        "0x{id:X}{}",
                                        if extended { "x" } else { "" }
                                    ))
                                    .icon(crate::icons::disclosure)
                                    .id_salt(("gpick_rawid", bus, id, extended))
                                    .show(ui, |ui| {
                                        let mut kinds: Vec<(String, RawKind)> = (0..len.max(1))
                                            .map(|n| (format!("Byte {n}"), RawKind::Byte(n as u8)))
                                            .collect();
                                        kinds.push(("DLC".into(), RawKind::Dlc));
                                        kinds.push(("Rate".into(), RawKind::Rate));
                                        kinds.push(("\u{394}t".into(), RawKind::DeltaT));
                                        ui.horizontal_wrapped(|ui| {
                                            for (label, kind) in kinds {
                                                if ui.button(label).clicked() {
                                                    picked = Some(SignalRef::Raw {
                                                        bus,
                                                        id,
                                                        extended,
                                                        kind,
                                                    });
                                                }
                                            }
                                        });
                                    });
                                }
                            });
                    });
            }
            CollapsingHeader::new("User signals")
                .icon(crate::icons::disclosure)
                .default_open(true)
                .show(ui, |ui| {
                    if users.is_empty() {
                        ui.weak("No user signals");
                    }
                    for u in users {
                        if ui.selectable_label(false, &u.name).clicked() {
                            picked = Some(SignalRef::User(u.id));
                        }
                    }
                });
        });
    picked
}

/// Frames of `bus` known from its DBC or seen recently in the store, with
/// the longest payload seen (or declared).
fn raw_frames(
    store: &FrameStore,
    names: &NameLookup,
    bus: operow_core::BusId,
) -> BTreeMap<(u32, bool), usize> {
    let mut ids: BTreeMap<(u32, bool), usize> = BTreeMap::new();
    if let Some(db) = names.dbcs.by_bus.get(&bus) {
        for m in &db.messages {
            ids.insert((m.id, m.extended), m.dlc as usize);
        }
    }
    let from = store.next_seq().saturating_sub(20_000);
    for (_, ev) in store.iter_from(from) {
        if ev.bus == bus
            && !ev.is_error()
            && let Some(f) = ev.frame.as_can()
        {
            let e = ids.entry((f.id, f.extended)).or_default();
            *e = (*e).max(f.payload().len());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::{BusId, CanFrame, Direction, NodeId, Timestamp};

    fn dq(v: &[[f64; 2]]) -> VecDeque<[f64; 2]> {
        v.iter().copied().collect()
    }

    fn event(t_ms: u64, b0: u8) -> BusEvent {
        BusEvent {
            time: Timestamp(t_ms * 1_000_000),
            bus: BusId(1),
            sender: NodeId(1),
            origin: NodeId(1),
            dir: Direction::Tx,
            frame_uid: 0,
            hop: 0,
            frame: CanFrame::new(0x100, false, &[b0, 0]).unwrap().into(),
            kind: Default::default(),
        }
    }

    fn byte0() -> SignalRef {
        SignalRef::Raw {
            bus: BusId(1),
            id: 0x100,
            extended: false,
            kind: RawKind::Byte(0),
        }
    }

    fn window_with(sig: SignalRef) -> GraphWindow {
        let mut w = GraphWindow::default();
        w.add_signal(sig, &DbcStore::default(), &[]);
        w
    }

    #[test]
    fn decimation_keeps_extremes() {
        // 20k samples of a slow wave with one spike and one dip.
        let mut pts = VecDeque::new();
        for i in 0..20_000 {
            let x = i as f64 / 1000.0;
            let mut y = (x * 0.7).sin();
            if i == 12_345 {
                y = 50.0;
            }
            if i == 3_333 {
                y = -40.0;
            }
            pts.push_back([x, y]);
        }
        let out = decimate(&pts, 0.0, 20.0, 200);
        assert!(out.len() <= 200 * 2 + 2, "{}", out.len());
        let max = out.iter().map(|p| p[1]).fold(f64::MIN, f64::max);
        let min = out.iter().map(|p| p[1]).fold(f64::MAX, f64::min);
        assert_eq!(max, 50.0);
        assert_eq!(min, -40.0);
        assert!(out.windows(2).all(|w| w[0][0] <= w[1][0]), "time order");
        // Each bucket keeps its own extremes, not just the global ones.
        let bucket_of = |x: f64| ((x / 20.0 * 200.0).floor() as usize).min(199);
        for k in 0..200 {
            let ext = |v: &mut dyn Iterator<Item = [f64; 2]>| {
                v.filter(|p| bucket_of(p[0]) == k)
                    .fold((f64::MAX, f64::MIN), |(lo, hi), p| {
                        (lo.min(p[1]), hi.max(p[1]))
                    })
            };
            assert_eq!(
                ext(&mut pts.iter().copied()),
                ext(&mut out.iter().copied()),
                "bucket {k}"
            );
        }
    }

    #[test]
    fn decimation_limits_to_visible_range() {
        let pts = dq(&(0..1000).map(|i| [i as f64, i as f64]).collect::<Vec<_>>());
        let out = decimate(&pts, 100.0, 110.0, 500);
        // The range plus one neighbour on each side, untouched.
        assert_eq!(out.first().unwrap()[0], 99.0);
        assert_eq!(out.last().unwrap()[0], 111.0);
        assert_eq!(out.len(), 13);
        assert!(decimate(&VecDeque::new(), 0.0, 1.0, 10).is_empty());
    }

    #[test]
    fn step_holds_values() {
        let s = to_step(&[[0.0, 1.0], [1.0, 3.0]], 2.0);
        assert_eq!(s, vec![[0.0, 1.0], [1.0, 1.0], [1.0, 3.0], [2.0, 3.0]]);
    }

    #[test]
    fn cursor_value_is_nearest_sample_before() {
        let pts = dq(&[[1.0, 10.0], [2.0, 20.0], [3.0, 30.0]]);
        assert_eq!(value_at(&pts, 0.5), None);
        assert_eq!(value_at(&pts, 1.0), Some(10.0));
        assert_eq!(value_at(&pts, 2.9), Some(20.0));
        assert_eq!(value_at(&pts, 99.0), Some(30.0));
    }

    #[test]
    fn backfills_from_whole_store_then_follows() {
        let mut store = FrameStore::new(1000);
        let batch: Vec<_> = (0..10).map(|i| event(i * 10, i as u8)).collect();
        store.push_batch(&batch);
        // Added after the data exists: backfill on the next update.
        let mut w = window_with(byte0());
        assert!(!w.update(&store, &DbcStore::default(), &[]));
        assert_eq!(w.series[0].pts.len(), 10);
        assert_eq!(w.series[0].pts[3], [0.03, 3.0]);
        // New events are appended once, not re-read.
        store.push_batch(&[event(100, 99)]);
        w.update(&store, &DbcStore::default(), &[]);
        w.update(&store, &DbcStore::default(), &[]);
        assert_eq!(w.series[0].pts.len(), 11);
        assert_eq!(w.series[0].pts.back(), Some(&[0.1, 99.0]));
        assert!((w.latest_t - 0.1).abs() < 1e-12);
    }

    #[test]
    fn series_trimmed_when_store_wraps() {
        let mut store = FrameStore::new(10);
        let mut w = window_with(byte0());
        let dbcs = DbcStore::default();
        for chunk in 0..6u64 {
            let batch: Vec<_> = (0..5).map(|i| event((chunk * 5 + i) * 10, 1)).collect();
            store.push_batch(&batch);
            w.update(&store, &dbcs, &[]);
        }
        // 30 events pushed, the store holds the last 10 (t = 200..290 ms).
        assert_eq!(store.len(), 10);
        assert_eq!(w.series[0].pts.len(), 10);
        assert!((w.series[0].pts[0][0] - 0.2).abs() < 1e-12);
        // The point cap trims the oldest first.
        let mut pts: VecDeque<_> = (0..10).map(|i| [i as f64, 0.0]).collect();
        trim_series(&mut pts, None, 4);
        assert_eq!(pts.front().unwrap()[0], 6.0);
    }

    #[test]
    fn series_reset_when_store_cleared() {
        let mut store = FrameStore::new(100);
        let dbcs = DbcStore::default();
        store.push_batch(&[event(0, 1), event(10, 2)]);
        let mut w = window_with(byte0());
        w.update(&store, &dbcs, &[]);
        w.cursor = Some(0.005);
        assert_eq!(w.series[0].pts.len(), 2);
        store.clear();
        w.update(&store, &dbcs, &[]);
        assert!(w.series[0].pts.is_empty());
        assert_eq!(w.cursor, None);
        // A new run starts again at t = 0.
        store.push_batch(&[event(0, 7)]);
        w.update(&store, &dbcs, &[]);
        assert_eq!(w.series[0].pts.len(), 1);
        assert_eq!(w.series[0].pts[0], [0.0, 7.0]);
    }

    #[test]
    fn rate_uses_previous_frame() {
        let mut store = FrameStore::new(100);
        store.push_batch(&[event(0, 0), event(10, 0), event(30, 0)]);
        let sig = SignalRef::Raw {
            bus: BusId(1),
            id: 0x100,
            extended: false,
            kind: RawKind::DeltaT,
        };
        let mut w = window_with(sig);
        w.update(&store, &DbcStore::default(), &[]);
        let ys: Vec<f64> = w.series[0].pts.iter().map(|p| p[1]).collect();
        assert_eq!(ys, vec![10.0, 20.0]);
    }

    #[test]
    fn default_style_choice() {
        let def = |size: u16, vals: bool| SignalDef {
            name: "S".into(),
            start_bit: 0,
            size,
            byte_order: operow_dbc::ByteOrder::Intel,
            value_type: operow_dbc::ValueType::Unsigned,
            factor: 1.0,
            offset: 0.0,
            min: 0.0,
            max: 0.0,
            unit: String::new(),
            receivers: Vec::new(),
            multiplexer: None,
            initial_raw: None,
            value_descriptions: if vals {
                vec![(0, "Off".into())]
            } else {
                Vec::new()
            },
            comment: None,
        };
        let sig = byte0();
        assert_eq!(
            default_style(&sig, Some(&def(16, false))),
            SeriesStyle::Line
        );
        assert_eq!(default_style(&sig, Some(&def(2, false))), SeriesStyle::Step);
        assert_eq!(default_style(&sig, Some(&def(1, false))), SeriesStyle::Step);
        assert_eq!(default_style(&sig, Some(&def(8, true))), SeriesStyle::Step);
        assert_eq!(default_style(&sig, None), SeriesStyle::Line);
        let dlc = SignalRef::Raw {
            bus: BusId(1),
            id: 1,
            extended: false,
            kind: RawKind::Dlc,
        };
        assert_eq!(default_style(&dlc, None), SeriesStyle::Step);
    }

    #[test]
    fn view_round_trips_through_json() {
        let mut w = window_with(byte0());
        w.title = Some("Engine".into());
        w.mode = GraphMode::Paused;
        w.window_s = 30;
        w.layout = GraphLayout::Stacked;
        w.signals[0].axis = YAxis::Y2;
        w.signals[0].style = SeriesStyle::Points;
        w.signals[0].color = [1, 2, 3];
        w.signals[0].visible = false;
        let json = serde_json::to_value(w.view()).unwrap();
        let back: GraphView = serde_json::from_value(json).unwrap();
        assert_eq!(back, w.view());
        let mut w2 = GraphWindow::default();
        w2.apply_view(back);
        assert_eq!(w2.view(), w.view());
        assert_eq!(w2.series.len(), 1);
        // Missing fields fall back to defaults.
        let d: GraphView = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(d, GraphView::default());
    }

    #[test]
    fn duplicate_signals_ignored_and_colors_differ() {
        let mut w = window_with(byte0());
        w.add_signal(byte0(), &DbcStore::default(), &[]);
        assert_eq!(w.signals.len(), 1);
        w.add_signal(
            SignalRef::Raw {
                bus: BusId(1),
                id: 0x100,
                extended: false,
                kind: RawKind::Dlc,
            },
            &DbcStore::default(),
            &[],
        );
        assert_ne!(w.signals[0].color, w.signals[1].color);
        w.remove_signal(0);
        assert_eq!((w.signals.len(), w.series.len()), (1, 1));
    }
}
