//! The Tests window: the project's test modules as a tree with live
//! status, run controls, and the details of the selected case. Runs happen
//! on a background thread with their own simulations; the live measurement
//! is not touched.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

use egui::collapsing_header::CollapsingState;
use egui_extras::{Column, TableBuilder};
use egui_flow::Icon;
use operow_core::BusEvent;
use operow_test::{
    CaseResult, EventSink, Progress, Project, RunOptions, RunReport, Source, Status, TestRunner,
    Totals, module_name,
};

use crate::icons;

/// Gap left between two cases in the trace when streaming.
const CASE_GAP_NS: u64 = 20_000_000;

/// What the Tests window or a test editor asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum TestsAction {
    RunAll,
    RunSelected,
    RunFailed,
    /// Run one module (by project path).
    RunModule(String),
    /// Run one case: module path and case name.
    RunCase(String, String),
    Stop,
    /// Re-read the module files and rebuild the tree.
    Refresh,
    /// Open a module in a test editor, at a line when given.
    OpenEditor {
        path: String,
        line: Option<u32>,
    },
    /// Show this simulation time (ns, with the streaming offset applied) in
    /// a Trace window.
    JumpTrace(u64),
}

/// What the worker thread sends to the UI.
pub enum TestEvent {
    Progress {
        ev: Progress,
        /// Trace time offset of the case that just started, when streaming.
        base_ns: Option<u64>,
    },
    /// The result of one job.
    Report(Box<RunReport>),
    /// Events of a simulation step, already shifted by the case offset.
    Frames(Vec<BusEvent>),
    Done,
}

pub struct CaseNode {
    pub name: String,
    pub status: Option<Status>,
    pub running: bool,
    pub result: Option<CaseResult>,
    /// Where the case starts in the trace, when its frames were streamed.
    pub trace_base_ns: Option<u64>,
}

impl CaseNode {
    fn new(name: &str) -> Self {
        CaseNode {
            name: name.to_string(),
            status: None,
            running: false,
            result: None,
            trace_base_ns: None,
        }
    }

    fn reset(&mut self) {
        *self = CaseNode::new(&self.name.clone());
    }
}

pub struct ModuleNode {
    /// The path as listed in the project.
    pub path: String,
    pub name: String,
    pub status: Option<Status>,
    pub running: bool,
    /// Compile or setup error of the last run.
    pub error: Option<String>,
    /// Why the cases could not be listed (unreadable file, compile error).
    pub list_error: Option<String>,
    pub cases: Vec<CaseNode>,
}

impl ModuleNode {
    fn new(path: &str) -> Self {
        ModuleNode {
            path: path.to_string(),
            name: module_name(path),
            status: None,
            running: false,
            error: None,
            list_error: None,
            cases: Vec::new(),
        }
    }

    /// The module status from its cases, or the run error.
    fn recompute_status(&mut self) {
        let any = |s: Status| self.cases.iter().any(|c| c.status == Some(s));
        let done = self.cases.iter().filter(|c| c.status.is_some()).count();
        self.status = if self.error.is_some() || any(Status::Error) {
            Some(Status::Error)
        } else if any(Status::Fail) {
            Some(Status::Fail)
        } else if done == 0 {
            None
        } else if self.cases.iter().all(|c| c.status == Some(Status::Skip)) {
            Some(Status::Skip)
        } else {
            Some(Status::Pass)
        };
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    #[default]
    None,
    Module(String),
    /// Module path and case name.
    Case(String, String),
}

/// One run of the runner: a module, optionally narrowed to one case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub path: String,
    pub case: Option<String>,
}

struct RunHandle {
    rx: Receiver<TestEvent>,
    stop: Arc<AtomicBool>,
}

pub struct TestsState {
    pub modules: Vec<ModuleNode>,
    pub selection: Selection,
    pub seed: u64,
    /// Forward the frames of every case into the shared frame store.
    pub stream: bool,
    /// The step row highlighted in the details.
    pub selected_step: Option<usize>,
    /// Result of the last export or other notice.
    pub message: Option<String>,
    project_name: String,
    started: String,
    duration_ms: u64,
    stopped: bool,
    run: Option<RunHandle>,
    /// Paths the tree was built from.
    listed: Vec<String>,
    dirty: bool,
}

impl Default for TestsState {
    fn default() -> Self {
        TestsState {
            modules: Vec::new(),
            selection: Selection::None,
            seed: 1,
            stream: false,
            selected_step: None,
            message: None,
            project_name: String::new(),
            started: String::new(),
            duration_ms: 0,
            stopped: false,
            run: None,
            listed: Vec::new(),
            dirty: true,
        }
    }
}

fn case_filter(path: &str, case: &str) -> String {
    format!("{}::{case}", module_name(path))
}

impl TestsState {
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Rebuild the tree on the next `sync_modules`.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn module_by_name_mut(&mut self, name: &str) -> Option<&mut ModuleNode> {
        self.modules.iter_mut().find(|m| m.name == name)
    }

    /// Make the tree list `paths` (the project's test modules), reading
    /// each source through `load` to find its cases. Results of cases that
    /// still exist are kept. Does nothing unless the paths changed or the
    /// tree was marked dirty.
    pub fn sync_modules(
        &mut self,
        paths: &[String],
        load: impl Fn(&str) -> Result<String, String>,
    ) {
        if !self.dirty && self.listed == paths {
            return;
        }
        self.dirty = false;
        self.listed = paths.to_vec();
        let mut old = std::mem::take(&mut self.modules);
        for p in paths {
            let mut node = match old.iter().position(|m| &m.path == p) {
                Some(i) => old.swap_remove(i),
                None => ModuleNode::new(p),
            };
            node.list_error = None;
            match load(p).and_then(|src| operow_test::list_cases(&src)) {
                Ok(names) => {
                    let mut prev = std::mem::take(&mut node.cases);
                    node.cases = names
                        .iter()
                        .map(|n| match prev.iter().position(|c| &c.name == n) {
                            Some(i) => prev.swap_remove(i),
                            None => CaseNode::new(n),
                        })
                        .collect();
                }
                Err(e) => {
                    node.list_error = Some(e);
                    node.cases.clear();
                }
            }
            node.recompute_status();
            self.modules.push(node);
        }
        let valid = |sel: &Selection, mods: &[ModuleNode]| match sel {
            Selection::None => true,
            Selection::Module(p) => mods.iter().any(|m| &m.path == p),
            Selection::Case(p, c) => mods
                .iter()
                .any(|m| &m.path == p && m.cases.iter().any(|k| &k.name == c)),
        };
        if !valid(&self.selection, &self.modules) {
            self.selection = Selection::None;
            self.selected_step = None;
        }
    }

    /// Apply one event of the worker. Returns the frames to push into the
    /// frame store.
    pub fn apply(&mut self, ev: TestEvent) -> Vec<BusEvent> {
        match ev {
            TestEvent::Frames(f) => return f,
            TestEvent::Progress { ev, base_ns } => self.apply_progress(ev, base_ns),
            TestEvent::Report(r) => self.apply_report(*r),
            TestEvent::Done => {
                self.run = None;
                for m in &mut self.modules {
                    m.running = false;
                    for c in &mut m.cases {
                        c.running = false;
                    }
                }
            }
        }
        Vec::new()
    }

    fn apply_progress(&mut self, ev: Progress, base_ns: Option<u64>) {
        match ev {
            Progress::ModuleStarted { module } => {
                if let Some(m) = self.module_by_name_mut(&module) {
                    m.running = true;
                    m.error = None;
                }
            }
            Progress::CaseStarted { module, case } => {
                if let Some(c) = self
                    .module_by_name_mut(&module)
                    .and_then(|m| m.cases.iter_mut().find(|c| c.name == case))
                {
                    c.running = true;
                    c.status = None;
                    c.trace_base_ns = base_ns;
                }
            }
            Progress::CaseFinished {
                module,
                case,
                status,
            } => {
                if let Some(m) = self.module_by_name_mut(&module) {
                    if let Some(c) = m.cases.iter_mut().find(|c| c.name == case) {
                        c.running = false;
                        c.status = Some(status);
                    }
                    m.recompute_status();
                }
            }
            Progress::ModuleFinished { module, status } => {
                if let Some(m) = self.module_by_name_mut(&module) {
                    m.running = false;
                    m.status = Some(status);
                }
            }
        }
    }

    fn apply_report(&mut self, r: RunReport) {
        self.started = r.started;
        self.duration_ms += r.duration_ms;
        self.stopped |= r.stopped;
        for mr in r.modules {
            let Some(m) = self.modules.iter_mut().find(|m| m.path == mr.path) else {
                continue;
            };
            m.error = mr.error;
            for cr in mr.cases {
                let status = cr.status;
                match m.cases.iter_mut().find(|c| c.name == cr.name) {
                    Some(c) => {
                        c.running = false;
                        c.status = Some(status);
                        c.result = Some(cr);
                    }
                    None => {
                        let mut c = CaseNode::new(&cr.name);
                        c.status = Some(status);
                        c.result = Some(cr);
                        m.cases.push(c);
                    }
                }
            }
            m.running = false;
            m.recompute_status();
        }
    }

    /// Drain the worker's channel. Returns the frames to push into the
    /// frame store.
    pub fn poll(&mut self) -> Vec<BusEvent> {
        let mut frames = Vec::new();
        while let Some(next) = self.run.as_ref().map(|r| r.rx.try_recv()) {
            match next {
                Ok(ev) => frames.extend(self.apply(ev)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.apply(TestEvent::Done);
                    break;
                }
            }
        }
        frames
    }

    pub fn jobs_all(&self) -> Vec<Job> {
        self.modules
            .iter()
            .map(|m| Job {
                path: m.path.clone(),
                case: None,
            })
            .collect()
    }

    pub fn jobs_selected(&self) -> Vec<Job> {
        match &self.selection {
            Selection::None => Vec::new(),
            Selection::Module(p) => vec![Job {
                path: p.clone(),
                case: None,
            }],
            Selection::Case(p, c) => vec![Job {
                path: p.clone(),
                case: Some(c.clone()),
            }],
        }
    }

    pub fn has_failed(&self) -> bool {
        self.modules.iter().any(|m| {
            m.error.is_some()
                || m.cases
                    .iter()
                    .any(|c| matches!(c.status, Some(Status::Fail | Status::Error)))
        })
    }

    pub fn jobs_failed(&self) -> Vec<Job> {
        let mut jobs = Vec::new();
        for m in &self.modules {
            let failed: Vec<_> = m
                .cases
                .iter()
                .filter(|c| matches!(c.status, Some(Status::Fail | Status::Error)))
                .collect();
            if failed.is_empty() && m.error.is_some() {
                jobs.push(Job {
                    path: m.path.clone(),
                    case: None,
                });
            }
            jobs.extend(failed.into_iter().map(|c| Job {
                path: m.path.clone(),
                case: Some(c.name.clone()),
            }));
        }
        jobs
    }

    /// Clear the results the jobs are about to replace.
    fn reset_for(&mut self, jobs: &[Job]) {
        for job in jobs {
            let Some(m) = self.modules.iter_mut().find(|m| m.path == job.path) else {
                continue;
            };
            m.error = None;
            for c in &mut m.cases {
                if job.case.as_ref().is_none_or(|n| n == &c.name) {
                    c.reset();
                }
            }
            m.recompute_status();
        }
        self.selected_step = None;
    }

    /// Start a run on a background thread. `sources` hold the text of every
    /// module (unsaved editor text included). With `stream`, the frames of
    /// each case are sent to the UI shifted by `base_ns` plus the time the
    /// earlier cases took, so cases follow each other in the trace.
    pub fn start(
        &mut self,
        project: Project,
        sources: Vec<Source>,
        jobs: Vec<Job>,
        base_ns: u64,
        ctx: &egui::Context,
    ) {
        if self.run.is_some() || jobs.is_empty() {
            return;
        }
        self.reset_for(&jobs);
        self.message = None;
        self.project_name = project.name.clone();
        self.started.clear();
        self.duration_ms = 0;
        self.stopped = false;
        let (tx, rx) = sync_channel(4096);
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            project,
            sources,
            jobs,
            seed: self.seed,
            stream: self.stream,
            base_ns,
            stop: stop.clone(),
            tx,
            ctx: ctx.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("operow-tests".into())
            .spawn(move || worker.run());
        match spawned {
            Ok(_) => self.run = Some(RunHandle { rx, stop }),
            Err(e) => self.message = Some(format!("cannot start the test run: {e}")),
        }
    }

    pub fn stop(&mut self) {
        if let Some(r) = &self.run {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    /// The results gathered so far as a report (modules without a result
    /// are left out); `None` before the first run.
    pub fn report(&self) -> Option<RunReport> {
        let modules: Vec<_> = self
            .modules
            .iter()
            .filter(|m| m.error.is_some() || m.cases.iter().any(|c| c.result.is_some()))
            .map(|m| operow_test::ModuleResult {
                path: m.path.clone(),
                name: m.name.clone(),
                status: m.status.unwrap_or(Status::Pass),
                error: m.error.clone(),
                cases: m.cases.iter().filter_map(|c| c.result.clone()).collect(),
            })
            .collect();
        if modules.is_empty() {
            return None;
        }
        let mut r = RunReport {
            project: self.project_name.clone(),
            started: self.started.clone(),
            duration_ms: self.duration_ms,
            stopped: self.stopped,
            modules,
            totals: Totals::default(),
        };
        r.recompute_totals();
        Some(r)
    }

    /// Counts of the cases by status: passed, failed, skipped, errors, not run.
    pub fn counts(&self) -> [usize; 5] {
        let mut n = [0; 5];
        for c in self.modules.iter().flat_map(|m| &m.cases) {
            n[match c.status {
                Some(Status::Pass) => 0,
                Some(Status::Fail) => 1,
                Some(Status::Skip) => 2,
                Some(Status::Error) => 3,
                None => 4,
            }] += 1;
        }
        n
    }
}

/// Everything the background thread needs.
struct Worker {
    project: Project,
    sources: Vec<Source>,
    jobs: Vec<Job>,
    seed: u64,
    stream: bool,
    base_ns: u64,
    stop: Arc<AtomicBool>,
    tx: SyncSender<TestEvent>,
    ctx: egui::Context,
}

impl Worker {
    fn run(self) {
        let Worker {
            project,
            sources,
            jobs,
            seed,
            stream,
            base_ns,
            stop,
            tx,
            ctx,
        } = self;
        // Trace offset of the running case and the latest event time in it.
        let base = Arc::new(AtomicU64::new(base_ns));
        let longest = Arc::new(AtomicU64::new(0));
        let progress = {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            let (base, longest) = (base.clone(), longest.clone());
            Arc::new(move |p: &Progress| {
                let now = base.load(Ordering::Relaxed);
                let base_ns = stream.then_some(now);
                let _ = tx.send(TestEvent::Progress {
                    ev: p.clone(),
                    base_ns,
                });
                if stream && matches!(p, Progress::CaseFinished { .. }) {
                    base.fetch_add(
                        longest.swap(0, Ordering::Relaxed) + CASE_GAP_NS,
                        Ordering::Relaxed,
                    );
                }
                ctx.request_repaint();
            })
        };
        let event_sink: Option<EventSink> = stream.then(|| {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            let sink: EventSink = Arc::new(move |evs: &[BusEvent]| {
                let shift = base.load(Ordering::Relaxed);
                let out = evs
                    .iter()
                    .map(|e| {
                        longest.fetch_max(e.time.0, Ordering::Relaxed);
                        let mut e = *e;
                        e.time.0 += shift;
                        e
                    })
                    .collect();
                let _ = tx.send(TestEvent::Frames(out));
                ctx.request_repaint();
            });
            sink
        });
        let options = RunOptions {
            seed,
            stop_flag: stop.clone(),
            progress: Some(progress),
            event_sink,
            ..RunOptions::default()
        };
        let runner = TestRunner::new(project, options);
        for job in jobs {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Some((path, src)) = sources.iter().find(|(p, _)| *p == job.path) else {
                continue;
            };
            let filter = job.case.as_ref().map(|c| case_filter(&job.path, c));
            let report = runner.run_loaded(vec![(path.clone(), src.clone())], filter.as_deref());
            let _ = tx.send(TestEvent::Report(Box::new(report)));
        }
        let _ = tx.send(TestEvent::Done);
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------
// UI

const GREEN: egui::Color32 = egui::Color32::from_rgb(0x2a, 0xa8, 0x4a);
const RED: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x38, 0x38);
const MAGENTA: egui::Color32 = egui::Color32::from_rgb(0xc0, 0x38, 0xc0);

fn status_color(ui: &egui::Ui, status: Option<Status>) -> egui::Color32 {
    match status {
        Some(Status::Pass) => GREEN,
        Some(Status::Fail) => RED,
        Some(Status::Error) => MAGENTA,
        Some(Status::Skip) | None => ui.visuals().weak_text_color(),
    }
}

fn status_label(status: Option<Status>, running: bool) -> &'static str {
    match (running, status) {
        (true, _) => "running",
        (_, Some(Status::Pass)) => "passed",
        (_, Some(Status::Fail)) => "failed",
        (_, Some(Status::Skip)) => "skipped",
        (_, Some(Status::Error)) => "error",
        (_, None) => "not run",
    }
}

/// A painted status mark (no font glyphs): check, cross, ring, "!", dash or
/// a spinner while running.
fn status_icon(ui: &mut egui::Ui, status: Option<Status>, running: bool) {
    let size = egui::vec2(14.0, 14.0);
    if running {
        ui.add(egui::Spinner::new().size(14.0));
        return;
    }
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let color = status_color(ui, status);
    let p = ui.painter();
    match status {
        Some(Status::Pass) => Icon::Check.paint(p, rect, color),
        Some(Status::Fail) => Icon::Close.paint(p, rect, color),
        Some(Status::Skip) => {
            p.circle_stroke(rect.center(), 4.5, egui::Stroke::new(1.5_f32, color));
        }
        Some(Status::Error) => {
            let c = rect.center();
            let stroke = egui::Stroke::new(2.0_f32, color);
            p.line_segment([c - egui::vec2(0.0, 5.0), c + egui::vec2(0.0, 1.5)], stroke);
            p.circle_filled(c + egui::vec2(0.0, 5.0), 1.4, color);
        }
        None => Icon::Minus.paint(p, rect, color),
    }
    resp.on_hover_text(status_label(status, false));
}

fn ok_icon(ui: &mut egui::Ui, ok: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    if ok {
        Icon::Check.paint(ui.painter(), rect, GREEN);
    } else {
        Icon::Close.paint(ui.painter(), rect, RED);
    }
}

#[derive(Clone, Copy)]
enum Export {
    Html,
    Junit,
    Json,
}

impl Export {
    fn file_name(self) -> &'static str {
        match self {
            Export::Html => "test-report.html",
            Export::Junit => "test-report.xml",
            Export::Json => "test-report.json",
        }
    }

    fn render(self, r: &RunReport) -> String {
        match self {
            Export::Html => operow_test::to_html(r),
            Export::Junit => operow_test::to_junit(r),
            Export::Json => operow_test::to_json(r),
        }
    }
}

/// Open a file with the OS default application.
fn open_with_os(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path).spawn().map(|_| ())
}

impl TestsState {
    fn export(&mut self, kind: Export) {
        let Some(report) = self.report() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(kind.file_name())
            .save_file()
        else {
            return;
        };
        self.message = Some(match std::fs::write(&path, kind.render(&report)) {
            Ok(()) => format!("exported {}", path.display()),
            Err(e) => format!("cannot write {}: {e}", path.display()),
        });
    }

    fn open_html(&mut self) {
        let Some(report) = self.report() else {
            return;
        };
        let path = std::env::temp_dir().join("operow-test-report.html");
        self.message = Some(
            match std::fs::write(&path, Export::Html.render(&report))
                .and_then(|()| open_with_os(&path))
            {
                Ok(()) => format!("opened {}", path.display()),
                Err(e) => format!("cannot open the report: {e}"),
            },
        );
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, id: egui::Id) -> Vec<TestsAction> {
        let mut actions = Vec::new();
        self.toolbar(ui, &mut actions);
        ui.separator();
        egui::SidePanel::left(id.with("tree"))
            .resizable(true)
            .default_width(250.0)
            .width_range(180.0..=520.0)
            .show_inside(ui, |ui| self.tree_ui(ui, &mut actions));
        egui::CentralPanel::default().show_inside(ui, |ui| self.details_ui(ui, &mut actions));
        actions
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<TestsAction>) {
        let running = self.running();
        let has_selection = self.selection != Selection::None;
        let has_failed = self.has_failed();
        let has_report = self.report().is_some();
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!running, |ui| {
                if icons::icon_text_button(ui, icons::play(), "Run all").clicked() {
                    actions.push(TestsAction::RunAll);
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Run selected"))
                    .on_hover_text("Run the selected module or case")
                    .clicked()
                {
                    actions.push(TestsAction::RunSelected);
                }
                if ui
                    .add_enabled(has_failed, egui::Button::new("Run failed"))
                    .clicked()
                {
                    actions.push(TestsAction::RunFailed);
                }
            });
            if icons::icon_button_enabled(ui, running, icons::stop(), "Stop the run").clicked() {
                actions.push(TestsAction::Stop);
            }
            ui.separator();
            ui.add_enabled_ui(!running, |ui| {
                ui.label("Seed");
                ui.add(egui::DragValue::new(&mut self.seed).speed(1.0))
                    .on_hover_text("Seed of fault injection and message control; the same seed gives the same result");
                ui.checkbox(&mut self.stream, "Stream frames to trace")
                    .on_hover_text(
                        "Show the frames of every case in the Trace windows, one case after the other",
                    );
            });
            ui.separator();
            ui.add_enabled_ui(has_report, |ui| {
                ui.menu_button("Export", |ui| {
                    if ui.button("HTML report...").clicked() {
                        self.export(Export::Html);
                        ui.close();
                    }
                    if ui.button("JUnit XML...").clicked() {
                        self.export(Export::Junit);
                        ui.close();
                    }
                    if ui.button("JSON...").clicked() {
                        self.export(Export::Json);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Open HTML report").clicked() {
                        self.open_html();
                        ui.close();
                    }
                });
            });
            if ui
                .button("Refresh")
                .on_hover_text("Read the module files again")
                .clicked()
            {
                actions.push(TestsAction::Refresh);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let [pass, fail, skip, err, not_run] = self.counts();
            if running {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label("Running...");
            }
            ui.colored_label(GREEN, format!("{pass} passed"));
            ui.colored_label(RED, format!("{fail} failed"));
            ui.weak(format!("{skip} skipped"));
            ui.colored_label(MAGENTA, format!("{err} errors"));
            ui.weak(format!("{not_run} not run"));
            if self.stopped && !running {
                ui.weak("(stopped)");
            }
            if let Some(m) = &self.message {
                ui.separator();
                ui.label(m);
            }
        });
    }

    fn tree_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<TestsAction>) {
        let running = self.running();
        let mut new_sel = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if self.modules.is_empty() {
                    ui.weak("No test modules in this project.");
                    ui.weak("Use \"+ New test module...\" in the project tree.");
                }
                for m in &self.modules {
                    let open_id = ui.make_persistent_id(("tests_module", &m.path));
                    let state = CollapsingState::load_with_default_open(ui.ctx(), open_id, true);
                    let header = state.show_header(ui, |ui| {
                        status_icon(ui, m.status, m.running);
                        let selected = self.selection == Selection::Module(m.path.clone());
                        let r = ui
                            .selectable_label(selected, format!("{} ({})", m.name, m.cases.len()));
                        if r.clicked() {
                            new_sel = Some(Selection::Module(m.path.clone()));
                        }
                        r.context_menu(|ui| {
                            if ui.add_enabled(!running, egui::Button::new("Run")).clicked() {
                                actions.push(TestsAction::RunModule(m.path.clone()));
                                ui.close();
                            }
                            if ui.button("Open in editor").clicked() {
                                actions.push(TestsAction::OpenEditor {
                                    path: m.path.clone(),
                                    line: None,
                                });
                                ui.close();
                            }
                        });
                    });
                    header.body(|ui| {
                        if let Some(e) = m.error.as_ref().or(m.list_error.as_ref()) {
                            ui.colored_label(MAGENTA, e);
                        }
                        for c in &m.cases {
                            ui.horizontal(|ui| {
                                status_icon(ui, c.status, c.running);
                                let selected = self.selection
                                    == Selection::Case(m.path.clone(), c.name.clone());
                                let r = ui.selectable_label(selected, &c.name);
                                if r.clicked() {
                                    new_sel = Some(Selection::Case(m.path.clone(), c.name.clone()));
                                }
                                r.context_menu(|ui| {
                                    if ui.add_enabled(!running, egui::Button::new("Run")).clicked()
                                    {
                                        actions.push(TestsAction::RunCase(
                                            m.path.clone(),
                                            c.name.clone(),
                                        ));
                                        ui.close();
                                    }
                                });
                            });
                        }
                    });
                }
            });
        if let Some(s) = new_sel {
            self.selection = s;
            self.selected_step = None;
        }
    }

    fn details_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<TestsAction>) {
        let running = self.running();
        let module = |path: &str| self.modules.iter().find(|m| m.path == path);
        match &self.selection {
            Selection::None => {
                ui.weak("Select a module or a case to see its result.");
            }
            Selection::Module(path) => {
                if let Some(m) = module(path) {
                    module_details(ui, m, running, actions);
                }
            }
            Selection::Case(path, case) => {
                let found = module(path)
                    .and_then(|m| m.cases.iter().find(|c| &c.name == case).map(|c| (m, c)));
                if let Some((m, c)) = found {
                    case_details(ui, m, c, &mut self.selected_step, actions);
                }
            }
        }
    }
}

fn module_details(
    ui: &mut egui::Ui,
    m: &ModuleNode,
    running: bool,
    actions: &mut Vec<TestsAction>,
) {
    ui.horizontal(|ui| {
        status_icon(ui, m.status, m.running);
        ui.heading(&m.name);
        ui.colored_label(
            status_color(ui, m.status),
            status_label(m.status, m.running),
        );
    });
    ui.weak(&m.path);
    ui.add_space(4.0);
    if let Some(e) = m.error.as_ref().or(m.list_error.as_ref()) {
        error_box(ui, "Module error", e, MAGENTA);
    }
    let count = |s: Status| m.cases.iter().filter(|c| c.status == Some(s)).count();
    ui.label(format!(
        "{} cases: {} passed, {} failed, {} skipped, {} errors",
        m.cases.len(),
        count(Status::Pass),
        count(Status::Fail),
        count(Status::Skip),
        count(Status::Error)
    ));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new("Run module"))
            .clicked()
        {
            actions.push(TestsAction::RunModule(m.path.clone()));
        }
        if ui.button("Open in editor").clicked() {
            actions.push(TestsAction::OpenEditor {
                path: m.path.clone(),
                line: None,
            });
        }
    });
}

fn error_box(ui: &mut egui::Ui, title: &str, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .corner_radius(4.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(color, egui::RichText::new(title).strong());
            ui.label(text);
        });
}

fn case_details(
    ui: &mut egui::Ui,
    m: &ModuleNode,
    c: &CaseNode,
    selected_step: &mut Option<usize>,
    actions: &mut Vec<TestsAction>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                status_icon(ui, c.status, c.running);
                ui.heading(&c.name);
                ui.colored_label(
                    status_color(ui, c.status),
                    status_label(c.status, c.running),
                );
                ui.weak(format!("in {}", m.name));
            });
            let Some(r) = &c.result else {
                ui.weak(if c.running {
                    "Running..."
                } else {
                    "Not run yet."
                });
                return;
            };
            ui.label(format!(
                "Virtual time {:.1} ms, wall time {:.1} ms",
                r.virtual_duration_ms, r.wall_duration_ms
            ));
            ui.add_space(4.0);
            if let Some(f) = &r.failure {
                let (title, color) = if r.status == Status::Error {
                    ("Error", MAGENTA)
                } else {
                    ("Failed", RED)
                };
                egui::Frame::new()
                    .fill(color.gamma_multiply(0.14))
                    .stroke(egui::Stroke::new(1.0_f32, color))
                    .corner_radius(4.0)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.colored_label(color, egui::RichText::new(title).strong());
                        ui.label(&f.message);
                        ui.horizontal(|ui| {
                            ui.weak(format!("at t = {:.3} ms", f.time_ms));
                            match f.line {
                                Some(line) => {
                                    ui.weak(format!("line {line}"));
                                    if ui
                                        .small_button("Go to line")
                                        .on_hover_text("Open the module in the editor at this line")
                                        .clicked()
                                    {
                                        actions.push(TestsAction::OpenEditor {
                                            path: m.path.clone(),
                                            line: Some(line),
                                        });
                                    }
                                }
                                None => {
                                    ui.weak("line unknown");
                                }
                            }
                        });
                    });
                ui.add_space(6.0);
            }

            ui.strong(format!("Steps ({})", r.steps.len()));
            if c.trace_base_ns.is_none() {
                ui.weak("Run with \"Stream frames to trace\" to jump from a step to the trace.");
            } else {
                ui.weak("Click a step to show it in the Trace window.");
            }
            let mut clicked = None;
            TableBuilder::new(ui)
                .id_salt("tests_steps")
                .striped(true)
                .vscroll(false)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(86.0))
                .column(Column::exact(70.0))
                .column(Column::remainder().at_least(120.0).clip(true))
                .column(Column::exact(28.0))
                .header(18.0, |mut h| {
                    for t in ["Time (ms)", "Kind", "Text", "OK"] {
                        h.col(|ui| {
                            ui.strong(t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(18.0, r.steps.len(), |mut row| {
                        let i = row.index();
                        let s = &r.steps[i];
                        row.set_selected(*selected_step == Some(i));
                        row.col(|ui| {
                            ui.monospace(format!("{:.3}", s.time_ms));
                        });
                        row.col(|ui| {
                            ui.label(&s.kind);
                        });
                        row.col(|ui| {
                            let t = egui::RichText::new(&s.text);
                            ui.label(if s.ok { t } else { t.color(RED) });
                        });
                        row.col(|ui| ok_icon(ui, s.ok));
                        if row.response().clicked() {
                            clicked = Some(i);
                        }
                    });
                });
            if let Some(i) = clicked {
                *selected_step = Some(i);
                if let Some(base) = c.trace_base_ns {
                    let ns = (r.steps[i].time_ms * 1e6).max(0.0) as u64;
                    actions.push(TestsAction::JumpTrace(base + ns));
                }
            }

            if !r.trace_extract.is_empty() {
                ui.add_space(8.0);
                ui.strong(format!(
                    "Bus traffic before the failure ({})",
                    r.trace_extract.len()
                ));
                TableBuilder::new(ui)
                    .id_salt("tests_extract")
                    .striped(true)
                    .vscroll(false)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(86.0))
                    .column(Column::exact(70.0))
                    .column(Column::exact(30.0))
                    .column(Column::exact(70.0))
                    .column(Column::exact(34.0))
                    .column(Column::remainder().at_least(120.0).clip(true))
                    .column(Column::exact(90.0))
                    .header(18.0, |mut h| {
                        for t in ["Time (ms)", "Bus", "Dir", "ID", "DLC", "Data", "Sender"] {
                            h.col(|ui| {
                                ui.strong(t);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(18.0, r.trace_extract.len(), |mut row| {
                            let t = &r.trace_extract[row.index()];
                            let color = t.error.is_some().then_some(RED);
                            let mono = |ui: &mut egui::Ui, s: String| {
                                let t = egui::RichText::new(s).monospace();
                                ui.label(match color {
                                    Some(c) => t.color(c),
                                    None => t,
                                });
                            };
                            row.col(|ui| mono(ui, format!("{:.3}", t.time_ms)));
                            row.col(|ui| {
                                ui.label(&t.bus);
                            });
                            row.col(|ui| {
                                ui.label(&t.dir);
                            });
                            row.col(|ui| {
                                mono(
                                    ui,
                                    if t.ext {
                                        format!("{:08X}", t.id)
                                    } else {
                                        format!("{:03X}", t.id)
                                    },
                                )
                            });
                            row.col(|ui| mono(ui, t.dlc.to_string()));
                            row.col(|ui| {
                                let data = match &t.error {
                                    Some(e) => format!("error: {e}"),
                                    None => t
                                        .data
                                        .iter()
                                        .map(|b| format!("{b:02X}"))
                                        .collect::<Vec<_>>()
                                        .join(" "),
                                };
                                mono(ui, data);
                            });
                            row.col(|ui| {
                                ui.label(&t.sender);
                            });
                        });
                    });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_test::{ModuleResult, Step};

    const SRC: &str = "fn test_a() {}\nfn test_b() {}\n";

    fn state() -> TestsState {
        let mut s = TestsState::default();
        s.sync_modules(&["tests/m.rhai".into()], |_| Ok(SRC.to_string()));
        s
    }

    fn case_status(s: &TestsState, case: &str) -> Option<Status> {
        s.modules[0]
            .cases
            .iter()
            .find(|c| c.name == case)
            .and_then(|c| c.status)
    }

    fn started(case: &str) -> TestEvent {
        TestEvent::Progress {
            ev: Progress::CaseStarted {
                module: "m".into(),
                case: case.into(),
            },
            base_ns: Some(5),
        }
    }

    fn result(name: &str, status: Status) -> CaseResult {
        CaseResult {
            name: name.into(),
            status,
            virtual_duration_ms: 1.0,
            wall_duration_ms: 1.0,
            steps: vec![Step {
                time_ms: 1.0,
                kind: "wait".into(),
                text: "wait 1 ms".into(),
                ok: true,
            }],
            failure: None,
            trace_extract: Vec::new(),
        }
    }

    #[test]
    fn tree_lists_cases_and_keeps_results_on_resync() {
        let mut s = state();
        assert_eq!(s.modules.len(), 1);
        assert_eq!(s.modules[0].name, "m");
        assert_eq!(s.modules[0].cases.len(), 2);
        s.modules[0].cases[0].status = Some(Status::Pass);
        s.mark_dirty();
        s.sync_modules(&["tests/m.rhai".into()], |_| {
            Ok("fn test_a() {}\nfn test_c() {}".into())
        });
        assert_eq!(case_status(&s, "test_a"), Some(Status::Pass));
        assert_eq!(s.modules[0].cases.len(), 2);
        assert_eq!(s.modules[0].cases[1].name, "test_c");
        // An unreadable or broken module lists no cases and says why.
        s.mark_dirty();
        s.sync_modules(&["tests/m.rhai".into()], |_| Err("gone".into()));
        assert!(s.modules[0].cases.is_empty());
        assert_eq!(s.modules[0].list_error.as_deref(), Some("gone"));
        s.mark_dirty();
        s.sync_modules(&["tests/m.rhai".into()], |_| Ok("fn test_a( {".into()));
        assert!(s.modules[0].list_error.is_some());
    }

    #[test]
    fn progress_events_update_the_tree() {
        let mut s = state();
        s.apply(TestEvent::Progress {
            ev: Progress::ModuleStarted { module: "m".into() },
            base_ns: None,
        });
        assert!(s.modules[0].running);
        s.apply(started("test_a"));
        assert!(s.modules[0].cases[0].running);
        assert_eq!(s.modules[0].cases[0].trace_base_ns, Some(5));
        s.apply(TestEvent::Progress {
            ev: Progress::CaseFinished {
                module: "m".into(),
                case: "test_a".into(),
                status: Status::Fail,
            },
            base_ns: None,
        });
        assert!(!s.modules[0].cases[0].running);
        assert_eq!(case_status(&s, "test_a"), Some(Status::Fail));
        assert_eq!(s.modules[0].status, Some(Status::Fail));
        assert_eq!(s.counts(), [0, 1, 0, 0, 1]);
        assert!(s.has_failed());
    }

    #[test]
    fn report_fills_results_and_exports() {
        let mut s = state();
        s.apply(TestEvent::Report(Box::new(RunReport {
            project: "p".into(),
            started: "2026-01-01T00:00:00Z".into(),
            duration_ms: 3,
            stopped: false,
            modules: vec![ModuleResult {
                path: "tests/m.rhai".into(),
                name: "m".into(),
                status: Status::Pass,
                error: None,
                cases: vec![
                    result("test_a", Status::Pass),
                    result("test_b", Status::Skip),
                ],
            }],
            totals: Totals::default(),
        })));
        assert_eq!(s.modules[0].status, Some(Status::Pass));
        let r = s.report().unwrap();
        assert_eq!(
            (r.totals.cases, r.totals.passed, r.totals.skipped),
            (2, 1, 1)
        );
        assert_eq!(r.started, "2026-01-01T00:00:00Z");
        assert!(operow_test::to_html(&r).contains("test_a"));
        // A module error shows up and counts as failed work to redo.
        s.apply(TestEvent::Report(Box::new(RunReport {
            project: "p".into(),
            started: String::new(),
            duration_ms: 0,
            stopped: false,
            modules: vec![ModuleResult {
                path: "tests/m.rhai".into(),
                name: "m".into(),
                status: Status::Error,
                error: Some("compile error".into()),
                cases: Vec::new(),
            }],
            totals: Totals::default(),
        })));
        assert_eq!(s.modules[0].status, Some(Status::Error));
        assert!(s.has_failed());
    }

    #[test]
    fn jobs_follow_selection_and_failures() {
        let mut s = state();
        assert!(s.jobs_selected().is_empty());
        assert_eq!(s.jobs_all().len(), 1);
        s.selection = Selection::Case("tests/m.rhai".into(), "test_b".into());
        assert_eq!(
            s.jobs_selected(),
            [Job {
                path: "tests/m.rhai".into(),
                case: Some("test_b".into())
            }]
        );
        s.modules[0].cases[1].status = Some(Status::Error);
        assert_eq!(s.jobs_failed().len(), 1);
        assert_eq!(case_filter("tests/m.rhai", "test_b"), "m::test_b");
    }

    #[test]
    fn frames_pass_through_apply() {
        let mut s = state();
        assert!(s.apply(TestEvent::Frames(Vec::new())).is_empty());
        s.apply(TestEvent::Done);
        assert!(!s.running());
    }

    /// A real run over the gateway example: events flow in, the tree ends
    /// with results, and streamed frames are shifted so cases do not overlap.
    #[test]
    fn background_run_streams_frames_sequentially() {
        let path = format!(
            "{}/../../examples/gateway.operow.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let project = Project::load(&path).unwrap();
        let src = "fn test_a() { wait_for_message_on(\"Body\", 0x100, 50); }\n\
                   fn test_b() { wait_for_message_on(\"Body\", 0x100, 50); }\n";
        let mut s = TestsState {
            stream: true,
            ..TestsState::default()
        };
        s.sync_modules(&["t.rhai".into()], |_| Ok(src.to_string()));
        let jobs = s.jobs_all();
        s.start(
            project,
            vec![("t.rhai".to_string(), Ok(src.to_string()))],
            jobs,
            1_000,
            &egui::Context::default(),
        );
        assert!(s.running());
        let mut frames = Vec::new();
        let t0 = std::time::Instant::now();
        while s.running() && t0.elapsed().as_secs() < 30 {
            frames.extend(s.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!s.running());
        assert_eq!(case_status(&s, "test_a"), Some(Status::Pass));
        assert_eq!(case_status(&s, "test_b"), Some(Status::Pass));
        let a = s.modules[0].cases[0].trace_base_ns.unwrap();
        let b = s.modules[0].cases[1].trace_base_ns.unwrap();
        assert_eq!(a, 1_000);
        assert!(b > a + CASE_GAP_NS, "{a} {b}");
        assert!(!frames.is_empty());
        // Every frame of case 2 lies after the first frame offset by `b`.
        assert!(frames.iter().all(|e| e.time.0 >= a));
        assert!(frames.iter().any(|e| e.time.0 >= b));
    }
}
