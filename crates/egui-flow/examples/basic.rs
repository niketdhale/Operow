//! Run with: `cargo run -p egui-flow --example basic`

use egui::{Color32, Ui};
use egui_flow::{
    Background, EdgeKind, Flow, FlowEvent, FlowOptions, FlowState, FlowViewer, Handle, HandleId,
    Node, Side,
};

enum Kind {
    Input,
    Process,
    Output,
}

struct Data {
    kind: Kind,
    label: String,
    gain: f32,
}

struct Viewer;

impl FlowViewer<Data, ()> for Viewer {
    fn node_ui(&mut self, ui: &mut Ui, node: &mut Node<Data>) {
        ui.set_max_width(180.0);
        match node.data.kind {
            Kind::Input => ui.strong("Input"),
            Kind::Process => ui.strong("Process"),
            Kind::Output => ui.strong("Output"),
        };
        ui.text_edit_singleline(&mut node.data.label);
        if matches!(node.data.kind, Kind::Process) {
            ui.add(egui::Slider::new(&mut node.data.gain, 0.0..=2.0).text("gain"));
        }
    }

    fn handles(&self, node: &Node<Data>) -> Vec<Handle> {
        match node.data.kind {
            Kind::Input => vec![Handle::source(Handle::DEFAULT_SOURCE, Side::Right)],
            Kind::Output => vec![Handle::target(Handle::DEFAULT_TARGET, Side::Left)],
            Kind::Process => vec![
                Handle::target(Handle::DEFAULT_TARGET, Side::Left),
                Handle::source(Handle::DEFAULT_SOURCE, Side::Right),
                Handle::source(HandleId(2), Side::Bottom),
            ],
        }
    }

    fn minimap_color(&self, node: &Node<Data>) -> Option<Color32> {
        Some(match node.data.kind {
            Kind::Input => Color32::from_rgb(80, 160, 90),
            Kind::Process => Color32::from_rgb(90, 120, 200),
            Kind::Output => Color32::from_rgb(200, 110, 80),
        })
    }
}

struct App {
    state: FlowState<Data, ()>,
    kind: EdgeKind,
    log: Vec<String>,
}

impl App {
    fn new() -> Self {
        let mut state = FlowState::new();
        let node = |kind, label: &str| Data {
            kind,
            label: label.into(),
            gain: 1.0,
        };
        let a = state.add_node(egui::pos2(0.0, 80.0), node(Kind::Input, "sensor"));
        let b = state.add_node(egui::pos2(260.0, 40.0), node(Kind::Process, "filter"));
        let c = state.add_node(egui::pos2(260.0, 220.0), node(Kind::Process, "scale"));
        let d = state.add_node(egui::pos2(540.0, 80.0), node(Kind::Output, "display"));
        state.connect(a, b, ());
        if let Some(e) = state.connect(b, d, ()) {
            let e = state.edge_mut(e).unwrap();
            e.animated = true;
            e.arrow = true;
            e.label = Some("stream".into());
        }
        state.connect(a, c, ());
        state.fit_view();
        Self {
            state,
            kind: EdgeKind::Bezier,
            log: Vec::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Edge style:");
                for (k, name) in [
                    (EdgeKind::Bezier, "Bezier"),
                    (EdgeKind::Straight, "Straight"),
                    (EdgeKind::Step, "Step"),
                    (EdgeKind::SmoothStep, "Smooth step"),
                ] {
                    ui.selectable_value(&mut self.kind, k, name);
                }
                ui.separator();
                ui.weak("drag background to pan · wheel to zoom · shift-drag to box select · Delete removes");
            });
        });
        egui::TopBottomPanel::bottom("log").show(ctx, |ui| {
            ui.label(
                self.log
                    .last()
                    .map(String::as_str)
                    .unwrap_or("no events yet"),
            );
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let opts = FlowOptions {
                    background: Background::Dots,
                    minimap: true,
                    default_edge_kind: self.kind,
                    snap_to_grid: Some(10.0),
                    ..Default::default()
                };
                let out = Flow::new("graph")
                    .options(opts)
                    .show(ui, &mut self.state, &mut Viewer);
                for event in out.events {
                    match event {
                        FlowEvent::Connected(id) => self.log.push(format!("connected edge {id:?}")),
                        FlowEvent::Deleted { nodes, edges } => self.log.push(format!(
                            "deleted {} nodes, {} edges",
                            nodes.len(),
                            edges.len()
                        )),
                        FlowEvent::ConnectionDropped { pos, .. } => self
                            .log
                            .push(format!("connection dropped at {:.0},{:.0}", pos.x, pos.y)),
                        _ => {}
                    }
                }
            });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-flow",
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::new(App::new()))),
    )
}
