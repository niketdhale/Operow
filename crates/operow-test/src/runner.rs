//! Compiles test modules, discovers their functions and runs every case
//! against its own simulation in virtual time.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rhai::{AST, CallFnOptions, Dynamic, Engine, EvalAltResult, Scope};

use crate::api::{self, Abort, Ctx, Shared, lock};
use crate::project::{Project, module_name};
use crate::report::{CaseResult, Failure, ModuleResult, RunReport, Status};

/// Progress of a run, reported through [`RunOptions::progress`].
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    ModuleStarted {
        module: String,
    },
    CaseStarted {
        module: String,
        case: String,
    },
    CaseFinished {
        module: String,
        case: String,
        status: Status,
    },
    ModuleFinished {
        module: String,
        status: Status,
    },
}

pub type ProgressFn = Arc<dyn Fn(&Progress) + Send + Sync>;

/// Settings of a run.
#[derive(Clone)]
pub struct RunOptions {
    /// Virtual time one function (setup, test or teardown) may take.
    pub per_test_timeout_ms: u64,
    /// Rhai operation limit per function call; 0 is unlimited.
    pub max_operations: u64,
    /// Build a new simulation for every case. When `false` one simulation
    /// serves the whole module and `module_setup` / `module_teardown` run on
    /// it; when `true` they run on a separate simulation of their own.
    pub fresh_sim_per_case: bool,
    /// Seed of the simulation's random generator (fault injection, message
    /// control). The same seed gives the same report.
    pub seed: u64,
    /// Granularity in which blocking calls advance the simulation.
    pub step_ns: u64,
    /// Virtual time before a failure shown in the trace extract.
    pub trace_extract_ms: u64,
    /// Set to abort the run: the running case ends and no further case
    /// starts.
    pub stop_flag: Arc<AtomicBool>,
    pub progress: Option<ProgressFn>,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            per_test_timeout_ms: 60_000,
            max_operations: 10_000_000,
            fresh_sim_per_case: true,
            seed: 1,
            step_ns: 1_000_000,
            trace_extract_ms: 200,
            stop_flag: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }
}

/// Runs the test modules of a [`Project`].
pub struct TestRunner {
    project: Project,
    options: RunOptions,
}

/// A module source, or why it could not be read.
type Source = (String, Result<String, String>);

impl TestRunner {
    pub fn new(project: Project, options: RunOptions) -> Self {
        TestRunner { project, options }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Run the project's test modules. `filter` is a glob (`*`, `?`) on the
    /// case name or the module name, or, when it contains `::`, on
    /// `module::case`; modules without a matching case are left out.
    pub fn run(&self, filter: Option<&str>) -> RunReport {
        let sources = self
            .project
            .tests
            .iter()
            .map(|m| {
                let text = std::fs::read_to_string(&m.file)
                    .map_err(|e| format!("cannot read {}: {e}", m.file.display()));
                (m.path.clone(), text)
            })
            .collect();
        self.run_all(sources, filter)
    }

    /// Run test modules given as `(path, source)`, ignoring the project's
    /// own list.
    pub fn run_sources(&self, sources: &[(&str, &str)], filter: Option<&str>) -> RunReport {
        let sources = sources
            .iter()
            .map(|(p, s)| (p.to_string(), Ok(s.to_string())))
            .collect();
        self.run_all(sources, filter)
    }

    fn run_all(&self, sources: Vec<Source>, filter: Option<&str>) -> RunReport {
        let started = rfc3339_now();
        let t0 = Instant::now();
        let mut modules = Vec::new();
        for (path, text) in sources {
            if self.stopped() {
                break;
            }
            let result = match text {
                Ok(src) => self.run_module(&path, &src, filter),
                Err(e) => Some(error_module(&path, e)),
            };
            modules.extend(result);
        }
        let totals = RunReport::compute_totals(&modules);
        RunReport {
            project: self.project.name.clone(),
            started,
            duration_ms: t0.elapsed().as_millis() as u64,
            stopped: self.stopped(),
            modules,
            totals,
        }
    }

    fn stopped(&self) -> bool {
        self.options.stop_flag.load(Ordering::Relaxed)
    }

    fn progress(&self, p: Progress) {
        if let Some(f) = &self.options.progress {
            f(&p);
        }
    }

    fn session(&self) -> Result<Session, String> {
        let o = &self.options;
        let ctx = Ctx::new(
            &self.project,
            o.seed,
            o.step_ns,
            o.per_test_timeout_ms,
            o.stop_flag.clone(),
        )
        .map_err(|e| format!("cannot build the simulation: {e}"))?;
        let shared: Shared = Arc::new(Mutex::new(ctx));
        let mut engine = Engine::new();
        engine.set_max_operations(o.max_operations);
        let stop = o.stop_flag.clone();
        engine.on_progress(move |_| stop.load(Ordering::Relaxed).then_some(Dynamic::UNIT));
        api::register(&mut engine, &shared);
        Ok(Session {
            shared,
            engine,
            max_operations: o.max_operations,
        })
    }

    fn run_module(&self, path: &str, src: &str, filter: Option<&str>) -> Option<ModuleResult> {
        let name = module_name(path);
        let ast = match Engine::new().compile(src) {
            Ok(ast) => ast,
            Err(e) => return Some(error_module(path, format!("compile error: {e}"))),
        };
        let fns = Fns::discover(&ast, src);
        let cases: Vec<&String> = fns
            .tests
            .iter()
            .filter(|c| filter.is_none_or(|f| matches_filter(f, &name, c)))
            .collect();
        if filter.is_some() && cases.is_empty() {
            return None;
        }
        self.progress(Progress::ModuleStarted {
            module: name.clone(),
        });
        let mut result = ModuleResult {
            path: path.to_string(),
            name: name.clone(),
            status: Status::Pass,
            error: None,
            cases: Vec::new(),
        };
        let shared_sim = !self.options.fresh_sim_per_case;
        let needs_module_sim =
            shared_sim || fns.module_setup.is_some() || fns.module_teardown.is_some();
        let module_sess = if needs_module_sim {
            match self.session() {
                Ok(s) => Some(s),
                Err(e) => {
                    result.status = Status::Error;
                    result.error = Some(e);
                    self.progress(Progress::ModuleFinished {
                        module: name,
                        status: result.status,
                    });
                    return Some(result);
                }
            }
        } else {
            None
        };

        let mut module_ok = true;
        if let (Some(sess), Some(f)) = (&module_sess, &fns.module_setup) {
            match sess.call(&ast, f) {
                Outcome::Pass => {}
                Outcome::Skip(why) => {
                    result.status = Status::Skip;
                    result.error = None;
                    module_ok = false;
                    let _ = why;
                }
                Outcome::Stopped => module_ok = false,
                Outcome::Fail { msg, line, .. } | Outcome::Error { msg, line, .. } => {
                    result.status = Status::Error;
                    result.error = Some(format!("module_setup failed: {}", with_line(&msg, line)));
                    module_ok = false;
                }
            }
        }

        if module_ok {
            for case in cases {
                if self.stopped() {
                    break;
                }
                self.progress(Progress::CaseStarted {
                    module: name.clone(),
                    case: case.clone(),
                });
                let fresh;
                let sess = if shared_sim {
                    module_sess.as_ref().expect("module session exists")
                } else {
                    match self.session() {
                        Ok(s) => {
                            fresh = s;
                            &fresh
                        }
                        Err(e) => {
                            result.cases.push(error_case(case, e));
                            continue;
                        }
                    }
                };
                let cr = self.run_case(sess, &ast, &fns, case);
                self.progress(Progress::CaseFinished {
                    module: name.clone(),
                    case: case.clone(),
                    status: cr.status,
                });
                result.cases.push(cr);
            }
        }

        if let (Some(sess), Some(f)) = (&module_sess, &fns.module_teardown) {
            match sess.call(&ast, f) {
                Outcome::Fail { msg, line, .. } | Outcome::Error { msg, line, .. } => {
                    result.status = Status::Error;
                    result.error =
                        Some(format!("module_teardown failed: {}", with_line(&msg, line)));
                }
                _ => {}
            }
        }

        if result.error.is_none() && result.status != Status::Skip {
            result.status = module_status(&result.cases);
        }
        self.progress(Progress::ModuleFinished {
            module: name,
            status: result.status,
        });
        Some(result)
    }

    fn run_case(&self, sess: &Session, ast: &AST, fns: &Fns, case: &str) -> CaseResult {
        let wall = Instant::now();
        let start_ns = {
            let mut c = lock(&sess.shared);
            c.clear_steps();
            c.now_ns
        };
        let mut outcome = Outcome::Pass;
        if let Some(f) = &fns.setup {
            outcome = sess.call(ast, f);
        }
        if matches!(outcome, Outcome::Pass) {
            outcome = sess.call(ast, case);
        }
        let stopped = matches!(outcome, Outcome::Stopped);
        if !stopped && let Some(f) = &fns.teardown {
            match sess.call(ast, f) {
                Outcome::Fail { msg, time_ns, line } | Outcome::Error { msg, time_ns, line } => {
                    let msg = format!("teardown failed: {msg}");
                    lock(&sess.shared).step("teardown", msg.clone(), false);
                    if matches!(outcome, Outcome::Pass) {
                        outcome = Outcome::Fail { msg, time_ns, line };
                    }
                }
                _ => {}
            }
        }

        let c = lock(&sess.shared);
        let virtual_ms = (c.now_ns - start_ns) as f64 / 1e6;
        let (status, failure, trace_ns) = match outcome {
            Outcome::Pass => (Status::Pass, None, 0),
            Outcome::Skip(_) => (Status::Skip, None, 0),
            Outcome::Fail { msg, time_ns, line } => {
                (Status::Fail, Some(failure(msg, time_ns, line)), time_ns)
            }
            Outcome::Error { msg, time_ns, line } => {
                (Status::Error, Some(failure(msg, time_ns, line)), time_ns)
            }
            Outcome::Stopped => (
                Status::Error,
                Some(failure("run stopped".into(), c.now_ns, None)),
                c.now_ns,
            ),
        };
        let trace_extract = if failure_status(status) {
            c.trace_extract(self.options.trace_extract_ms, trace_ns)
        } else {
            Vec::new()
        };
        CaseResult {
            name: case.to_string(),
            status,
            virtual_duration_ms: virtual_ms,
            wall_duration_ms: wall.elapsed().as_secs_f64() * 1e3,
            steps: c.steps.clone(),
            failure,
            trace_extract,
        }
    }
}

fn failure_status(s: Status) -> bool {
    matches!(s, Status::Fail | Status::Error)
}

fn failure(message: String, time_ns: u64, line: Option<u32>) -> Failure {
    Failure {
        message,
        time_ms: time_ns as f64 / 1e6,
        line,
    }
}

fn with_line(msg: &str, line: Option<u32>) -> String {
    match line {
        Some(l) => format!("{msg} (line {l})"),
        None => msg.to_string(),
    }
}

fn error_module(path: &str, msg: String) -> ModuleResult {
    ModuleResult {
        path: path.to_string(),
        name: module_name(path),
        status: Status::Error,
        error: Some(msg),
        cases: Vec::new(),
    }
}

fn error_case(name: &str, msg: String) -> CaseResult {
    CaseResult {
        name: name.to_string(),
        status: Status::Error,
        virtual_duration_ms: 0.0,
        wall_duration_ms: 0.0,
        steps: Vec::new(),
        failure: Some(failure(msg, 0, None)),
        trace_extract: Vec::new(),
    }
}

fn module_status(cases: &[CaseResult]) -> Status {
    let any = |s: Status| cases.iter().any(|c| c.status == s);
    if any(Status::Error) {
        Status::Error
    } else if any(Status::Fail) {
        Status::Fail
    } else if !cases.is_empty() && cases.iter().all(|c| c.status == Status::Skip) {
        Status::Skip
    } else {
        Status::Pass
    }
}

/// A simulation plus the Rhai engine bound to it.
struct Session {
    shared: Shared,
    engine: Engine,
    max_operations: u64,
}

#[derive(Debug)]
enum Outcome {
    Pass,
    Fail {
        msg: String,
        time_ns: u64,
        line: Option<u32>,
    },
    Error {
        msg: String,
        time_ns: u64,
        line: Option<u32>,
    },
    Skip(String),
    Stopped,
}

/// The innermost error of a chain of function-call errors.
fn innermost(e: &EvalAltResult) -> &EvalAltResult {
    match e {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => innermost(inner),
        other => other,
    }
}

impl Session {
    /// Call a script function without arguments and classify how it ended.
    fn call(&self, ast: &AST, name: &str) -> Outcome {
        lock(&self.shared).begin_phase();
        let mut scope = Scope::new();
        let opts = CallFnOptions::new().eval_ast(false).rewind_scope(false);
        let r = self
            .engine
            .call_fn_with_options::<Dynamic>(opts, &mut scope, ast, name, ());
        let (abort, now_ns) = {
            let mut c = lock(&self.shared);
            (c.abort.take(), c.now_ns)
        };
        let inner = r.as_ref().err().map(|e| innermost(e));
        let line = inner.and_then(|e| e.position().line()).map(|l| l as u32);
        if let Some((a, time_ns)) = abort {
            return match a {
                Abort::Fail(msg) => Outcome::Fail { msg, time_ns, line },
                Abort::Error(msg) => Outcome::Error { msg, time_ns, line },
                Abort::Skip(why) => Outcome::Skip(why),
                Abort::Stopped => Outcome::Stopped,
            };
        }
        match inner {
            None => Outcome::Pass,
            Some(EvalAltResult::ErrorTerminated(..)) => Outcome::Stopped,
            Some(EvalAltResult::ErrorTooManyOperations(_)) => Outcome::Error {
                msg: format!(
                    "operation limit of {} exceeded (endless loop?)",
                    self.max_operations
                ),
                time_ns: now_ns,
                line,
            },
            Some(e) => Outcome::Error {
                msg: e.to_string(),
                time_ns: now_ns,
                line,
            },
        }
    }
}

/// The functions of a module the runner cares about.
struct Fns {
    module_setup: Option<String>,
    module_teardown: Option<String>,
    setup: Option<String>,
    teardown: Option<String>,
    /// In source order.
    tests: Vec<String>,
}

impl Fns {
    fn discover(ast: &AST, src: &str) -> Fns {
        let have: HashSet<String> = ast
            .iter_functions()
            .filter(|f| f.params.is_empty())
            .map(|f| f.name.to_string())
            .collect();
        let mut ordered: Vec<String> = Vec::new();
        for n in scan_fn_names(src) {
            if have.contains(&n) && !ordered.contains(&n) {
                ordered.push(n);
            }
        }
        let mut rest: Vec<&String> = have.iter().filter(|n| !ordered.contains(n)).collect();
        rest.sort();
        ordered.extend(rest.into_iter().cloned());
        let pick = |n: &str| have.contains(n).then(|| n.to_string());
        Fns {
            module_setup: pick("module_setup"),
            module_teardown: pick("module_teardown"),
            setup: pick("setup"),
            teardown: pick("teardown"),
            tests: ordered
                .into_iter()
                .filter(|n| n.starts_with("test_"))
                .collect(),
        }
    }
}

/// Names of the top-level `fn` definitions of Rhai source in order of
/// appearance, skipping comments and string literals.
fn scan_fn_names(src: &str) -> Vec<String> {
    let c: Vec<char> = src.chars().collect();
    let mut names = Vec::new();
    let (mut i, mut depth) = (0usize, 0i32);
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    while i < c.len() {
        match c[i] {
            '/' if c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                let mut nest = 1;
                i += 2;
                while i < c.len() && nest > 0 {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        nest += 1;
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        nest -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            q @ ('"' | '`' | '\'') => {
                i += 1;
                while i < c.len() && c[i] != q {
                    if c[i] == '\\' && q != '`' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '{' => {
                depth += 1;
                i += 1;
            }
            '}' => {
                depth -= 1;
                i += 1;
            }
            'f' if depth == 0
                && c.get(i + 1) == Some(&'n')
                && (i == 0 || !ident(c[i - 1]))
                && c.get(i + 2).is_some_and(|ch| ch.is_whitespace()) =>
            {
                let mut j = i + 2;
                while j < c.len() && c[j].is_whitespace() {
                    j += 1;
                }
                let start = j;
                while j < c.len() && ident(c[j]) {
                    j += 1;
                }
                if j > start {
                    names.push(c[start..j].iter().collect());
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    names
}

/// Whether `filter` selects case `case` of module `module`.
fn matches_filter(filter: &str, module: &str, case: &str) -> bool {
    if filter.contains("::") {
        glob(filter, &format!("{module}::{case}"))
    } else {
        glob(filter, case) || glob(filter, module)
    }
}

/// Case-insensitive glob with `*` and `?`.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The current UTC time as RFC 3339, to the second.
fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
