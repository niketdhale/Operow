//! DBC databases attached to buses: path handling, loading, and the
//! formatting used by the trace and inspector to show decoded signals.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use operow_core::{BusId, CanFrame, DbcRef, TxMessage};
use operow_dbc::{Database, MessageDef, SignalDef};

/// Parsed databases keyed by the bus they describe.
#[derive(Default, Clone)]
pub struct DbcStore {
    pub by_bus: HashMap<BusId, Arc<Database>>,
}

impl DbcStore {
    /// The DBC message for a transmit entry: looked up on its own bus, or
    /// on any of the node's `linked` buses when it sends on all of them.
    pub fn message_for_tx(
        &self,
        msg: &TxMessage,
        linked: &[(BusId, String)],
    ) -> Option<&MessageDef> {
        let (id, ext) = (msg.frame.id, msg.frame.extended);
        match msg.bus {
            Some(b) => self.by_bus.get(&b)?.message(id, ext),
            None => linked
                .iter()
                .find_map(|(b, _)| self.by_bus.get(b)?.message(id, ext)),
        }
    }
}

/// Load every referenced database. Relative paths resolve against
/// `project_dir`. Returns the store plus one error line per file that could
/// not be read or parsed.
pub fn load_all(refs: &[DbcRef], project_dir: Option<&Path>) -> (DbcStore, Vec<String>) {
    let mut store = DbcStore::default();
    let mut errors = Vec::new();
    for r in refs {
        let path = resolve_path(project_dir, &r.path);
        match load_file(&path) {
            Ok(db) => {
                store.by_bus.insert(r.bus, Arc::new(db));
            }
            Err(e) => errors.push(format!("error: DBC {}: {e}", path.display())),
        }
    }
    (store, errors)
}

pub fn load_file(path: &Path) -> Result<Database, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    Database::parse(&text).map_err(|e| e.to_string())
}

/// Resolve a stored DBC path: absolute paths are kept, relative ones are
/// joined onto the project directory (or the working directory).
pub fn resolve_path(project_dir: Option<&Path>, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    match project_dir {
        Some(dir) if p.is_relative() => dir.join(p),
        _ => p.to_path_buf(),
    }
}

/// The form to store in the project: relative to `project_dir` (using `/`
/// and `..` as needed) when known and both paths are absolute, otherwise
/// the path as given.
pub fn stored_path(dbc: &Path, project_dir: Option<&Path>) -> String {
    let rel = project_dir
        .filter(|d| d.is_absolute() && dbc.is_absolute())
        .map(|d| relative_to(dbc, d));
    match rel {
        Some(r) if r.is_relative() => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        _ => dbc.to_string_lossy().into_owned(),
    }
}

/// `target` expressed relative to the directory `base` (both absolute).
fn relative_to(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return target.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push(Component::ParentDir);
    }
    for c in &t[common..] {
        out.push(c);
    }
    out
}

/// Format a physical value: integers without decimals when the factor is
/// integral, otherwise up to 3 decimals with trailing zeros trimmed.
pub fn format_value(value: f64, factor: f64) -> String {
    if factor.fract() == 0.0 && value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let s = format!("{value:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// `Name = value unit (Description)`, with the description taken from
/// `VAL_` when the raw value matches.
pub fn signal_line(sig: &SignalDef, value: f64) -> String {
    let mut s = format!("{} = {}", sig.name, format_value(value, sig.factor));
    if !sig.unit.is_empty() {
        s.push(' ');
        s.push_str(&sig.unit);
    }
    if sig.factor != 0.0 {
        let raw = ((value - sig.offset) / sig.factor).round() as i64;
        if let Some((_, text)) = sig.value_descriptions.iter().find(|(v, _)| *v == raw) {
            s.push_str(&format!(" ({text})"));
        }
    }
    s
}

/// One line per decodable signal of `msg` in `frame` (mux aware).
pub fn decode_lines(msg: &MessageDef, frame: &CanFrame) -> Vec<String> {
    msg.decode(frame)
        .into_iter()
        .filter_map(|(name, v)| {
            let sig = msg.signals.iter().find(|s| s.name == name)?;
            Some(signal_line(sig, v))
        })
        .collect()
}

/// `start|size@order` as in DBC, e.g. `7|16@0`.
pub fn bit_layout(sig: &SignalDef) -> String {
    let order = match sig.byte_order {
        operow_dbc::ByteOrder::Intel => 1,
        operow_dbc::ByteOrder::Motorola => 0,
    };
    format!("{}|{}@{order}", sig.start_bit, sig.size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path for the current OS (drive-prefixed on Windows).
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    #[test]
    fn stored_path_relative_to_project() {
        let dir = abs("/home/u/proj");
        let dir = dir.as_path();
        assert_eq!(
            stored_path(&abs("/home/u/proj/sample.dbc"), Some(dir)),
            "sample.dbc"
        );
        assert_eq!(
            stored_path(&abs("/home/u/proj/db/a.dbc"), Some(dir)),
            "db/a.dbc"
        );
        assert_eq!(
            stored_path(&abs("/home/u/other/a.dbc"), Some(dir)),
            "../other/a.dbc"
        );
        let d = abs("/data/a.dbc");
        assert_eq!(stored_path(&d, None), d.to_string_lossy());
    }

    #[test]
    fn resolve_relative_and_absolute() {
        let dir = abs("/home/u/proj");
        let dir = dir.as_path();
        assert_eq!(resolve_path(Some(dir), "db/a.dbc"), dir.join("db/a.dbc"));
        assert_eq!(
            resolve_path(Some(dir), &abs("/abs/a.dbc").to_string_lossy()),
            abs("/abs/a.dbc")
        );
        assert_eq!(resolve_path(None, "a.dbc"), PathBuf::from("a.dbc"));
        // Round trip.
        let p = abs("/home/u/other/a.dbc");
        assert_eq!(
            resolve_path(Some(dir), &stored_path(&p, Some(dir))),
            dir.join("../other/a.dbc")
        );
    }

    #[test]
    fn value_formatting() {
        assert_eq!(format_value(3.0, 1.0), "3");
        assert_eq!(format_value(-40.0, 1.0), "-40");
        assert_eq!(format_value(812.5, 0.25), "812.5");
        assert_eq!(format_value(12.0, 0.1), "12");
        assert_eq!(format_value(12.3456, 0.0001), "12.346");
        assert_eq!(format_value(0.1 * 3.0, 0.1), "0.3");
        assert_eq!(format_value(-0.0001, 0.1), "0");
    }

    fn sample() -> Database {
        Database::parse(include_str!("../../operow-dbc/tests/fixtures/sample.dbc")).unwrap()
    }

    #[test]
    fn signal_lines_with_units_and_descriptions() {
        let db = sample();
        let msg = db.message(256, false).unwrap();
        let mut data = [0u8; 8];
        for s in &msg.signals {
            match s.name.as_str() {
                "EngineSpeed" => s.encode(&mut data, 3000.0),
                "CoolantTemp" => s.encode(&mut data, 50.0),
                "Running" => s.encode(&mut data, 1.0),
                _ => {}
            }
        }
        let frame = CanFrame::new(256, false, &data).unwrap();
        let lines = decode_lines(msg, &frame);
        assert_eq!(lines[0], "EngineSpeed = 3000 rpm");
        assert_eq!(lines[1], "CoolantTemp = 50 degC");
        assert_eq!(lines[3], "Running = 1 (On)");
    }

    #[test]
    fn name_lookup_prefers_dbc_for_bus() {
        use crate::trace::NameLookup;
        let db = Arc::new(sample());
        let mut names = NameLookup::default();
        names.bus_names.insert(BusId(1), "PT".into());
        assert_eq!(
            names.msg_name(BusId(1), operow_core::NodeId(9), 256, false),
            ""
        );
        names.dbcs.by_bus.insert(BusId(1), db);
        assert_eq!(
            names.msg_name(BusId(1), operow_core::NodeId(9), 256, false),
            "EngineData"
        );
        assert_eq!(
            names.msg_name(BusId(2), operow_core::NodeId(9), 256, false),
            ""
        );
        assert_eq!(
            names.dbc_for_bus_name("PT").map(|d| d.messages.len()),
            Some(3)
        );
    }
}
