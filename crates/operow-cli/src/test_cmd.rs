//! `operow-cli test`.

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use operow_test::{
    ModuleResult, Progress, Project, RunOptions, RunReport, Status, TestRunner, to_html, to_json,
    to_junit,
};

use crate::TestArgs;

fn mark(s: Status) -> &'static str {
    match s {
        Status::Pass => "\u{2713}",
        Status::Fail => "\u{2717}",
        Status::Skip => "\u{25CB}",
        Status::Error => "!",
    }
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Print a finished module the way live progress would.
fn print_module(m: &ModuleResult, quiet: bool) {
    if quiet {
        return;
    }
    println!("{}", m.name);
    for c in &m.cases {
        println!(
            "  {} {} ({:.1} ms)",
            mark(c.status),
            c.name,
            c.virtual_duration_ms
        );
    }
    if let Some(e) = &m.error {
        println!("  ! {e}");
    }
}

pub fn run(a: &TestArgs) -> Result<ExitCode, String> {
    let project = Project::load(&a.project).map_err(|e| e.to_string())?;
    if project.tests.is_empty() {
        return Err(format!("{} lists no test modules", a.project.display()));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let jobs = a.jobs.max(1).min(project.tests.len());
    let mut options = RunOptions {
        seed: a.seed,
        per_test_timeout_ms: a.timeout_ms,
        stop_flag: stop.clone(),
        ..RunOptions::default()
    };
    let fail_fast = a.fail_fast;
    let quiet = a.quiet;
    let live = jobs == 1;
    let fast_stop = stop.clone();
    options.progress = Some(Arc::new(move |p: &Progress| match p {
        Progress::ModuleStarted { module } if live && !quiet => println!("{module}"),
        Progress::CaseFinished { case, status, .. } => {
            if live && !quiet {
                println!("  {} {case}", mark(*status));
            }
            if fail_fast && matches!(status, Status::Fail | Status::Error) {
                fast_stop.store(true, Ordering::Relaxed);
            }
        }
        _ => {}
    }));

    let t0 = Instant::now();
    let mut report = if live {
        TestRunner::new(project, options).run(a.filter.as_deref())
    } else {
        run_parallel(&project, &options, a.filter.as_deref(), jobs, t0)?
    };
    if !live {
        for m in &report.modules {
            print_module(m, quiet);
        }
    }
    if fail_fast && report.stopped {
        // Cases cut short by the stop flag are not results.
        for m in &mut report.modules {
            m.cases.retain(|c| {
                c.failure
                    .as_ref()
                    .is_none_or(|f| c.status != Status::Error || f.message != "run stopped")
            });
        }
        report.recompute_totals();
    }

    print_summary(&report);
    if let Some(dir) = &a.report {
        let file = dir.join("report.html");
        write_file(&file, &to_html(&report))?;
        println!("HTML report: {}", file.display());
    }
    if let Some(f) = &a.junit {
        write_file(f, &to_junit(&report))?;
        println!("JUnit report: {}", f.display());
    }
    if let Some(f) = &a.json {
        write_file(f, &to_json(&report))?;
        println!("JSON report: {}", f.display());
    }
    Ok(ExitCode::from(exit_code(&report)))
}

fn exit_code(r: &RunReport) -> u8 {
    if r.totals.errors > 0 {
        2
    } else if r.totals.failed > 0 {
        1
    } else {
        0
    }
}

/// One runner per module, `jobs` at a time, merged in module order.
fn run_parallel(
    project: &Project,
    options: &RunOptions,
    filter: Option<&str>,
    jobs: usize,
    t0: Instant,
) -> Result<RunReport, String> {
    let next = Mutex::new(0usize);
    let slots: Vec<Mutex<Option<RunReport>>> =
        project.tests.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap_or_else(|e| e.into_inner());
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(module) = project.tests.get(i) else {
                        break;
                    };
                    if options.stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut p = project.clone();
                    p.tests = vec![module.clone()];
                    let r = TestRunner::new(p, options.clone()).run(filter);
                    *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                }
            });
        }
    });
    let mut parts: Vec<RunReport> = slots
        .into_iter()
        .filter_map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect();
    if parts.is_empty() {
        // Every module was filtered out or the run was stopped at once.
        parts.push(
            TestRunner::new(
                Project {
                    tests: Vec::new(),
                    ..project.clone()
                },
                options.clone(),
            )
            .run(filter),
        );
    }
    RunReport::merge(parts, t0.elapsed().as_millis() as u64).ok_or_else(|| "no report".into())
}

fn print_summary(r: &RunReport) {
    let bad: Vec<_> = r
        .modules
        .iter()
        .flat_map(|m| {
            let module_err = m
                .error
                .iter()
                .map(|e| (format!("{} (module)", m.name), e.clone(), None));
            let cases = m.cases.iter().filter_map(|c| {
                let f = c.failure.as_ref()?;
                Some((format!("{}::{}", m.name, c.name), f.message.clone(), f.line))
            });
            module_err.chain(cases).collect::<Vec<_>>()
        })
        .collect();
    if !bad.is_empty() {
        println!("\nFailures:");
        for (name, msg, line) in bad {
            match line {
                Some(l) => println!("  {name}: {msg} (line {l})"),
                None => println!("  {name}: {msg}"),
            }
        }
    }
    let t = &r.totals;
    println!(
        "\n{} modules, {} cases: {} passed, {} failed, {} skipped, {} errors in {:.2} s{}",
        t.modules,
        t.cases,
        t.passed,
        t.failed,
        t.skipped,
        t.errors,
        r.duration_ms as f64 / 1e3,
        if r.stopped { " (stopped early)" } else { "" }
    );
}
