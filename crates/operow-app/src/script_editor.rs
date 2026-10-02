//! Rhai script editor widget (inspector section / pop-out window) and the
//! non-UI helpers behind it.

use crate::icons;

/// Commented starting point inserted by "Add script".
pub const TEMPLATE: &str = r#"// Operow script (Rhai). All handlers are optional.
// Persistent state lives in `this` (an object map shared by all handlers).

// Runs once when the measurement starts.
fn on_start() {
    this.count = 0;
    print("script started");
    // set_timer(1, 100);   // fire on_timer(1) after 100 ms
}

// Runs when a timer set with set_timer(id, ms) expires.
fn on_timer(id) {
    // set_timer(id, 100);  // re-arm for a periodic timer
    // output(#{ id: 0x200, data: [1, 2, 3] });
}

// Runs for every frame received. msg: id, extended, fd, dlc, data, bus, time_ns
fn on_message(msg) {
    if msg.id == 0x100 {
        this.count += 1;
        // Send a frame: id, data, optional extended / fd / brs / dlc / bus
        output(#{ id: 0x101, data: [this.count % 256] });
    }
}

// Other API: now_ms(), now_ns(), print(x), trigger(i), set_payload(i, data)
"#;

/// Collapsing-header suffix: "(none)" or "(N lines)".
pub fn line_count_label(script: Option<&str>) -> String {
    match script {
        None => "(none)".to_string(),
        Some(s) => {
            let n = s.lines().count();
            format!("({n} line{})", if n == 1 { "" } else { "s" })
        }
    }
}

/// Whether a log line reports a script or engine error (drawn in red).
pub fn is_error_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("script error") || l.starts_with("error:")
}

/// Cached result of the last compile check, keyed by the checked text.
#[derive(Default)]
pub struct ScriptCheck {
    text: Option<String>,
    result: Option<Result<(), String>>,
}

impl ScriptCheck {
    /// Re-run the check only when `src` differs from the last checked text.
    pub fn update(&mut self, src: &str) -> &Result<(), String> {
        if self.text.as_deref() != Some(src) {
            self.result = Some(operow_engine::check_script(src));
            self.text = Some(src.to_string());
        }
        self.result.as_ref().expect("result set above")
    }
}

/// Draw the code editor plus the compile status. Returns true if edited.
pub fn editor_ui(
    ui: &mut egui::Ui,
    id: egui::Id,
    script: &mut String,
    editable: bool,
    check: &mut ScriptCheck,
    rows: usize,
) -> bool {
    let theme = egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style());
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = egui_extras::syntax_highlighting::highlight(
            ui.ctx(),
            ui.style(),
            &theme,
            text.as_str(),
            "rs",
        );
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut changed = false;
    egui::ScrollArea::vertical()
        .id_salt(id.with("scroll"))
        .max_height(ui.available_height().max(120.0))
        .show(ui, |ui| {
            changed = ui
                .add(
                    egui::TextEdit::multiline(script)
                        .id(id)
                        .code_editor()
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(rows)
                        .desired_width(f32::INFINITY)
                        .interactive(editable)
                        .layouter(&mut layouter),
                )
                .changed();
        });
    ui.horizontal(|ui| {
        let ok = ui.button("Check").clicked();
        let _ = ok; // result below is always current; the button forces a refresh
        if ok {
            check.text = None;
        }
        match check.update(script) {
            Ok(()) => {
                ui.colored_label(egui::Color32::from_rgb(0x1a, 0x9c, 0x3a), "Compiles OK");
            }
            Err(e) => {
                ui.colored_label(egui::Color32::from_rgb(0xd0, 0x30, 0x30), e);
            }
        }
    });
    changed
}

/// Section header: script icon + "Script" + line-count label.
pub fn header_ui(ui: &mut egui::Ui, script: Option<&str>) {
    ui.add(icons::icon_image(ui, icons::script()));
    ui.label(format!("Script {}", line_count_label(script)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_compiles() {
        assert_eq!(operow_engine::check_script(TEMPLATE), Ok(()));
    }

    #[test]
    fn line_labels() {
        assert_eq!(line_count_label(None), "(none)");
        assert_eq!(line_count_label(Some("a")), "(1 line)");
        assert_eq!(line_count_label(Some("a\nb\n")), "(2 lines)");
    }

    #[test]
    fn error_classifier() {
        assert!(is_error_line("[Ecu 1.0ms] script error: boom"));
        assert!(is_error_line("error: invalid topology"));
        assert!(!is_error_line("[Ecu 1.0ms] responder ready"));
    }

    #[test]
    fn check_cache_detects_errors() {
        let mut c = ScriptCheck::default();
        assert!(c.update("fn f() {}").is_ok());
        assert!(c.update("fn f( {").is_err());
    }
}
