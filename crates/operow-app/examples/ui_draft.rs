//! Visual draft of the proposed Operow UI: light-green theme, workspaces,
//! dockable/floating windows and a mixed CAN / LIN / Ethernet network.
//! Static sample data only; nothing is wired to the engine.
//!
//! Run: `cargo run -p operow-app --example ui_draft`

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, ImageSource, Pos2, Rect, RichText, Sense,
    Shape, Stroke, StrokeKind, Vec2, include_image, pos2, vec2,
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
    MeasurementSetup,
    OfflineMode,
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
            Tab::MeasurementSetup => "Measurement Setup",
            Tab::OfflineMode => "Offline Mode",
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
            Tab::MeasurementSetup => icon::filter(),
            Tab::OfflineMode => icon::open(),
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
                t.split_below(main, 0.6, vec![Tab::Trace, Tab::Log, Tab::Statistics]);
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

// ---------------------------------------------------------------- networks

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Ecu,
    Gen,
    Replay,
}

struct NetDef {
    name: &'static str,
    proto: Proto,
    channel: &'static str,
    kind: &'static str,
    nodes: &'static [&'static str],
    gens: &'static [&'static str],
    replays: &'static [&'static str],
    dbs: &'static [&'static str],
}

const NETS: [NetDef; 4] = [
    NetDef {
        name: "Powertrain",
        proto: Proto::CanFd,
        channel: "CAN 1",
        kind: "CAN FD network",
        nodes: &["Engine", "Central GW"],
        gens: &["CAN IG"],
        replays: &["drive_cycle.blf"],
        dbs: &["powertrain.dbc"],
    },
    NetDef {
        name: "Body",
        proto: Proto::Can,
        channel: "CAN 2",
        kind: "CAN network",
        nodes: &["BodyCtrl", "DoorLeft", "SeatCtrl", "Central GW"],
        gens: &["Body IG"],
        replays: &[],
        dbs: &["body.dbc"],
    },
    NetDef {
        name: "SeatLIN",
        proto: Proto::Lin,
        channel: "LIN 1",
        kind: "LIN network",
        nodes: &["SeatCtrl", "SeatMotor"],
        gens: &["LIN IG"],
        replays: &[],
        dbs: &["seat.ldf"],
    },
    NetDef {
        name: "Backbone",
        proto: Proto::Eth,
        channel: "Eth 1",
        kind: "Switched Ethernet",
        nodes: &["ADAS", "Infotainment", "Telematics", "Central GW"],
        gens: &["Ethernet Packet Builder"],
        replays: &[],
        dbs: &["backbone.arxml"],
    },
];

// ---------------------------------------------------------------- trace views

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TraceView {
    #[default]
    Mixed,
    Can,
    Lin,
    EthIp,
}

impl TraceView {
    const ALL: [TraceView; 4] = [
        TraceView::Mixed,
        TraceView::Can,
        TraceView::Lin,
        TraceView::EthIp,
    ];
    fn label(self) -> &'static str {
        match self {
            TraceView::Mixed => "All networks",
            TraceView::Can => "CAN / CAN FD",
            TraceView::Lin => "LIN",
            TraceView::EthIp => "Ethernet IP/UDP",
        }
    }
    fn headers(self) -> &'static [&'static str] {
        match self {
            TraceView::Mixed => &[
                "Time (s)",
                "Chn",
                "ID / Addr",
                "Proto",
                "Name",
                "Dir",
                "Details",
                "Data",
            ],
            TraceView::Can => &[
                "Time (s)",
                "Chn",
                "ID",
                "Name",
                "Event type",
                "Dir",
                "DLC",
                "Len",
                "Data",
                "BRS",
                "ESI",
            ],
            TraceView::Lin => &[
                "Time (s)", "Chn", "PID", "Name", "Dir", "Len", "Data", "Checksum", "Schedule",
            ],
            TraceView::EthIp => &[
                "Time (s)",
                "Chn",
                "Port",
                "VLAN",
                "Dir",
                "Protocol",
                "Source IP",
                "Destination IP",
                "Src port",
                "Dst port",
                "Name",
                "Interpretation",
                "Len",
                "Data",
            ],
        }
    }
}

#[derive(Default)]
struct TraceState {
    view: TraceView,
    sort: Option<(usize, bool)>,
    filters: Vec<(usize, Filt)>,
    search: String,
    custom: Option<CustomEdit>,
}

struct TRow {
    cells: Vec<String>,
    proto: Proto,
    changed: &'static [usize],
}

/// (time, port, vlan, tx, protocol, src, dst, sport, dport, name, interpretation, data, changed)
type EthRow = (
    &'static str,
    &'static str,
    &'static str,
    bool,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [usize],
);

const ETH_ROWS: &[EthRow] = &[
    (
        "2.331044",
        "P1",
        "10",
        true,
        "SOME/IP",
        "192.168.1.1",
        "192.168.1.10",
        "30490",
        "30509",
        "VehicleSpeed",
        "Notification 0x1234.0x8001",
        "00 00 12 34 80 01 00 08",
        &[6, 7],
    ),
    (
        "2.336500",
        "P2",
        "10",
        false,
        "SOME/IP-SD",
        "192.168.1.20",
        "224.244.224.245",
        "30490",
        "30490",
        "OfferService",
        "Offer 0x0101 v1 TTL 3",
        "FF FF 81 00 00 00 00 30",
        &[],
    ),
    (
        "2.352210",
        "P2",
        "20",
        false,
        "SOME/IP",
        "192.168.1.20",
        "192.168.1.30",
        "40001",
        "30509",
        "GetRoute",
        "Request 0x0101.0x0001",
        "00 00 01 01 00 01 00 10",
        &[],
    ),
    (
        "2.353870",
        "P3",
        "20",
        true,
        "SOME/IP",
        "192.168.1.30",
        "192.168.1.20",
        "30509",
        "40001",
        "GetRoute",
        "Response 0x0101.0x0001 E_OK",
        "00 00 01 01 00 01 00 18",
        &[7],
    ),
    (
        "2.360120",
        "P1",
        "10",
        true,
        "UDP",
        "192.168.1.10",
        "192.168.1.255",
        "5000",
        "5000",
        "ObjectList",
        "48 objects",
        "30 00 A1 0C 7F 12 00 00",
        &[2, 3],
    ),
    (
        "2.371004",
        "P4",
        "-",
        false,
        "DoIP",
        "192.168.1.99",
        "192.168.1.1",
        "13400",
        "13400",
        "RoutingActivation",
        "Tester 0x0E00",
        "02 FD 00 05 00 00 00 07",
        &[],
    ),
];

fn trace_rows(view: TraceView) -> Vec<TRow> {
    let can_like = |r: &Row| matches!(r.proto, Proto::Can | Proto::CanFd);
    let dir = |tx: bool| if tx { "TX" } else { "RX" }.to_string();
    let len = |d: &str| d.split(' ').count().to_string();
    match view {
        TraceView::Mixed => ROWS
            .iter()
            .map(|r| TRow {
                cells: vec![
                    r.t.into(),
                    r.net.into(),
                    r.id.into(),
                    String::new(),
                    r.name.into(),
                    dir(r.tx),
                    r.details.into(),
                    r.data.into(),
                ],
                proto: r.proto,
                changed: r.changed,
            })
            .collect(),
        TraceView::Can => ROWS
            .iter()
            .filter(|r| can_like(r))
            .map(|r| {
                let fd = r.proto == Proto::CanFd;
                TRow {
                    cells: vec![
                        r.t.into(),
                        r.net.into(),
                        r.id.into(),
                        r.name.into(),
                        if fd { "CAN FD Frame" } else { "CAN Frame" }.into(),
                        dir(r.tx),
                        len(r.data),
                        len(r.data),
                        r.data.into(),
                        if fd { "1" } else { "-" }.into(),
                        if fd { "0" } else { "-" }.into(),
                    ],
                    proto: r.proto,
                    changed: r.changed,
                }
            })
            .collect(),
        TraceView::Lin => ROWS
            .iter()
            .filter(|r| r.proto == Proto::Lin)
            .map(|r| TRow {
                cells: vec![
                    r.t.into(),
                    r.net.into(),
                    r.id.into(),
                    r.name.into(),
                    dir(r.tx),
                    len(r.data),
                    r.data.into(),
                    "Enhanced".into(),
                    r.details.split(" · ").next().unwrap_or("").into(),
                ],
                proto: r.proto,
                changed: r.changed,
            })
            .collect(),
        TraceView::EthIp => ETH_ROWS
            .iter()
            .map(|e| TRow {
                cells: vec![
                    e.0.into(),
                    "Backbone".into(),
                    e.1.into(),
                    e.2.into(),
                    dir(e.3),
                    e.4.into(),
                    e.5.into(),
                    e.6.into(),
                    e.7.into(),
                    e.8.into(),
                    e.9.into(),
                    e.10.into(),
                    len(e.11),
                    e.11.into(),
                ],
                proto: Proto::Eth,
                changed: e.12,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------- column filters

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rel {
    Equals,
    NotEquals,
    Contains,
    StartsWith,
    Greater,
    Less,
}

impl Rel {
    const ALL: [Rel; 6] = [
        Rel::Equals,
        Rel::NotEquals,
        Rel::Contains,
        Rel::StartsWith,
        Rel::Greater,
        Rel::Less,
    ];
    fn label(self) -> &'static str {
        match self {
            Rel::Equals => "equals",
            Rel::NotEquals => "not equals",
            Rel::Contains => "contains",
            Rel::StartsWith => "starts with",
            Rel::Greater => "greater than",
            Rel::Less => "less than",
        }
    }
    fn test(self, v: &str, want: &str) -> bool {
        let num = |s: &str| {
            let s = s.trim().trim_start_matches("PID ");
            s.strip_prefix("0x")
                .and_then(|h| u64::from_str_radix(h, 16).ok().map(|n| n as f64))
                .or_else(|| s.parse::<f64>().ok())
        };
        match self {
            Rel::Equals => v.eq_ignore_ascii_case(want),
            Rel::NotEquals => !v.eq_ignore_ascii_case(want),
            Rel::Contains => v.to_lowercase().contains(&want.to_lowercase()),
            Rel::StartsWith => v.to_lowercase().starts_with(&want.to_lowercase()),
            Rel::Greater => matches!((num(v), num(want)), (Some(a), Some(b)) if a > b),
            Rel::Less => matches!((num(v), num(want)), (Some(a), Some(b)) if a < b),
        }
    }
}

#[derive(Clone)]
struct Cond {
    on: bool,
    rel: Rel,
    value: String,
}

#[derive(Clone)]
enum Filt {
    Eq(String),
    Custom { and: bool, conds: Vec<Cond> },
}

impl Filt {
    fn matches(&self, v: &str) -> bool {
        match self {
            Filt::Eq(s) => v == s,
            Filt::Custom { and, conds } => {
                let mut res = conds
                    .iter()
                    .filter(|c| c.on && !c.value.is_empty())
                    .map(|c| c.rel.test(v, &c.value))
                    .peekable();
                if res.peek().is_none() {
                    true
                } else if *and {
                    res.all(|b| b)
                } else {
                    res.any(|b| b)
                }
            }
        }
    }
}

/// Dialog being edited: column, AND (true) / OR (false), conditions.
struct CustomEdit {
    col: usize,
    and: bool,
    conds: Vec<Cond>,
}

/// Hex view as stored; decimal converts `0x..` tokens and data bytes.
fn fmt_num(v: &str, hex: bool, data: bool) -> String {
    if hex {
        return v.to_string();
    }
    v.split(' ')
        .map(|t| {
            if let Some(h) = t.strip_prefix("0x") {
                let parts: Vec<String> = h
                    .split('.')
                    .map(|x| {
                        u64::from_str_radix(x, 16)
                            .map(|n| n.to_string())
                            .unwrap_or_else(|_| x.to_string())
                    })
                    .collect();
                parts.join(".")
            } else if data && t.len() == 2 {
                u8::from_str_radix(t, 16)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|_| t.to_string())
            } else {
                t.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const RIBBON_TABS: [&str; 8] = [
    "File",
    "Home",
    "Analysis",
    "Simulation",
    "Test",
    "Diagnostics",
    "Hardware",
    "Tools",
];

/// Large ribbon button: icon over label.
fn big(
    ui: &mut egui::Ui,
    p: &Pal,
    src: ImageSource<'static>,
    label: &str,
    on: bool,
    enabled: bool,
    primary: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(66.0, 62.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hov = enabled && resp.hovered();
    let (fill, fg) = if primary && enabled {
        (
            if hov {
                p.green.gamma_multiply(0.9)
            } else {
                p.green
            },
            p.on_green,
        )
    } else if on {
        (p.soft, p.green)
    } else if hov {
        (p.soft, p.fg)
    } else {
        (Color32::TRANSPARENT, if enabled { p.fg } else { p.faint })
    };
    ui.painter().rect_filled(rect, 8, fill);
    if on && !primary {
        ui.painter()
            .rect_stroke(rect, 8, Stroke::new(1.0_f32, p.green), StrokeKind::Inside);
    }
    let ir = Rect::from_center_size(rect.center_top() + vec2(0.0, 20.0), Vec2::splat(22.0));
    img(
        src,
        if primary && enabled {
            p.on_green
        } else if enabled {
            if on { p.green } else { p.muted }
        } else {
            p.faint
        },
        22.0,
    )
    .paint_at(ui, ir);
    ui.painter().text(
        rect.center_bottom() - vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        fg,
    );
    resp
}

/// Small stacked ribbon button.
fn small(
    ui: &mut egui::Ui,
    p: &Pal,
    src: ImageSource<'static>,
    label: &str,
    on: bool,
    enabled: bool,
) -> egui::Response {
    let tint = if !enabled {
        p.faint
    } else if on {
        p.green
    } else {
        p.muted
    };
    let txt = RichText::new(label).color(if !enabled {
        p.faint
    } else if on {
        p.green
    } else {
        p.fg
    });
    let b = egui::Button::image_and_text(img(src, tint, 14.0), txt)
        .frame(on)
        .fill(p.soft)
        .min_size(vec2(120.0, 22.0));
    ui.add_enabled(enabled, b)
}

/// Titled ribbon group followed by a divider.
fn group(ui: &mut egui::Ui, p: &Pal, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(64.0);
            add(ui)
        });
        ui.label(RichText::new(title).small().color(p.muted));
    });
    ui.add(egui::Separator::default().vertical().spacing(14.0));
}

impl Draft {
    fn ribbon_body(&mut self, ui: &mut egui::Ui, p: &Pal, open: &mut Option<Tab>) {
        let is_open = |s: &Self, t: Tab| s.docks[s.ws as usize].find_tab(&t).is_some();
        match self.ribbon {
            0 => {
                group(ui, p, "Project", |ui| {
                    big(ui, p, icon::open(), "Open", false, true, false);
                    big(ui, p, icon::save(), "Save", false, true, false);
                    big(ui, p, icon::save(), "Save as", false, true, false);
                });
                group(ui, p, "Recent", |ui| {
                    ui.vertical(|ui| {
                        for f in [
                            "body_gateway.operow",
                            "powertrain_hil.operow",
                            "eth_backbone.operow",
                        ] {
                            let _ = ui
                                .add(egui::Button::new(RichText::new(f).color(p.fg)).frame(false));
                        }
                    });
                });
            }
            1 => {
                group(ui, p, "Measurement", |ui| {
                    if big(ui, p, icon::play(), "Start", false, !self.running, true).clicked() {
                        self.running = true;
                    }
                    if big(ui, p, icon::stop(), "Stop", false, self.running, false).clicked() {
                        self.running = false;
                    }
                    ui.vertical(|ui| {
                        small(ui, p, icon::play(), "Step", false, !self.running);
                        small(ui, p, icon::pause(), "Break", false, self.running);
                        if small(ui, p, icon::replay(), "Animate", self.animate, true).clicked() {
                            self.animate = !self.animate;
                        }
                    });
                });
                group(ui, p, "Mode", |ui| {
                    ui.vertical(|ui| {
                        let txt = if self.online {
                            "Online mode"
                        } else {
                            "Offline mode"
                        };
                        if small(ui, p, icon::bus(), txt, true, !self.running)
                            .on_hover_text("Real bus or offline log replay")
                            .clicked()
                        {
                            self.online = !self.online;
                        }
                        let txt = if self.real_bus {
                            "Real bus"
                        } else {
                            "Simulated bus"
                        };
                        if small(ui, p, icon::ecu(), txt, self.real_bus, !self.running).clicked() {
                            self.real_bus = !self.real_bus;
                        }
                        small(ui, p, icon::gateway(), "Hardware: Virtual", false, true);
                    });
                });
                group(ui, p, "Appearance", |ui| {
                    egui::Grid::new("fmt").spacing([4.0, 4.0]).show(ui, |ui| {
                        ui.selectable_value(&mut self.hex, false, "dec");
                        ui.selectable_value(&mut self.hex, true, "hex");
                        ui.end_row();
                        ui.selectable_value(&mut self.sym, true, "sym");
                        ui.selectable_value(&mut self.sym, false, "num");
                        ui.end_row();
                    });
                });
                group(ui, p, "Windows", |ui| {
                    big(ui, p, icon::replay(), "Sync", false, true, false)
                        .on_hover_text("Synchronise the time cursor across windows");
                    if big(
                        ui,
                        p,
                        icon::trace(),
                        "Trace",
                        is_open(self, Tab::Trace),
                        true,
                        false,
                    )
                    .clicked()
                    {
                        *open = Some(Tab::Trace);
                    }
                });
            }
            2 => {
                group(ui, p, "Configuration", |ui| {
                    if big(
                        ui,
                        p,
                        icon::filter(),
                        "Measure",
                        is_open(self, Tab::MeasurementSetup),
                        true,
                        false,
                    )
                    .on_hover_text("Measurement setup")
                    .clicked()
                    {
                        *open = Some(Tab::MeasurementSetup);
                    }
                    if big(
                        ui,
                        p,
                        icon::open(),
                        "Offline",
                        is_open(self, Tab::OfflineMode),
                        true,
                        false,
                    )
                    .on_hover_text("Offline mode sources")
                    .clicked()
                    {
                        *open = Some(Tab::OfflineMode);
                    }
                    big(ui, p, icon::filter(), "Filter", false, false, false);
                    if big(
                        ui,
                        p,
                        icon::save(),
                        "Logging",
                        is_open(self, Tab::Log),
                        true,
                        false,
                    )
                    .clicked()
                    {
                        *open = Some(Tab::Log);
                    }
                });
                group(ui, p, "Bus analysis", |ui| {
                    for (t, l) in [
                        (Tab::Trace, "Trace"),
                        (Tab::Graph, "Graphics"),
                        (Tab::Properties, "Data"),
                        (Tab::Statistics, "Statistics"),
                    ] {
                        if big(ui, p, t.icon(), l, is_open(self, t), true, false).clicked() {
                            *open = Some(t);
                        }
                    }
                });
            }
            3 => {
                group(ui, p, "Setup", |ui| {
                    if big(
                        ui,
                        p,
                        icon::bus(),
                        "Networks",
                        is_open(self, Tab::Network),
                        true,
                        false,
                    )
                    .on_hover_text("Simulation setup")
                    .clicked()
                    {
                        *open = Some(Tab::Network);
                    }
                    big(ui, p, icon::ecu(), "Add ECU", false, !self.running, false);
                    big(
                        ui,
                        p,
                        icon::gateway(),
                        "Gateway",
                        false,
                        !self.running,
                        false,
                    );
                });
                group(ui, p, "Stimulus", |ui| {
                    big(ui, p, icon::send(), "Generator", false, true, false);
                    big(ui, p, icon::replay(), "Replay", false, true, false);
                    big(ui, p, icon::clear(), "Faults", false, true, false);
                });
            }
            4 => {
                group(ui, p, "Tests", |ui| {
                    if big(ui, p, icon::play(), "Run all", false, true, true).clicked() {
                        *open = Some(Tab::Tests);
                    }
                    if big(
                        ui,
                        p,
                        icon::script(),
                        "Modules",
                        is_open(self, Tab::Tests),
                        true,
                        false,
                    )
                    .clicked()
                    {
                        *open = Some(Tab::Tests);
                    }
                    big(ui, p, icon::save(), "Report", false, true, false);
                });
            }
            5 => {
                group(ui, p, "Diagnostics", |ui| {
                    if big(
                        ui,
                        p,
                        icon::send(),
                        "Console",
                        is_open(self, Tab::Diagnostics),
                        true,
                        false,
                    )
                    .clicked()
                    {
                        *open = Some(Tab::Diagnostics);
                    }
                    big(ui, p, icon::filter(), "DTCs", false, true, false);
                    big(ui, p, icon::ecu(), "Security", false, true, false);
                });
            }
            6 => {
                group(ui, p, "Driver", |ui| {
                    for d in ["Virtual", "PCAN", "Vector XL", "SocketCAN", "UDP"] {
                        big(ui, p, icon::gateway(), d, d == "Virtual", true, false);
                    }
                });
                group(ui, p, "Channels", |ui| {
                    big(ui, p, icon::bus(), "Mapping", false, true, false);
                });
            }
            _ => {
                group(ui, p, "Logs", |ui| {
                    big(ui, p, icon::replay(), "Convert", false, true, false)
                        .on_hover_text("ASC <> BLF");
                });
                group(ui, p, "App", |ui| {
                    big(ui, p, icon::script(), "Settings", false, true, false);
                });
            }
        }
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
    trace: TraceState,
    name: String,
    net_view: usize,
    ribbon: usize,
    ribbon_min: bool,
    hex: bool,
    sym: bool,
    online: bool,
    real_bus: bool,
    animate: bool,
    ms: [bool; 5],
    offline: [bool; 3],
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
            trace: TraceState::default(),
            name: "Engine".into(),
            net_view: 0,
            ribbon: 1,
            ribbon_min: false,
            hex: true,
            sym: true,
            online: true,
            real_bus: false,
            animate: true,
            ms: [false, false, false, false, true],
            offline: [true, true, false],
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
    Open(Tab),
    Maximise(Tab),
    Minimise(Tab),
    Float(Tab),
}

struct Viewer<'a> {
    p: Pal,
    t: f64,
    running: bool,
    selected: &'a mut &'static str,
    trace: &'a mut TraceState,
    net_view: &'a mut usize,
    hex: bool,
    online: &'a mut bool,
    ms: &'a mut [bool; 5],
    offline: &'a mut [bool; 3],
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
            Tab::MeasurementSetup => self.measurement_setup(ui),
            Tab::OfflineMode => self.offline_mode(ui),
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
            Tab::Network | Tab::Trace | Tab::Graph | Tab::MeasurementSetup | Tab::OfflineMode => {
                [false, false]
            }
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

        let avail = ui.available_size() - vec2(0.0, 32.0);
        if *self.net_view > 0 {
            self.net_detail(ui, avail, *self.net_view - 1);
        } else {
            self.overview(ui, avail);
        }
        self.net_tabs(ui);
    }

    fn overview(&mut self, ui: &mut egui::Ui, avail: Vec2) {
        let p = self.p;
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
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
            .clamp(0.85, 1.6);
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
                b.right_center() - vec2(130.0 * s, 0.0),
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
            if rect.contains_rect(ir) {
                img(src, if name.contains("GW") { p.gw } else { p.fg }, 15.0).paint_at(ui, ir);
            }
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

    /// Sheet-style tabs under the canvas: the overview plus one view per network.
    fn net_tabs(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let mut tab = |ui: &mut egui::Ui, i: usize, label: &str, c: Option<Color32>| {
                let on = *self.net_view == i;
                let r = ui.add(
                    egui::Button::new(
                        RichText::new(label)
                            .color(if on { p.green } else { p.muted })
                            .strong(),
                    )
                    .fill(if on { p.panel } else { Color32::TRANSPARENT })
                    .stroke(if on {
                        Stroke::new(1.0_f32, p.line)
                    } else {
                        Stroke::NONE
                    })
                    .min_size(vec2(0.0, 24.0)),
                );
                if let Some(c) = c {
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            r.rect.left_top() + vec2(0.0, 0.0),
                            vec2(r.rect.width(), 2.0),
                        ),
                        0,
                        c,
                    );
                }
                if r.clicked() {
                    *self.net_view = i;
                }
            };
            tab(ui, 0, "Overview", None);
            for (i, n) in NETS.iter().enumerate() {
                tab(ui, i + 1, n.name, Some(net_color(&p, n.name)));
            }
        });
    }

    fn net_detail(&mut self, ui: &mut egui::Ui, avail: Vec2, idx: usize) {
        let p = self.p;
        let net = &NETS[idx];
        let col = net_color(&p, net.name);
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
        let painter = ui.painter_at(rect);
        gradient(&painter, rect, p.g1, p.g2);
        let s = (rect.width() / 900.0)
            .min(rect.height() / 420.0)
            .clamp(0.8, 1.4);
        let o = rect.min + vec2(10.0, 10.0);
        let at = |x: f32, y: f32| o + vec2(x, y) * s;
        let font = |px: f32| FontId::proportional(px * s.max(0.85));

        let mut blocks: Vec<(Block, &str)> = net.nodes.iter().map(|n| (Block::Ecu, *n)).collect();
        blocks.extend(net.gens.iter().map(|n| (Block::Gen, *n)));
        blocks.extend(net.replays.iter().map(|n| (Block::Replay, *n)));

        let eth = net.proto == Proto::Eth;
        let bus_y = 205.0;
        let nb = Rect::from_min_size(at(680.0, 150.0), vec2(190.0, 110.0) * s);
        let pos = |i: usize| {
            let row = i / 3;
            let x = 30.0 + (i % 3) as f32 * 205.0;
            let y = if row == 0 { 20.0 } else { 290.0 };
            Rect::from_min_size(at(x, y), vec2(175.0, 100.0) * s)
        };

        // Wiring
        let w = Stroke::new(2.0 * s.max(0.8), col);
        if eth {
            for i in 0..blocks.len() {
                let b = pos(i);
                let (sx, sy) = if i / 3 == 0 {
                    (b.center().x, b.bottom())
                } else {
                    (b.center().x, b.top())
                };
                let ly = at(0.0, bus_y - 30.0 + i as f32 * 12.0).y;
                let ty = nb.top() + (12.0 + i as f32 * 14.0) * s;
                let pts = vec![
                    pos2(sx, sy),
                    pos2(sx, ly),
                    pos2(nb.left() - (20.0 + i as f32 * 6.0) * s, ly),
                    pos2(nb.left() - (20.0 + i as f32 * 6.0) * s, ty),
                    pos2(nb.left(), ty),
                ];
                painter.add(Shape::line(pts, w));
                painter.text(
                    pos2(nb.left() - 4.0, ty - 7.0),
                    Align2::RIGHT_CENTER,
                    format!("P{}", i + 1),
                    FontId::monospace(9.5),
                    p.muted,
                );
            }
        } else {
            let y = at(0.0, bus_y).y;
            for dy in [-2.5, 2.5] {
                painter.hline(at(20.0, 0.0).x..=nb.left(), y + dy, w);
            }
            for i in 0..blocks.len() {
                let b = pos(i);
                let from = if i / 3 == 0 { b.bottom() } else { b.top() };
                painter.vline(b.center().x, from.min(y)..=from.max(y), w);
            }
            if self.running {
                let t = (self.t as f32 / 2.0).fract();
                let x = at(20.0, 0.0).x + (nb.left() - at(20.0, 0.0).x) * t;
                painter.circle_filled(pos2(x, y), 4.0, p.green);
                ui.ctx().request_repaint();
            }
        }

        // Blocks
        let click = resp
            .clicked()
            .then(|| resp.interact_pointer_pos())
            .flatten();
        for (i, (kind, name)) in blocks.iter().enumerate() {
            let b = pos(i);
            if click.is_some_and(|c| b.contains(c)) && *kind == Block::Ecu {
                *self.selected = name;
            }
            let label = match kind {
                Block::Ecu if name.contains("GW") => "Gateway",
                Block::Ecu => "ECU",
                Block::Gen => "Interactive generator",
                Block::Replay => "Replay",
            };
            let sub = match kind {
                Block::Ecu if *name == "SeatCtrl" && net.proto == Proto::Lin => {
                    "LIN master".to_string()
                }
                Block::Ecu if net.proto == Proto::Lin => "LIN slave".to_string(),
                Block::Ecu => format!("{}.rhai", name.to_lowercase().replace(' ', "_")),
                Block::Gen => "3 messages".to_string(),
                Block::Replay => "loop · 1.0x".to_string(),
            };
            painter.add(
                egui::Shadow {
                    offset: [0, 3],
                    blur: 12,
                    spread: 0,
                    color: Color32::from_black_alpha(22),
                }
                .as_shape(b, 10),
            );
            painter.rect(
                b,
                10,
                p.panel,
                Stroke::new(1.0_f32, p.line),
                StrokeKind::Inside,
            );
            let foot = Rect::from_min_max(pos2(b.left(), b.bottom() - 28.0 * s), b.max);
            painter.rect_filled(
                foot,
                CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 10,
                    se: 10,
                },
                p.soft,
            );
            painter.text(
                b.center_top() + vec2(0.0, 14.0 * s),
                Align2::CENTER_CENTER,
                label,
                font(10.5),
                p.muted,
            );
            painter.text(
                b.center_top() + vec2(0.0, 32.0 * s),
                Align2::CENTER_CENTER,
                *name,
                font(13.5),
                p.fg,
            );
            painter.text(
                b.center_top() + vec2(0.0, 50.0 * s),
                Align2::CENTER_CENTER,
                sub,
                FontId::monospace(10.0 * s.max(0.85)),
                p.muted,
            );
            if *self.selected == *name {
                painter.rect_stroke(
                    b.expand(3.0),
                    12,
                    Stroke::new(2.0_f32, p.green),
                    StrokeKind::Outside,
                );
            }
            let icons: Vec<(ImageSource<'static>, &str)> = match kind {
                Block::Ecu => vec![
                    (icon::script(), "Edit script"),
                    (icon::ecu(), "Node settings"),
                    (icon::bus(), "Online / offline"),
                ],
                Block::Gen => vec![(icon::send(), "Open generator")],
                Block::Replay => vec![
                    (icon::play(), "Play"),
                    (icon::pause(), "Pause"),
                    (icon::stop(), "Stop"),
                ],
            };
            for (k, (src, tip)) in icons.into_iter().enumerate() {
                let r = Rect::from_min_size(
                    foot.left_top() + vec2(10.0 + k as f32 * 24.0, 6.0) * s,
                    Vec2::splat(16.0 * s.max(0.85)),
                );
                let hov = ui.rect_contains_pointer(r);
                if rect.contains_rect(r) {
                    img(src, if hov { p.green } else { p.muted }, 16.0).paint_at(ui, r);
                }
                ui.interact(r, Id::new(("blk", idx, i, k)), Sense::hover())
                    .on_hover_text(tip);
            }
        }

        // Network block
        painter.add(
            egui::Shadow {
                offset: [0, 3],
                blur: 12,
                spread: 0,
                color: Color32::from_black_alpha(22),
            }
            .as_shape(nb, 10),
        );
        painter.rect(
            nb,
            10,
            p.panel,
            Stroke::new(1.0_f32, p.line),
            StrokeKind::Inside,
        );
        painter.rect_filled(
            Rect::from_min_size(nb.min, vec2(nb.width(), 4.0)),
            CornerRadius {
                nw: 10,
                ne: 10,
                sw: 0,
                se: 0,
            },
            col,
        );
        let foot = Rect::from_min_max(pos2(nb.left(), nb.bottom() - 28.0 * s), nb.max);
        painter.rect_filled(
            foot,
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: 10,
                se: 10,
            },
            p.soft,
        );
        painter.text(
            nb.center_top() + vec2(0.0, 18.0 * s),
            Align2::CENTER_CENTER,
            net.kind,
            font(10.5),
            p.muted,
        );
        painter.text(
            nb.center_top() + vec2(0.0, 38.0 * s),
            Align2::CENTER_CENTER,
            net.name,
            font(14.0),
            p.fg,
        );
        painter.text(
            nb.center_top() + vec2(0.0, 58.0 * s),
            Align2::CENTER_CENTER,
            net.channel,
            FontId::monospace(11.0 * s.max(0.85)),
            col,
        );
        let load = match net.proto {
            Proto::Eth => "62 Mbit/s",
            Proto::Lin => "sched 42 %",
            Proto::CanFd => "load 3.0 %",
            Proto::Can => "load 5.9 %",
        };
        painter.text(
            foot.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            load,
            FontId::monospace(10.5 * s.max(0.85)),
            p.muted,
        );
        painter.text(
            rect.left_top() + vec2(12.0, 12.0),
            Align2::LEFT_TOP,
            format!("{} · {} blocks", net.name, blocks.len()),
            FontId::proportional(11.0),
            p.faint,
        );
    }

    fn measurement_setup(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let painter = ui.painter_at(rect);
        gradient(&painter, rect, p.g1, p.g2);
        let s = (rect.width() / 860.0)
            .min(rect.height() / 440.0)
            .clamp(0.8, 1.4);
        let o = rect.min + vec2(14.0, 14.0);
        let at = |x: f32, y: f32| o + vec2(x, y) * s;
        let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(at(x, y), vec2(w, h) * s);
        let font = |px: f32| FontId::proportional(px * s.max(0.85));
        let click = resp
            .clicked()
            .then(|| resp.interact_pointer_pos())
            .flatten();
        let dbl = resp
            .double_clicked()
            .then(|| resp.interact_pointer_pos())
            .flatten();
        let line = Stroke::new(2.0 * s.max(0.8), p.green);
        let dim = Stroke::new(1.5_f32, p.faint);

        // Sources
        let real = r(10.0, 250.0, 70.0, 40.0);
        let off = r(10.0, 120.0, 70.0, 40.0);
        let online = *self.online;
        for (b, label, on) in [(real, "Real bus", online), (off, "Offline log", !online)] {
            painter.rect(
                b,
                8,
                if on { p.soft } else { p.panel },
                Stroke::new(1.0_f32, if on { p.green } else { p.line }),
                StrokeKind::Inside,
            );
            painter.text(
                b.center(),
                Align2::CENTER_CENTER,
                label,
                font(11.0),
                if on { p.green } else { p.muted },
            );
        }
        let sw = r(130.0, 175.0, 90.0, 80.0);
        painter.add(Shape::line(
            vec![
                real.right_center(),
                pos2(sw.center().x, real.center().y),
                pos2(sw.center().x, sw.bottom()),
            ],
            if online { line } else { dim },
        ));
        painter.add(Shape::line(
            vec![
                off.right_center(),
                pos2(sw.center().x, off.center().y),
                pos2(sw.center().x, sw.top()),
            ],
            if online { dim } else { line },
        ));
        painter.rect(
            sw,
            10,
            p.panel,
            Stroke::new(1.0_f32, p.line),
            StrokeKind::Inside,
        );
        painter.text(
            sw.center_top() + vec2(0.0, 12.0 * s),
            Align2::CENTER_CENTER,
            "Offline",
            font(10.0),
            if online { p.faint } else { p.green },
        );
        painter.text(
            sw.center_bottom() - vec2(0.0, 12.0 * s),
            Align2::CENTER_CENTER,
            "Online",
            font(10.0),
            if online { p.green } else { p.faint },
        );
        let pivot = sw.center() - vec2(18.0 * s, 0.0);
        let tip = sw.center() + vec2(18.0 * s, if online { 10.0 } else { -10.0 } * s);
        painter.line_segment([pivot, tip], Stroke::new(3.0_f32, p.green));
        painter.circle_filled(pivot, 3.5, p.green);
        if click.is_some_and(|c| sw.contains(c)) {
            *self.online = !*self.online;
        }

        // Trunk and branches
        let trunk_x = at(300.0, 0.0).x;
        painter.line_segment([sw.right_center(), pos2(trunk_x, sw.center().y)], line);
        let names = [
            ("Statistics", Tab::Statistics),
            ("Trace", Tab::Trace),
            ("Data", Tab::Properties),
            ("Graphics", Tab::Graph),
            ("Logging", Tab::Log),
        ];
        let ys: Vec<f32> = (0..names.len()).map(|i| 20.0 + i as f32 * 82.0).collect();
        painter.vline(
            trunk_x,
            at(0.0, ys[0] + 30.0).y..=at(0.0, ys[4] + 30.0).y,
            line,
        );
        for (i, (name, tab)) in names.iter().enumerate() {
            let b = r(400.0, ys[i], 210.0, 62.0);
            let y = b.center().y;
            let blocked = self.ms[i];
            let sq = Rect::from_center_size(pos2(at(350.0, 0.0).x, y), Vec2::splat(14.0 * s));
            painter.line_segment([pos2(trunk_x, y), sq.left_center()], line);
            painter.line_segment(
                [sq.right_center(), b.left_center()],
                if blocked { dim } else { line },
            );
            if blocked {
                for dx in [-3.0, 3.0] {
                    painter.vline(
                        sq.center().x + dx,
                        sq.y_range(),
                        Stroke::new(3.0_f32, p.amber),
                    );
                }
            } else {
                painter.rect_filled(sq, 3, p.green);
            }
            if click.is_some_and(|c| sq.expand(4.0).contains(c)) {
                self.ms[i] = !blocked;
            }
            let fill = if blocked { p.page } else { p.panel };
            painter.add(
                egui::Shadow {
                    offset: [0, 2],
                    blur: 10,
                    spread: 0,
                    color: Color32::from_black_alpha(if blocked { 0 } else { 20 }),
                }
                .as_shape(b, 10),
            );
            painter.rect(
                b,
                10,
                fill,
                Stroke::new(1.0_f32, p.line),
                StrokeKind::Inside,
            );
            let foot = Rect::from_min_max(pos2(b.left(), b.bottom() - 24.0 * s), b.max);
            painter.rect_filled(
                foot,
                CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 10,
                    se: 10,
                },
                if blocked { p.line } else { p.soft },
            );
            painter.text(
                b.left_top() + vec2(12.0, 16.0) * s,
                Align2::LEFT_CENTER,
                *name,
                font(13.0),
                if blocked { p.faint } else { p.fg },
            );
            let state = if blocked { "paused" } else { "active" };
            painter.text(
                b.right_top() + vec2(-12.0, 16.0) * s,
                Align2::RIGHT_CENTER,
                state,
                font(10.0),
                if blocked { p.amber } else { p.green },
            );
            if rect.contains_rect(Rect::from_min_size(foot.left_top(), Vec2::splat(20.0))) {
                img(tab.icon(), if blocked { p.faint } else { p.muted }, 14.0).paint_at(
                    ui,
                    Rect::from_min_size(
                        foot.left_top() + vec2(10.0, 5.0) * s,
                        Vec2::splat(14.0 * s.max(0.85)),
                    ),
                );
            }
            painter.text(
                foot.left_center() + vec2(32.0 * s, 0.0),
                Align2::LEFT_CENTER,
                "double-click to open",
                font(9.5),
                p.faint,
            );
            if dbl.is_some_and(|c| b.contains(c)) {
                self.req = Some(Req::Open(*tab));
            }
            if *name == "Logging" {
                let f = r(660.0, ys[i] + 12.0, 150.0, 38.0);
                painter.arrow(
                    b.right_center(),
                    vec2(f.left() - b.right() - 6.0, 0.0),
                    if blocked { dim } else { line },
                );
                painter.rect(
                    f,
                    6,
                    p.panel,
                    Stroke::new(1.0_f32, p.line),
                    StrokeKind::Inside,
                );
                painter.text(
                    f.center(),
                    Align2::CENTER_CENTER,
                    "run_0007.blf",
                    FontId::monospace(11.0 * s.max(0.85)),
                    if blocked { p.faint } else { p.fg },
                );
            }
        }
        painter.text(rect.left_bottom() + vec2(12.0, -10.0), Align2::LEFT_BOTTOM, "Click the switch to change source · click a square to pause a branch · double-click a block to open it", FontId::proportional(11.0), p.faint);
    }

    fn offline_mode(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        self.toolbar(ui, |ui| {
            ui.add(egui::Button::image_and_text(
                img(icon::open(), p.muted, 14.0),
                "Add log file...",
            ));
            ui.add(egui::Button::image_and_text(
                img(icon::clear(), p.muted, 14.0),
                "Remove",
            ));
            let _ = ui.button("Move up");
            let _ = ui.button("Move down");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (t, c) = if *self.online {
                    ("Source: real bus (switch in Measurement Setup)", p.muted)
                } else {
                    ("Source: offline logs", p.green)
                };
                ui.label(RichText::new(t).small().color(c));
            });
        });
        let files = [
            (
                "drive_cycle.blf",
                "0.000",
                "612.402",
                "0.000014",
                "0.000",
                "CAN 1 > Powertrain",
                "logs/drive_cycle.blf",
            ),
            (
                "door_test.asc",
                "0.000",
                "45.880",
                "0.120000",
                "+2.000",
                "CAN 2 > Body",
                "logs/door_test.asc",
            ),
            (
                "eth_capture.pcapng",
                "0.000",
                "30.002",
                "0.000310",
                "0.000",
                "Eth 1 > Backbone",
                "logs/eth_capture.pcapng",
            ),
        ];
        use egui_extras::{Column, TableBuilder};
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(52.0))
            .columns(Column::auto().at_least(70.0), 6)
            .column(Column::remainder())
            .header(24.0, |mut h| {
                for c in [
                    "Active",
                    "Offline source",
                    "Start",
                    "End",
                    "First event",
                    "Offset",
                    "Channel mapping",
                    "Path",
                ] {
                    h.col(|ui| {
                        ui.label(RichText::new(c).strong().small().color(p.muted));
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, files.len(), |mut row| {
                    let i = row.index();
                    let f = files[i];
                    row.col(|ui| {
                        ui.checkbox(&mut self.offline[i], "");
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(f.0).strong());
                    });
                    for v in [f.1, f.2, f.3, f.4] {
                        row.col(|ui| {
                            ui.label(RichText::new(v).monospace());
                        });
                    }
                    row.col(|ui| {
                        ui.label(RichText::new(f.5).color(p.green));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(f.6).monospace().color(p.muted));
                    });
                });
            });
    }

    fn trace(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let mut st = std::mem::take(self.trace);
        let headers = st.view.headers();
        let mut rows = trace_rows(st.view);
        let all_rows = trace_rows(st.view);
        let q = st.search.to_lowercase();
        rows.retain(|r| {
            st.filters.iter().all(|(c, f)| f.matches(&r.cells[*c]))
                && (q.is_empty() || r.cells.iter().any(|c| c.to_lowercase().contains(&q)))
        });
        if let Some((c, asc)) = st.sort {
            rows.sort_by(|a, b| a.cells[c].cmp(&b.cells[c]));
            if !asc {
                rows.reverse();
            }
        }
        self.toolbar_trace(ui, &mut st, rows.len());

        use egui_extras::{Column, TableBuilder};
        egui::ScrollArea::horizontal().show(ui, |ui| {
            let mut tb = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for h in headers {
                tb = tb.column(match *h {
                    "Data" => Column::initial(190.0).at_least(120.0),
                    "Details" | "Interpretation" => Column::initial(200.0).clip(true),
                    "Time (s)" => Column::initial(84.0),
                    "Source IP" | "Destination IP" => Column::initial(108.0),
                    _ => Column::auto().at_least(40.0),
                });
            }
            tb.header(26.0, |mut hr| {
                for (ci, h) in headers.iter().enumerate() {
                    hr.col(|ui| {
                        let filtered = st.filters.iter().any(|(c, _)| *c == ci);
                        let arrow = match st.sort {
                            Some((c, true)) if c == ci => " ^",
                            Some((c, false)) if c == ci => " v",
                            _ => "",
                        };
                        let color = if filtered || !arrow.is_empty() {
                            p.green
                        } else {
                            p.muted
                        };
                        let r = ui.add(
                            egui::Label::new(
                                RichText::new(format!("{h}{arrow}"))
                                    .strong()
                                    .small()
                                    .color(color),
                            )
                            .sense(Sense::click()),
                        );
                        if r.clicked() {
                            st.sort = match st.sort {
                                Some((c, true)) if c == ci => Some((ci, false)),
                                Some((c, false)) if c == ci => None,
                                _ => Some((ci, true)),
                            };
                        }
                        r.on_hover_text("Click to sort, right-click for column options")
                            .context_menu(|ui| {
                                if ui.button("Sort ascending").clicked() {
                                    st.sort = Some((ci, true));
                                }
                                if ui.button("Sort descending").clicked() {
                                    st.sort = Some((ci, false));
                                }
                                if ui
                                    .add_enabled(st.sort.is_some(), egui::Button::new("Reset sort"))
                                    .clicked()
                                {
                                    st.sort = None;
                                }
                                ui.separator();
                                let _ = ui.button("Adapt column widths");
                                if ui
                                    .add_enabled(
                                        !st.filters.is_empty(),
                                        egui::Button::new("Reset all column filters"),
                                    )
                                    .clicked()
                                {
                                    st.filters.clear();
                                }
                                ui.separator();
                                let _ = ui.button("Field chooser...");
                                let _ = ui.button("Column configuration...");
                            });
                        // Per-column value filter
                        let tint = if filtered { p.green } else { p.faint };
                        ui.menu_image_button(img(icon::filter(), tint, 11.0), |ui| {
                            ui.set_min_width(180.0);
                            if ui.button("(Reset filter)").clicked() {
                                st.filters.retain(|(c, _)| *c != ci);
                                ui.close();
                            }
                            if ui.button("(Custom...)").clicked() {
                                let conds = match st.filters.iter().find(|(c, _)| *c == ci) {
                                    Some((_, Filt::Custom { conds, .. })) => conds.clone(),
                                    Some((_, Filt::Eq(v))) => vec![Cond {
                                        on: true,
                                        rel: Rel::Equals,
                                        value: v.clone(),
                                    }],
                                    None => vec![Cond {
                                        on: true,
                                        rel: Rel::Equals,
                                        value: String::new(),
                                    }],
                                };
                                let and = matches!(
                                    st.filters.iter().find(|(c, _)| *c == ci),
                                    Some((_, Filt::Custom { and: true, .. }))
                                );
                                st.custom = Some(CustomEdit {
                                    col: ci,
                                    and,
                                    conds,
                                });
                                ui.close();
                            }
                            ui.separator();
                            let mut vals: Vec<&String> = all_rows
                                .iter()
                                .map(|r| &r.cells[ci])
                                .filter(|v| !v.is_empty())
                                .collect();
                            vals.sort();
                            vals.dedup();
                            if vals.is_empty() {
                                ui.label(
                                    RichText::new("No further items available.").color(p.muted),
                                );
                            }
                            egui::ScrollArea::vertical()
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    for v in vals {
                                        let on = st.filters.iter().any(|(c, f)| {
                                            *c == ci && matches!(f, Filt::Eq(x) if x == v)
                                        });
                                        if ui.selectable_label(on, v.as_str()).clicked() {
                                            st.filters.retain(|(c, _)| *c != ci);
                                            if !on {
                                                st.filters.push((ci, Filt::Eq(v.clone())));
                                            }
                                            ui.close();
                                        }
                                    }
                                });
                        });
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    for (ci, h) in headers.iter().enumerate() {
                        let v = r.cells[ci].as_str();
                        row.col(|ui| match *h {
                            "Time (s)" => {
                                let (a, z) = v.split_at(v.trim_end_matches('0').len());
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label(RichText::new(a).monospace());
                                ui.label(RichText::new(z).monospace().color(p.faint));
                            }
                            "Chn" => {
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                ui.painter().rect_filled(rect, 2, net_color(&p, v));
                                ui.label(RichText::new(v).monospace());
                            }
                            "Proto" => {
                                let (t, c) = proto_style(&p, r.proto);
                                tag(ui, t, c);
                            }
                            "Protocol" => tag(ui, v, p.eth),
                            "Dir" => {
                                let c = if v == "TX" { p.green } else { p.can_a };
                                ui.label(RichText::new(v).small().strong().color(c));
                            }
                            "Data" => {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                let v = fmt_num(v, self.hex, true);
                                for (i, b) in v.split(' ').enumerate() {
                                    let t = RichText::new(b).monospace();
                                    if r.changed.contains(&i) {
                                        ui.label(
                                            t.background_color(p.green.gamma_multiply(0.25))
                                                .strong(),
                                        );
                                    } else {
                                        ui.label(t);
                                    }
                                }
                            }
                            "Details" | "Interpretation" | "Event type" | "Schedule"
                            | "Checksum" => {
                                ui.label(RichText::new(v).color(p.muted));
                            }
                            "Name" => {
                                ui.label(v);
                            }
                            "ID" | "ID / Addr" | "PID" => {
                                ui.label(RichText::new(fmt_num(v, self.hex, false)).monospace());
                            }
                            _ => {
                                ui.label(RichText::new(v).monospace());
                            }
                        });
                    }
                });
            });
        });
        self.custom_filter_dialog(ui.ctx(), &mut st, headers, &all_rows);
        *self.trace = st;
    }

    fn custom_filter_dialog(
        &self,
        ctx: &egui::Context,
        st: &mut TraceState,
        headers: &[&str],
        all_rows: &[TRow],
    ) {
        let p = self.p;
        let Some(ed) = st.custom.as_mut() else { return };
        let mut done: Option<bool> = None;
        let mut open = true;
        egui::Window::new("Custom column filter")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("Column: {}", headers[ed.col]))
                        .strong()
                        .size(14.0),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Match").color(p.muted));
                    ui.selectable_value(&mut ed.and, false, "any condition (OR)");
                    ui.selectable_value(&mut ed.and, true, "all conditions (AND)");
                });
                ui.add_space(6.0);
                let mut vals: Vec<&String> = all_rows
                    .iter()
                    .map(|r| &r.cells[ed.col])
                    .filter(|v| !v.is_empty())
                    .collect();
                vals.sort();
                vals.dedup();
                let mut remove = None;
                egui::Frame::new()
                    .stroke(Stroke::new(1.0_f32, p.line))
                    .corner_radius(6)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        egui::Grid::new("conds")
                            .num_columns(4)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                for h in ["Use", "Relation", "Value", ""] {
                                    ui.label(RichText::new(h).small().strong().color(p.muted));
                                }
                                ui.end_row();
                                for (i, c) in ed.conds.iter_mut().enumerate() {
                                    ui.checkbox(&mut c.on, "");
                                    egui::ComboBox::from_id_salt(("rel", i))
                                        .selected_text(c.rel.label())
                                        .width(110.0)
                                        .show_ui(ui, |ui| {
                                            for r in Rel::ALL {
                                                ui.selectable_value(&mut c.rel, r, r.label());
                                            }
                                        });
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut c.value)
                                                .hint_text("value")
                                                .desired_width(170.0),
                                        );
                                        egui::ComboBox::from_id_salt(("val", i))
                                            .selected_text("")
                                            .width(24.0)
                                            .show_ui(ui, |ui| {
                                                for v in &vals {
                                                    if ui
                                                        .selectable_label(
                                                            c.value == **v,
                                                            v.as_str(),
                                                        )
                                                        .clicked()
                                                    {
                                                        c.value = (*v).clone();
                                                    }
                                                }
                                            });
                                    });
                                    if ui
                                        .add(egui::Button::image(img(icon::clear(), p.muted, 12.0)))
                                        .on_hover_text("Remove condition")
                                        .clicked()
                                    {
                                        remove = Some(i);
                                    }
                                    ui.end_row();
                                }
                            });
                        if ui.button("+ Add condition").clicked() {
                            ed.conds.push(Cond {
                                on: true,
                                rel: Rel::Equals,
                                value: String::new(),
                            });
                        }
                    });
                if let Some(i) = remove {
                    ed.conds.remove(i);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Numbers accept hex (0x1F) or decimal.")
                            .small()
                            .color(p.muted),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("Cancel").min_size(vec2(72.0, 28.0)))
                            .clicked()
                        {
                            done = Some(false);
                        }
                        if primary(ui, &p, None, "   OK   ").clicked() {
                            done = Some(true);
                        }
                    });
                });
            });
        if !open {
            done = Some(false);
        }
        if let Some(ok) = done {
            let ed = st.custom.take().unwrap();
            if ok {
                st.filters.retain(|(c, _)| *c != ed.col);
                st.filters.push((
                    ed.col,
                    Filt::Custom {
                        and: ed.and,
                        conds: ed.conds,
                    },
                ));
            }
        }
    }

    fn toolbar_trace(&self, ui: &mut egui::Ui, st: &mut TraceState, shown: usize) {
        let p = self.p;
        self.toolbar(ui, |ui| {
            ui.add(egui::Button::image(img(icon::pause(), p.muted, 14.0)))
                .on_hover_text("Pause view");
            ui.add(egui::Button::image(img(icon::clear(), p.muted, 14.0)))
                .on_hover_text("Clear view");
            ui.separator();
            ui.label(RichText::new("Columns for").color(p.muted));
            let before = st.view;
            egui::ComboBox::from_id_salt("trace_view")
                .selected_text(st.view.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for v in TraceView::ALL {
                        ui.selectable_value(&mut st.view, v, v.label());
                    }
                });
            if st.view != before {
                st.sort = None;
                st.filters.clear();
            }
            ui.add(
                egui::TextEdit::singleline(&mut st.search)
                    .hint_text("Search")
                    .desired_width(140.0),
            );
            ui.separator();
            let n = st.filters.len();
            let label = if n == 0 {
                "No filters".to_string()
            } else {
                format!("{n} filter{} - clear", if n == 1 { "" } else { "s" })
            };
            let b = egui::Button::image_and_text(
                img(icon::filter(), if n > 0 { p.green } else { p.muted }, 14.0),
                RichText::new(label).color(if n > 0 { p.green } else { p.muted }),
            );
            let b = if n > 0 {
                b.stroke(Stroke::new(1.0_f32, p.green))
            } else {
                b
            };
            if ui.add(b).clicked() {
                st.filters.clear();
            }
            let _ = ui.button("Export CSV");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{shown} rows · 412 msg/s"))
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

        // Title row with ribbon tabs
        let mut open: Option<Tab> = None;
        egui::TopBottomPanel::top("cmd")
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 6,
                bottom: 0,
            }))
            .show(ctx, |ui| {
                gradient(
                    ui.painter(),
                    ui.max_rect().expand2(vec2(12.0, 6.0)),
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
                    ui.add_space(14.0);
                    for (i, t) in RIBBON_TABS.iter().enumerate() {
                        let on = self.ribbon == i && !self.ribbon_min;
                        let b = if i == 0 {
                            egui::Button::new(RichText::new(*t).color(p.on_green).strong())
                                .fill(p.green)
                        } else {
                            egui::Button::new(RichText::new(*t).color(if on {
                                p.green
                            } else {
                                p.fg
                            }))
                            .frame(false)
                        };
                        let r = ui.add(b.min_size(vec2(0.0, 30.0)));
                        if on && i != 0 {
                            ui.painter().hline(
                                r.rect.x_range(),
                                r.rect.bottom() + 1.0,
                                Stroke::new(2.0_f32, p.green),
                            );
                        }
                        if r.clicked() {
                            if self.ribbon == i {
                                self.ribbon_min = !self.ribbon_min;
                            } else {
                                self.ribbon = i;
                                self.ribbon_min = false;
                            }
                        }
                    }
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
                        let t = if self.running {
                            ctx.input(|i| i.time) % 1000.0
                        } else {
                            0.0
                        };
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
                        ui.label(
                            RichText::new(format!("t = {t:.3} s"))
                                .monospace()
                                .size(16.0),
                        );
                        let mode = if self.online {
                            "Online · real bus"
                        } else {
                            "Offline · 2 logs"
                        };
                        ui.label(RichText::new(mode).small().color(p.muted));
                    });
                });
            });

        // Ribbon body
        if !self.ribbon_min {
            egui::TopBottomPanel::top("ribbon")
                .frame(
                    egui::Frame::new()
                        .fill(p.panel)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .stroke(Stroke::new(1.0_f32, p.line)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(78.0);
                        self.ribbon_body(ui, &p, &mut open);
                    });
                });
        }

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
        let mut trace = std::mem::take(&mut self.trace);
        let mut net_view = self.net_view;
        let mut online = self.online;
        let mut ms = self.ms;
        let mut offline = self.offline;
        let mut name = std::mem::take(&mut self.name);
        let mut viewer = Viewer {
            p,
            t: ctx.input(|i| i.time),
            running: self.running,
            selected: &mut selected,
            trace: &mut trace,
            net_view: &mut net_view,
            hex: self.hex,
            online: &mut online,
            ms: &mut ms,
            offline: &mut offline,
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
        self.trace = trace;
        self.net_view = net_view;
        self.online = online;
        self.ms = ms;
        self.offline = offline;
        self.name = name;
        match req {
            Some(Req::Open(tab)) => self.open(tab),
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
        let row = |ui: &mut egui::Ui,
                   src: ImageSource<'static>,
                   tint: Color32,
                   txt: &str,
                   color: Color32| {
            ui.horizontal(|ui| {
                ui.add(img(src, tint, 14.0));
                ui.label(RichText::new(txt).color(color));
            });
        };
        egui::CollapsingHeader::new(RichText::new("Networks").strong())
            .default_open(true)
            .show(ui, |ui| {
                for (group, protos) in [
                    ("CAN networks", &[Proto::CanFd, Proto::Can][..]),
                    ("LIN networks", &[Proto::Lin]),
                    ("Ethernet networks", &[Proto::Eth]),
                ] {
                    egui::CollapsingHeader::new(RichText::new(group).strong())
                        .default_open(true)
                        .show(ui, |ui| {
                            for (ni, net) in NETS
                                .iter()
                                .enumerate()
                                .filter(|(_, n)| protos.contains(&n.proto))
                            {
                                let c = net_color(p, net.name);
                                let on = self.net_view == ni + 1;
                                let hdr = egui::CollapsingHeader::new(
                                    RichText::new(net.name).strong().color(if on {
                                        p.green
                                    } else {
                                        p.fg
                                    }),
                                )
                                .id_salt(("net", ni))
                                .default_open(ni == 0 || ni == 3)
                                .show(ui, |ui| {
                                    egui::CollapsingHeader::new("Nodes")
                                        .id_salt(("nodes", ni))
                                        .default_open(true)
                                        .show(ui, |ui| {
                                            for n in net.nodes {
                                                let sel = self.selected == *n;
                                                ui.horizontal(|ui| {
                                                    let src = if n.contains("GW") {
                                                        icon::gateway()
                                                    } else {
                                                        icon::ecu()
                                                    };
                                                    ui.add(img(
                                                        src,
                                                        if sel { p.green } else { c },
                                                        14.0,
                                                    ));
                                                    if ui.selectable_label(sel, *n).clicked() {
                                                        self.selected = n;
                                                    }
                                                });
                                            }
                                        });
                                    egui::CollapsingHeader::new("Interactive generators")
                                        .id_salt(("gen", ni))
                                        .default_open(true)
                                        .show(ui, |ui| {
                                            for g in net.gens {
                                                row(ui, icon::send(), c, g, p.fg);
                                            }
                                        });
                                    egui::CollapsingHeader::new("Replay blocks")
                                        .id_salt(("rep", ni))
                                        .show(ui, |ui| {
                                            if net.replays.is_empty() {
                                                ui.label(
                                                    RichText::new("none").small().color(p.faint),
                                                );
                                            }
                                            for r in net.replays {
                                                row(ui, icon::replay(), c, r, p.fg);
                                            }
                                        });
                                    egui::CollapsingHeader::new("Databases")
                                        .id_salt(("db", ni))
                                        .show(ui, |ui| {
                                            for d in net.dbs {
                                                row(ui, icon::open(), c, d, p.muted);
                                            }
                                        });
                                    egui::CollapsingHeader::new("Channels")
                                        .id_salt(("ch", ni))
                                        .show(ui, |ui| {
                                            row(ui, icon::bus(), c, net.channel, p.muted);
                                        });
                                });
                                // Colour tag and type next to the network name; click opens its view.
                                let r = hdr.header_response;
                                let (t, tc) = proto_style(p, net.proto);
                                let g = ui.painter().layout_no_wrap(
                                    t.into(),
                                    FontId::proportional(9.0),
                                    Color32::WHITE,
                                );
                                let tr = Rect::from_min_size(
                                    pos2(r.rect.right() + 6.0, r.rect.center().y - 7.0),
                                    g.size() + vec2(8.0, 4.0),
                                );
                                ui.painter().rect_filled(tr, 3, tc);
                                ui.painter()
                                    .galley(tr.min + vec2(4.0, 2.0), g, Color32::WHITE);
                                if r.double_clicked() || r.secondary_clicked() {
                                    self.net_view = ni + 1;
                                    self.open(Tab::Network);
                                }
                                r.on_hover_text("Double-click to open this network's view");
                            }
                        });
                }
            });
        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Domains").strong()).show(ui, |ui| {
            for d in ["Powertrain", "Body & comfort", "Ethernet backbone"] {
                ui.label(d);
            }
        });
        egui::CollapsingHeader::new(RichText::new("Tests").strong()).show(ui, |ui| {
            for t in ["gateway_tests", "fault_tests"] {
                row(ui, icon::script(), p.muted, t, p.fg);
            }
        });
        let _ = ui
            .add(egui::Button::new(RichText::new("+ New network...").color(p.faint)).frame(false));
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
