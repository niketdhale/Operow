//! Operow: a native desktop CAN bus simulation workbench.

mod app;
mod graph;
mod icons;
mod inspector;
mod script_editor;
mod theme;
mod trace;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut screenshot_path: Option<PathBuf> = None;
    let mut fixed_trace = false;
    let mut topology_path: Option<PathBuf> = None;
    let mut select: Option<String> = None;
    let mut no_start = false;
    let mut show_log = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--screenshot" {
            screenshot_path = args.next().map(PathBuf::from);
        } else if arg == "--fixed-trace" {
            fixed_trace = true;
        } else if arg == "--select" {
            select = args.next();
        } else if arg == "--no-start" {
            no_start = true;
        } else if arg == "--show-log" {
            show_log = true;
        } else if arg == "--topology" {
            topology_path = args.next().map(PathBuf::from);
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
            egui_extras::install_image_loaders(&cc.egui_ctx);
            theme::AppTheme::Light.apply(&cc.egui_ctx);
            let mut app = app::OperowApp::new(screenshot_path);
            app.configure_startup(
                fixed_trace,
                topology_path.as_deref(),
                select.as_deref(),
                no_start,
                show_log,
            );
            Ok(Box::new(app))
        }),
    )
}
