// Release builds on Windows should not open a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Operow: a native desktop CAN bus simulation workbench.

mod app;
mod dbcs;
mod graph;
mod icons;
mod inspector;
mod script_editor;
mod theme;
mod trace;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut screenshot_path: Option<PathBuf> = None;
    let mut opts = app::StartupOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--screenshot" => screenshot_path = args.next().map(PathBuf::from),
            "--fixed-trace" => opts.fixed_trace = true,
            "--expand-signals" => opts.expand_signals = true,
            "--select" => opts.select = args.next(),
            "--no-start" => opts.no_start = true,
            "--show-log" => opts.show_log = true,
            "--topology" => opts.topology = args.next().map(PathBuf::from),
            "--dbc" => opts.dbc = args.next().map(PathBuf::from),
            "--dbc-bus" => opts.dbc_bus = args.next(),
            "--open-import-dialog" => opts.import_dialog = args.next().map(PathBuf::from),
            _ => {}
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
            app.configure_startup(opts);
            Ok(Box::new(app))
        }),
    )
}
