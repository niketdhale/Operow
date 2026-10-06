// Release builds on Windows should not open a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Operow: a native desktop CAN bus simulation workbench.

mod app;
mod dbcs;
mod diag_group;
mod diag_props;
mod diag_window;
mod filters;
mod generator_window;
mod graph;
mod graph_window;
mod hw_ui;
mod icons;
mod inspector;
mod logging;
mod logging_window;
mod network_view;
mod project_tree;
mod replay;
mod runtime;
mod script_editor;
mod settings;
mod signal_dialog;
mod signals;
mod store;
mod test_editor;
mod tests_window;
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
            "--demo-diag" => opts.demo_diag = true,
            "--demo-diag-dtcs" => {
                opts.demo_diag = true;
                opts.demo_diag_dtcs = true;
            }
            "--open-log" => opts.open_log = args.next().map(PathBuf::from),
            "--demo-tests" => opts.demo_tests = true,
            "--demo-test-editor" => opts.demo_test_editor = true,
            "--demo-logging" => opts.demo_logging = true,
            "--demo-errors" => opts.demo_errors = true,
            "--demo-faults" => opts.demo_faults = true,
            "--demo-busoff" => opts.demo_busoff = true,
            "--demo-hw-udp" => opts.demo_hw_udp = true,
            "--demo-hw-confirm" => opts.demo_hw_confirm = true,
            "--network-view" => {
                opts.network_view = args
                    .next()
                    .and_then(|v| network_view::NetworkView::parse(&v))
            }
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
