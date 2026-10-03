//! Light/dark visual themes, loosely inspired by classic CAN bus tooling.

use egui::{Color32, Visuals};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTheme {
    Light,
    Dark,
}

impl AppTheme {
    pub fn apply(self, ctx: &egui::Context) {
        let visuals = match self {
            AppTheme::Light => light_visuals(),
            AppTheme::Dark => dark_visuals(),
        };
        ctx.set_visuals(visuals);
    }

    pub fn bus_color(self, index: usize) -> Color32 {
        const LIGHT: [Color32; 4] = [
            Color32::from_rgb(0x1a, 0x5f, 0xb4),
            Color32::from_rgb(0xb4, 0x5a, 0x1a),
            Color32::from_rgb(0x1a, 0x8a, 0x4a),
            Color32::from_rgb(0x8a, 0x1a, 0xa0),
        ];
        const DARK: [Color32; 4] = [
            Color32::from_rgb(0x6c, 0xb6, 0xff),
            Color32::from_rgb(0xff, 0xb0, 0x66),
            Color32::from_rgb(0x6c, 0xff, 0xa8),
            Color32::from_rgb(0xd8, 0x8a, 0xff),
        ];
        let table = match self {
            AppTheme::Light => &LIGHT,
            AppTheme::Dark => &DARK,
        };
        table[index % table.len()]
    }

    /// Accent for gateway nodes.
    pub fn gateway_color(self) -> Color32 {
        match self {
            AppTheme::Light => Color32::from_rgb(0x0e, 0x80, 0x8a),
            AppTheme::Dark => Color32::from_rgb(0x4d, 0xdc, 0xe6),
        }
    }
}

impl AppTheme {
    /// Colour of pulses for frames sent by a Generator window.
    pub fn generator_color(self) -> Color32 {
        match self {
            AppTheme::Light => Color32::from_rgb(0xc0, 0x1a, 0x7a),
            AppTheme::Dark => Color32::from_rgb(0xff, 0x6e, 0xc7),
        }
    }
}

fn light_visuals() -> Visuals {
    let mut v = Visuals::light();
    v.panel_fill = Color32::from_rgb(0xf3, 0xf4, 0xf6);
    v.window_fill = Color32::from_rgb(0xfa, 0xfa, 0xfb);
    v.extreme_bg_color = Color32::from_rgb(0xff, 0xff, 0xff);
    v.faint_bg_color = Color32::from_rgb(0xec, 0xee, 0xf1);
    v.widgets.noninteractive.bg_fill = Color32::from_rgb(0xe9, 0xea, 0xed);
    v.selection.bg_fill = Color32::from_rgb(0x1a, 0x5f, 0xb4);
    v
}

fn dark_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(0x1e, 0x21, 0x26);
    v.window_fill = Color32::from_rgb(0x24, 0x27, 0x2e);
    v.extreme_bg_color = Color32::from_rgb(0x15, 0x17, 0x1b);
    v.faint_bg_color = Color32::from_rgb(0x2a, 0x2d, 0x34);
    v.selection.bg_fill = Color32::from_rgb(0x6c, 0xb6, 0xff);
    v
}
