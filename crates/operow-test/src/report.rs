//! The result model of a test run.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pass,
    Fail,
    Skip,
    /// The test could not run properly: a script error, an unknown name, a
    /// compile error.
    Error,
}

/// One recorded action or check of a test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Virtual time of the simulation when the step happened.
    pub time_ms: f64,
    /// `wait`, `expect`, `send`, `fault`, `uds`, `log`, ...
    pub kind: String,
    pub text: String,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub message: String,
    pub time_ms: f64,
    /// Line in the test module, where the script position is known.
    pub line: Option<u32>,
}

/// A bus event shown around a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceRow {
    pub time_ms: f64,
    pub bus: String,
    /// `Tx` or `Rx`.
    pub dir: String,
    pub id: u32,
    pub ext: bool,
    pub dlc: u8,
    pub data: Vec<u8>,
    pub sender: String,
    /// The CAN error kind, for error frames.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub name: String,
    pub status: Status,
    pub virtual_duration_ms: f64,
    pub wall_duration_ms: f64,
    pub steps: Vec<Step>,
    pub failure: Option<Failure>,
    /// Bus traffic of the last `trace_extract_ms` before a failure or error.
    pub trace_extract: Vec<TraceRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleResult {
    /// The path as listed in the project.
    pub path: String,
    pub name: String,
    pub status: Status,
    /// Compile error, simulation build error or module setup/teardown
    /// problem.
    pub error: Option<String>,
    pub cases: Vec<CaseResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub modules: u32,
    pub cases: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    /// Cases with status Error plus modules that failed to run.
    pub errors: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub project: String,
    /// UTC start time, RFC 3339.
    pub started: String,
    /// Wall-clock duration of the whole run.
    pub duration_ms: u64,
    /// Whether the run was cut short by the stop flag.
    #[serde(default)]
    pub stopped: bool,
    pub modules: Vec<ModuleResult>,
    pub totals: Totals,
}

impl RunReport {
    /// Whether every case passed or was skipped and no module errored.
    pub fn success(&self) -> bool {
        self.totals.failed == 0 && self.totals.errors == 0
    }

    /// A copy with every wall-clock value cleared, for comparing runs.
    pub fn without_wall_times(&self) -> RunReport {
        let mut r = self.clone();
        r.started.clear();
        r.duration_ms = 0;
        for c in r.modules.iter_mut().flat_map(|m| m.cases.iter_mut()) {
            c.wall_duration_ms = 0.0;
        }
        r
    }

    pub(crate) fn compute_totals(modules: &[ModuleResult]) -> Totals {
        let mut t = Totals {
            modules: modules.len() as u32,
            ..Totals::default()
        };
        for m in modules {
            if m.error.is_some() {
                t.errors += 1;
            }
            for c in &m.cases {
                t.cases += 1;
                match c.status {
                    Status::Pass => t.passed += 1,
                    Status::Fail => t.failed += 1,
                    Status::Skip => t.skipped += 1,
                    Status::Error => t.errors += 1,
                }
            }
        }
        t
    }
}
