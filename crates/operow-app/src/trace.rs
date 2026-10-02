//! The bottom trace panel: a virtualized log of bus events, either
//! chronological (ring-buffered) or fixed-position (one row per frame key).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui_extras::{Column, TableBuilder};
use operow_core::{BusEvent, BusId, Direction, NodeId};

use crate::dbcs::{self, DbcStore};
use crate::icons;

const MAX_ROWS: usize = 100_000;
/// How long a changed data byte stays highlighted in fixed-position mode.
const HIGHLIGHT: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct TraceRow {
    /// Monotonic push counter; identifies a row for signal expansion.
    pub seq: u64,
    pub time_s: f64,
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
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DirFilter {
    #[default]
    All,
    Tx,
    Rx,
}

impl DirFilter {
    fn label(self) -> &'static str {
        match self {
            DirFilter::All => "All",
            DirFilter::Tx => "Tx",
            DirFilter::Rx => "Rx",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum TraceMode {
    #[default]
    Chronological,
    Fixed,
}

/// Row filters: free text (hex ID or name), direction and bus.
#[derive(Default)]
pub struct TraceFilter {
    pub text: String,
    pub dir: DirFilter,
    /// `None` shows every bus.
    pub bus: Option<String>,
}

impl TraceFilter {
    pub fn matches(&self, row: &TraceRow) -> bool {
        match self.dir {
            DirFilter::All => {}
            DirFilter::Tx if row.dir != Direction::Tx => return false,
            DirFilter::Rx if row.dir != Direction::Rx => return false,
            _ => {}
        }
        if self.bus.as_ref().is_some_and(|b| *b != row.bus_name) {
            return false;
        }
        let text = self.text.trim();
        if text.is_empty() {
            return true;
        }
        let hex = text.trim_start_matches("0x").trim_start_matches("0X");
        if let Ok(want) = u32::from_str_radix(hex, 16) {
            row.id == want
        } else {
            let needle = text.to_lowercase();
            row.msg_name.to_lowercase().contains(&needle)
                || row.sender_name.to_lowercase().contains(&needle)
        }
    }
}

/// Identity of a row in fixed-position mode. Field order gives the display
/// order: bus, then ID.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct FixedKey {
    bus_name: String,
    id: u32,
    extended: bool,
    /// `true` for Rx (so Tx sorts first).
    rx: bool,
}

/// One fixed-position row: the latest frame for a key plus aggregates.
pub struct FixedRow {
    pub row: TraceRow,
    pub count: u64,
    /// Milliseconds since the previous frame with the same key.
    pub dt_ms: Option<f64>,
    /// When each data byte last changed value.
    changed_at: [Option<Instant>; 64],
}

/// Bitmask (bit i = byte i) of data bytes that differ between two frames.
/// Bytes beyond the previous length count as changed.
pub fn changed_mask(prev: &TraceRow, new: &TraceRow) -> u64 {
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
        (0..self.row.dlc as usize).any(|i| self.is_changed(i, now))
    }
}

pub struct Trace {
    rows: VecDeque<TraceRow>,
    fixed: BTreeMap<FixedKey, FixedRow>,
    buses: BTreeSet<String>,
    pub paused: bool,
    pub filter: TraceFilter,
    pub autoscroll: bool,
    pub mode: TraceMode,
    /// Chronological rows (by `seq`) whose signals are shown.
    expanded: HashSet<u64>,
    /// Fixed-mode keys whose signals are shown.
    expanded_fixed: HashSet<FixedKey>,
    /// Show the signals of every fixed-mode row.
    pub expand_all: bool,
    next_seq: u64,
}

impl Default for Trace {
    fn default() -> Self {
        Trace {
            rows: VecDeque::with_capacity(1024),
            fixed: BTreeMap::new(),
            buses: BTreeSet::new(),
            paused: false,
            filter: TraceFilter::default(),
            autoscroll: true,
            mode: TraceMode::default(),
            expanded: HashSet::new(),
            expanded_fixed: HashSet::new(),
            expand_all: false,
            next_seq: 0,
        }
    }
}

impl Trace {
    pub fn clear(&mut self) {
        self.rows.clear();
        self.fixed.clear();
        self.buses.clear();
        self.expanded.clear();
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Push one bus event, resolving names via lookup closures. No-op while
    /// paused. Feeds both display modes.
    pub fn push(
        &mut self,
        ev: &BusEvent,
        bus_name: &str,
        sender_name: &str,
        origin_name: &str,
        msg_name: &str,
    ) {
        self.push_at(
            ev,
            bus_name,
            sender_name,
            origin_name,
            msg_name,
            Instant::now(),
        );
    }

    fn push_at(
        &mut self,
        ev: &BusEvent,
        bus_name: &str,
        sender_name: &str,
        origin_name: &str,
        msg_name: &str,
        now: Instant,
    ) {
        if self.paused {
            return;
        }
        if !self.buses.contains(bus_name) {
            self.buses.insert(bus_name.to_string());
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let row = TraceRow {
            seq,
            time_s: ev.time.as_secs_f64(),
            bus_name: bus_name.to_string(),
            sender_name: sender_name.to_string(),
            origin_name: origin_name.to_string(),
            dir: ev.dir,
            hop: ev.hop,
            id: ev.frame.id,
            extended: ev.frame.extended,
            fd: ev.frame.fd,
            brs: ev.frame.brs,
            dlc: ev.frame.dlc,
            data: ev.frame.data,
            msg_name: msg_name.to_string(),
        };

        let key = FixedKey {
            bus_name: row.bus_name.clone(),
            id: row.id,
            extended: row.extended,
            rx: row.dir == Direction::Rx,
        };
        match self.fixed.get_mut(&key) {
            Some(f) => {
                let mask = changed_mask(&f.row, &row);
                for i in 0..row.dlc as usize {
                    if mask & (1 << i) != 0 {
                        f.changed_at[i] = Some(now);
                    }
                }
                f.dt_ms = Some((row.time_s - f.row.time_s) * 1000.0);
                f.count += 1;
                f.row = row.clone();
            }
            None => {
                self.fixed.insert(
                    key,
                    FixedRow {
                        row: row.clone(),
                        count: 1,
                        dt_ms: None,
                        changed_at: [None; 64],
                    },
                );
            }
        }

        if self.rows.len() >= MAX_ROWS
            && let Some(old) = self.rows.pop_front()
        {
            self.expanded.remove(&old.seq);
        }
        self.rows.push_back(row);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, names: &NameLookup) {
        let rx_color = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(0x7f, 0xb4, 0xff)
        } else {
            egui::Color32::from_rgb(0x1f, 0x5f, 0xc0)
        };
        let fixed_mode = self.mode == TraceMode::Fixed;

        ui.horizontal(|ui| {
            ui.heading("Trace");
            ui.separator();
            ui.checkbox(&mut self.paused, "Pause");
            if icons::icon_button(ui, icons::clear(), "Clear trace").clicked() {
                self.clear();
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
            ui.add(icons::icon_image(ui, icons::filter()));
            ui.text_edit_singleline(&mut self.filter.text)
                .on_hover_text("Filter by ID (hex) or name");
            egui::ComboBox::from_id_salt("trace_dir_filter")
                .selected_text(format!("Dir: {}", self.filter.dir.label()))
                .show_ui(ui, |ui| {
                    for d in [DirFilter::All, DirFilter::Tx, DirFilter::Rx] {
                        ui.selectable_value(&mut self.filter.dir, d, d.label());
                    }
                });
            egui::ComboBox::from_id_salt("trace_bus_filter")
                .selected_text(format!(
                    "Bus: {}",
                    self.filter.bus.as_deref().unwrap_or("All")
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter.bus, None, "All");
                    for b in &self.buses {
                        ui.selectable_value(&mut self.filter.bus, Some(b.clone()), b);
                    }
                });
            ui.separator();
            if fixed_mode {
                ui.label(format!("{} ids", self.fixed.len()));
            } else {
                ui.label(format!("{} rows", self.rows.len()));
            }
        });
        ui.separator();

        let now = Instant::now();
        let text_height = ui.text_style_height(&egui::TextStyle::Body);
        let filter = &self.filter;
        let views: Vec<RowView> = if fixed_mode {
            self.fixed
                .iter()
                .filter(|(_, f)| filter.matches(&f.row))
                .map(|(k, f)| RowView {
                    row: &f.row,
                    fixed: Some(f),
                    key: Some(k),
                })
                .collect()
        } else {
            self.rows
                .iter()
                .filter(|r| filter.matches(r))
                .map(|r| RowView {
                    row: r,
                    fixed: None,
                    key: None,
                })
                .collect()
        };
        if fixed_mode
            && views
                .iter()
                .any(|v| v.fixed.is_some_and(|f| f.any_highlight(now)))
        {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }

        // Flat list of table rows: frames plus, for expanded ones, one row
        // per decoded signal. Decoding happens only for expanded rows.
        let (expanded, expanded_fixed, expand_all) =
            (&self.expanded, &self.expanded_fixed, self.expand_all);
        let mut display: Vec<DisplayRow> = Vec::with_capacity(views.len());
        for (i, v) in views.iter().enumerate() {
            let msg_def = message_def(names, v.row);
            display.push(DisplayRow::Frame {
                view: i,
                decodable: msg_def.is_some(),
            });
            let open = match v.key {
                Some(k) => expand_all || expanded_fixed.contains(k),
                None => expanded.contains(&v.row.seq),
            };
            if let (true, Some(def)) = (open, msg_def) {
                let frame = row_frame(v.row);
                for line in dbcs::decode_lines(def, &frame) {
                    display.push(DisplayRow::Signal(line));
                }
            }
        }
        let mut toggled: Option<(Option<FixedKey>, u64)> = None;
        let n_cols = if fixed_mode { 13 } else { 11 };

        let mut table = TableBuilder::new(ui)
            .id_salt(("trace_table", fixed_mode))
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(90.0)) // time
            .column(Column::exact(60.0)) // chn
            .column(Column::exact(70.0)) // id
            .column(Column::remainder().at_least(100.0)) // name
            .column(Column::exact(40.0)) // dir
            .column(Column::exact(36.0)) // hop
            .column(Column::exact(80.0)); // type
        if fixed_mode {
            table = table
                .column(Column::exact(60.0)) // count
                .column(Column::exact(80.0)); // dt
        }
        let mut table = table
            .column(Column::exact(40.0)) // dlc
            .column(Column::exact(36.0)) // len
            .column(Column::exact(220.0)) // data
            .column(Column::exact(160.0)); // sender

        if self.autoscroll && !fixed_mode {
            table = table.scroll_to_row(display.len(), Some(egui::Align::BOTTOM));
        }

        table
            .header(20.0, |mut header| {
                let mut labels = vec!["Time (s)", "Chn", "ID", "Name", "Dir", "Hop", "Type"];
                if fixed_mode {
                    labels.extend(["Count", "Δt (ms)"]);
                }
                labels.extend(["DLC", "Len", "Data", "Sender"]);
                for label in labels {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|body| {
                body.rows(text_height, display.len(), |mut row| {
                    let (vi, decodable) = match &display[row.index()] {
                        DisplayRow::Frame { view, decodable } => (*view, *decodable),
                        DisplayRow::Signal(line) => {
                            for c in 0..n_cols {
                                row.col(|ui| {
                                    if c == 3 {
                                        ui.add_space(22.0);
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(line).monospace())
                                                .truncate(),
                                        );
                                    }
                                });
                            }
                            return;
                        }
                    };
                    let v = &views[vi];
                    let r = v.row;
                    let tint = (r.dir == Direction::Rx).then_some(rx_color);
                    let cell = |ui: &mut egui::Ui| {
                        if let Some(c) = tint {
                            ui.visuals_mut().override_text_color = Some(c);
                        }
                    };
                    row.col(|ui| {
                        cell(ui);
                        ui.monospace(format!("{:.6}", r.time_s));
                    });
                    row.col(|ui| {
                        cell(ui);
                        ui.label(&r.bus_name);
                    });
                    row.col(|ui| {
                        cell(ui);
                        ui.monospace(format!("{:03X}{}", r.id, if r.extended { "x" } else { "" }));
                    });
                    row.col(|ui| {
                        cell(ui);
                        if decodable {
                            let open = match v.key {
                                Some(k) => expand_all || expanded_fixed.contains(k),
                                None => expanded.contains(&r.seq),
                            };
                            let (rect, resp) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
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
                            if resp.on_hover_text("Show decoded signals").clicked() {
                                toggled = Some((v.key.cloned(), r.seq));
                            }
                        } else {
                            ui.add_space(14.0 + ui.spacing().item_spacing.x);
                        }
                        ui.label(&r.msg_name);
                    });
                    row.col(|ui| {
                        cell(ui);
                        ui.label(r.dir_label());
                    });
                    row.col(|ui| {
                        cell(ui);
                        ui.label(r.hop.to_string());
                    });
                    row.col(|ui| {
                        cell(ui);
                        ui.label(r.frame_type());
                    });
                    if fixed_mode {
                        row.col(|ui| {
                            cell(ui);
                            ui.monospace(v.fixed.map_or(0, |f| f.count).to_string());
                        });
                        row.col(|ui| {
                            cell(ui);
                            match v.fixed.and_then(|f| f.dt_ms) {
                                Some(dt) => ui.monospace(format!("{dt:.3}")),
                                None => ui.monospace("-"),
                            };
                        });
                    }
                    row.col(|ui| {
                        cell(ui);
                        ui.label(
                            operow_core::len_to_dlc(r.dlc as usize)
                                .unwrap_or(0)
                                .to_string(),
                        );
                    });
                    row.col(|ui| {
                        cell(ui);
                        ui.label(r.dlc.to_string());
                    });
                    row.col(|ui| {
                        cell(ui);
                        data_cell(ui, v, now);
                    });
                    row.col(|ui| {
                        cell(ui);
                        if r.origin_name != r.sender_name {
                            ui.label(format!("{} (from {})", r.sender_name, r.origin_name));
                        } else {
                            ui.label(&r.sender_name);
                        }
                    });
                });
            });

        if let Some((key, seq)) = toggled {
            match key {
                Some(k) => {
                    if self.expand_all {
                        // Leaving "expand all": keep the other rows open.
                        self.expand_all = false;
                        self.expanded_fixed.extend(self.fixed.keys().cloned());
                    }
                    if !self.expanded_fixed.insert(k.clone()) {
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
    }
}

/// One line of the table: a frame, or a decoded signal under it.
enum DisplayRow {
    Frame { view: usize, decodable: bool },
    Signal(String),
}

/// DBC definition of the message in `row`, via the DBC attached to its bus.
fn message_def<'a>(names: &'a NameLookup, row: &TraceRow) -> Option<&'a operow_dbc::MessageDef> {
    names
        .dbc_for_bus_name(&row.bus_name)?
        .message(row.id, row.extended)
}

fn row_frame(r: &TraceRow) -> operow_core::CanFrame {
    operow_core::CanFrame {
        id: r.id,
        extended: r.extended,
        fd: r.fd,
        brs: r.brs,
        dlc: r.dlc,
        data: r.data,
    }
}

/// A row to draw, with fixed-mode aggregates when applicable.
struct RowView<'a> {
    row: &'a TraceRow,
    fixed: Option<&'a FixedRow>,
    key: Option<&'a FixedKey>,
}

/// Data bytes, truncated to 8 with a hover for the full payload. Bytes that
/// recently changed (fixed mode) get a highlighted background.
fn data_cell(ui: &mut egui::Ui, v: &RowView, now: Instant) {
    const TRUNCATE_AT: usize = 8;
    let r = v.row;
    let len = r.dlc as usize;
    let full: String = r.data[..len]
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().text_color();
    let hl = if ui.visuals().dark_mode {
        egui::Color32::from_rgba_unmultiplied(0xff, 0xc0, 0x30, 90)
    } else {
        egui::Color32::from_rgba_unmultiplied(0xff, 0xb0, 0x00, 120)
    };
    let mut job = LayoutJob::default();
    for i in 0..len.min(TRUNCATE_AT) {
        let changed = v.fixed.is_some_and(|f| f.is_changed(i, now));
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
            "…",
            0.0,
            TextFormat {
                font_id: font,
                color,
                ..Default::default()
            },
        );
    }
    ui.label(job).on_hover_text(full);
}

/// Helper lookups shared by the trace and inspector: node/bus names, and
/// tx-message names by (sender, id).
#[derive(Default)]
pub struct NameLookup {
    pub node_names: HashMap<NodeId, String>,
    pub bus_names: HashMap<BusId, String>,
    pub msg_names: HashMap<(NodeId, u32), String>,
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
        self.node_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Node{}", id.0))
    }

    pub fn bus_name(&self, id: BusId) -> String {
        self.bus_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Bus{}", id.0))
    }

    /// The database attached to the bus called `name`.
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
    use operow_core::{CanFrame, Timestamp};

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

    fn push(t: &mut Trace, bus: &str, e: &BusEvent, now: Instant) {
        t.push_at(e, bus, "ECU", "ECU", "Msg", now);
    }

    #[test]
    fn fixed_mode_aggregates_in_place() {
        let mut t = Trace::default();
        let now = Instant::now();
        push(
            &mut t,
            "CAN1",
            &ev(1, 0x100, Direction::Tx, 10, &[1, 2, 3]),
            now,
        );
        push(
            &mut t,
            "CAN1",
            &ev(1, 0x100, Direction::Tx, 30, &[1, 9, 3]),
            now,
        );
        push(
            &mut t,
            "CAN1",
            &ev(1, 0x100, Direction::Rx, 31, &[1, 9, 3]),
            now,
        );
        assert_eq!(t.fixed.len(), 2, "dir is part of the key");
        assert_eq!(t.len(), 3, "chronological keeps every frame");
        let tx = t
            .fixed
            .values()
            .find(|f| f.row.dir == Direction::Tx)
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
        let mut t = Trace::default();
        let now = Instant::now();
        push(&mut t, "CAN2", &ev(2, 0x001, Direction::Tx, 1, &[0]), now);
        push(&mut t, "CAN1", &ev(1, 0x200, Direction::Tx, 2, &[0]), now);
        push(&mut t, "CAN1", &ev(1, 0x100, Direction::Tx, 3, &[0]), now);
        let order: Vec<_> = t
            .fixed
            .values()
            .map(|f| (f.row.bus_name.clone(), f.row.id))
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
        let now = Instant::now();
        let mut t = Trace::default();
        push(&mut t, "A", &ev(1, 1, Direction::Tx, 0, &[1, 2]), now);
        push(&mut t, "A", &ev(1, 1, Direction::Tx, 1, &[1, 3, 4]), now);
        let rows: Vec<_> = t.rows.iter().cloned().collect();
        assert_eq!(changed_mask(&rows[0], &rows[1]), 0b110);
        assert_eq!(changed_mask(&rows[1], &rows[1]), 0);
    }

    #[test]
    fn clear_and_pause() {
        let mut t = Trace::default();
        let now = Instant::now();
        t.paused = true;
        push(&mut t, "A", &ev(1, 1, Direction::Tx, 0, &[1]), now);
        assert_eq!((t.len(), t.fixed.len()), (0, 0));
        t.paused = false;
        push(&mut t, "A", &ev(1, 1, Direction::Tx, 0, &[1]), now);
        t.clear();
        assert_eq!((t.len(), t.fixed.len(), t.buses.len()), (0, 0, 0));
    }

    #[test]
    fn filter_matching() {
        let mut t = Trace::default();
        let now = Instant::now();
        push(&mut t, "CAN1", &ev(1, 0x1A0, Direction::Tx, 0, &[1]), now);
        push(&mut t, "CAN2", &ev(2, 0x1A0, Direction::Rx, 0, &[1]), now);
        let (a, b) = (t.rows[0].clone(), t.rows[1].clone());
        let mut f = TraceFilter::default();
        assert!(f.matches(&a) && f.matches(&b));
        f.dir = DirFilter::Rx;
        assert!(!f.matches(&a) && f.matches(&b));
        f.dir = DirFilter::All;
        f.bus = Some("CAN1".into());
        assert!(f.matches(&a) && !f.matches(&b));
        f.bus = None;
        f.text = "0x1a0".into();
        assert!(f.matches(&a));
        f.text = "msg".into();
        assert!(f.matches(&a));
        f.text = "nomatch".into();
        assert!(!f.matches(&a));
        f.text = "1A1".into();
        assert!(!f.matches(&a));
    }
}
