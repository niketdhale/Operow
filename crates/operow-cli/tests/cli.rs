//! Runs the built `operow-cli` binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_operow-cli"))
        .args(args)
        .output()
        .expect("run operow-cli")
}

fn example(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
        .display()
        .to_string()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exit code")
}

/// A copy of the gateway project in `dir` with the given test module.
fn project_with(dir: &Path, modules: &[(&str, &str)]) -> PathBuf {
    let src = std::fs::read_to_string(example("gateway.operow.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&src).unwrap();
    let names: Vec<String> = modules.iter().map(|(n, _)| format!("{n}.rhai")).collect();
    v["tests"] = serde_json::json!(names);
    let path = dir.join("p.operow.json");
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    for (n, body) in modules {
        std::fs::write(dir.join(format!("{n}.rhai")), body).unwrap();
    }
    path
}

fn totals(out: &str) -> &str {
    out.lines()
        .find(|l| l.contains(" modules, "))
        .map(|l| l.split(" in ").next().unwrap())
        .expect("summary line")
}

#[test]
fn version_and_help() {
    let o = cli(&["--version"]);
    assert_eq!(code(&o), 0);
    assert!(text(&o).starts_with("operow-cli "));
    let o = cli(&["--help"]);
    assert!(text(&o).contains("Examples:"));
}

#[test]
fn test_passes_and_writes_reports() {
    let dir = tempfile::tempdir().unwrap();
    let (report, junit, json) = (
        dir.path().join("rep"),
        dir.path().join("j/junit.xml"),
        dir.path().join("r.json"),
    );
    let o = cli(&[
        "test",
        &example("gateway.operow.json"),
        "--report",
        report.to_str().unwrap(),
        "--junit",
        junit.to_str().unwrap(),
        "--json",
        json.to_str().unwrap(),
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o));
    assert!(text(&o).contains("0 failed"));
    let html = std::fs::read_to_string(report.join("report.html")).unwrap();
    assert!(html.contains("test_forwards_engine_data"));
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.starts_with("<?xml") && xml.contains("<testsuite "));
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(j["totals"]["failed"], 0);
}

#[test]
fn failing_test_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let p = project_with(
        dir.path(),
        &[(
            "bad",
            "fn test_ok() { expect_eq(1, 1); }\nfn test_bad() { expect_eq(1, 2); }\nfn test_after() { expect_eq(1, 1); }\n",
        )],
    );
    let o = cli(&["test", p.to_str().unwrap()]);
    assert_eq!(code(&o), 1, "{}", text(&o));
    assert!(text(&o).contains("bad::test_bad"));
    let o = cli(&["test", p.to_str().unwrap(), "--fail-fast"]);
    assert_eq!(code(&o), 1, "{}", text(&o));
    assert!(!text(&o).contains("test_after"), "{}", text(&o));
}

#[test]
fn script_error_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let p = project_with(dir.path(), &[("broken", "fn test_x( {\n")]);
    let o = cli(&["test", p.to_str().unwrap()]);
    assert_eq!(code(&o), 2, "{}", text(&o));
    assert!(text(&o).contains("compile error"));
    let o = cli(&["test", dir.path().join("missing.json").to_str().unwrap()]);
    assert_eq!(code(&o), 2);
}

#[test]
fn parallel_matches_serial() {
    let p = example("gateway.operow.json");
    let serial = cli(&["test", &p]);
    let par = cli(&["test", &p, "--jobs", "4"]);
    assert_eq!(code(&serial), 0);
    assert_eq!(code(&par), 0);
    assert_eq!(totals(&text(&serial)), totals(&text(&par)));
    // Output order follows the project's module order.
    let t = text(&par);
    assert!(t.find("gateway_tests").unwrap() < t.find("fault_tests").unwrap());
}

#[test]
fn run_writes_a_readable_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("t.blf");
    let o = cli(&[
        "run",
        &example("gateway.operow.json"),
        "--duration",
        "1s",
        "--log",
        log.to_str().unwrap(),
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o));
    assert!(text(&o).contains("Powertrain"));
    let records: Vec<_> = operow_log::open_log(&log)
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let n = records.len();
    assert!(n > 100, "{n}");
}

#[test]
fn replay_csv_decodes_signals() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("o.csv");
    let o = cli(&[
        "replay",
        &example("sample_log.asc"),
        "--export",
        "csv",
        "--out",
        out.to_str().unwrap(),
        "--dbc",
        &example("sample.dbc"),
        "--dbc-bus-channel",
        "1",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o));
    let csv = std::fs::read_to_string(&out).unwrap();
    let mut lines = csv.lines();
    let header = lines.next().unwrap();
    assert!(header.starts_with("time_s,channel,dir,id_hex,ext,fd,dlc,data_hex,EngineData."));
    let first = lines.next().unwrap();
    assert!(first.starts_with("0.010115,1,Tx,100,0,0,8,28 16 1E 01"));
    assert_eq!(first.split(',').count(), header.split(',').count());
}

#[test]
fn convert_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let blf = dir.path().join("a.blf");
    let asc = dir.path().join("b.asc");
    assert_eq!(
        code(&cli(&[
            "convert",
            &example("sample_log.asc"),
            blf.to_str().unwrap()
        ])),
        0
    );
    assert_eq!(
        code(&cli(&[
            "convert",
            blf.to_str().unwrap(),
            asc.to_str().unwrap()
        ])),
        0
    );
    let read = |p: &Path| {
        operow_log::open_log(p)
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        read(&asc).len(),
        read(Path::new(&example("sample_log.asc"))).len()
    );
    let o = cli(&["convert", &example("sample_log.asc"), "x.txt"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn diag_reads_the_vin() {
    let p = example("diag_demo.operow.json");
    let o = cli(&["diag", &p, "--node", "Engine", "--req", "22 F1 90"]);
    assert_eq!(code(&o), 0, "{}", text(&o));
    assert!(text(&o).contains("WAUZZZ8K9BA123456"));
    let o = cli(&["diag", &p, "--node", "Engine", "--req", "22 FF FF"]);
    assert_eq!(code(&o), 1, "{}", text(&o));
    assert!(text(&o).contains("requestOutOfRange"));
    let o = cli(&["diag", &p, "--node", "Nope", "--req", "22 F1 90"]);
    assert_eq!(code(&o), 2);
}
