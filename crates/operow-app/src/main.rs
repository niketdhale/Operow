//! Operow: a native desktop CAN bus simulation workbench.

mod app;
mod graph;
mod inspector;
mod theme;
mod trace;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut screenshot_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--screenshot" {
            screenshot_path = args.next().map(PathBuf::from);
        }
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_title("Operow"),
        ..Default::default()
    };

    eframe::run_native(
        "Operow",
        native_options,
        Box::new(|cc| {
            theme::AppTheme::Light.apply(&cc.egui_ctx);
            Ok(Box::new(app::OperowApp::new(screenshot_path)))
        }),
    )
}
