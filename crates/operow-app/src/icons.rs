//! SVG line icons (24x24, white stroke) embedded in the binary and tinted
//! at draw time so they follow the light/dark theme.

#![allow(dead_code)]

use egui::{ImageSource, include_image};

pub fn play() -> ImageSource<'static> {
    include_image!("../assets/icons/play.svg")
}

pub fn stop() -> ImageSource<'static> {
    include_image!("../assets/icons/stop.svg")
}

pub fn pause() -> ImageSource<'static> {
    include_image!("../assets/icons/pause.svg")
}

pub fn new() -> ImageSource<'static> {
    include_image!("../assets/icons/new.svg")
}

pub fn open() -> ImageSource<'static> {
    include_image!("../assets/icons/open.svg")
}

pub fn save() -> ImageSource<'static> {
    include_image!("../assets/icons/save.svg")
}

pub fn import() -> ImageSource<'static> {
    include_image!("../assets/icons/import.svg")
}

pub fn ecu() -> ImageSource<'static> {
    include_image!("../assets/icons/ecu.svg")
}

pub fn gateway() -> ImageSource<'static> {
    include_image!("../assets/icons/gateway.svg")
}

pub fn replay() -> ImageSource<'static> {
    include_image!("../assets/icons/replay.svg")
}

pub fn bus() -> ImageSource<'static> {
    include_image!("../assets/icons/bus.svg")
}

pub fn script() -> ImageSource<'static> {
    include_image!("../assets/icons/script.svg")
}

pub fn trace_chronological() -> ImageSource<'static> {
    include_image!("../assets/icons/trace-chronological.svg")
}

pub fn trace_fixed() -> ImageSource<'static> {
    include_image!("../assets/icons/trace-fixed.svg")
}

pub fn clear() -> ImageSource<'static> {
    include_image!("../assets/icons/clear.svg")
}

pub fn filter() -> ImageSource<'static> {
    include_image!("../assets/icons/filter.svg")
}

pub fn send() -> ImageSource<'static> {
    include_image!("../assets/icons/send.svg")
}

const ICON_SIZE: f32 = 16.0;

fn tinted(ui: &egui::Ui, icon: ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE))
        .tint(ui.visuals().text_color())
}

/// A square icon-only button with a hover tooltip.
pub fn icon_button(ui: &mut egui::Ui, icon: ImageSource<'static>, tooltip: &str) -> egui::Response {
    icon_button_enabled(ui, true, icon, tooltip)
}

/// Like [`icon_button`], greyed out and inert when `enabled` is false.
pub fn icon_button_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: ImageSource<'static>,
    tooltip: &str,
) -> egui::Response {
    let img = tinted(ui, icon);
    ui.add_enabled(enabled, egui::Button::image(img))
        .on_hover_text(tooltip)
}

/// A button with an icon followed by text.
pub fn icon_text_button(
    ui: &mut egui::Ui,
    icon: ImageSource<'static>,
    text: &str,
) -> egui::Response {
    let img = tinted(ui, icon);
    ui.add(egui::Button::image_and_text(img, text))
}

/// A small icon for labels and node headers.
pub fn icon_image(ui: &egui::Ui, icon: ImageSource<'static>) -> egui::Image<'static> {
    tinted(ui, icon)
}

/// Disclosure triangle for `CollapsingHeader::icon`: down when open, right
/// when closed, drawn with `egui-flow`'s painter icons (no font glyph).
pub fn disclosure(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let icon = if openness > 0.5 {
        egui_flow::Icon::TriangleDown
    } else {
        egui_flow::Icon::TriangleRight
    };
    let color = ui.style().interact(response).fg_stroke.color;
    icon.paint(ui.painter(), response.rect.expand(2.0), color);
}
