// Release builds on Windows should not open a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Operow: a native desktop CAN bus simulation workbench.

mod app;
mod dbcs;
mod filters;
mod generator_window;
mod graph;
mod graph_window;
mod icons;
mod inspector;
mod project_tree;
mod script_editor;
mod settings;
mod signal_dialog;
mod signals;
mod store;
mod theme;
mod trace;
mod windows;
mod workspace;

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
            "--open-settings" => opts.open_settings = true,
            "--layout-demo" => opts.layout_demo = true,
            "--demo-filters" => opts.demo_filters = true,
            "--open-new-signal" => opts.open_new_signal = true,
            "--demo-graph" => opts.demo_graph = true,
            "--demo-generator" => opts.demo_generator = true,
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
            let settings = settings::AppSettings::load(cc.storage);
            if settings.dark_theme {
                theme::AppTheme::Dark.apply(&cc.egui_ctx);
            } else {
                theme::AppTheme::Light.apply(&cc.egui_ctx);
            }
            let mut app = app::OperowApp::new(screenshot_path, settings);
            app.configure_startup(opts);
            Ok(Box::new(app))
        }),
    )
}
