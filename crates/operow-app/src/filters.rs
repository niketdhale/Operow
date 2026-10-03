//! Per-column trace filters: serializable settings, pure parsers for the
//! filter expressions and a compiled matcher.

use std::collections::BTreeSet;

use operow_core::Direction;
use serde::{Deserialize, Serialize};

/// A filter that is a single text expression.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextFilter {
    pub enabled: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeFilter {
    pub enabled: bool,
    /// Seconds; blank is open-ended.
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BusFilter {
    pub enabled: bool,
    /// Bus names to show; an empty selection filters nothing.
    pub selected: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DirFilter {
    pub enabled: bool,
    pub tx: bool,
    pub rx: bool,
}

impl Default for DirFilter {
    fn default() -> Self {
        DirFilter {
            enabled: false,
            tx: true,
            rx: true,
        }
    }
}

/// The filters of one trace window: one per column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TraceFilters {
    /// Master switch; when off nothing is filtered but settings are kept.
    pub global: bool,
    pub time: TimeFilter,
    pub bus: BusFilter,
    pub dir: DirFilter,
    pub id: TextFilter,
    pub name: TextFilter,
    pub sender: TextFilter,
    pub data: TextFilter,
    pub dlc: TextFilter,
    pub hop: TextFilter,
}

impl Default for TraceFilters {
    fn default() -> Self {
        TraceFilters {
            global: true,
            time: Default::default(),
            bus: Default::default(),
            dir: Default::default(),
            id: Default::default(),
            name: Default::default(),
            sender: Default::default(),
            data: Default::default(),
            dlc: Default::default(),
            hop: Default::default(),
        }
    }
}

// ---------------------------------------------------------------- parsers

pub use operow_core::IdExpr;

/// Payload prefix pattern: `None` entries are wildcards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataPattern(Vec<Option<u8>>);

impl DataPattern {
    /// Parse `"xx 01 ?? FF"`; `xx`, `XX`, `?` and `??` are wildcards.
    pub fn parse(s: &str) -> Result<DataPattern, String> {
        let mut v = Vec::new();
        for tok in s.split_whitespace() {
            if tok.eq_ignore_ascii_case("xx") || tok == "?" || tok == "??" {
                v.push(None);
            } else if tok.len() <= 2 {
                v.push(Some(
                    u8::from_str_radix(tok, 16).map_err(|_| format!("bad byte \"{tok}\""))?,
                ));
            } else {
                return Err(format!("bad byte \"{tok}\""));
            }
        }
        Ok(DataPattern(v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the pattern matches the start of `payload`; a payload
    /// shorter than the pattern never matches.
    pub fn matches(&self, payload: &[u8]) -> bool {
        payload.len() >= self.0.len()
            && self
                .0
                .iter()
                .zip(payload)
                .all(|(p, b)| p.is_none_or(|p| p == *b))
    }
}

/// A numeric comparison such as `8`, `>4` or `<=2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cmp {
    op: CmpOp,
    value: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cmp {
    /// `None` for blank input.
    pub fn parse(s: &str) -> Result<Option<Cmp>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let ops = [
            ("<=", CmpOp::Le),
            (">=", CmpOp::Ge),
            ("!=", CmpOp::Ne),
            ("==", CmpOp::Eq),
            ("<", CmpOp::Lt),
            (">", CmpOp::Gt),
            ("=", CmpOp::Eq),
        ];
        let (op, rest) = ops
            .iter()
            .find_map(|(p, op)| s.strip_prefix(p).map(|r| (*op, r)))
            .unwrap_or((CmpOp::Eq, s));
        let rest = rest.trim();
        let value = match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            Some(h) => u32::from_str_radix(h, 16),
            None => rest.parse(),
        }
        .map_err(|_| format!("bad number \"{rest}\""))?;
        Ok(Some(Cmp { op, value }))
    }

    pub fn matches(&self, v: u32) -> bool {
        match self.op {
            CmpOp::Eq => v == self.value,
            CmpOp::Ne => v != self.value,
            CmpOp::Lt => v < self.value,
            CmpOp::Le => v <= self.value,
            CmpOp::Gt => v > self.value,
            CmpOp::Ge => v >= self.value,
        }
    }
}

/// Open-ended time window in seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TimeRange {
    pub from: Option<f64>,
    pub to: Option<f64>,
}

impl TimeRange {
    fn parse_one(s: &str) -> Result<Option<f64>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        s.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| format!("bad time \"{s}\""))
    }

    pub fn parse(from: &str, to: &str) -> Result<TimeRange, String> {
        Ok(TimeRange {
            from: Self::parse_one(from)?,
            to: Self::parse_one(to)?,
        })
    }

    pub fn is_open(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    pub fn matches(&self, t: f64) -> bool {
        self.from.is_none_or(|f| t >= f) && self.to.is_none_or(|e| t <= e)
    }
}

// ---------------------------------------------------------------- matcher

/// The fields of one frame that filters look at.
pub struct RowFields<'a> {
    pub time_s: f64,
    pub bus: &'a str,
    pub dir: Direction,
    pub id: u32,
    /// Message name; only needed when [`CompiledFilters::needs_name`].
    pub name: &'a str,
    /// Sender name; only needed when [`CompiledFilters::needs_sender`].
    pub sender: &'a str,
    pub data: &'a [u8],
    /// The 4-bit DLC code.
    pub dlc: u8,
    pub hop: u8,
}

/// Parsed, ready-to-run form of [`TraceFilters`]. Filters that are
/// disabled, blank, invalid or switched off globally are `None`.
#[derive(Debug, Clone, Default)]
pub struct CompiledFilters {
    time: Option<TimeRange>,
    bus: Option<BTreeSet<String>>,
    dir: Option<(bool, bool)>,
    id: Option<IdExpr>,
    name: Option<String>,
    sender: Option<String>,
    data: Option<DataPattern>,
    dlc: Option<Cmp>,
    hop: Option<Cmp>,
}

fn nonblank(f: &TextFilter) -> Option<&str> {
    let t = f.text.trim();
    (f.enabled && !t.is_empty()).then_some(t)
}

impl TraceFilters {
    fn compile_inner(&self) -> CompiledFilters {
        CompiledFilters {
            time: self
                .time
                .enabled
                .then(|| TimeRange::parse(&self.time.from, &self.time.to).ok())
                .flatten()
                .filter(|r| !r.is_open()),
            bus: (self.bus.enabled && !self.bus.selected.is_empty())
                .then(|| self.bus.selected.clone()),
            dir: (self.dir.enabled && !(self.dir.tx && self.dir.rx))
                .then_some((self.dir.tx, self.dir.rx)),
            id: nonblank(&self.id)
                .and_then(|t| IdExpr::parse(t).ok())
                .filter(|e| !e.is_empty()),
            name: nonblank(&self.name).map(str::to_lowercase),
            sender: nonblank(&self.sender).map(str::to_lowercase),
            data: nonblank(&self.data)
                .and_then(|t| DataPattern::parse(t).ok())
                .filter(|p| !p.is_empty()),
            dlc: nonblank(&self.dlc).and_then(|t| Cmp::parse(t).ok().flatten()),
            hop: nonblank(&self.hop).and_then(|t| Cmp::parse(t).ok().flatten()),
        }
    }

    /// The matcher; accepts everything while `global` is off.
    pub fn compile(&self) -> CompiledFilters {
        if self.global {
            self.compile_inner()
        } else {
            CompiledFilters::default()
        }
    }

    /// Number of filters that would take effect (ignoring `global`).
    pub fn active_count(&self) -> usize {
        self.compile_inner().active_count()
    }

    /// Whether every filter is at its default (the global switch aside).
    pub fn is_clear(&self) -> bool {
        *self
            == TraceFilters {
                global: self.global,
                ..Default::default()
            }
    }

    /// Reset every filter, keeping the global switch.
    pub fn clear(&mut self) {
        *self = TraceFilters {
            global: self.global,
            ..Default::default()
        };
    }

    /// Parse errors of the expression filters, for highlighting.
    pub fn time_error(&self) -> Option<String> {
        TimeRange::parse(&self.time.from, &self.time.to).err()
    }

    pub fn id_error(&self) -> Option<String> {
        IdExpr::parse(&self.id.text).err()
    }

    pub fn data_error(&self) -> Option<String> {
        DataPattern::parse(&self.data.text).err()
    }

    pub fn dlc_error(&self) -> Option<String> {
        Cmp::parse(&self.dlc.text).err()
    }

    pub fn hop_error(&self) -> Option<String> {
        Cmp::parse(&self.hop.text).err()
    }
}

impl CompiledFilters {
    pub fn active_count(&self) -> usize {
        [
            self.time.is_some(),
            self.bus.is_some(),
            self.dir.is_some(),
            self.id.is_some(),
            self.name.is_some(),
            self.sender.is_some(),
            self.data.is_some(),
            self.dlc.is_some(),
            self.hop.is_some(),
        ]
        .into_iter()
        .filter(|b| *b)
        .count()
    }

    pub fn needs_name(&self) -> bool {
        self.name.is_some()
    }

    pub fn needs_sender(&self) -> bool {
        self.sender.is_some()
    }

    pub fn matches(&self, r: &RowFields) -> bool {
        if self.time.is_some_and(|t| !t.matches(r.time_s)) {
            return false;
        }
        if self.bus.as_ref().is_some_and(|b| !b.contains(r.bus)) {
            return false;
        }
        if let Some((tx, rx)) = self.dir {
            let ok = match r.dir {
                Direction::Tx => tx,
                Direction::Rx => rx,
            };
            if !ok {
                return false;
            }
        }
        if self.id.as_ref().is_some_and(|e| !e.matches(r.id)) {
            return false;
        }
        if self
            .name
            .as_ref()
            .is_some_and(|n| !r.name.to_lowercase().contains(n))
        {
            return false;
        }
        if self
            .sender
            .as_ref()
            .is_some_and(|n| !r.sender.to_lowercase().contains(n))
        {
            return false;
        }
        if self.data.as_ref().is_some_and(|p| !p.matches(r.data)) {
            return false;
        }
        if self.dlc.is_some_and(|c| !c.matches(r.dlc as u32)) {
            return false;
        }
        if self.hop.is_some_and(|c| !c.matches(r.hop as u32)) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>() -> RowFields<'a> {
        RowFields {
            time_s: 1.5,
            bus: "Body",
            dir: Direction::Tx,
            id: 0x1A0,
            name: "EngineData",
            sender: "Gateway",
            data: &[0x01, 0xFF, 0x10],
            dlc: 3,
            hop: 0,
        }
    }

    #[test]
    fn id_expr_ranges_singles_excludes() {
        let e = IdExpr::parse("100-1FF, 3A0, !1A0").unwrap();
        assert!(e.matches(0x100) && e.matches(0x1FF) && e.matches(0x3A0));
        assert!(!e.matches(0x1A0), "excluded inside range");
        assert!(!e.matches(0x200) && !e.matches(0xFF));
        // Only excludes: everything else passes.
        let e = IdExpr::parse("!7DF").unwrap();
        assert!(e.matches(0x100) && !e.matches(0x7DF));
        // Reversed range, 0x prefix, whitespace separators.
        let e = IdExpr::parse("0x1FF-100 5").unwrap();
        assert!(e.matches(0x150) && e.matches(5) && !e.matches(6));
        assert!(IdExpr::parse("  ").unwrap().is_empty());
    }

    #[test]
    fn id_expr_invalid_input() {
        for bad in ["zz", "100-", "-5", "1-2-3", "!", "20000000", "10,,x"] {
            assert!(IdExpr::parse(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn data_patterns() {
        let p = DataPattern::parse("xx FF ??").unwrap();
        assert!(p.matches(&[1, 0xFF, 9, 4]));
        assert!(!p.matches(&[1, 0xFE, 9]));
        assert!(!p.matches(&[1, 0xFF]), "payload shorter than pattern");
        let p = DataPattern::parse("01").unwrap();
        assert!(p.matches(&[1, 2, 3]) && !p.matches(&[2, 1]));
        assert!(DataPattern::parse("").unwrap().is_empty());
        assert!(DataPattern::parse("0G").is_err());
        assert!(DataPattern::parse("123").is_err());
    }

    #[test]
    fn comparisons() {
        let c = |s: &str| Cmp::parse(s).unwrap().unwrap();
        assert!(c("8").matches(8) && !c("8").matches(7));
        assert!(c(">4").matches(5) && !c(">4").matches(4));
        assert!(c("<=2").matches(2) && !c("<=2").matches(3));
        assert!(c(">= 3").matches(3));
        assert!(c("<3").matches(2) && !c("<3").matches(3));
        assert!(c("!=1").matches(0) && !c("!=1").matches(1));
        assert!(c("==0").matches(0) && c("=0").matches(0));
        assert!(Cmp::parse("  ").unwrap().is_none());
        assert!(Cmp::parse(">x").is_err());
        assert!(Cmp::parse("1.5").is_err());
    }

    #[test]
    fn time_ranges() {
        let r = TimeRange::parse("1", "2").unwrap();
        assert!(r.matches(1.0) && r.matches(2.0) && !r.matches(0.9) && !r.matches(2.1));
        let r = TimeRange::parse("", "2").unwrap();
        assert!(r.matches(-5.0) && !r.matches(3.0));
        let r = TimeRange::parse("1.5", "").unwrap();
        assert!(r.matches(100.0) && !r.matches(1.0));
        assert!(TimeRange::parse("", "").unwrap().is_open());
        assert!(TimeRange::parse("a", "").is_err());
        assert!(TimeRange::parse("", "inf").is_err());
    }

    #[test]
    fn default_filters_match_everything() {
        let f = TraceFilters::default();
        assert_eq!(f.active_count(), 0);
        assert!(f.compile().matches(&row()));
    }

    #[test]
    fn each_column_filter() {
        let mut f = TraceFilters::default();
        f.bus.enabled = true;
        f.bus.selected.insert("Body".into());
        f.id.enabled = true;
        f.id.text = "100-1FF".into();
        f.name.enabled = true;
        f.name.text = "engine".into();
        f.sender.enabled = true;
        f.sender.text = "GATE".into();
        f.data.enabled = true;
        f.data.text = "01 xx".into();
        f.dlc.enabled = true;
        f.dlc.text = ">=3".into();
        f.hop.enabled = true;
        f.hop.text = "0".into();
        f.time.enabled = true;
        f.time.from = "1".into();
        f.dir.enabled = true;
        f.dir.rx = false;
        assert_eq!(f.active_count(), 9);
        let c = f.compile();
        assert!(c.matches(&row()));
        assert!(c.needs_name() && c.needs_sender());
        let base = row();
        let fails = [
            RowFields {
                bus: "Powertrain",
                ..row()
            },
            RowFields { id: 0x200, ..row() },
            RowFields {
                name: "Other",
                ..row()
            },
            RowFields {
                sender: "Engine",
                ..row()
            },
            RowFields {
                data: &[2, 0, 0],
                ..row()
            },
            RowFields { dlc: 2, ..row() },
            RowFields { hop: 1, ..row() },
            RowFields {
                time_s: 0.5,
                ..row()
            },
            RowFields {
                dir: Direction::Rx,
                ..row()
            },
        ];
        for (i, r) in fails.iter().enumerate() {
            assert!(!c.matches(r), "case {i} should be rejected");
        }
        assert!(c.matches(&base));
    }

    #[test]
    fn disabled_blank_and_invalid_filters_are_inactive() {
        let mut f = TraceFilters::default();
        f.id.text = "200".into(); // text but not enabled
        assert_eq!(f.active_count(), 0);
        f.id.enabled = true;
        assert_eq!(f.active_count(), 1);
        f.id.text = "garbage".into();
        assert_eq!(f.active_count(), 0, "invalid expression filters nothing");
        assert!(f.id_error().is_some());
        assert!(f.compile().matches(&row()));
        f.bus.enabled = true;
        assert_eq!(f.active_count(), 0, "empty bus selection filters nothing");
        f.dir.enabled = true;
        assert_eq!(f.active_count(), 0, "both directions selected");
    }

    #[test]
    fn global_toggle_keeps_settings_but_filters_nothing() {
        let mut f = TraceFilters::default();
        f.id.enabled = true;
        f.id.text = "200".into();
        assert!(!f.compile().matches(&row()));
        f.global = false;
        assert!(f.compile().matches(&row()));
        assert_eq!(f.active_count(), 1, "settings are kept");
        f.global = true;
        assert!(!f.compile().matches(&row()));
        assert!(!f.is_clear());
        f.clear();
        assert_eq!(f, TraceFilters::default());
        assert!(f.is_clear());
    }

    #[test]
    fn filters_serde_round_trip_and_partial_json() {
        let mut f = TraceFilters::default();
        f.bus.selected.insert("A".into());
        f.id.text = "1-2".into();
        let back: TraceFilters = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        assert_eq!(back, f);
        let partial: TraceFilters = serde_json::from_str(r#"{"global": false}"#).unwrap();
        assert!(!partial.global && partial.dir.tx);
    }
}
