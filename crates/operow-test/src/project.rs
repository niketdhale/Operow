//! A loaded Operow project: topology, DBC databases and test modules.

use std::path::{Path, PathBuf};

use operow_core::Topology;
use operow_project::{DbcStore, load_all, resolve_path};
use thiserror::Error;

/// Errors from [`Project::load`].
#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("cannot read {path}: {msg}")]
    Read { path: String, msg: String },
    #[error("invalid project {path}: {msg}")]
    Json { path: String, msg: String },
    #[error("{}", .0.join("; "))]
    Dbc(Vec<String>),
}

/// One `.rhai` test module listed in the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestModule {
    /// The path as stored in the project (relative to the project file).
    pub path: String,
    /// The resolved file.
    pub file: PathBuf,
}

impl TestModule {
    /// The module name: the file name without extension.
    pub fn name(&self) -> String {
        module_name(&self.path)
    }
}

pub(crate) fn module_name(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned())
}

/// A project ready to be tested.
#[derive(Clone)]
pub struct Project {
    /// File name of the project, or a label for in-memory projects.
    pub name: String,
    /// Folder of the project file, when loaded from disk.
    pub dir: Option<PathBuf>,
    pub topology: Topology,
    /// Parsed DBCs by bus.
    pub dbcs: DbcStore,
    pub tests: Vec<TestModule>,
}

impl Project {
    /// Load a project file: the topology JSON, every referenced DBC and the
    /// paths of the test modules (relative paths resolve against the
    /// project's folder). A DBC that cannot be read is an error.
    pub fn load(path: impl AsRef<Path>) -> Result<Project, ProjectError> {
        let path = path.as_ref();
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| ProjectError::Read {
            path: shown.clone(),
            msg: e.to_string(),
        })?;
        let topology = Topology::from_json(&text).map_err(|e| ProjectError::Json {
            path: shown,
            msg: e.to_string(),
        })?;
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .filter(|d| !d.as_os_str().is_empty());
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Project::from_topology(name, topology, dir.as_deref())
    }

    /// Build a project from an in-memory topology; relative DBC and test
    /// paths resolve against `dir`.
    pub fn from_topology(
        name: impl Into<String>,
        topology: Topology,
        dir: Option<&Path>,
    ) -> Result<Project, ProjectError> {
        let (dbcs, errors) = load_all(&topology.databases, dir);
        if !errors.is_empty() {
            return Err(ProjectError::Dbc(errors));
        }
        let tests = topology
            .tests
            .iter()
            .map(|p| TestModule {
                path: p.clone(),
                file: resolve_path(dir, p),
            })
            .collect();
        Ok(Project {
            name: name.into(),
            dir: dir.map(Path::to_path_buf),
            topology,
            dbcs,
            tests,
        })
    }
}
