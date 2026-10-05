use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use operow_core::BusId;
use operow_core::DbcRef;

use crate::runner::glob;
use crate::*;

fn example(name: &str) -> Project {
    Project::load(format!(
        "{}/../../examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn gateway() -> Project {
    example("gateway.operow.json")
}

fn run_with(project: Project, src: &str, opts: RunOptions) -> RunReport {
    TestRunner::new(project, opts).run_sources(&[("tests/t.rhai", src)], None)
}

fn run(project: Project, src: &str) -> RunReport {
    run_with(project, src, RunOptions::default())
}

fn case<'a>(r: &'a RunReport, name: &str) -> &'a CaseResult {
    r.modules
        .iter()
        .flat_map(|m| &m.cases)
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no case {name}: {r:#?}"))
}

/// Table of `(name, body, expectation)`: `P` pass, `S` skip, `F:text` fail
/// and `E:text` error with `text` in the message.
fn check_table(project: Project, table: &[(&str, &str, &str)]) {
    let mut src = String::new();
    for (name, body, _) in table {
        src += &format!("fn test_{name}() {{\n{body}\n}}\n");
    }
    let r = run(project, &src);
    assert_eq!(r.modules.len(), 1, "{r:#?}");
    assert!(r.modules[0].error.is_none(), "{:?}", r.modules[0].error);
    for (name, _, want) in table {
        let c = case(&r, &format!("test_{name}"));
        let got = c.failure.as_ref().map_or("", |f| f.message.as_str());
        let (status, text) = match want.split_once(':') {
            Some(("F", t)) => (Status::Fail, t),
            Some(("E", t)) => (Status::Error, t),
            _ if *want == "S" => (Status::Skip, ""),
            _ => (Status::Pass, ""),
        };
        assert_eq!(c.status, status, "{name}: {:?}", c.failure);
        assert!(got.contains(text), "{name}: {got:?} lacks {text:?}");
    }
}

#[test]
fn project_loads_tests_and_dbcs() {
    let p = example("dbc_demo.operow.json");
    assert_eq!(p.tests.len(), 1);
    assert_eq!(p.tests[0].name(), "dbc_tests");
    assert!(p.tests[0].file.ends_with("tests/dbc_tests.rhai"));
    assert_eq!(p.dbcs.by_bus.len(), 1);
    assert_eq!(gateway().tests.len(), 2);
    assert!(Project::load("/nonexistent/x.json").is_err());
}

#[test]
fn api_gateway_table() {
    check_table(
        gateway(),
        &[
            ("wait", "wait(10); wait(2.5); expect_eq(now_ms(), 12);", "P"),
            (
                "wfm",
                "let f = wait_for_message(0x100, 50); expect_eq(f.id, 0x100); expect_true(f.time_ms >= 0.0);",
                "P",
            ),
            (
                "wfm_on",
                "let f = wait_for_message_on(\"Body\", 0x100, 50); expect_eq(f.bus, \"Body\");",
                "P",
            ),
            (
                "eq",
                "expect_eq(1, 1); expect_eq(1, 1.0); expect_eq(\"a\", \"a\"); expect_eq([1, 2], [1, 2]); expect_eq(#{a: 1}, #{a: 1});",
                "P",
            ),
            ("ne", "expect_ne(1, 2); expect_ne(\"a\", \"b\");", "P"),
            (
                "lt_gt",
                "expect_lt(1, 2); expect_gt(2.5, 2); expect_lt(\"a\", \"b\");",
                "P",
            ),
            ("near", "expect_near(1.0, 1.05, 0.1);", "P"),
            (
                "true",
                "expect_true(true); expect_true(true, \"yes\");",
                "P",
            ),
            ("no_msg", "expect_no_message(0x555, 30);", "P"),
            (
                "no_msg_on",
                "expect_no_message_on(\"Body\", 0x555, 30);",
                "P",
            ),
            (
                "cycle",
                "let m = expect_cycle_time(0x100, 10, 5, 200); expect_near(m, 10.0, 0.5);",
                "P",
            ),
            (
                "send",
                "send(\"Powertrain\", 0x1F0, [9]); let f = wait_for_message_on(\"Body\", 0x1F0, 20); expect_eq(f.data, [9]);",
                "P",
            ),
            (
                "send_ext",
                "send(\"Powertrain\", 0x18DA00F1, [1]); let f = wait_for_message_on(\"Powertrain\", 0x18DA00F1, 5); expect_true(f.extended); expect_eq(f.sender, \"Test\");",
                "P",
            ),
            (
                "set_payload",
                "set_payload(\"Engine\", 0, [1, 2, 3, 4, 5, 6, 7, 8]); let f = wait_for_message_on(\"Powertrain\", 0x100, 20); expect_eq(f.data, [1, 2, 3, 4, 5, 6, 7, 8]);",
                "P",
            ),
            (
                "set_payload_name",
                "set_payload(\"Engine\", \"EngineData\", [1]);",
                "P",
            ),
            (
                "trigger",
                "trigger(\"Engine\", 0); trigger(\"Engine\", \"EngineData\");",
                "P",
            ),
            (
                "inject_variants",
                "inject_errors(#{bus: \"Powertrain\", every: 2, kind: \"bit\", limit: 3}); inject_errors(#{bus: \"Powertrain\", probability: 0.1, kind: \"stuff\"}); inject_errors(#{bus: \"Body\", node: \"Body\", id: 0x200, kind: \"ack\", count: 1}); wait(20);",
                "P",
            ),
            (
                "offline",
                "node_offline(\"Gateway\"); node_online(\"Gateway\");",
                "P",
            ),
            (
                "bus_off",
                "force_bus_off(\"Engine\", \"Powertrain\"); expect_eq(node_state(\"Engine\", \"Powertrain\"), \"BusOff\"); expect_gt(node_tec(\"Engine\", \"Powertrain\"), 255); expect_eq(node_rec(\"Engine\", \"Powertrain\"), 0); recover_bus_off(\"Engine\", \"Powertrain\");",
                "P",
            ),
            (
                "msg_control",
                "msg_control(\"Engine\", 0x100, #{drop_pct: 100.0}); expect_no_message_on(\"Powertrain\", 0x100, 50); msg_control(\"Engine\", 0x100, #{delay_ms: 2, jitter_ms: 1});",
                "P",
            ),
            (
                "last_frame",
                "expect_eq(last_frame(\"Powertrain\", 0x100), ()); wait(5); let f = last_frame(\"Powertrain\", 0x100); expect_eq(f.dlc, 8);",
                "P",
            ),
            ("log", "log(\"hello\"); print(\"printed\");", "P"),
            ("skip", "skip(\"later\"); expect_eq(1, 2);", "S"),
            ("fail", "fail(\"boom\");", "F:boom"),
            ("bad_eq", "expect_eq(1, 2);", "F:expect_eq failed: 1 != 2"),
            ("bad_eq_type", "expect_eq(\"1\", 1);", "F:expect_eq failed"),
            ("bad_ne", "expect_ne(1, 1);", "F:expect_ne failed"),
            ("bad_lt", "expect_lt(2, 1);", "F:expect_lt failed"),
            ("bad_gt", "expect_gt(1, 2);", "F:expect_gt failed"),
            (
                "bad_near",
                "expect_near(1.0, 2.0, 0.5);",
                "F:expect_near failed",
            ),
            (
                "bad_true",
                "expect_true(false, \"custom msg\");",
                "F:custom msg",
            ),
            ("bad_true2", "expect_true(false);", "F:expect_true failed"),
            (
                "bad_wfm",
                "wait_for_message(0x777, 20);",
                "F:timeout after 20 ms waiting for message 0x777",
            ),
            (
                "bad_wfm_on",
                "wait_for_message_on(\"Body\", 0x600, 20);",
                "F:on Body",
            ),
            (
                "bad_no_msg",
                "expect_no_message(0x100, 50);",
                "F:unexpected message 0x100",
            ),
            (
                "bad_cycle",
                "expect_cycle_time(0x100, 50, 5, 200);",
                "F:cycle time of 0x100",
            ),
            (
                "bad_cycle_none",
                "expect_cycle_time(0x777, 10, 5, 100);",
                "F:need at least 2",
            ),
            (
                "bad_offline",
                "node_offline(\"Gateway\"); wait_for_message_on(\"Body\", 0x100, 30);",
                "F:timeout",
            ),
            (
                "err_bus",
                "send(\"Nope\", 1, []);",
                "E:unknown bus \"Nope\"",
            ),
            (
                "err_node",
                "node_offline(\"Zed\");",
                "E:unknown node \"Zed\"",
            ),
            (
                "err_bytes",
                "send(\"Powertrain\", 1, [300]);",
                "E:byte arrays",
            ),
            (
                "err_id",
                "send(\"Powertrain\", -1, []);",
                "E:invalid CAN id",
            ),
            (
                "err_kind",
                "inject_errors(#{bus: \"Powertrain\", kind: \"zzz\"});",
                "E:unknown error kind",
            ),
            ("err_uds", "uds(\"Engine\", [0x22]);", "E:no diagnostics"),
            ("err_signal_no_dbc", "signal(\"A.B\");", "E:unknown signal"),
            ("err_undefined", "no_such_function();", "E:no_such_function"),
            ("err_msg_index", "trigger(\"Engine\", 7);", "E:out of range"),
        ],
    );
}

#[test]
fn api_dbc_table() {
    check_table(
        example("dbc_demo.operow.json"),
        &[
            (
                "wfs",
                "let v = wait_for_signal(\"EngineData.EngineSpeed\", \">\", 0, 100); expect_true(v > 0.0);",
                "P",
            ),
            (
                "signal_value",
                "wait(5); expect_eq(signal(\"EngineData.CoolantTemp\"), 87); expect_eq(signal(\"Powertrain::EngineData.CoolantTemp\"), 87);",
                "P",
            ),
            (
                "expect_signal",
                "wait(5); expect_signal(\"EngineData.CoolantTemp\", \"==\", 87); expect_signal(\"EngineData.CoolantTemp\", \">=\", 87); expect_signal(\"EngineData.CoolantTemp\", \"!=\", 1); expect_signal(\"EngineData.CoolantTemp\", \"<\", 88); expect_signal(\"EngineData.CoolantTemp\", \"<=\", 87); expect_signal(\"EngineData.CoolantTemp\", \">\", 0);",
                "P",
            ),
            (
                "set_signal",
                "set_signal(\"DashCmd.Brightness\", 99); wait_for_signal(\"DashCmd.Brightness\", \"==\", 99, 10);",
                "P",
            ),
            (
                "unseen",
                "expect_eq(signal(\"DashCmd.Brightness\"), ());",
                "P",
            ),
            (
                "bad_wfs",
                "wait_for_signal(\"DashCmd.Brightness\", \"==\", 5, 20);",
                "F:timeout after 20 ms waiting for signal DashCmd.Brightness == 5 (never seen)",
            ),
            (
                "bad_expect_signal",
                "wait(5); expect_signal(\"EngineData.CoolantTemp\", \"==\", 1);",
                "F:signal EngineData.CoolantTemp is 87",
            ),
            (
                "bad_unseen",
                "expect_signal(\"DashCmd.Brightness\", \"==\", 1);",
                "F:has not been observed",
            ),
            (
                "err_unknown",
                "signal(\"EngineData.Nope\");",
                "E:unknown signal",
            ),
            ("err_format", "signal(\"nodot\");", "E:bad signal name"),
            (
                "err_op",
                "expect_signal(\"EngineData.CoolantTemp\", \"=~\", 1);",
                "E:unknown comparison",
            ),
            (
                "err_bus",
                "signal(\"Nope::EngineData.CoolantTemp\");",
                "E:unknown bus",
            ),
        ],
    );
}

#[test]
fn ambiguous_signal_is_an_error() {
    let base = example("dbc_demo.operow.json");
    let mut topo = base.topology.clone();
    topo.buses.push(topo.buses[0].clone());
    topo.buses[1].id = BusId(2);
    topo.buses[1].name = "Chassis".into();
    topo.databases.push(DbcRef {
        path: "sample.dbc".into(),
        bus: BusId(2),
    });
    let p = Project::from_topology("amb", topo, base.dir.as_deref()).unwrap();
    check_table(
        p,
        &[
            (
                "amb",
                "signal(\"EngineData.EngineSpeed\");",
                "E:ambiguous signal",
            ),
            (
                "qualified",
                "signal(\"Chassis::EngineData.EngineSpeed\");",
                "P",
            ),
        ],
    );
}

#[test]
fn api_uds_table() {
    check_table(
        example("diag_demo.operow.json"),
        &[
            (
                "ok",
                "let r = uds(\"Engine\", [0x10, 0x01]); expect_eq(r, [0x50, 0x01, 0x00, 0x32, 0x01, 0xF4]);",
                "P",
            ),
            (
                "nrc",
                "uds_expect_nrc(\"Engine\", [0x22, 0xFF, 0xFF], 0x31);",
                "P",
            ),
            (
                "bad_nrc",
                "uds_expect_nrc(\"Engine\", [0x22, 0xFF, 0xFF], 0x11);",
                "F:expected NRC 0x11",
            ),
            (
                "bad_nrc_positive",
                "uds_expect_nrc(\"Engine\", [0x10, 0x01], 0x11);",
                "F:expected NRC",
            ),
            (
                "err_node",
                "uds(\"Dashboard\", [0x10, 0x01]);",
                "E:no diagnostics",
            ),
            (
                "err_unknown_node",
                "uds(\"Zed\", [0x10, 0x01]);",
                "E:unknown node",
            ),
        ],
    );
}

#[test]
fn per_test_timeout_message() {
    let opts = RunOptions {
        per_test_timeout_ms: 50,
        ..RunOptions::default()
    };
    let r = run_with(
        gateway(),
        "fn test_a() { wait(100); }\nfn test_b() { wait_for_message(0x777, 1000); }\nfn test_c() { wait(50); }",
        opts,
    );
    for n in ["test_a", "test_b"] {
        let c = case(&r, n);
        assert_eq!(c.status, Status::Fail);
        assert!(
            c.failure
                .as_ref()
                .unwrap()
                .message
                .contains("per-test timeout of 50 ms")
        );
    }
    assert_eq!(case(&r, "test_c").status, Status::Pass);
}

#[test]
fn teardown_runs_after_failure_and_setup_failure() {
    let src = r#"
fn setup() { log("setup"); if now_ms() < 0 { fail("x"); } }
fn teardown() { log("teardown ran"); }
fn test_fails() { log("body"); fail("nope"); }
"#;
    let r = run(gateway(), src);
    let c = case(&r, "test_fails");
    assert_eq!(c.status, Status::Fail);
    let logs: Vec<&str> = c
        .steps
        .iter()
        .filter(|s| s.kind == "log")
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(logs, ["setup", "body", "teardown ran"]);

    let src = r#"
fn setup() { fail("setup broke"); }
fn teardown() { log("teardown ran"); }
fn test_x() { log("body"); }
"#;
    let r = run(gateway(), src);
    let c = case(&r, "test_x");
    assert_eq!(c.status, Status::Fail);
    assert!(c.steps.iter().any(|s| s.text == "teardown ran"));
    assert!(!c.steps.iter().any(|s| s.text == "body"));
}

#[test]
fn teardown_failure_fails_a_passing_case() {
    let r = run(
        gateway(),
        "fn teardown() { expect_eq(1, 2); }\nfn test_x() { }",
    );
    let c = case(&r, "test_x");
    assert_eq!(c.status, Status::Fail);
    assert!(
        c.failure
            .as_ref()
            .unwrap()
            .message
            .starts_with("teardown failed")
    );
}

#[test]
fn compile_error_reports_line() {
    let r = run(gateway(), "fn test_a() {\n  let x = ;\n}\n");
    let m = &r.modules[0];
    assert_eq!(m.status, Status::Error);
    let e = m.error.as_ref().unwrap();
    assert!(e.contains("compile error") && e.contains("line 2"), "{e}");
    assert!(m.cases.is_empty());
    assert_eq!(r.totals.errors, 1);
    assert!(!r.success());
}

#[test]
fn failure_line_and_nested_helper() {
    let src = "fn helper() {\n  expect_eq(1, 2);\n}\n\nfn test_a() {\n  log(\"x\");\n  helper();\n}\nfn test_b() {\n  let a = 1;\n  fail(\"here\");\n}\n";
    let r = run(gateway(), src);
    assert_eq!(case(&r, "test_a").failure.as_ref().unwrap().line, Some(2));
    assert_eq!(case(&r, "test_b").failure.as_ref().unwrap().line, Some(11));
}

#[test]
fn script_cannot_swallow_a_failure() {
    let r = run(gateway(), "fn test_a() { try { fail(\"x\"); } catch { } }");
    assert_eq!(case(&r, "test_a").status, Status::Fail);
}

#[test]
fn endless_loop_hits_operation_limit() {
    let opts = RunOptions {
        max_operations: 10_000,
        ..RunOptions::default()
    };
    let r = run_with(gateway(), "fn test_a() { loop { } }\nfn test_b() { }", opts);
    let c = case(&r, "test_a");
    assert_eq!(c.status, Status::Error);
    assert!(
        c.failure
            .as_ref()
            .unwrap()
            .message
            .contains("operation limit")
    );
    assert_eq!(case(&r, "test_b").status, Status::Pass);
}

#[test]
fn cases_in_source_order() {
    let src = "// fn test_comment() {}\nfn test_zeta() { }\nfn helper() { }\nfn test_alpha() { let s = \"fn test_str() {}\"; }\nfn test_mid() { }\n";
    let r = run(gateway(), src);
    let names: Vec<&str> = r.modules[0].cases.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["test_zeta", "test_alpha", "test_mid"]);
}

#[test]
fn fresh_sim_isolates_cases() {
    let src = "fn test_a() { node_offline(\"Gateway\"); wait(20); expect_gt(now_ms(), 19); }\nfn test_b() { expect_eq(now_ms(), 0); wait_for_message_on(\"Body\", 0x100, 50); }";
    let r = run(gateway(), src);
    assert_eq!(case(&r, "test_b").status, Status::Pass, "{r:#?}");
    // Sharing one simulation carries the offline gateway into test_b.
    let opts = RunOptions {
        fresh_sim_per_case: false,
        ..RunOptions::default()
    };
    let r = run_with(gateway(), src, opts);
    assert_eq!(case(&r, "test_a").status, Status::Pass);
    assert_eq!(case(&r, "test_b").status, Status::Fail);
}

#[test]
fn module_setup_and_teardown() {
    let src = "fn module_setup() { log(\"ms\"); }\nfn module_teardown() { log(\"mt\"); }\nfn test_a() { }";
    let r = run(gateway(), src);
    assert_eq!(r.modules[0].status, Status::Pass);
    let r = run(
        gateway(),
        "fn module_setup() { fail(\"nope\"); }\nfn test_a() { }",
    );
    let m = &r.modules[0];
    assert_eq!(m.status, Status::Error);
    assert!(
        m.error
            .as_ref()
            .unwrap()
            .contains("module_setup failed: nope")
    );
    assert!(m.cases.is_empty());
    let r = run(
        gateway(),
        "fn module_teardown() { fail(\"nope\"); }\nfn test_a() { }",
    );
    assert!(
        r.modules[0]
            .error
            .as_ref()
            .unwrap()
            .contains("module_teardown failed")
    );
    assert_eq!(r.modules[0].cases[0].status, Status::Pass);
}

#[test]
fn filter_globs() {
    let src = "fn test_alpha() { }\nfn test_beta() { }\nfn test_alpine() { }";
    let runner = TestRunner::new(gateway(), RunOptions::default());
    let names = |f: Option<&str>| -> Vec<String> {
        runner
            .run_sources(&[("tests/mod_a.rhai", src)], f)
            .modules
            .iter()
            .flat_map(|m| m.cases.iter().map(|c| c.name.clone()))
            .collect()
    };
    assert_eq!(names(None).len(), 3);
    assert_eq!(names(Some("test_al*")), ["test_alpha", "test_alpine"]);
    assert_eq!(names(Some("test_b?ta")), ["test_beta"]);
    assert_eq!(names(Some("mod_a")).len(), 3);
    assert_eq!(names(Some("mod_a::test_beta")), ["test_beta"]);
    assert_eq!(names(Some("mod_*::*pine")), ["test_alpine"]);
    assert!(names(Some("other::*")).is_empty());
    let r = runner.run_sources(&[("tests/mod_a.rhai", src)], Some("nothing"));
    assert!(r.modules.is_empty());
}

#[test]
fn glob_matcher() {
    assert!(glob("*", ""));
    assert!(glob("a*c", "abc") && glob("a*c", "ac") && !glob("a*c", "ab"));
    assert!(glob("A?C", "abc") && !glob("a?c", "ac"));
    assert!(glob("*b*", "abc") && !glob("*d*", "abc"));
}

#[test]
fn stop_flag_ends_the_run() {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let opts = RunOptions {
        stop_flag: stop,
        progress: Some(Arc::new(move |p| {
            if matches!(p, Progress::CaseFinished { .. }) {
                flag.store(true, Ordering::Relaxed);
            }
        })),
        ..RunOptions::default()
    };
    let r = run_with(
        gateway(),
        "fn test_a() { }\nfn test_b() { }\nfn test_c() { }",
        opts,
    );
    assert!(r.stopped);
    assert_eq!(r.modules[0].cases.len(), 1);

    // Set beforehand: nothing runs.
    let opts = RunOptions {
        stop_flag: Arc::new(AtomicBool::new(true)),
        ..RunOptions::default()
    };
    let r = run_with(gateway(), "fn test_a() { }", opts);
    assert!(r.stopped && r.modules.is_empty());
}

#[test]
fn stop_flag_interrupts_a_running_case() {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let opts = RunOptions {
        stop_flag: stop,
        progress: Some(Arc::new(move |p| {
            if matches!(p, Progress::CaseStarted { .. }) {
                flag.store(true, Ordering::Relaxed);
            }
        })),
        ..RunOptions::default()
    };
    let r = run_with(
        gateway(),
        "fn test_a() { wait(1000); fail(\"not reached\"); }",
        opts,
    );
    let c = &r.modules[0].cases[0];
    assert_eq!(c.status, Status::Error);
    assert_eq!(c.failure.as_ref().unwrap().message, "run stopped");
    assert!(c.virtual_duration_ms < 10.0);
}

#[test]
fn progress_events() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    let opts = RunOptions {
        progress: Some(Arc::new(move |p| l.lock().unwrap().push(p.clone()))),
        ..RunOptions::default()
    };
    run_with(
        gateway(),
        "fn test_a() { }\nfn test_b() { fail(\"x\"); }",
        opts,
    );
    let ev = log.lock().unwrap().clone();
    assert_eq!(ev.len(), 6);
    assert_eq!(ev[0], Progress::ModuleStarted { module: "t".into() });
    assert_eq!(
        ev[4],
        Progress::CaseFinished {
            module: "t".into(),
            case: "test_b".into(),
            status: Status::Fail
        }
    );
    assert_eq!(
        ev[5],
        Progress::ModuleFinished {
            module: "t".into(),
            status: Status::Fail
        }
    );
}

#[test]
fn trace_extract_on_failure_only() {
    let src = "fn test_fail() { wait(300); fail(\"x\"); }\nfn test_pass() { wait(300); }";
    let r = run(gateway(), src);
    assert!(case(&r, "test_pass").trace_extract.is_empty());
    let c = case(&r, "test_fail");
    assert!(!c.trace_extract.is_empty());
    let t = c.failure.as_ref().unwrap().time_ms;
    assert!(
        c.trace_extract
            .iter()
            .all(|row| row.time_ms >= t - 200.0 && row.time_ms <= t)
    );
    let row = c.trace_extract.iter().find(|r| r.id == 0x100).unwrap();
    assert_eq!(
        (row.bus.as_str(), row.sender.as_str(), row.dir.as_str()),
        ("Powertrain", "Engine", "Tx")
    );
    assert_eq!(row.data.len(), 8);
}

#[test]
fn trace_extract_shows_error_frames() {
    let src = "fn test_a() { inject_errors(#{bus: \"Powertrain\", kind: \"crc\", count: 1}); wait(5); fail(\"x\"); }";
    let r = run(gateway(), src);
    let c = case(&r, "test_a");
    assert!(
        c.trace_extract
            .iter()
            .any(|r| r.error.as_deref() == Some("CRC"))
    );
}

#[test]
fn steps_are_recorded() {
    let r = run(
        gateway(),
        "fn test_a() { wait(5); send(\"Powertrain\", 0x1F0, [1]); expect_eq(1, 1); }",
    );
    let c = case(&r, "test_a");
    let kinds: Vec<&str> = c.steps.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, ["wait", "send", "expect_eq"]);
    assert!(c.steps.iter().all(|s| s.ok));
    assert_eq!(c.steps[1].time_ms, 5.0);
    assert_eq!(c.virtual_duration_ms, 5.0);
}

const RANDOM_SRC: &str = r#"
fn test_random() {
    inject_errors(#{ bus: "Powertrain", probability: 0.3, kind: "crc" });
    msg_control("Engine", 0x100, #{ drop_pct: 40.0, jitter_ms: 2 });
    wait(300);
    log("tec " + node_tec("Engine", "Powertrain"));
    let f = wait_for_message_on("Powertrain", 0x100, 100);
    log("t " + f.time_ms);
}
fn test_cycle() { expect_cycle_time(0x100, 10, 5, 300); }
"#;

#[test]
fn same_seed_same_report() {
    let go = |seed: u64| {
        let opts = RunOptions {
            seed,
            ..RunOptions::default()
        };
        run_with(gateway(), RANDOM_SRC, opts)
    };
    let (a, b) = (go(42), go(42));
    assert_eq!(a.without_wall_times(), b.without_wall_times());
    let text = |r: &RunReport| format!("{:?}", r.modules[0].cases[0].steps);
    assert_ne!(text(&go(1)), text(&go(2)), "different seeds should differ");
    // Also with the examples.
    let proj = || TestRunner::new(gateway(), RunOptions::default()).run(None);
    assert_eq!(proj().without_wall_times(), proj().without_wall_times());
}

#[test]
fn report_serde_roundtrip() {
    let r = run(gateway(), "fn test_a() { fail(\"x\"); }\nfn test_b() { }");
    let json = serde_json::to_string(&r).unwrap();
    let back: RunReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    assert_eq!(
        r.totals,
        Totals {
            modules: 1,
            cases: 2,
            passed: 1,
            failed: 1,
            skipped: 0,
            errors: 0
        }
    );
    assert!(
        r.started.ends_with('Z') && r.started.len() == 20,
        "{}",
        r.started
    );
}

#[test]
fn unreadable_module_is_an_error() {
    let mut p = gateway();
    p.tests[0].file = "/nonexistent/x.rhai".into();
    p.tests.truncate(1);
    let r = TestRunner::new(p, RunOptions::default()).run(None);
    assert_eq!(r.modules[0].status, Status::Error);
    assert!(r.modules[0].error.as_ref().unwrap().contains("cannot read"));
}

#[test]
fn examples_all_pass() {
    for (name, skipped) in [
        ("gateway.operow.json", 1),
        ("diag_demo.operow.json", 0),
        ("dbc_demo.operow.json", 0),
    ] {
        let r = TestRunner::new(example(name), RunOptions::default()).run(None);
        let bad: Vec<String> = r
            .modules
            .iter()
            .flat_map(|m| m.cases.iter().map(move |c| (m, c)))
            .filter(|(_, c)| !matches!(c.status, Status::Pass | Status::Skip))
            .map(|(m, c)| format!("{}::{} {:?}", m.name, c.name, c.failure))
            .collect();
        assert!(
            r.success() && r.totals.cases > 0 && bad.is_empty(),
            "{name}: {bad:?} {:?}",
            r.modules.iter().map(|m| &m.error).collect::<Vec<_>>()
        );
        assert_eq!(r.totals.skipped, skipped, "{name}");
    }
}

#[test]
fn event_sink_receives_step_events() {
    let seen = Arc::new(Mutex::new(Vec::<operow_core::BusEvent>::new()));
    let s = seen.clone();
    let opts = RunOptions {
        event_sink: Some(Arc::new(move |evs| {
            s.lock().unwrap().extend_from_slice(evs)
        })),
        ..RunOptions::default()
    };
    let r = run_with(
        gateway(),
        "fn test_a() { wait_for_message_on(\"Body\", 0x100, 50); }",
        opts,
    );
    assert_eq!(case(&r, "test_a").status, Status::Pass);
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|e| e.frame.id == 0x100), "{}", seen.len());
    // Times are those of the case's own simulation, ascending per step.
    assert!(seen.iter().all(|e| e.time.0 <= 51_000_000));
}

#[test]
fn check_module_reports_compile_errors_only() {
    assert_eq!(
        check_module("fn test_a() { wait(5); unknown_api(1); }"),
        Ok(())
    );
    let e = check_module("fn test_a( {").unwrap_err();
    assert!(!e.is_empty());
}

#[test]
fn project_from_parts_keeps_given_dbcs() {
    let p = example("dbc_demo.operow.json");
    let q = Project::from_parts("mem", p.topology.clone(), p.dbcs.clone(), p.dir.as_deref());
    assert_eq!(q.tests.len(), 1);
    assert_eq!(q.dbcs.by_bus.len(), 1);
    assert_eq!(q.name, "mem");
}
