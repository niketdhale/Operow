//! Exporting a [`RunReport`] as a self-contained HTML page, JUnit XML or
//! JSON.

use std::fmt::Write;

use crate::report::{CaseResult, ModuleResult, RunReport, Status, TraceRow};

/// The report as pretty-printed JSON.
pub fn to_json(report: &RunReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

// --- JUnit ---------------------------------------------------------------

/// Escape text for XML content or a double-quoted attribute. Characters that
/// are not allowed in XML 1.0 are dropped.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if (c as u32) < 0x20 || matches!(c as u32, 0xFFFE | 0xFFFF) => {}
            c => out.push(c),
        }
    }
    out
}

fn secs(ms: f64) -> String {
    format!("{:.3}", ms / 1e3)
}

fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn id_text(id: u32, ext: bool) -> String {
    if ext {
        format!("{id:08X}x")
    } else {
        format!("{id:03X}")
    }
}

fn trace_line(r: &TraceRow) -> String {
    let mut s = format!(
        "{:10.3} ms  {}  {}  {}  [{}]  {}  ({})",
        r.time_ms,
        r.bus,
        r.dir,
        id_text(r.id, r.ext),
        r.dlc,
        hex(&r.data),
        r.sender
    );
    if let Some(e) = &r.error {
        let _ = write!(s, "  ERROR {e}");
    }
    s
}

/// The text of a failed case: failure, steps and trace extract.
fn case_text(c: &CaseResult) -> String {
    let mut s = String::new();
    if let Some(f) = &c.failure {
        let _ = write!(s, "{}", f.message);
        if let Some(l) = f.line {
            let _ = write!(s, " (line {l})");
        }
        let _ = writeln!(s, " at {:.3} ms", f.time_ms);
    }
    if !c.steps.is_empty() {
        s.push_str("\nSteps:\n");
        for st in &c.steps {
            let _ = writeln!(
                s,
                "{:10.3} ms  {}  {:<7} {}",
                st.time_ms,
                if st.ok { "ok  " } else { "FAIL" },
                st.kind,
                st.text
            );
        }
    }
    if !c.trace_extract.is_empty() {
        s.push_str("\nTrace extract:\n");
        for r in &c.trace_extract {
            let _ = writeln!(s, "{}", trace_line(r));
        }
    }
    s
}

fn failure_message(c: &CaseResult) -> String {
    c.failure
        .as_ref()
        .map_or_else(|| "failed".to_string(), |f| f.message.clone())
}

/// The report as JUnit XML: one `testsuite` per module. A module that could
/// not run (or whose setup/teardown failed) gets an extra test case named
/// `(module)` carrying the `error`.
pub fn to_junit(report: &RunReport) -> String {
    struct Suite {
        xml: String,
        tests: u32,
        failures: u32,
        errors: u32,
        skipped: u32,
        time_ms: f64,
    }
    let mut suites = Vec::new();
    for m in &report.modules {
        let mut s = Suite {
            xml: String::new(),
            tests: 0,
            failures: 0,
            errors: 0,
            skipped: 0,
            time_ms: 0.0,
        };
        let class = xml_escape(&m.name);
        if let Some(e) = &m.error {
            s.tests += 1;
            s.errors += 1;
            let _ = write!(
                s.xml,
                "    <testcase classname=\"{class}\" name=\"(module)\" time=\"0.000\">\n      <error message=\"{}\">{}</error>\n    </testcase>\n",
                xml_escape(e),
                xml_escape(e)
            );
        }
        for c in &m.cases {
            s.tests += 1;
            s.time_ms += c.virtual_duration_ms;
            let _ = write!(
                s.xml,
                "    <testcase classname=\"{class}\" name=\"{}\" time=\"{}\"",
                xml_escape(&c.name),
                secs(c.virtual_duration_ms)
            );
            match c.status {
                Status::Pass => s.xml.push_str("/>\n"),
                Status::Skip => {
                    s.skipped += 1;
                    let why = c
                        .steps
                        .iter()
                        .rev()
                        .find(|st| st.kind == "skip")
                        .map_or("skipped", |st| st.text.as_str());
                    let _ = writeln!(
                        s.xml,
                        ">\n      <skipped message=\"{}\"/>\n    </testcase>",
                        xml_escape(why)
                    );
                }
                Status::Fail | Status::Error => {
                    let tag = if c.status == Status::Fail {
                        s.failures += 1;
                        "failure"
                    } else {
                        s.errors += 1;
                        "error"
                    };
                    let _ = write!(
                        s.xml,
                        ">\n      <{tag} message=\"{}\">{}</{tag}>\n    </testcase>\n",
                        xml_escape(&failure_message(c)),
                        xml_escape(&case_text(c))
                    );
                }
            }
        }
        suites.push((m, s));
    }
    let sum = |f: fn(&Suite) -> u32| suites.iter().map(|(_, s)| f(s)).sum::<u32>();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<testsuites name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{}\">",
        xml_escape(&report.project),
        sum(|s| s.tests),
        sum(|s| s.failures),
        sum(|s| s.errors),
        sum(|s| s.skipped),
        secs(suites.iter().map(|(_, s)| s.time_ms).sum())
    );
    for (m, s) in &suites {
        let _ = write!(
            out,
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{}\" timestamp=\"{}\">\n{}  </testsuite>\n",
            xml_escape(&m.name),
            s.tests,
            s.failures,
            s.errors,
            s.skipped,
            secs(s.time_ms),
            xml_escape(report.started.trim_end_matches('Z')),
            s.xml
        );
    }
    out.push_str("</testsuites>\n");
    out
}

// --- HTML ----------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn status_class(s: Status) -> &'static str {
    match s {
        Status::Pass => "pass",
        Status::Fail => "fail",
        Status::Skip => "skip",
        Status::Error => "error",
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Pass => "PASS",
        Status::Fail => "FAIL",
        Status::Skip => "SKIP",
        Status::Error => "ERROR",
    }
}

fn dur(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1e3)
    } else {
        format!("{ms:.1} ms")
    }
}

const CSS: &str = "\
:root{--bg:#f6f7f9;--fg:#1d2330;--muted:#667085;--card:#fff;--line:#e3e6ec;--code:#f1f3f7;\
--pass:#1a7f46;--pass-bg:#e0f4e8;--fail:#c62828;--fail-bg:#fde7e7;--skip:#8a6d00;--skip-bg:#fff3cd;\
--error:#a8269c;--error-bg:#f9e4f7}\
@media (prefers-color-scheme:dark){:root{--bg:#12151c;--fg:#e6e9f0;--muted:#98a2b3;--card:#1b1f29;\
--line:#2b313e;--code:#242a36;--pass:#5fd38d;--pass-bg:#173a27;--fail:#ff8a80;--fail-bg:#451c1c;\
--skip:#f2c94c;--skip-bg:#40350f;--error:#e79be0;--error-bg:#3d1c3a}}\
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);\
font:14px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif}\
main{max-width:1100px;margin:0 auto;padding:24px 16px 64px}\
h1{font-size:22px;margin:0 0 4px}.meta{color:var(--muted);margin-bottom:16px}\
.chips{display:flex;flex-wrap:wrap;gap:8px;margin-bottom:20px}\
.chip{padding:3px 12px;border-radius:999px;font-weight:600;font-size:13px}\
.chip.total{background:var(--code)}\
.pass{color:var(--pass)}.fail{color:var(--fail)}.skip{color:var(--skip)}.error{color:var(--error)}\
.chip.pass{background:var(--pass-bg)}.chip.fail{background:var(--fail-bg)}\
.chip.skip{background:var(--skip-bg)}.chip.error{background:var(--error-bg)}\
.tools{margin-bottom:12px}button{font:inherit;background:var(--card);color:var(--fg);\
border:1px solid var(--line);border-radius:6px;padding:3px 10px;cursor:pointer}\
details{background:var(--card);border:1px solid var(--line);border-radius:8px;margin-bottom:10px}\
details details{border:0;border-top:1px solid var(--line);border-radius:0;margin:0}\
summary{cursor:pointer;padding:9px 14px;display:flex;gap:10px;align-items:baseline}\
summary:hover{background:var(--code)}\
.mod>summary{font-weight:600;font-size:15px}.grow{flex:1}.dim{color:var(--muted);font-weight:400;font-size:13px}\
.badge{font-size:11px;font-weight:700;padding:1px 8px;border-radius:4px;min-width:52px;text-align:center}\
.badge.pass{background:var(--pass-bg)}.badge.fail{background:var(--fail-bg)}\
.badge.skip{background:var(--skip-bg)}.badge.error{background:var(--error-bg)}\
.body{padding:4px 14px 14px}\
.msg{background:var(--fail-bg);color:var(--fail);border-left:4px solid var(--fail);\
padding:8px 12px;border-radius:4px;margin:8px 0;white-space:pre-wrap;word-break:break-word}\
.msg.error{background:var(--error-bg);color:var(--error);border-color:var(--error)}\
h4{margin:14px 0 4px;font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}\
table{border-collapse:collapse;width:100%;font-size:13px}\
th,td{text-align:left;padding:3px 8px;border-bottom:1px solid var(--line);vertical-align:top}\
th{color:var(--muted);font-weight:600}\
td.n,th.n{text-align:right;white-space:nowrap}\
.mono,td.mono{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px}\
tr.bad td{background:var(--fail-bg)}tr.bad td.ok{color:var(--fail);font-weight:700}\
td.ok{color:var(--pass);width:28px;text-align:center}td.txt{word-break:break-word}\
.empty{color:var(--muted);padding:8px 0}";

const JS: &str = "\
function setAll(open){document.querySelectorAll('details').forEach(function(d){d.open=open})}\
document.querySelectorAll('details.case.open').forEach(function(d){d.open=true});";

fn chip(out: &mut String, class: &str, n: u32, label: &str) {
    let _ = write!(out, "<span class=\"chip {class}\">{n} {label}</span>");
}

fn steps_table(out: &mut String, c: &CaseResult) {
    if c.steps.is_empty() {
        out.push_str("<div class=\"empty\">No steps recorded.</div>");
        return;
    }
    out.push_str(
        "<table><tr><th class=\"n\">Time (ms)</th><th>Kind</th><th>Text</th><th></th></tr>",
    );
    for s in &c.steps {
        let _ = write!(
            out,
            "<tr{}><td class=\"n mono\">{:.3}</td><td class=\"mono\">{}</td><td class=\"txt\">{}</td><td class=\"ok\">{}</td></tr>",
            if s.ok { "" } else { " class=\"bad\"" },
            s.time_ms,
            esc(&s.kind),
            esc(&s.text),
            if s.ok { "\u{2713}" } else { "\u{2717}" }
        );
    }
    out.push_str("</table>");
}

fn trace_table(out: &mut String, rows: &[TraceRow]) {
    out.push_str("<h4>Trace extract</h4><table><tr><th class=\"n\">Time (ms)</th><th>Bus</th><th>Dir</th><th>ID</th><th class=\"n\">DLC</th><th>Data</th><th>Sender</th></tr>");
    for r in rows {
        let data = match &r.error {
            Some(e) => format!("ERROR FRAME ({e})"),
            None => hex(&r.data),
        };
        let _ = write!(
            out,
            "<tr{}><td class=\"n mono\">{:.3}</td><td>{}</td><td>{}</td><td class=\"mono\">{}</td><td class=\"n mono\">{}</td><td class=\"mono\">{}</td><td>{}</td></tr>",
            if r.error.is_some() {
                " class=\"bad\""
            } else {
                ""
            },
            r.time_ms,
            esc(&r.bus),
            esc(&r.dir),
            id_text(r.id, r.ext),
            r.dlc,
            esc(&data),
            esc(&r.sender)
        );
    }
    out.push_str("</table>");
}

fn case_html(out: &mut String, c: &CaseResult) {
    let open = matches!(c.status, Status::Fail | Status::Error);
    let _ = write!(
        out,
        "<details class=\"case{}\"><summary><span class=\"badge {cls}\">{}</span><span class=\"grow\">{}</span><span class=\"dim\">{} virtual, {} wall</span></summary><div class=\"body\">",
        if open { " open" } else { "" },
        status_label(c.status),
        esc(&c.name),
        dur(c.virtual_duration_ms),
        dur(c.wall_duration_ms),
        cls = status_class(c.status)
    );
    if let Some(f) = &c.failure {
        let _ = write!(
            out,
            "<div class=\"msg {}\">{}<br><span class=\"dim\">at {:.3} ms{}</span></div>",
            status_class(c.status),
            esc(&f.message),
            f.time_ms,
            f.line.map_or_else(String::new, |l| format!(", line {l}"))
        );
    }
    out.push_str("<h4>Steps</h4>");
    steps_table(out, c);
    if !c.trace_extract.is_empty() {
        trace_table(out, &c.trace_extract);
    }
    out.push_str("</div></details>");
}

fn module_html(out: &mut String, m: &ModuleResult) {
    let n = |s: Status| m.cases.iter().filter(|c| c.status == s).count();
    let bad = matches!(m.status, Status::Fail | Status::Error);
    let _ = write!(
        out,
        "<details class=\"mod\"{}><summary><span class=\"badge {}\">{}</span><span class=\"grow\">{}</span><span class=\"dim\">{} cases, {} passed, {} failed, {} skipped, {} errors</span></summary><div class=\"body\">",
        if bad { " open" } else { "" },
        status_class(m.status),
        status_label(m.status),
        esc(&m.name),
        m.cases.len(),
        n(Status::Pass),
        n(Status::Fail),
        n(Status::Skip),
        n(Status::Error)
    );
    let _ = write!(out, "<div class=\"dim\">{}</div>", esc(&m.path));
    if let Some(e) = &m.error {
        let _ = write!(out, "<div class=\"msg error\">{}</div>", esc(e));
    }
    if m.cases.is_empty() && m.error.is_none() {
        out.push_str("<div class=\"empty\">No test cases.</div>");
    }
    for c in &m.cases {
        case_html(out, c);
    }
    out.push_str("</div></details>");
}

/// The report as one self-contained HTML page: inline CSS and a little
/// script, no external assets. Follows the system light/dark setting.
pub fn to_html(report: &RunReport) -> String {
    let t = &report.totals;
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Operow test report: {}</title><style>{CSS}</style></head><body><main>",
        esc(&report.project)
    );
    let _ = write!(
        out,
        "<h1>{}</h1><div class=\"meta\">Started {} &middot; duration {}{}</div><div class=\"chips\">",
        esc(&report.project),
        esc(&report.started),
        dur(report.duration_ms as f64),
        if report.stopped {
            " &middot; <b>run stopped early</b>"
        } else {
            ""
        }
    );
    chip(&mut out, "total", t.cases, "cases");
    chip(&mut out, "pass", t.passed, "passed");
    chip(&mut out, "fail", t.failed, "failed");
    chip(&mut out, "skip", t.skipped, "skipped");
    chip(&mut out, "error", t.errors, "errors");
    let _ = write!(
        out,
        "<span class=\"chip total\">{} modules</span></div>",
        t.modules
    );
    out.push_str("<div class=\"tools\"><button onclick=\"setAll(true)\">Expand all</button> <button onclick=\"setAll(false)\">Collapse all</button></div>");
    for m in &report.modules {
        module_html(&mut out, m);
    }
    let _ = write!(out, "</main><script>{JS}</script></body></html>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Failure, Step};
    use quick_xml::Reader;
    use quick_xml::events::Event;

    const NASTY: &str = "a<b>&\"c\" 'd'";

    fn case(name: &str, status: Status) -> CaseResult {
        let bad = matches!(status, Status::Fail | Status::Error);
        CaseResult {
            name: name.into(),
            status,
            virtual_duration_ms: 12.5,
            wall_duration_ms: 1.0,
            steps: vec![
                Step {
                    time_ms: 1.0,
                    kind: "expect".into(),
                    text: NASTY.into(),
                    ok: !bad,
                },
                Step {
                    time_ms: 2.0,
                    kind: "skip".into(),
                    text: "not now <skip>".into(),
                    ok: true,
                },
            ],
            failure: bad.then(|| Failure {
                message: NASTY.into(),
                time_ms: 1.0,
                line: Some(7),
            }),
            trace_extract: if bad {
                vec![TraceRow {
                    time_ms: 0.5,
                    bus: "Power<train>".into(),
                    dir: "Tx".into(),
                    id: 0x100,
                    ext: false,
                    dlc: 2,
                    data: vec![1, 0xAB],
                    sender: "Engine".into(),
                    error: None,
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn report() -> RunReport {
        let m1 = ModuleResult {
            path: "tests/a.rhai".into(),
            name: "a".into(),
            status: Status::Fail,
            error: None,
            cases: vec![
                case("test_pass", Status::Pass),
                case("test_fail", Status::Fail),
                case("test_skip", Status::Skip),
                case("test_err", Status::Error),
            ],
        };
        let m2 = ModuleResult {
            path: "tests/b.rhai".into(),
            name: "b".into(),
            status: Status::Error,
            error: Some(format!("compile error: {NASTY}\u{1}")),
            cases: Vec::new(),
        };
        let totals = RunReport::compute_totals(&[m1.clone(), m2.clone()]);
        RunReport {
            project: "proj <x>.json".into(),
            started: "2026-01-02T03:04:05Z".into(),
            duration_ms: 1234,
            stopped: false,
            modules: vec![m1, m2],
            totals,
        }
    }

    #[test]
    fn junit_is_well_formed_and_counts_match() {
        let r = report();
        let xml = to_junit(&r);
        let mut rd = Reader::from_str(&xml);
        let (mut cases, mut failures, mut errors, mut skipped, mut suites) = (0, 0, 0, 0, 0);
        let mut messages = Vec::new();
        loop {
            match rd.read_event().expect("well-formed XML") {
                Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                    "testsuite" => suites += 1,
                    "testcase" => cases += 1,
                    "failure" | "error" => {
                        if e.name().as_ref() == "failure" {
                            failures += 1;
                        } else {
                            errors += 1;
                        }
                        let a = e.try_get_attribute("message").unwrap().unwrap();
                        messages.push(
                            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .unwrap()
                                .into_owned(),
                        );
                    }
                    "skipped" => skipped += 1,
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(suites, 2);
        assert_eq!(cases, 5, "4 cases plus the module error");
        assert_eq!(failures, r.totals.failed);
        assert_eq!(errors, r.totals.errors);
        assert_eq!(skipped, r.totals.skipped);
        assert!(messages.iter().any(|m| m == NASTY), "{messages:?}");
        assert!(xml.contains("&lt;b&gt;&amp;&quot;c&quot; &apos;d&apos;"));
        assert!(!xml.contains('\u{1}'));
        assert!(xml.contains("skipped message=\"not now &lt;skip&gt;\""));
    }

    #[test]
    fn html_escapes_and_lists_cases() {
        let r = report();
        let html = to_html(&r);
        for n in ["test_pass", "test_fail", "test_skip", "test_err"] {
            assert!(html.contains(n), "{n}");
        }
        assert!(html.contains("a&lt;b&gt;&amp;&quot;c&quot; &#39;d&#39;"));
        assert!(!html.contains(NASTY));
        assert!(html.contains("Power&lt;train&gt;"));
        assert!(html.contains("proj &lt;x&gt;.json"));
        assert!(html.contains("01 AB"));
        assert!(html.contains("1 passed") || html.contains(">1 passed<"));
        assert!(!html.contains("http://") && !html.contains("https://"));
    }

    #[test]
    fn json_round_trips() {
        let r = report();
        let back: RunReport = serde_json::from_str(&to_json(&r)).unwrap();
        assert_eq!(back, r);
    }
}
