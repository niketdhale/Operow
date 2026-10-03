//! Application-wide settings, persisted with eframe's storage.

use serde::{Deserialize, Serialize};

/// Default size of the frame buffer, in frames.
pub const DEFAULT_BUFFER_FRAMES: usize = 1_000_000;
/// Rough in-memory size of one buffered trace row.
pub const BYTES_PER_FRAME: u64 = 96;
/// Preset buffer sizes offered in the settings dialog.
pub const BUFFER_PRESETS: [usize; 4] = [100_000, 1_000_000, 5_000_000, 10_000_000];

pub const STORAGE_KEY: &str = "operow_settings";

/// What happens when the frame buffer is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WhenFull {
    #[default]
    DropOldest,
    StopMeasurement,
}

impl WhenFull {
    pub fn label(self) -> &'static str {
        match self {
            WhenFull::DropOldest => "Drop oldest",
            WhenFull::StopMeasurement => "Stop measurement",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Trace capacity in frames.
    pub frame_buffer_size: usize,
    pub when_full: WhenFull,
    /// Repaint interval while a measurement runs.
    pub ui_refresh_ms: u32,
    pub dark_theme: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            frame_buffer_size: DEFAULT_BUFFER_FRAMES,
            when_full: WhenFull::default(),
            ui_refresh_ms: 33,
            dark_theme: true,
        }
    }
}

impl AppSettings {
    /// Estimated RAM for one full trace buffer, in bytes.
    pub fn estimated_ram_bytes(&self) -> u64 {
        self.frame_buffer_size as u64 * BYTES_PER_FRAME
    }

    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        storage
            .and_then(|s| eframe::get_value(s, STORAGE_KEY))
            .unwrap_or_default()
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, self);
    }
}

/// Human-readable frame count: `100k`, `1M`, `2.5M`.
pub fn format_frames(n: usize) -> String {
    if n >= 1_000_000 && n.is_multiple_of(100_000) {
        let m = n as f64 / 1e6;
        if n.is_multiple_of(1_000_000) {
            format!("{m:.0}M")
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1000 && n.is_multiple_of(1000) {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

pub fn format_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else {
        format!("{:.0} MB", b / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let s = AppSettings::default();
        assert_eq!(s.frame_buffer_size, 1_000_000);
        assert_eq!(s.when_full, WhenFull::DropOldest);
        assert!(s.dark_theme);
    }

    #[test]
    fn ram_estimate() {
        let s = AppSettings::default();
        assert_eq!(s.estimated_ram_bytes(), 96_000_000);
        assert_eq!(format_bytes(s.estimated_ram_bytes()), "96 MB");
        let big = AppSettings {
            frame_buffer_size: 10_000_000,
            ..Default::default()
        };
        assert_eq!(format_bytes(big.estimated_ram_bytes()), "960 MB");
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s: AppSettings = serde_json::from_str(r#"{"ui_refresh_ms": 100}"#).unwrap();
        assert_eq!(s.ui_refresh_ms, 100);
        assert_eq!(s.frame_buffer_size, DEFAULT_BUFFER_FRAMES);
    }

    #[test]
    fn frame_formatting() {
        assert_eq!(format_frames(100_000), "100k");
        assert_eq!(format_frames(1_000_000), "1M");
        assert_eq!(format_frames(2_500_000), "2.5M");
    }
}
