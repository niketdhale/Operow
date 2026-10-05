//! Test module templates and the "Test editor" window: a Rhai editor for
//! one `.rhai` file of the project's test list.

use std::path::PathBuf;

use crate::script_editor::{self, ScriptCheck};
use crate::tests_window::TestsAction;

/// Starting point for a new test module: waits for a frame and checks it.
/// Runs against any project with a `Powertrain` bus carrying 0x100.
pub const TEMPLATE_BASIC: &str = r#"// Basic test module. Every `test_*` function is one case; each case runs on
// a fresh simulation in virtual time (no real waiting).
// Signal names are "Message.Signal"; bus names come from the project.

fn setup() {
    // Runs before every case.
}

fn test_frame_is_sent() {
    // Wait up to 100 ms for frame 0x100 on the bus; a timeout fails the case.
    let f = wait_for_message_on("Powertrain", 0x100, 100);
    expect_eq(f.id, 0x100);
    expect_true(f.dlc > 0, "frame carries data");
}

fn test_frame_is_cyclic() {
    // 10 ms cycle, 10 % tolerance, observed over 200 ms.
    expect_cycle_time_on("Powertrain", 0x100, 10, 10, 200);
}
"#;

/// Reads and writes signals through the project's DBC.
pub const TEMPLATE_SIGNAL: &str = r#"// Signal checks through the project's DBC (for example dbc_demo).

fn test_signal_in_range() {
    // Returns as soon as the condition holds; fails after 500 ms.
    let rpm = wait_for_signal("EngineData.EngineSpeed", ">", 1000, 500);
    expect_true(rpm >= 1000.0 && rpm <= 5000.0, "rpm within range");
    expect_signal("EngineData.EngineSpeed", ">", 1000);
}

fn test_set_signal_round_trip() {
    // The test sends the frame as virtual node "Test".
    set_signal("DashCmd.Brightness", 200);
    let v = wait_for_signal("DashCmd.Brightness", "==", 200, 20);
    expect_eq(v, 200);
}
"#;

/// UDS requests to an ECU with a diagnostic server.
pub const TEMPLATE_UDS: &str = r#"// UDS checks against an ECU with a diagnostic server (diag_demo).

fn test_default_session() {
    let r = uds("Engine", [0x10, 0x01]);
    expect_eq(r[0], 0x50);
    expect_eq(r[1], 0x01);
}

fn test_unknown_service_is_rejected() {
    // Negative response 0x11: service not supported.
    uds_expect_nrc("Engine", [0x99], 0x11);
}
"#;

/// Faults: error frames and nodes going offline.
pub const TEMPLATE_FAULT: &str = r#"// Fault injection and node loss (gateway example).

fn test_crc_errors_raise_the_counter() {
    inject_errors(#{ bus: "Powertrain", node: "Engine", kind: "crc", count: 3 });
    wait(5);
    expect_gt(node_tec("Engine", "Powertrain"), 0);
    // The frames still get through after the retransmissions.
    wait_for_message_on("Powertrain", 0x100, 50);
}

fn test_offline_node_goes_silent() {
    wait_for_message_on("Powertrain", 0x100, 50);
    node_offline("Engine");
    wait(5);
    expect_no_message_on("Powertrain", 0x100, 100);
    node_online("Engine");
    wait_for_message_on("Powertrain", 0x100, 50);
}
"#;

/// The templates offered by "+ New test module...".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Basic,
    SignalCheck,
    Uds,
    FaultInjection,
}

impl Template {
    pub const ALL: [Template; 4] = [
        Template::Basic,
        Template::SignalCheck,
        Template::Uds,
        Template::FaultInjection,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Template::Basic => "Basic",
            Template::SignalCheck => "Signal check",
            Template::Uds => "UDS",
            Template::FaultInjection => "Fault injection",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Template::Basic => "Wait for a frame and check its cycle time",
            Template::SignalCheck => "Read and set DBC signals",
            Template::Uds => "Diagnostic requests and negative responses",
            Template::FaultInjection => "Error frames and offline nodes",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            Template::Basic => TEMPLATE_BASIC,
            Template::SignalCheck => TEMPLATE_SIGNAL,
            Template::Uds => TEMPLATE_UDS,
            Template::FaultInjection => TEMPLATE_FAULT,
        }
    }
}

/// One open test module.
pub struct TestEditor {
    /// The path as listed in the project.
    pub path: String,
    pub file: PathBuf,
    pub text: String,
    saved: String,
    check: ScriptCheck,
    /// Line to put the cursor on at the next frame.
    goto: Option<u32>,
    /// Last save error.
    error: Option<String>,
}

impl Default for TestEditor {
    fn default() -> Self {
        TestEditor {
            path: String::new(),
            file: PathBuf::new(),
            text: String::new(),
            saved: String::new(),
            check: ScriptCheck::with_checker(operow_test::check_module),
            goto: None,
            error: None,
        }
    }
}

impl TestEditor {
    /// Open `file`; an unreadable file opens empty with the error shown.
    pub fn open(path: String, file: PathBuf) -> Self {
        let (text, error) = match std::fs::read_to_string(&file) {
            Ok(t) => (t, None),
            Err(e) => (String::new(), Some(format!("cannot read: {e}"))),
        };
        TestEditor {
            path,
            file,
            saved: text.clone(),
            text,
            error,
            ..TestEditor::default()
        }
    }

    /// Replace the text (and the saved baseline), for in-memory modules.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.saved = self.text.clone();
        self.error = None;
    }

    pub fn dirty(&self) -> bool {
        self.text != self.saved
    }

    /// Tab title: file name, with `*` while unsaved.
    pub fn title(&self) -> String {
        let name = self
            .file
            .file_name()
            .map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned());
        format!("{name}{}", if self.dirty() { " *" } else { "" })
    }

    pub fn goto_line(&mut self, line: u32) {
        self.goto = Some(line);
    }

    pub fn save(&mut self) -> Result<(), String> {
        std::fs::write(&self.file, &self.text)
            .map_err(|e| format!("cannot write {}: {e}", self.file.display()))?;
        self.saved = self.text.clone();
        self.error = None;
        Ok(())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, id: egui::Id, running: bool) -> Vec<TestsAction> {
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.dirty(), egui::Button::new("Save"))
                .on_hover_text("Write the file")
                .clicked()
            {
                match self.save() {
                    Ok(()) => actions.push(TestsAction::Refresh),
                    Err(e) => self.error = Some(e),
                }
            }
            if ui
                .add_enabled(!running, egui::Button::new("Run this module"))
                .on_hover_text("Runs the text shown here, saved or not")
                .clicked()
            {
                actions.push(TestsAction::RunModule(self.path.clone()));
            }
            ui.weak(self.file.display().to_string());
        });
        if let Some(e) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), e);
        }
        ui.separator();
        let goto = self.goto.take();
        script_editor::editor_ui_at(ui, id, &mut self.text, true, &mut self.check, 24, goto);
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_test::{Project, RunOptions, Status, TestRunner};

    fn example(name: &str) -> Project {
        Project::load(format!(
            "{}/../../examples/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn run(example_name: &str, t: Template) -> operow_test::RunReport {
        TestRunner::new(example(example_name), RunOptions::default())
            .run_sources(&[("tests/template.rhai", t.source())], None)
    }

    #[test]
    fn templates_compile() {
        for t in Template::ALL {
            assert_eq!(operow_test::check_module(t.source()), Ok(()), "{t:?}");
            let cases = operow_test::list_cases(t.source()).unwrap();
            assert!(!cases.is_empty(), "{t:?}");
        }
    }

    #[test]
    fn basic_and_signal_templates_pass() {
        for (example_name, t) in [
            ("dbc_demo.operow.json", Template::Basic),
            ("gateway.operow.json", Template::Basic),
            ("dbc_demo.operow.json", Template::SignalCheck),
        ] {
            let r = run(example_name, t);
            assert!(r.modules[0].error.is_none(), "{t:?}: {r:#?}");
            for c in &r.modules[0].cases {
                assert_eq!(c.status, Status::Pass, "{t:?} {}: {:?}", c.name, c.failure);
            }
        }
    }

    #[test]
    fn uds_and_fault_templates_pass_on_their_examples() {
        for (example_name, t) in [
            ("diag_demo.operow.json", Template::Uds),
            ("gateway.operow.json", Template::FaultInjection),
        ] {
            let r = run(example_name, t);
            for c in &r.modules[0].cases {
                assert_eq!(c.status, Status::Pass, "{t:?} {}: {:?}", c.name, c.failure);
            }
        }
    }

    #[test]
    fn editor_tracks_dirty_and_saves() {
        let dir = std::env::temp_dir().join(format!("operow-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.rhai");
        std::fs::write(&file, TEMPLATE_BASIC).unwrap();
        let mut e = TestEditor::open("t.rhai".into(), file.clone());
        assert!(!e.dirty());
        assert_eq!(e.title(), "t.rhai");
        e.text.push_str("\n// edit\n");
        assert!(e.dirty());
        assert_eq!(e.title(), "t.rhai *");
        e.save().unwrap();
        assert!(!e.dirty());
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .ends_with("// edit\n")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
