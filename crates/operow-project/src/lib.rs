//! Project-level helpers shared by the Operow app and the test runner: DBC
//! databases attached to buses, their loading and path handling.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use operow_core::{BusId, DbcRef, TxMessage};
use operow_dbc::{Database, MessageDef};

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
}
