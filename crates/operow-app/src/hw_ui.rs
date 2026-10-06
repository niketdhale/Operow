//! Hardware buses in the UI: classification (real adapter or virtual), the
//! Connection section of Bus Properties, the LIVE banner, the transmit
//! confirmation and the status texts. The helpers are pure so they can be
//! tested without a display.

use std::collections::HashSet;
use std::sync::Mutex;

use operow_core::{CanBusConfig, HwBinding, Topology};
use operow_engine::{HwBusStatus, HwLink};
use operow_hw::ChannelInfo;

const REAL_COLOR: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x40, 0x20);
const VIRTUAL_COLOR: egui::Color32 = egui::Color32::from_rgb(0x4a, 0x7f, 0xb5);
const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x9c, 0x3a);
const ERROR_RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x30, 0x30);

/// The `driver` part of `driver:channel` (the whole text without a colon).
pub fn driver_of(interface: &str) -> &str {
    interface.split_once(':').map_or(interface, |(d, _)| d)
}

/// Interfaces the last scan reported as virtual (`ChannelInfo::is_virtual`),
/// e.g. Vector virtual channels.
static VIRTUAL_INTERFACES: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Remembers which of `channels` are virtual, replacing the previous scan.
pub fn remember_virtual(channels: &[ChannelInfo]) {
    let set = channels
        .iter()
        .filter(|c| c.is_virtual)
        .map(|c| c.name.clone())
        .collect();
    if let Ok(mut g) = VIRTUAL_INTERFACES.lock() {
        *g = Some(set);
    }
}

/// Whether `interface` is a real adapter (SocketCAN, PCAN, Vector, ...) as
/// opposed to virtual ones that never touch a vehicle. Channels of the last
/// scan are classified by `ChannelInfo::is_virtual`; names typed in by hand
/// (`udp:anything`) fall back on the `udp` / `virtual` prefixes.
pub fn is_real_interface(interface: &str) -> bool {
    if VIRTUAL_INTERFACES
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(interface)))
        .unwrap_or(false)
    {
        return false;
    }
    !matches!(driver_of(interface).trim(), "udp" | "virtual" | "")
}

/// Whether the driver can set the bitrate itself. SocketCAN cannot: the
/// interface is configured with `ip link`.
pub fn driver_sets_bitrate(interface: &str) -> bool {
    driver_of(interface) != "socketcan"
}

/// Interface text for labels: `socketcan:` is dropped (`can0`), other
/// drivers keep their prefix (`udp:bench`).
pub fn short_name(interface: &str) -> &str {
    interface.strip_prefix("socketcan:").unwrap_or(interface)
}

/// A bus bound to hardware, as far as the safety UI cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwBusRef {
    pub name: String,
    pub interface: String,
    pub listen_only: bool,
}

impl HwBusRef {
    pub fn is_real(&self) -> bool {
        is_real_interface(&self.interface)
    }
}

/// The hardware buses of `topo`.
pub fn hw_buses(topo: &Topology) -> Vec<HwBusRef> {
    topo.buses
        .iter()
        .filter_map(|b| {
            let hw = b.hardware.as_ref()?;
            Some(HwBusRef {
                name: b.name.clone(),
                interface: hw.interface.clone(),
                listen_only: hw.listen_only,
            })
        })
        .collect()
}

/// The real buses that Operow would transmit on (not listen-only): what the
/// confirmation before Start lists. Empty means no confirmation is needed.
pub fn transmit_buses(buses: &[HwBusRef]) -> Vec<&HwBusRef> {
    buses
        .iter()
        .filter(|b| b.is_real() && !b.listen_only)
        .collect()
}

/// Whether Start must ask first: some real bus transmits and the user has not
/// ticked "don't ask again".
pub fn confirm_needed(buses: &[HwBusRef], skip: bool) -> bool {
    !skip && !transmit_buses(buses).is_empty()
}

/// Force every real hardware bus of `topo` listen-only (Start listen-only).
pub fn force_listen_only(topo: &mut Topology) {
    for b in &mut topo.buses {
        if let Some(hw) = &mut b.hardware
            && is_real_interface(&hw.interface)
        {
            hw.listen_only = true;
        }
    }
}

/// The strip under the toolbar while a measurement runs with hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// A real adapter is involved (red) rather than only virtual ones.
    pub real: bool,
    pub text: String,
}

pub fn banner(buses: &[HwBusRef]) -> Option<Banner> {
    let real: Vec<String> = buses
        .iter()
        .filter(|b| b.is_real())
        .map(|b| {
            let n = short_name(&b.interface).to_string();
            if b.listen_only {
                format!("{n} (listen-only)")
            } else {
                format!("{n} (TRANSMITTING)")
            }
        })
        .collect();
    if !real.is_empty() {
        return Some(Banner {
            real: true,
            text: format!("LIVE BUS: {}", real.join(", ")),
        });
    }
    if buses.is_empty() {
        return None;
    }
    let names: Vec<&str> = buses.iter().map(|b| b.interface.as_str()).collect();
    Some(Banner {
        real: false,
        text: format!("Virtual hardware: {}", names.join(", ")),
    })
}

pub fn banner_ui(ui: &mut egui::Ui, b: &Banner) {
    let color = if b.real { REAL_COLOR } else { VIRTUAL_COLOR };
    egui::Frame::new()
        .fill(color)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(&b.text)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
        });
}

/// `Name: link, controller state, rx/tx`, for the log and tooltips.
pub fn format_status(name: &str, s: &HwBusStatus) -> String {
    let link = match &s.link {
        HwLink::Open => "open".to_string(),
        HwLink::Closed => "closed".to_string(),
        HwLink::Error(e) => format!("error: {e}"),
    };
    format!(
        "{name} ({}): {link}, {} (TEC {}, REC {}), rx {}, tx {}",
        s.interface,
        s.controller.state.label(),
        s.controller.tec,
        s.controller.rec,
        s.rx_frames,
        s.tx_frames
    )
}

/// Status-bar chip: `HW: can0 \u{2714}` or `HW: can0 \u{2716} <error or state>`, and whether it is
/// healthy.
pub fn status_chip(s: &HwBusStatus) -> (String, bool) {
    let n = short_name(&s.interface);
    match &s.link {
        HwLink::Open => match s.controller.state {
            operow_core::NodeErrorState::ErrorActive => (format!("HW: {n} \u{2714}"), true),
            st => (format!("HW: {n} \u{26a0} {}", st.label()), false),
        },
        HwLink::Closed => (format!("HW: {n} closed"), true),
        HwLink::Error(e) => (format!("HW: {n} \u{2716} {e}"), false),
    }
}

pub fn status_chip_ui(ui: &mut egui::Ui, s: &HwBusStatus, name: &str) {
    let (text, ok) = status_chip(s);
    let r = if ok {
        ui.colored_label(OK_GREEN, text)
    } else {
        ui.colored_label(ERROR_RED, text)
    };
    r.on_hover_text(format_status(name, s));
}

/// Label of the chip on the canvas: `HW can0`, `HW can0 (listen)`.
pub fn chip_text(hw: &HwBinding) -> String {
    let listen = if hw.listen_only { " (listen)" } else { "" };
    format!("HW {}{listen}", short_name(&hw.interface))
}

/// ` \u{b7} HW can0` / ` \u{b7} HW can0 (listen)` inside a bus bar label.
pub fn bar_suffix(hw: &HwBinding) -> String {
    format!(" \u{b7} {}", chip_text(hw))
}

/// A small coloured chip: orange for a real adapter, blue for virtual ones.
pub fn chip_ui(ui: &mut egui::Ui, hw: &HwBinding) {
    let color = if is_real_interface(&hw.interface) {
        REAL_COLOR
    } else {
        VIRTUAL_COLOR
    };
    egui::Frame::new()
        .fill(color)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(chip_text(hw))
                    .small()
                    .strong()
                    .color(egui::Color32::WHITE),
            );
        });
}

/// One driver with its availability and channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverGroup {
    pub driver: String,
    /// `Err(why)` when the driver cannot be used on this machine.
    pub available: Result<(), String>,
    pub channels: Vec<ChannelInfo>,
}

/// Groups `channels` under their drivers, in the order of `drivers`; a driver
/// without channels still gets a (possibly empty) group, and channels of an
/// unknown driver get one of their own at the end.
pub fn group_channels(
    drivers: &[(String, Result<(), String>)],
    channels: &[ChannelInfo],
) -> Vec<DriverGroup> {
    let mut groups: Vec<DriverGroup> = drivers
        .iter()
        .map(|(d, a)| DriverGroup {
            driver: d.clone(),
            available: a.clone(),
            channels: Vec::new(),
        })
        .collect();
    for c in channels {
        match groups.iter_mut().find(|g| g.driver == c.driver) {
            Some(g) => g.channels.push(c.clone()),
            None => groups.push(DriverGroup {
                driver: c.driver.clone(),
                available: Ok(()),
                channels: vec![c.clone()],
            }),
        }
    }
    groups
}

/// Query every compiled-in driver (the Refresh button).
pub fn scan_drivers() -> Vec<DriverGroup> {
    let drivers = operow_hw::drivers();
    let status: Vec<(String, Result<(), String>)> = drivers
        .iter()
        .map(|d| (d.name().to_string(), d.available()))
        .collect();
    let channels = operow_hw::list_all_channels();
    remember_virtual(&channels);
    group_channels(&status, &channels)
}

/// The Connection section of Bus Properties. `groups` is the cached
/// interface scan (filled on first use, refreshed by the button).
pub fn connection_ui(
    ui: &mut egui::Ui,
    sel: impl std::hash::Hash,
    bus: &mut CanBusConfig,
    groups: &mut Option<Vec<DriverGroup>>,
) {
    ui.add_space(6.0);
    ui.strong("Connection");
    let mut hardware = bus.hardware.is_some();
    ui.horizontal(|ui| {
        if ui.radio_value(&mut hardware, false, "Simulated").clicked() {
            bus.hardware = None;
        }
        if ui.radio_value(&mut hardware, true, "Hardware").clicked() && bus.hardware.is_none() {
            bus.hardware = Some(HwBinding::new(""));
        }
    });
    let Some(hw) = bus.hardware.as_mut() else {
        return;
    };
    let groups = groups.get_or_insert_with(scan_drivers);
    ui.horizontal(|ui| {
        ui.label("Interface:");
        egui::ComboBox::from_id_salt(("hw_iface", &sel))
            .width(180.0)
            .selected_text(if hw.interface.is_empty() {
                "(choose)"
            } else {
                hw.interface.as_str()
            })
            .show_ui(ui, |ui| {
                for g in groups.iter() {
                    match &g.available {
                        Ok(()) => ui.label(
                            egui::RichText::new(format!("{} \u{2714} available", g.driver))
                                .strong(),
                        ),
                        Err(why) => ui
                            .label(
                                egui::RichText::new(format!("{} \u{2716} unavailable", g.driver))
                                    .strong()
                                    .color(ERROR_RED),
                            )
                            .on_hover_text(why),
                    };
                    if g.available.is_ok() && g.channels.is_empty() {
                        ui.weak("   no channels found");
                    }
                    for c in &g.channels {
                        let text = if c.description.is_empty() {
                            c.name.clone()
                        } else {
                            format!("{}  \u{b7} {}", c.name, c.description)
                        };
                        ui.selectable_value(&mut hw.interface, c.name.clone(), text);
                    }
                }
            });
        if ui
            .button("Refresh")
            .on_hover_text("Rescan adapters")
            .clicked()
        {
            *groups = scan_drivers();
        }
    });
    ui.horizontal(|ui| {
        ui.label("Custom:");
        ui.add(
            egui::TextEdit::singleline(&mut hw.interface)
                .hint_text("udp:mybus, virtual:x, socketcan:can0")
                .desired_width(200.0),
        );
    });
    if !hw.interface.is_empty() && operow_hw::split_interface(&hw.interface).is_err() {
        ui.colored_label(ERROR_RED, "Use driver:channel, e.g. socketcan:can0");
    }
    if let Some(g) = groups.iter().find(|g| g.driver == driver_of(&hw.interface))
        && let Err(why) = &g.available
    {
        ui.colored_label(ERROR_RED, format!("Driver unavailable: {why}"));
    }
    ui.checkbox(&mut hw.listen_only, "Listen-only")
        .on_hover_text(
            "Operow only receives. Frames from simulated nodes are not sent to the real bus.",
        );
    ui.label(
        egui::RichText::new(if hw.listen_only {
            "Safe: nothing is transmitted on the real bus."
        } else {
            "Simulated nodes, generators and gateways will transmit on the real bus."
        })
        .small()
        .weak(),
    );
    ui.checkbox(&mut hw.receive_own, "Receive own (echo)")
        .on_hover_text("Ask the adapter to loop back the frames Operow transmits");
    if !hw.interface.is_empty() && !driver_sets_bitrate(&hw.interface) {
        ui.label(
            egui::RichText::new(format!(
                "SocketCAN cannot set the bitrate: the interface must be configured for {} bit/s \
                 (ip link set ... type can bitrate {}).",
                bus.bitrate, bus.bitrate
            ))
            .small()
            .weak(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::NodeErrorState;
    use operow_hw::HwBusState;

    fn r(interface: &str, listen_only: bool) -> HwBusRef {
        HwBusRef {
            name: "B".into(),
            interface: interface.into(),
            listen_only,
        }
    }

    fn status(link: HwLink, state: NodeErrorState) -> HwBusStatus {
        HwBusStatus {
            bus: operow_core::BusId(1),
            interface: "socketcan:can0".into(),
            listen_only: true,
            link,
            controller: HwBusState {
                state,
                tec: 96,
                rec: 3,
            },
            rx_frames: 10,
            tx_frames: 2,
        }
    }

    #[test]
    fn classifies_real_and_virtual() {
        assert!(is_real_interface("socketcan:can0"));
        assert!(is_real_interface("pcan:USB1"));
        assert!(is_real_interface("vector:0"));
        assert!(!is_real_interface("udp:bus"));
        assert!(!is_real_interface("virtual:x"));
        // A scanned virtual channel of an adapter driver is not real.
        let vch = |name: &str, is_virtual| ChannelInfo {
            driver: "vector".into(),
            name: name.into(),
            description: String::new(),
            fd_capable: true,
            is_virtual,
        };
        assert!(is_real_interface("vector:Virtual Channel 1"));
        remember_virtual(&[
            vch("vector:Virtual Channel 1", true),
            vch("vector:VN1630 Channel 1", false),
        ]);
        assert!(!is_real_interface("vector:Virtual Channel 1"));
        assert!(is_real_interface("vector:VN1630 Channel 1"));
        remember_virtual(&[]);
        assert!(!driver_sets_bitrate("socketcan:can0"));
        assert!(driver_sets_bitrate("pcan:USB1"));
        assert_eq!(short_name("socketcan:can0"), "can0");
        assert_eq!(short_name("udp:x"), "udp:x");
    }

    #[test]
    fn confirmation_only_for_transmitting_real_buses() {
        let listen = [r("socketcan:can0", true), r("udp:x", false)];
        assert!(!confirm_needed(&listen, false));
        let tx = [r("socketcan:can0", false), r("udp:x", false)];
        assert!(confirm_needed(&tx, false));
        assert!(!confirm_needed(&tx, true), "don't ask again");
        assert_eq!(transmit_buses(&tx).len(), 1);
        assert!(!confirm_needed(&[], false));
    }

    #[test]
    fn listen_only_override_spares_virtual_buses() {
        let mut topo = Topology::default();
        let b = |id, i: &str| CanBusConfig {
            id: operow_core::BusId(id),
            name: format!("B{id}"),
            bitrate: 500_000,
            fd_enabled: false,
            data_bitrate: 2_000_000,
            simulate_ack: false,
            hardware: Some(HwBinding {
                interface: i.into(),
                listen_only: false,
                receive_own: false,
            }),
        };
        topo.buses = vec![b(1, "socketcan:can0"), b(2, "udp:x")];
        force_listen_only(&mut topo);
        assert!(topo.buses[0].hardware.as_ref().unwrap().listen_only);
        assert!(!topo.buses[1].hardware.as_ref().unwrap().listen_only);
    }

    #[test]
    fn banner_distinguishes_live_from_virtual() {
        assert_eq!(banner(&[]), None);
        let live = banner(&[r("socketcan:can0", true), r("udp:x", true)]).unwrap();
        assert!(live.real);
        assert_eq!(live.text, "LIVE BUS: can0 (listen-only)");
        let tx = banner(&[r("socketcan:can0", false)]).unwrap();
        assert_eq!(tx.text, "LIVE BUS: can0 (TRANSMITTING)");
        let v = banner(&[r("udp:x", false)]).unwrap();
        assert!(!v.real);
        assert_eq!(v.text, "Virtual hardware: udp:x");
    }

    #[test]
    fn status_texts() {
        let ok = status(HwLink::Open, NodeErrorState::ErrorActive);
        assert_eq!(status_chip(&ok), ("HW: can0 \u{2714}".to_string(), true));
        let passive = status(HwLink::Open, NodeErrorState::ErrorPassive);
        assert!(!status_chip(&passive).1);
        let err = status(HwLink::Error("gone".into()), NodeErrorState::ErrorActive);
        assert_eq!(
            status_chip(&err),
            ("HW: can0 \u{2716} gone".to_string(), false)
        );
        let text = format_status("Body", &ok);
        assert!(text.contains("TEC 96") && text.contains("rx 10") && text.contains("open"));
    }

    #[test]
    fn chip_and_bar_text() {
        let mut hw = HwBinding::new("socketcan:can0");
        assert_eq!(chip_text(&hw), "HW can0 (listen)");
        hw.listen_only = false;
        assert_eq!(bar_suffix(&hw), " \u{b7} HW can0");
    }

    #[test]
    fn groups_channels_by_driver() {
        let ch = |d: &str, n: &str| ChannelInfo {
            driver: d.into(),
            name: n.into(),
            description: String::new(),
            fd_capable: false,
            is_virtual: d == "virtual",
        };
        let drivers = vec![
            ("socketcan".to_string(), Err("no".to_string())),
            ("virtual".to_string(), Ok(())),
        ];
        let g = group_channels(
            &drivers,
            &[
                ch("virtual", "virtual:a"),
                ch("virtual", "virtual:b"),
                ch("x", "x:1"),
            ],
        );
        assert_eq!(g.len(), 3);
        assert!(g[0].available.is_err() && g[0].channels.is_empty());
        assert_eq!(g[1].channels.len(), 2);
        assert_eq!(g[2].driver, "x");
    }
}
