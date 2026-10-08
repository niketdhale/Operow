//! Visual draft of the proposed Operow UI: light-green theme, workspaces,
//! dockable/floating windows and a mixed CAN / LIN / Ethernet network.
//! Static sample data only; nothing is wired to the engine.
//!
//! Run: `cargo run -p operow-app --example ui_draft`

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, ImageSource, Pos2, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, Vec2, include_image, pos2, vec2,
};
use egui_dock::{DockArea, DockState, NodeIndex, SurfaceIndex, TabViewer};

// ---------------------------------------------------------------- theme

#[derive(Clone, Copy)]
struct Pal {
    page: Color32,
    panel: Color32,
    line: Color32,
    soft: Color32,
    g1: Color32,
    g2: Color32,
    grid: Color32,
    green: Color32,
    on_green: Color32,
    fg: Color32,
    muted: Color32,
    faint: Color32,
    amber: Color32,
    red: Color32,
    can_a: Color32,
    can_b: Color32,
    lin: Color32,
    eth: Color32,
    gw: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const LIGHT: Pal = Pal {
    page: rgb(0xF7, 0xFB, 0xF8),
    panel: rgb(0xFF, 0xFF, 0xFF),
    line: rgb(0xD7, 0xE9, 0xDC),
    soft: rgb(0xDF, 0xF3, 0xE5),
    g1: rgb(0xFF, 0xFF, 0xFF),
    g2: rgb(0xE3, 0xF3, 0xE8),
    grid: rgb(0xCF, 0xE3, 0xD5),
    green: rgb(0x2E, 0x9E, 0x5B),
    on_green: rgb(0xFF, 0xFF, 0xFF),
    fg: rgb(0x1E, 0x2A, 0x23),
    muted: rgb(0x5E, 0x6E, 0x64),
    faint: rgb(0x9A, 0xAE, 0xA1),
    amber: rgb(0xD9, 0x8E, 0x04),
    red: rgb(0xD1, 0x43, 0x43),
    can_a: rgb(0x1A, 0x6F, 0xC4),
    can_b: rgb(0x7B, 0x4F, 0xC9),
    lin: rgb(0xB4, 0x56, 0x1A),
    eth: rgb(0xBE, 0x18, 0x5D),
    gw: rgb(0x0E, 0x80, 0x8A),
};

const DARK: Pal = Pal {
    page: rgb(0x11, 0x17, 0x14),
    panel: rgb(0x16, 0x1E, 0x1A),
    line: rgb(0x25, 0x33, 0x2B),
    soft: rgb(0x1C, 0x33, 0x26),
    g1: rgb(0x18, 0x22, 0x1D),
    g2: rgb(0x0F, 0x1A, 0x14),
    grid: rgb(0x1F, 0x2C, 0x25),
    green: rgb(0x3D, 0xBB, 0x72),
    on_green: rgb(0x06, 0x14, 0x0B),
    fg: rgb(0xE3, 0xED, 0xE6),
    muted: rgb(0x93, 0xA6, 0x9A),
    faint: rgb(0x5C, 0x6F, 0x63),
    amber: rgb(0xF0, 0xA8, 0x30),
    red: rgb(0xF0, 0x6C, 0x6C),
    can_a: rgb(0x6C, 0xB6, 0xFF),
    can_b: rgb(0xB7, 0x9B, 0xFF),
    lin: rgb(0xFF, 0xB0, 0x66),
    eth: rgb(0xFF, 0x7A, 0xB6),
    gw: rgb(0x4D, 0xDC, 0xE6),
};

fn visuals(p: &Pal, dark: bool) -> egui::Visuals {
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.page;
    v.faint_bg_color = p.soft.gamma_multiply(0.45);
    v.window_stroke = Stroke::new(1.0_f32, p.line);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(40),
    };
    v.selection.bg_fill = p.soft;
    v.selection.stroke = Stroke::new(1.0_f32, p.green);
    v.hyperlink_color = p.green;
    let r = CornerRadius::same(6);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.fg);
    v.widgets.inactive.weak_bg_fill = p.panel;
    v.widgets.inactive.bg_fill = p.page;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.fg);
    v.widgets.hovered.weak_bg_fill = p.soft;
    v.widgets.hovered.bg_fill = p.soft;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.green);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.fg);
    v.widgets.active.weak_bg_fill = p.soft;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.green);
    v.widgets.open.weak_bg_fill = p.soft;
    v
}

/// Vertical gradient fill; egui has no gradient brush, so paint a 4-vertex mesh.
fn gradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(rect.left_top(), top);
    m.colored_vertex(rect.right_top(), top);
    m.colored_vertex(rect.right_bottom(), bottom);
    m.colored_vertex(rect.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(m));
}

// ---------------------------------------------------------------- icons

mod icon {
    use super::*;
    pub fn play() -> ImageSource<'static> {
        include_image!("../assets/icons/play.svg")
    }
    pub fn stop() -> ImageSource<'static> {
        include_image!("../assets/icons/stop.svg")
    }
    pub fn pause() -> ImageSource<'static> {
        include_image!("../assets/icons/pause.svg")
    }
    pub fn ecu() -> ImageSource<'static> {
        include_image!("../assets/icons/ecu.svg")
    }
    pub fn gateway() -> ImageSource<'static> {
        include_image!("../assets/icons/gateway.svg")
    }
    pub fn bus() -> ImageSource<'static> {
        include_image!("../assets/icons/bus.svg")
    }
    pub fn filter() -> ImageSource<'static> {
        include_image!("../assets/icons/filter.svg")
    }
    pub fn send() -> ImageSource<'static> {
        include_image!("../assets/icons/send.svg")
    }
    pub fn script() -> ImageSource<'static> {
        include_image!("../assets/icons/script.svg")
    }
    pub fn clear() -> ImageSource<'static> {
        include_image!("../assets/icons/clear.svg")
    }
    pub fn trace() -> ImageSource<'static> {
        include_image!("../assets/icons/trace-chronological.svg")
    }
    pub fn replay() -> ImageSource<'static> {
        include_image!("../assets/icons/replay.svg")
    }
    pub fn open() -> ImageSource<'static> {
        include_image!("../assets/icons/open.svg")
    }
    pub fn save() -> ImageSource<'static> {
        include_image!("../assets/icons/save.svg")
    }
}

fn img(src: ImageSource<'static>, tint: Color32, size: f32) -> egui::Image<'static> {
    egui::Image::new(src)
        .tint(tint)
        .fit_to_exact_size(Vec2::splat(size))
}

/// The one green call-to-action of an area.
fn primary(
    ui: &mut egui::Ui,
    p: &Pal,
    src: Option<ImageSource<'static>>,
    text: &str,
) -> egui::Response {
    let text = RichText::new(text).color(p.on_green).strong();
    let b = match src {
        Some(s) => egui::Button::image_and_text(img(s, p.on_green, 14.0), text),
        None => egui::Button::new(text),
    };
    ui.add(
        b.fill(p.green)
            .stroke(Stroke::new(1.0_f32, p.green))
            .min_size(vec2(0.0, 28.0)),
    )
}

fn icon_btn(
    ui: &mut egui::Ui,
    p: &Pal,
    src: ImageSource<'static>,
    tip: &str,
    on: bool,
) -> egui::Response {
    let tint = if on { p.green } else { p.muted };
    let b = egui::Button::image(img(src, tint, 18.0))
        .fill(if on { p.soft } else { Color32::TRANSPARENT })
        .stroke(Stroke::NONE)
        .min_size(vec2(36.0, 36.0));
    ui.add(b).on_hover_text(tip)
}

fn tag(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.into(), FontId::proportional(9.5), Color32::WHITE);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(10.0, 4.0), Sense::hover());
    ui.painter().rect_filled(rect, 3, color);
    ui.painter()
        .galley(rect.min + vec2(5.0, 2.0), galley, Color32::WHITE);
}

fn meter(ui: &mut egui::Ui, frac: f32, color: Color32, track: Color32, w: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 5.0), Sense::hover());
    ui.painter().rect_filled(rect, 3, track);
    let mut f = rect;
    f.set_width(w * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(f, 3, color);
}

// ---------------------------------------------------------------- tabs

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Network,
    Trace,
    Properties,
    Graph,
    Diagnostics,
    Tests,
    Log,
    Statistics,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Network => "Network",
            Tab::Trace => "Trace",
            Tab::Properties => "Properties",
            Tab::Graph => "Graph 1 · EngineSpeed",
            Tab::Diagnostics => "Diagnostics · Engine",
            Tab::Tests => "Tests",
            Tab::Log => "Log",
            Tab::Statistics => "Statistics",
        }
    }
    fn icon(self) -> ImageSource<'static> {
        match self {
            Tab::Network => icon::bus(),
            Tab::Trace => icon::trace(),
            Tab::Properties => icon::ecu(),
            Tab::Graph => icon::replay(),
            Tab::Diagnostics => icon::send(),
            Tab::Tests => icon::script(),
            Tab::Log => icon::open(),
            Tab::Statistics => icon::filter(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Workspace {
    Design,
    Run,
    Diag,
    Test,
}

impl Workspace {
    const ALL: [Workspace; 4] = [
        Workspace::Design,
        Workspace::Run,
        Workspace::Diag,
        Workspace::Test,
    ];
    fn label(self) -> &'static str {
        match self {
            Workspace::Design => "Design",
            Workspace::Run => "Run & Analyse",
            Workspace::Diag => "Diagnostics",
            Workspace::Test => "Test",
        }
    }
    fn layout(self) -> DockState<Tab> {
        let mut d;
        match self {
            Workspace::Design => {
                d = DockState::new(vec![Tab::Network]);
                d.main_surface_mut()
                    .split_right(NodeIndex::root(), 0.76, vec![Tab::Properties]);
            }
            Workspace::Run => {
                d = DockState::new(vec![Tab::Network]);
                let t = d.main_surface_mut();
                let [main, _] = t.split_right(NodeIndex::root(), 0.78, vec![Tab::Properties]);
                t.split_below(main, 0.56, vec![Tab::Trace, Tab::Log, Tab::Statistics]);
                let w = d.add_window(vec![Tab::Graph]);
                if let Some(s) = d.get_window_state_mut(w) {
                    s.set_position(pos2(780.0, 330.0))
                        .set_size(vec2(380.0, 210.0));
                }
            }
            Workspace::Diag => {
                d = DockState::new(vec![Tab::Diagnostics]);
                let t = d.main_surface_mut();
                t.split_below(NodeIndex::root(), 0.5, vec![Tab::Trace]);
                t.split_right(NodeIndex::root(), 0.62, vec![Tab::Properties]);
            }
            Workspace::Test => {
                d = DockState::new(vec![Tab::Tests]);
                d.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.5,
                    vec![Tab::Trace, Tab::Log],
                );
            }
        }
        d
    }
}

// ---------------------------------------------------------------- sample data

#[derive(Clone, Copy, PartialEq, Eq)]
enum Proto {
    Can,
    CanFd,
    Lin,
    Eth,
}

struct Row {
    t: &'static str,
    net: &'static str,
    id: &'static str,
    proto: Proto,
    name: &'static str,
    tx: bool,
    details: &'static str,
    data: &'static str,
    changed: &'static [usize],
}

const ROWS: &[Row] = &[
    Row {
        t: "2.330810",
        net: "Powertrain",
        id: "0x100",
        proto: Proto::CanFd,
        name: "EngineData",
        tx: false,
        details: "BRS · 2 Mbit/s",
        data: "0C 1F 00 00 A0 0F 00 00",
        changed: &[0, 1],
    },
    Row {
        t: "2.331044",
        net: "Backbone",
        id: "0x1234.8001",
        proto: Proto::Eth,
        name: "VehicleSpeed",
        tx: true,
        details: "SOME/IP notif · VLAN 10 · .1 > .10",
        data: "00 00 12 34 80 01 00 08",
        changed: &[6, 7],
    },
    Row {
        t: "2.340810",
        net: "Body",
        id: "0x100",
        proto: Proto::Can,
        name: "EngineData",
        tx: false,
        details: "via Central GW",
        data: "0C 1F 00 00 A0 0F 00 00",
        changed: &[],
    },
    Row {
        t: "2.345000",
        net: "SeatLIN",
        id: "PID 0x21",
        proto: Proto::Lin,
        name: "SeatPosition",
        tx: false,
        details: "slot 3 · enhanced cs",
        data: "3C 12 00 7F",
        changed: &[1],
    },
    Row {
        t: "2.350810",
        net: "Powertrain",
        id: "0x100",
        proto: Proto::CanFd,
        name: "EngineData",
        tx: false,
        details: "BRS · 2 Mbit/s",
        data: "0C 20 00 00 A0 0F 00 00",
        changed: &[1],
    },
    Row {
        t: "2.352210",
        net: "Backbone",
        id: "0x0101.0001",
        proto: Proto::Eth,
        name: "GetRoute",
        tx: false,
        details: "SOME/IP request · .20 > .30",
        data: "00 00 01 01 00 01 00 10",
        changed: &[],
    },
    Row {
        t: "2.360000",
        net: "SeatLIN",
        id: "PID 0x3C",
        proto: Proto::Lin,
        name: "MasterReq",
        tx: true,
        details: "diag · NAD 2",
        data: "02 06 B2 00 FF 7F FF FF",
        changed: &[],
    },
    Row {
        t: "2.400300",
        net: "Body",
        id: "0x200",
        proto: Proto::Can,
        name: "DoorStatus",
        tx: true,
        details: "",
        data: "01 00",
        changed: &[0],
    },
    Row {
        t: "2.400840",
        net: "Powertrain",
        id: "0x200",
        proto: Proto::CanFd,
        name: "DoorStatus",
        tx: false,
        details: "via Central GW",
        data: "01 00",
        changed: &[],
    },
    Row {
        t: "2.410810",
        net: "Powertrain",
        id: "0x100",
        proto: Proto::CanFd,
        name: "EngineData",
        tx: false,
        details: "BRS · 2 Mbit/s",
        data: "0C 21 00 00 A1 0F 00 00",
        changed: &[1, 4],
    },
];

fn proto_style(p: &Pal, proto: Proto) -> (&'static str, Color32) {
    match proto {
        Proto::Can => ("CAN", p.can_b),
        Proto::CanFd => ("CAN FD", p.can_a),
        Proto::Lin => ("LIN", p.lin),
        Proto::Eth => ("ETH", p.eth),
    }
}

fn net_color(p: &Pal, net: &str) -> Color32 {
    match net {
        "Powertrain" => p.can_a,
        "Body" => p.can_b,
        "SeatLIN" => p.lin,
        _ => p.eth,
    }
}

// ---------------------------------------------------------------- app

struct Draft {
    dark: bool,
    ws: Workspace,
    docks: [DockState<Tab>; 4],
    /// Layout to restore when leaving a maximised tab.
    saved: Option<DockState<Tab>>,
    minimised: Vec<Tab>,
    focus: bool,
    running: bool,
    selected: &'static str,
    trace_filter: Option<Proto>,
    name: String,
}

impl Draft {
    fn new() -> Self {
        Self {
            dark: false,
            ws: Workspace::Run,
            docks: Workspace::ALL.map(Workspace::layout),
            saved: None,
            minimised: vec![Tab::Diagnostics, Tab::Tests],
            focus: false,
            running: true,
            selected: "Engine",
            trace_filter: None,
            name: "Engine".into(),
        }
    }
    fn pal(&self) -> Pal {
        if self.dark { DARK } else { LIGHT }
    }
    fn dock(&mut self) -> &mut DockState<Tab> {
        &mut self.docks[self.ws as usize]
    }
    fn open(&mut self, tab: Tab) {
        self.minimised.retain(|t| *t != tab);
        let d = self.dock();
        if let Some(loc) = d.find_tab(&tab) {
            d.set_active_tab(loc);
        } else {
            d.push_to_focused_leaf(tab);
        }
    }
}

enum Req {
    Maximise(Tab),
    Minimise(Tab),
    Float(Tab),
}

struct Viewer<'a> {
    p: Pal,
    t: f64,
    running: bool,
    selected: &'a mut &'static str,
    trace_filter: &'a mut Option<Proto>,
    name: &'a mut String,
    req: Option<Req>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Network => self.network(ui),
            Tab::Trace => self.trace(ui),
            Tab::Properties => self.properties(ui),
            Tab::Graph => self.graph(ui),
            Tab::Diagnostics => self.diagnostics(ui),
            Tab::Tests => self.tests(ui),
            Tab::Log => {
                ui.add_space(6.0);
                for l in [
                    "[2.000] Engine: script started",
                    "[2.100] SeatCtrl: LIN schedule 'normal' active",
                    "[2.300] DoorLeft: error passive (TEC 132)",
                ] {
                    ui.label(RichText::new(l).monospace().color(self.p.muted));
                }
            }
            Tab::Statistics => self.statistics(ui),
        }
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        if response.double_clicked() {
            self.req = Some(Req::Maximise(*tab));
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Tab, _s: SurfaceIndex, _n: NodeIndex) {
        if ui.button("Maximise / restore (double-click)").clicked() {
            self.req = Some(Req::Maximise(*tab));
        }
        if ui.button("Minimise to edge strip").clicked() {
            self.req = Some(Req::Minimise(*tab));
        }
        if ui.button("Float").clicked() {
            self.req = Some(Req::Float(*tab));
        }
        ui.add_enabled(false, egui::Button::new("Pop out to OS window (next step)"));
    }

    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::Network | Tab::Trace | Tab::Graph => [false, false],
            _ => [false, true],
        }
    }
}

impl Viewer<'_> {
    fn toolbar(&self, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| ui.horizontal(add));
        let r = ui.min_rect();
        ui.painter().hline(
            r.x_range(),
            ui.cursor().top(),
            Stroke::new(1.0_f32, self.p.line),
        );
    }

    fn network(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        self.toolbar(ui, |ui| {
            ui.add(egui::Button::image_and_text(
                img(icon::ecu(), p.muted, 14.0),
                "ECU",
            ));
            ui.add(egui::Button::image_and_text(
                img(icon::gateway(), p.muted, 14.0),
                "Gateway",
            ));
            ui.add(egui::Button::image_and_text(
                img(icon::bus(), p.muted, 14.0),
                "Network",
            ));
            ui.separator();
            for (i, l) in ["All", "CAN", "LIN", "ETH"].iter().enumerate() {
                let _ = ui.selectable_label(i == 0, *l);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.running {
                    ui.label(
                        RichText::new("Read-only while running")
                            .color(p.muted)
                            .small(),
                    );
                }
            });
        });

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let painter = ui.painter_at(rect);
        gradient(&painter, rect, p.g1, p.g2);
        let step = 22.0;
        let mut y = rect.top() + step;
        while y < rect.bottom() {
            let mut x = rect.left() + step;
            while x < rect.right() {
                painter.circle_filled(pos2(x, y), 1.0, p.grid);
                x += step;
            }
            y += step;
        }

        // Design space is 900 x 540; fit it into the tab.
        let s = (rect.width() / 920.0)
            .min(rect.height() / 560.0)
            .clamp(0.45, 1.6);
        let o = rect.min + vec2(10.0, 10.0);
        let at = |x: f32, y: f32| o + vec2(x, y) * s;
        let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(at(x, y), vec2(w, h) * s);
        let font = |px: f32| FontId::proportional(px * s.max(0.8));
        let mono = |px: f32| FontId::monospace(px * s.max(0.8));

        // Domains
        for (x, y, w, h, name) in [
            (20.0, 22.0, 500.0, 136.0, "Powertrain"),
            (20.0, 190.0, 500.0, 330.0, "Body & comfort"),
            (700.0, 22.0, 192.0, 498.0, "Ethernet backbone"),
        ] {
            let d = r(x, y, w, h);
            painter.rect_filled(d, 12, p.panel.gamma_multiply(0.45));
            let c = [
                d.left_top(),
                d.right_top(),
                d.right_bottom(),
                d.left_bottom(),
                d.left_top(),
            ];
            painter.extend(Shape::dashed_line(
                &c,
                Stroke::new(1.5_f32, p.line),
                6.0,
                4.0,
            ));
            let g = painter.layout_no_wrap(name.into(), font(11.0), p.muted);
            let pill =
                Rect::from_min_size(d.left_top() + vec2(12.0, -9.0), g.size() + vec2(16.0, 4.0));
            painter.rect(
                pill,
                9,
                p.panel,
                Stroke::new(1.0_f32, p.line),
                StrokeKind::Inside,
            );
            painter.galley(pill.min + vec2(8.0, 2.0), g, p.muted);
        }

        // Wires
        let wire = |pts: &[(f32, f32)], c: Color32, w: f32, dashed: bool| {
            let pts: Vec<Pos2> = pts.iter().map(|(x, y)| at(*x, *y)).collect();
            if dashed {
                painter.extend(Shape::dashed_line(&pts, Stroke::new(w, c), 4.0, 3.0));
            } else {
                painter.add(Shape::line(pts, Stroke::new(w * s.max(0.8), c)));
            }
        };
        wire(&[(135.0, 102.0), (135.0, 116.0)], p.can_a, 2.0, false);
        wire(&[(135.0, 268.0), (135.0, 292.0)], p.can_b, 2.0, false);
        wire(&[(375.0, 268.0), (375.0, 292.0)], p.can_b, 2.0, false);
        wire(&[(135.0, 326.0), (135.0, 400.0)], p.can_b, 2.0, false);
        wire(&[(135.0, 458.0), (135.0, 474.0)], p.lin, 2.0, false);
        wire(&[(375.0, 458.0), (375.0, 474.0)], p.lin, 2.0, false);
        let gpt = [(500.0, 133.0), (615.0, 133.0), (615.0, 200.0)];
        let gbd = [(500.0, 309.0), (615.0, 309.0), (615.0, 258.0)];
        let geth = [(690.0, 229.0), (702.0, 229.0), (702.0, 89.0), (715.0, 89.0)];
        let e1 = [(790.0, 118.0), (790.0, 200.0)];
        let e2 = [
            (840.0, 118.0),
            (840.0, 128.0),
            (880.0, 128.0),
            (880.0, 359.0),
            (865.0, 359.0),
        ];
        let e3 = [
            (855.0, 118.0),
            (855.0, 122.0),
            (887.0, 122.0),
            (887.0, 479.0),
            (865.0, 479.0),
        ];
        wire(&gpt, p.gw, 2.0, true);
        wire(&gbd, p.gw, 2.0, true);
        for w in [&geth[..], &e1, &e2, &e3] {
            wire(w, p.eth, 2.5, false);
        }

        // Buses
        let bus = |y: f32, h: f32, c: Color32, label: &str, load: &str, frac: f32| {
            let b = r(40.0, y, 460.0, h);
            painter.add(
                egui::Shadow {
                    offset: [0, 2],
                    blur: 8,
                    spread: 0,
                    color: Color32::from_black_alpha(25),
                }
                .as_shape(b, 8),
            );
            painter.rect_filled(b, 8, c);
            painter.text(
                b.left_center() + vec2(12.0 * s, 0.0),
                Align2::LEFT_CENTER,
                label,
                font(12.5),
                Color32::WHITE,
            );
            let m = Rect::from_center_size(
                b.right_center() - vec2(110.0 * s, 0.0),
                vec2(70.0 * s, 6.0),
            );
            painter.rect_filled(m, 3, Color32::from_white_alpha(70));
            painter.rect_filled(
                Rect::from_min_size(m.min, vec2(m.width() * frac, 6.0)),
                3,
                Color32::WHITE,
            );
            painter.text(
                b.right_center() - vec2(12.0 * s, 0.0),
                Align2::RIGHT_CENTER,
                load,
                mono(11.0),
                Color32::WHITE,
            );
        };
        bus(
            116.0,
            34.0,
            p.can_a,
            "Powertrain · CAN FD · 500k/2M",
            "3.0 %",
            0.30,
        );
        bus(292.0, 34.0, p.can_b, "Body · CAN · 250k", "5.9 %", 0.59);
        bus(
            474.0,
            26.0,
            p.lin,
            "SeatLIN · LIN 2.2 · 19.2 kbit/s",
            "sched 42 %",
            0.42,
        );

        // Moving frames
        let t = self.t as f32;
        let dot = |path: &[(f32, f32)], period: f32, phase: f32, c: Color32| {
            let pts: Vec<Pos2> = path.iter().map(|(x, y)| at(*x, *y)).collect();
            let lens: Vec<f32> = pts.windows(2).map(|w| w[0].distance(w[1])).collect();
            let total: f32 = lens.iter().sum();
            let mut d = ((t / period + phase).fract()) * total;
            for (i, l) in lens.iter().enumerate() {
                if d <= *l {
                    painter.circle_filled(pts[i].lerp(pts[i + 1], d / l), 4.0 * s.max(0.8), c);
                    return;
                }
                d -= l;
            }
        };
        if self.running {
            dot(&[(60.0, 133.0), (500.0, 133.0)], 1.6, 0.0, p.green);
            dot(&[(60.0, 133.0), (500.0, 133.0)], 1.6, 0.5, p.green);
            dot(&[(500.0, 309.0), (60.0, 309.0)], 2.0, 0.0, p.green);
            dot(&[(60.0, 487.0), (500.0, 487.0)], 3.5, 0.0, p.lin);
            dot(&gpt, 1.2, 0.0, p.gw);
            dot(&geth, 0.7, 0.0, p.eth);
            dot(&e1, 0.6, 0.0, p.eth);
            dot(&e2, 0.9, 0.3, p.eth);
            ui.ctx().request_repaint();
        }

        // Nodes
        let nodes = [
            (
                "Engine", 60.0, 44.0, 150.0, p.can_a, "TX 100/s", "ACTIVE", p.green,
            ),
            (
                "BodyCtrl", 60.0, 210.0, 150.0, p.can_b, "TX 10/s", "ACTIVE", p.green,
            ),
            (
                "DoorLeft", 300.0, 210.0, 150.0, p.can_b, "TEC 132", "PASSIVE", p.amber,
            ),
            (
                "SeatCtrl",
                60.0,
                400.0,
                170.0,
                p.lin,
                "LIN master",
                "CAN+LIN",
                p.green,
            ),
            (
                "SeatMotor",
                300.0,
                400.0,
                150.0,
                p.lin,
                "NAD 2",
                "PID 21",
                p.muted,
            ),
            (
                "Central GW",
                540.0,
                200.0,
                150.0,
                p.gw,
                "CAN-ETH",
                "SOME/IP",
                p.muted,
            ),
            (
                "ETH Switch",
                715.0,
                60.0,
                150.0,
                p.eth,
                "5 ports",
                "VLAN 10,20",
                p.muted,
            ),
            (
                "ADAS",
                715.0,
                200.0,
                150.0,
                p.eth,
                ".10",
                "48 Mbit/s",
                p.muted,
            ),
            (
                "Infotainment",
                715.0,
                330.0,
                150.0,
                p.eth,
                ".20",
                "12 Mbit/s",
                p.muted,
            ),
            (
                "Telematics",
                715.0,
                450.0,
                150.0,
                p.eth,
                ".30",
                "2 Mbit/s",
                p.muted,
            ),
        ];
        let click = resp
            .clicked()
            .then(|| resp.interact_pointer_pos())
            .flatten();
        for (name, x, y, w, c, a, b, bc) in nodes {
            let n = r(x, y, w, 58.0);
            if click.is_some_and(|c| n.contains(c)) {
                *self.selected = name;
            }
            painter.add(
                egui::Shadow {
                    offset: [0, 3],
                    blur: 12,
                    spread: 0,
                    color: Color32::from_black_alpha(22),
                }
                .as_shape(n, 10),
            );
            painter.rect(
                n,
                10,
                p.panel,
                Stroke::new(1.0_f32, p.line),
                StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_min_size(n.min, vec2(n.width(), 3.0)),
                CornerRadius {
                    nw: 10,
                    ne: 10,
                    sw: 0,
                    se: 0,
                },
                c,
            );
            if *self.selected == name {
                painter.rect_stroke(
                    n.expand(3.0),
                    12,
                    Stroke::new(2.0_f32, p.green),
                    StrokeKind::Outside,
                );
            }
            let src = if name.contains("GW") {
                icon::gateway()
            } else {
                icon::ecu()
            };
            let ir =
                Rect::from_min_size(n.min + vec2(10.0, 9.0) * s, Vec2::splat(15.0 * s.max(0.8)));
            img(src, if name.contains("GW") { p.gw } else { p.fg }, 15.0).paint_at(ui, ir);
            painter.text(
                n.min + vec2(32.0, 16.0) * s,
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(13.0 * s.max(0.8)),
                p.fg,
            );
            let led = if b == "PASSIVE" { p.amber } else { p.green };
            painter.circle_filled(pos2(n.right() - 12.0 * s, n.top() + 16.0 * s), 4.0, led);
            painter.text(
                n.left_bottom() + vec2(10.0, -14.0) * s,
                Align2::LEFT_CENTER,
                a,
                mono(10.5),
                p.muted,
            );
            painter.text(
                n.right_bottom() + vec2(-10.0, -14.0) * s,
                Align2::RIGHT_CENTER,
                b,
                mono(10.0),
                bc,
            );
        }

        // Link labels
        let pill = |x: f32, y: f32, text: &str| {
            let g = painter.layout_no_wrap(text.into(), mono(9.5), p.muted);
            let b = Rect::from_center_size(at(x, y), g.size() + vec2(12.0, 4.0));
            painter.rect(
                b,
                8,
                p.panel,
                Stroke::new(1.0_f32, p.line),
                StrokeKind::Inside,
            );
            painter.galley(b.min + vec2(6.0, 2.0), g, p.muted);
        };
        pill(752.0, 158.0, "1000BASE-T1");
        pill(580.0, 178.0, "100-1FF");

        // Zoom readout
        let z = Rect::from_min_size(rect.left_bottom() + vec2(12.0, -40.0), vec2(120.0, 28.0));
        painter.rect(
            z,
            8,
            p.panel,
            Stroke::new(1.0_f32, p.line),
            StrokeKind::Inside,
        );
        painter.text(
            z.center(),
            Align2::CENTER_CENTER,
            format!("-   {:.0}%   +", s * 100.0),
            mono(11.0),
            p.muted,
        );
    }

    fn trace(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let mut f = *self.trace_filter;
        self.toolbar_trace(ui, &mut f);
        *self.trace_filter = f;
        let filter = f;
        let rows: Vec<&Row> = ROWS
            .iter()
            .filter(|r| {
                filter.is_none_or(|f| f == r.proto || (f == Proto::Can && r.proto == Proto::CanFd))
            })
            .collect();
        use egui_extras::{Column, TableBuilder};
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(86.0))
            .column(Column::exact(104.0))
            .column(Column::exact(100.0))
            .column(Column::exact(58.0))
            .column(Column::exact(104.0))
            .column(Column::exact(38.0))
            .column(Column::initial(220.0).clip(true))
            .column(Column::remainder())
            .header(24.0, |mut h| {
                for (c, f) in [
                    ("Time (s)", false),
                    ("Chn", true),
                    ("ID / Addr", true),
                    ("Proto", false),
                    ("Name", false),
                    ("Dir", false),
                    ("Details", false),
                    ("Data", false),
                ] {
                    h.col(|ui| {
                        ui.label(RichText::new(c).strong().small().color(if f {
                            p.green
                        } else {
                            p.muted
                        }));
                        if f {
                            ui.add(img(icon::filter(), p.green, 11.0));
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let r = rows[row.index()];
                    row.col(|ui| {
                        let (a, z) = r.t.split_at(r.t.trim_end_matches('0').len());
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(RichText::new(a).monospace());
                        ui.label(RichText::new(z).monospace().color(p.faint));
                    });
                    row.col(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2, net_color(&p, r.net));
                        ui.label(RichText::new(r.net).monospace());
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(r.id).monospace());
                    });
                    row.col(|ui| {
                        let (t, c) = proto_style(&p, r.proto);
                        tag(ui, t, c);
                    });
                    row.col(|ui| {
                        ui.label(r.name);
                    });
                    row.col(|ui| {
                        let (t, c) = if r.tx {
                            ("TX", p.green)
                        } else {
                            ("RX", p.can_a)
                        };
                        ui.label(RichText::new(t).small().strong().color(c));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(r.details).color(p.muted));
                    });
                    row.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        for (i, b) in r.data.split(' ').enumerate() {
                            let t = RichText::new(b).monospace();
                            if r.changed.contains(&i) {
                                ui.label(t.background_color(p.green.gamma_multiply(0.25)).strong());
                            } else {
                                ui.label(t);
                            }
                        }
                    });
                });
            });
    }

    fn toolbar_trace(&self, ui: &mut egui::Ui, filter: &mut Option<Proto>) {
        let p = self.p;
        self.toolbar(ui, |ui| {
            ui.add(egui::Button::image(img(icon::pause(), p.muted, 14.0)))
                .on_hover_text("Pause view");
            ui.add(egui::Button::image(img(icon::clear(), p.muted, 14.0)))
                .on_hover_text("Clear view");
            ui.separator();
            for (l, v) in [
                ("All", None),
                ("CAN", Some(Proto::Can)),
                ("LIN", Some(Proto::Lin)),
                ("ETH", Some(Proto::Eth)),
            ] {
                if ui.selectable_label(*filter == v, l).clicked() {
                    *filter = v;
                }
            }
            ui.separator();
            ui.add(
                egui::Button::image_and_text(
                    img(icon::filter(), p.green, 14.0),
                    RichText::new("2 filters").color(p.green),
                )
                .stroke(Stroke::new(1.0_f32, p.green)),
            );
            let _ = ui.button("Columns");
            let _ = ui.button("Export CSV");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("278 rows · 412 msg/s")
                        .monospace()
                        .color(p.muted),
                );
            });
        });
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let card = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Frame::new()
                .stroke(Stroke::new(1.0_f32, p.line))
                .corner_radius(8)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    add(ui)
                });
            ui.add_space(8.0);
        };
        ui.add_space(8.0);
        let sel = *self.selected;
        card(ui, &mut |ui| {
            ui.horizontal(|ui| {
                ui.add(img(icon::ecu(), p.fg, 16.0));
                ui.strong(sel);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (t, c) = if sel == "DoorLeft" {
                        ("ERROR PASSIVE", p.amber)
                    } else {
                        ("ERROR ACTIVE", p.green)
                    };
                    ui.label(RichText::new(t).small().strong().color(c));
                });
            });
            egui::Grid::new("kv")
                .num_columns(2)
                .spacing([20.0, 4.0])
                .show(ui, |ui| {
                    for (k, v) in [
                        ("Bus", "Powertrain"),
                        ("TEC / REC", "0 / 0"),
                        ("Messages", "1"),
                        ("TX rate", "100 /s"),
                    ] {
                        ui.label(RichText::new(k).color(p.muted));
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    }
                });
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
            let ys = [30.0, 28.0, 29.0, 22.0, 24.0, 18.0, 20.0, 16.0];
            let pts: Vec<Pos2> = ys
                .iter()
                .enumerate()
                .map(|(i, y)| pos2(rect.left() + rect.width() * i as f32 / 7.0, rect.top() + y))
                .collect();
            let mut fill = pts.clone();
            fill.push(rect.right_bottom());
            fill.push(rect.left_bottom());
            ui.painter().add(Shape::convex_polygon(
                vec![
                    pts[0],
                    *pts.last().unwrap(),
                    rect.right_bottom(),
                    rect.left_bottom(),
                ],
                p.soft,
                Stroke::NONE,
            ));
            ui.painter()
                .add(Shape::line(pts, Stroke::new(1.5_f32, p.green)));
        });
        card(ui, &mut |ui| {
            ui.label(RichText::new("Name").small().color(p.muted));
            ui.add(egui::TextEdit::singleline(self.name).desired_width(f32::INFINITY));
            ui.label(RichText::new("Script").small().color(p.muted));
            ui.add(
                egui::TextEdit::singleline(&mut "engine.rhai".to_string())
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                primary(ui, &p, None, "  Apply  ");
                let _ = ui.add(egui::Button::new("Revert").min_size(vec2(0.0, 28.0)));
            });
        });
        card(ui, &mut |ui| {
            ui.strong("EngineData · 0x100");
            ui.horizontal(|ui| {
                ui.label(RichText::new("Send type").color(p.muted));
                ui.label(RichText::new("Cyclic 10 ms").monospace());
            });
            primary(ui, &p, Some(icon::send()), "Send once");
        });
    }

    fn graph(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let rect = rect.shrink(10.0);
        let painter = ui.painter_at(rect.expand(10.0));
        for i in 1..4 {
            let y = rect.top() + rect.height() * i as f32 / 4.0;
            painter.hline(rect.x_range(), y, Stroke::new(1.0_f32, p.line));
        }
        let t = self.t as f32;
        let n = 120;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let x = i as f32 / n as f32;
                let v = 0.5 + 0.3 * (x * 6.0 + t * 0.8).sin() + 0.1 * (x * 17.0 + t).sin();
                pos2(
                    rect.left() + x * rect.width(),
                    rect.bottom() - v * rect.height(),
                )
            })
            .collect();
        let gear: Vec<Pos2> = (0..=n)
            .map(|i| {
                let x = i as f32 / n as f32;
                let v = (((x * 3.0 + t * 0.2).sin() + 1.0) * 2.0).floor() / 6.0 + 0.1;
                pos2(
                    rect.left() + x * rect.width(),
                    rect.bottom() - v * rect.height(),
                )
            })
            .collect();
        painter.add(Shape::line(gear, Stroke::new(1.5_f32, p.can_a)));
        painter.add(Shape::line(pts.clone(), Stroke::new(2.0_f32, p.green)));
        painter.circle_filled(*pts.last().unwrap(), 3.5, p.green);
        painter.text(
            rect.left_top(),
            Align2::LEFT_TOP,
            "EngineSpeed 3120 rpm   Gear 3",
            FontId::monospace(11.0),
            p.muted,
        );
        if self.running {
            ui.ctx().request_repaint();
        }
    }

    fn diagnostics(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        ui.add_space(6.0);
        egui::Grid::new("dg")
            .num_columns(2)
            .spacing([20.0, 4.0])
            .show(ui, |ui| {
                for (k, v) in [("Session", "Extended (0x03)"), ("Security", "Unlocked L1")] {
                    ui.label(RichText::new(k).color(p.muted));
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                }
            });
        egui::Frame::new()
            .fill(p.page)
            .stroke(Stroke::new(1.0_f32, p.line))
            .corner_radius(6)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("> 22 F1 90").monospace());
                ui.label(
                    RichText::new("< 62 F1 90 57 30 4C ... VIN")
                        .monospace()
                        .color(p.green),
                );
                ui.label(RichText::new("> 19 02 FF").monospace());
                ui.label(
                    RichText::new("< 59 02 FF 01 23 45 09")
                        .monospace()
                        .color(p.green),
                );
            });
        ui.add_space(6.0);
        primary(ui, &p, Some(icon::send()), "Send request");
    }

    fn tests(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            primary(ui, &p, Some(icon::play()), "Run tests");
            ui.label(RichText::new("11 passed").color(p.green));
            ui.label(RichText::new("1 failed").color(p.red));
        });
        meter(ui, 0.92, p.green, p.red, ui.available_width());
        ui.add_space(6.0);
        for (n, ok, d) in [
            ("route_100_to_body", true, "0.5 ms"),
            ("route_latency_under_1ms", false, "1.4 ms"),
            ("door_status_on_change", true, ""),
            ("lin_seat_schedule", true, ""),
        ] {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if ok { "PASS" } else { "FAIL" })
                        .small()
                        .strong()
                        .color(if ok { p.green } else { p.red }),
                );
                ui.label(RichText::new(n).monospace());
                ui.label(RichText::new(d).color(p.muted));
            });
        }
    }

    fn statistics(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        ui.add_space(6.0);
        egui::Grid::new("st")
            .num_columns(4)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for h in ["Network", "Type", "Load", ""] {
                    ui.label(RichText::new(h).strong().small().color(p.muted));
                }
                ui.end_row();
                for (n, t, frac, txt) in [
                    ("Powertrain", Proto::CanFd, 0.03, "3.0 %"),
                    ("Body", Proto::Can, 0.059, "5.9 %"),
                    ("SeatLIN", Proto::Lin, 0.42, "42 % sched"),
                    ("Backbone", Proto::Eth, 0.062, "62 Mbit/s of 1 G"),
                ] {
                    ui.label(n);
                    let (l, c) = proto_style(&p, t);
                    tag(ui, l, c);
                    meter(ui, frac * 4.0, net_color(&p, n), p.line, 120.0);
                    ui.label(RichText::new(txt).monospace());
                    ui.end_row();
                }
            });
    }
}

impl eframe::App for Draft {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let p = self.pal();
        ctx.set_visuals(visuals(&p, self.dark));

        // Command bar
        egui::TopBottomPanel::top("cmd")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ctx, |ui| {
                gradient(
                    ui.painter(),
                    ui.max_rect().expand2(vec2(12.0, 8.0)),
                    p.g1,
                    p.g2,
                );
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    ui.painter().rect_filled(r, 6, p.green);
                    ui.painter().text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        "O",
                        FontId::proportional(13.0),
                        p.on_green,
                    );
                    ui.label(RichText::new("Operow").strong().size(15.0));
                    ui.label(RichText::new("· body_gateway.operow").color(p.muted));
                    ui.add_space(8.0);
                    for m in ["File", "Edit", "View", "Simulation", "Tools", "Help"] {
                        ui.menu_button(m, |ui| {
                            ui.add(egui::Button::image_and_text(
                                img(icon::open(), p.muted, 14.0),
                                "Open project…",
                            ));
                            ui.add(egui::Button::image_and_text(
                                img(icon::save(), p.muted, 14.0),
                                "Save",
                            ));
                        });
                    }
                    ui.add_space(ui.available_width() / 2.0 - 330.0);
                    egui::Frame::new()
                        .fill(p.panel)
                        .stroke(Stroke::new(1.0_f32, p.line))
                        .corner_radius(9)
                        .inner_margin(4)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if self.running {
                                    let _ = ui.add_enabled(
                                        false,
                                        egui::Button::image_and_text(
                                            img(icon::play(), p.faint, 14.0),
                                            "Start",
                                        )
                                        .min_size(vec2(0.0, 28.0)),
                                    );
                                } else if primary(ui, &p, Some(icon::play()), "Start").clicked() {
                                    self.running = true;
                                }
                                let _ = ui
                                    .add(
                                        egui::Button::image(img(icon::pause(), p.muted, 14.0))
                                            .min_size(vec2(28.0, 28.0)),
                                    )
                                    .on_hover_text("Pause");
                                if ui
                                    .add(
                                        egui::Button::image(img(icon::stop(), p.muted, 14.0))
                                            .min_size(vec2(28.0, 28.0)),
                                    )
                                    .on_hover_text("Stop")
                                    .clicked()
                                {
                                    self.running = false;
                                }
                                let t = if self.running {
                                    ctx.input(|i| i.time) % 1000.0
                                } else {
                                    0.0
                                };
                                ui.label(
                                    RichText::new(format!(" t = {t:.3} s "))
                                        .monospace()
                                        .size(17.0),
                                );
                                let (txt, c) = if self.running {
                                    ("RUNNING", p.green)
                                } else {
                                    ("STOPPED", p.muted)
                                };
                                egui::Frame::new()
                                    .fill(if self.running { p.soft } else { p.page })
                                    .corner_radius(99)
                                    .inner_margin(egui::Margin::symmetric(9, 3))
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(txt).small().strong().color(c));
                                    });
                                let _ = ui.button("Real-time x1");
                                let _ = ui.add(
                                    egui::Button::new(RichText::new("Record").color(p.red))
                                        .stroke(Stroke::new(1.0_f32, p.red.gamma_multiply(0.5))),
                                );
                            });
                        });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(if self.dark {
                                "Light theme"
                            } else {
                                "Dark theme"
                            })
                            .clicked()
                        {
                            self.dark = !self.dark;
                        }
                    });
                });
            });

        // Workspace bar
        egui::TopBottomPanel::top("ws")
            .frame(
                egui::Frame::new()
                    .fill(p.panel)
                    .inner_margin(egui::Margin::symmetric(12, 0))
                    .stroke(Stroke::new(1.0_f32, p.line)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(36.0);
                    for w in Workspace::ALL {
                        let on = self.ws == w;
                        let txt =
                            RichText::new(w.label()).color(if on { p.green } else { p.muted });
                        let r = ui.add(
                            egui::Button::new(if on { txt.strong() } else { txt })
                                .frame(false)
                                .min_size(vec2(0.0, 34.0)),
                        );
                        if on {
                            ui.painter().hline(
                                r.rect.x_range(),
                                r.rect.bottom() + 1.0,
                                Stroke::new(2.0_f32, p.green),
                            );
                        }
                        if r.clicked() && !on {
                            if let Some(s) = self.saved.take() {
                                *self.dock() = s;
                            }
                            self.ws = w;
                        }
                    }
                    let _ = ui
                        .add(egui::Button::new(RichText::new("+").color(p.muted)).frame(false))
                        .on_hover_text("Save current layout as a workspace");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset layout").clicked() {
                            self.saved = None;
                            *self.dock() = self.ws.layout();
                        }
                        let f = ui.add(
                            egui::Button::new(if self.focus {
                                RichText::new("Focus mode").color(p.green)
                            } else {
                                RichText::new("Focus mode")
                            })
                            .selected(self.focus),
                        );
                        if f.on_hover_text("Hide side panels").clicked() {
                            self.focus = !self.focus;
                        }
                        if self.saved.is_some() {
                            if primary(ui, &p, None, "Restore layout").clicked() {
                                *self.dock() = self.saved.take().unwrap();
                            }
                            ui.label(
                                RichText::new("Maximised: double-click the tab or").color(p.muted),
                            );
                        }
                    });
                });
            });

        // Status bar
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .stroke(Stroke::new(1.0_f32, p.line)),
            )
            .show(ctx, |ui| {
                gradient(
                    ui.painter(),
                    ui.max_rect().expand2(vec2(10.0, 4.0)),
                    p.g2,
                    p.g1,
                );
                ui.horizontal(|ui| {
                    let (t, c) = if self.running {
                        ("Running", p.green)
                    } else {
                        ("Stopped", p.muted)
                    };
                    ui.label(RichText::new(t).small().strong().color(c));
                    for (n, frac, txt) in [
                        ("Powertrain", 0.30, "3.0 %"),
                        ("Body", 0.59, "5.9 %"),
                        ("SeatLIN", 0.42, "42 % sched"),
                        ("Backbone", 0.06, "62 Mbit/s of 1 G"),
                    ] {
                        ui.separator();
                        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2, net_color(&p, n));
                        ui.label(RichText::new(n).small().color(p.muted));
                        meter(ui, frac, net_color(&p, n), p.line, 44.0);
                        ui.label(RichText::new(txt).small().color(p.muted));
                    }
                    ui.separator();
                    ui.label(RichText::new("1 node error passive").small().color(p.amber));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("556 / 1 M frames").small().color(p.muted));
                        meter(ui, 0.04, p.green, p.line, 44.0);
                        ui.label(RichText::new("Buffer").small().color(p.muted));
                    });
                });
            });

        // Activity rail
        let mut open = None;
        egui::SidePanel::left("rail")
            .exact_width(48.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.panel)
                    .inner_margin(6)
                    .stroke(Stroke::new(1.0_f32, p.line)),
            )
            .show(ctx, |ui| {
                for t in [
                    Tab::Network,
                    Tab::Trace,
                    Tab::Graph,
                    Tab::Diagnostics,
                    Tab::Tests,
                    Tab::Statistics,
                ] {
                    let on = self.docks[self.ws as usize].find_tab(&t).is_some();
                    if icon_btn(ui, &p, t.icon(), t.title(), on).clicked() {
                        open = Some(t);
                    }
                }
            });

        // Project tree
        if !self.focus {
            egui::SidePanel::left("tree")
                .default_width(230.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.panel)
                        .inner_margin(8)
                        .stroke(Stroke::new(1.0_f32, p.line)),
                )
                .show(ctx, |ui| {
                    ui.label(RichText::new("PROJECT").small().strong().color(p.muted));
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().show(ui, |ui| self.tree(ui, &p));
                });
        }

        // Edge strip of minimised panels
        egui::SidePanel::right("strip")
            .exact_width(40.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.panel)
                    .inner_margin(4)
                    .stroke(Stroke::new(1.0_f32, p.line)),
            )
            .show(ctx, |ui| {
                for t in self.minimised.clone() {
                    if icon_btn(ui, &p, t.icon(), &format!("Restore {}", t.title()), false)
                        .clicked()
                    {
                        open = Some(t);
                    }
                }
            });
        if let Some(t) = open {
            self.open(t);
        }

        // Dock area
        let mut selected = self.selected;
        let mut filter = self.trace_filter;
        let mut name = std::mem::take(&mut self.name);
        let mut viewer = Viewer {
            p,
            t: ctx.input(|i| i.time),
            running: self.running,
            selected: &mut selected,
            trace_filter: &mut filter,
            name: &mut name,
            req: None,
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.page).inner_margin(4))
            .show(ctx, |ui| {
                let mut style = egui_dock::Style::from_egui(ui.style().as_ref());
                style.tab_bar.bg_fill = p.panel;
                style.tab_bar.hline_color = p.line;
                style.tab.active.bg_fill = p.panel;
                style.tab.active.text_color = p.fg;
                style.tab.focused.bg_fill = p.panel;
                style.tab.focused.text_color = p.green;
                style.tab.inactive.text_color = p.muted;
                style.tab.hline_below_active_tab_name = true;
                style.tab.tab_body.bg_fill = p.panel;
                style.tab.tab_body.stroke = Stroke::new(1.0_f32, p.line);
                style.overlay.selection_color = p.green.gamma_multiply(0.35);
                style.overlay.button_color = p.green;
                style.overlay.button_border_stroke = Stroke::new(1.0_f32, p.green);
                let dock = &mut self.docks[self.ws as usize];
                DockArea::new(dock)
                    .style(style)
                    .show_close_buttons(true)
                    .show_leaf_collapse_buttons(true)
                    .show_inside(ui, &mut viewer);
            });
        let req = viewer.req.take();
        self.selected = selected;
        self.trace_filter = filter;
        self.name = name;
        match req {
            Some(Req::Maximise(tab)) => {
                if let Some(s) = self.saved.take() {
                    *self.dock() = s;
                } else {
                    let full = DockState::new(vec![tab]);
                    self.saved = Some(std::mem::replace(self.dock(), full));
                }
            }
            Some(Req::Minimise(tab)) => {
                if let Some(loc) = self.dock().find_tab(&tab) {
                    self.dock().remove_tab(loc);
                    self.minimised.push(tab);
                }
            }
            Some(Req::Float(tab)) => {
                if let Some(loc) = self.dock().find_tab(&tab) {
                    self.dock().remove_tab(loc);
                    let w = self.dock().add_window(vec![tab]);
                    if let Some(s) = self.dock().get_window_state_mut(w) {
                        s.set_position(pos2(400.0, 220.0))
                            .set_size(vec2(460.0, 280.0));
                    }
                }
            }
            None => {}
        }
    }
}

impl Draft {
    fn tree(&mut self, ui: &mut egui::Ui, p: &Pal) {
        let header = |ui: &mut egui::Ui, txt: &str, n: usize| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(txt).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(n.to_string()).small().color(p.muted));
                });
            });
        };
        header(ui, "Networks", 4);
        for (n, proto, rate) in [
            ("Powertrain", Proto::CanFd, "500k"),
            ("Body", Proto::Can, "250k"),
            ("SeatLIN", Proto::Lin, "19.2k"),
            ("Backbone", Proto::Eth, "1G"),
        ] {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().rect_filled(rect, 2, net_color(p, n));
                ui.label(n);
                let (t, c) = proto_style(p, proto);
                tag(ui, t, c);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(rate).small().color(p.muted));
                });
            });
        }
        ui.add_space(6.0);
        header(ui, "Domains", 3);
        for (d, nodes) in [
            ("Powertrain", &["Engine"][..]),
            (
                "Body & comfort",
                &["BodyCtrl", "DoorLeft", "SeatCtrl", "SeatMotor"],
            ),
            (
                "Ethernet backbone",
                &["ETH Switch", "ADAS", "Infotainment", "Telematics"],
            ),
        ] {
            egui::CollapsingHeader::new(d)
                .default_open(true)
                .show(ui, |ui| {
                    for n in nodes {
                        let on = self.selected == *n;
                        ui.horizontal(|ui| {
                            ui.add(img(icon::ecu(), if on { p.green } else { p.muted }, 14.0));
                            if ui
                                .selectable_label(
                                    on,
                                    RichText::new(*n).color(if on { p.green } else { p.fg }),
                                )
                                .clicked()
                            {
                                self.selected = n;
                            }
                        });
                    }
                });
        }
        ui.horizontal(|ui| {
            ui.add(img(icon::gateway(), p.gw, 14.0));
            ui.label("Central Gateway");
        });
        ui.add_space(6.0);
        header(ui, "Databases", 3);
        for d in ["powertrain.dbc", "seat.ldf", "backbone.arxml"] {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(d).color(p.muted));
            });
        }
        ui.add_space(6.0);
        header(ui, "Tests", 2);
        let _ =
            ui.add(egui::Button::new(RichText::new("+ New network…").color(p.faint)).frame(false));
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Operow: UI draft",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1480.0, 940.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(Draft::new()))
        }),
    )
}
