//! "Diagnostics (UDS)" section of the ECU inspector: edits an ECU's
//! [`DiagConfig`] (ids, DID and DTC tables, SecurityAccess, sessions).

use std::collections::HashMap;

use egui_flow::NodeId as FlowId;
use operow_core::{BusId, DiagConfig, DidEntry, DtcEntry, KeyAlgo, SecurityConfig};
use operow_uds::{dtc_to_string, parse_dtc};

use crate::icons;
use crate::inspector::{RED, bus_label, hex_bytes, parse_hex_bytes, parse_hex_u32};

const F_REQ: u8 = 0;
const F_RESP: u8 = 1;
const F_FUNC: u8 = 2;
const F_PAD: u8 = 3;
const F_SESSIONS: u8 = 4;
const F_SEC_LEVEL: u8 = 5;
const F_SEC_SEED: u8 = 6;
const F_SEC_CONST: u8 = 7;
const F_DID: u8 = 10;
const F_DID_DATA: u8 = 11;
const F_DTC: u8 = 20;
const F_DTC_STATUS: u8 = 21;

#[derive(Default)]
pub struct DiagProps {
    /// In-progress text of the hex / DTC fields keyed by (field, row), so
    /// half-typed values are kept and invalid ones are shown red.
    bufs: HashMap<(u8, usize), String>,
}

/// Text field bound to a value through `parse`; invalid text is red and not
/// applied. Returns the parsed value while the text is valid.
fn parsed_edit<T>(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<(u8, usize), String>,
    key: (u8, usize),
    init: impl FnOnce() -> String,
    width: f32,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    let buf = bufs.entry(key).or_insert_with(init);
    let parsed = parse(buf);
    let mut te = egui::TextEdit::singleline(buf).desired_width(width);
    if parsed.is_none() {
        te = te.text_color(RED);
    }
    ui.add(te);
    parsed
}

fn hex_u32_edit(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<(u8, usize), String>,
    key: (u8, usize),
    value: &mut u32,
    width: f32,
) {
    let cur = *value;
    if let Some(v) = parsed_edit(ui, bufs, key, || format!("{cur:X}"), width, parse_hex_u32) {
        *value = v;
    }
}

fn hex_bytes_edit(
    ui: &mut egui::Ui,
    bufs: &mut HashMap<(u8, usize), String>,
    key: (u8, usize),
    value: &mut Vec<u8>,
    width: f32,
) {
    let init = || hex_bytes(value);
    if let Some(v) = parsed_edit(ui, bufs, key, init, width, parse_hex_bytes) {
        *value = v;
    }
}

fn algo_label(a: &KeyAlgo) -> &'static str {
    match a {
        KeyAlgo::XorConst(_) => "XOR constant",
        KeyAlgo::AddConst(_) => "Add constant",
        KeyAlgo::Script => "Script (on_security_key)",
    }
}

impl DiagProps {
    /// Forget scratch text, e.g. when the selection changes.
    pub fn clear(&mut self) {
        self.bufs.clear();
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        sel: FlowId,
        diag: &mut Option<DiagConfig>,
        linked: &[(BusId, String)],
        running: bool,
    ) {
        egui::CollapsingHeader::new("Diagnostics (UDS)")
            .icon(icons::disclosure)
            .id_salt(("diag_section", sel))
            .default_open(diag.is_some())
            .show(ui, |ui| {
                ui.add_enabled_ui(!running, |ui| {
                    let mut on = diag.is_some();
                    if ui.checkbox(&mut on, "Enable UDS server").changed() {
                        *diag = on.then(DiagConfig::default);
                        self.bufs.clear();
                    }
                    let Some(d) = diag.as_mut() else { return };
                    self.settings_ui(ui, sel, d, linked);
                    ui.separator();
                    self.dids_ui(ui, &mut d.dids);
                    ui.separator();
                    self.dtcs_ui(ui, &mut d.dtcs);
                    ui.separator();
                    self.security_ui(ui, sel, &mut d.security);
                });
            });
    }

    fn settings_ui(
        &mut self,
        ui: &mut egui::Ui,
        sel: FlowId,
        d: &mut DiagConfig,
        linked: &[(BusId, String)],
    ) {
        let bufs = &mut self.bufs;
        egui::Grid::new(("diag_grid", sel))
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Bus:");
                egui::ComboBox::from_id_salt(("diag_bus", sel))
                    .selected_text(bus_label(d.bus, linked))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.bus, None, bus_label(None, linked));
                        for (id, name) in linked {
                            ui.selectable_value(&mut d.bus, Some(*id), name);
                        }
                    });
                ui.end_row();

                ui.label("Request ID (hex):");
                hex_u32_edit(ui, bufs, (F_REQ, 0), &mut d.req_id, 80.0);
                ui.end_row();

                ui.label("Response ID (hex):");
                hex_u32_edit(ui, bufs, (F_RESP, 0), &mut d.resp_id, 80.0);
                ui.end_row();

                ui.label("Functional ID (hex):");
                ui.horizontal(|ui| {
                    let mut has = d.functional_id.is_some();
                    if ui.checkbox(&mut has, "").changed() {
                        d.functional_id = has.then_some(0x7DF);
                        bufs.remove(&(F_FUNC, 0));
                    }
                    if let Some(id) = d.functional_id.as_mut() {
                        hex_u32_edit(ui, bufs, (F_FUNC, 0), id, 80.0);
                    }
                });
                ui.end_row();

                ui.label("Addressing:");
                ui.checkbox(&mut d.extended_ids, "29-bit IDs");
                ui.end_row();

                ui.label("Frame type:");
                ui.checkbox(&mut d.fd, "CAN FD");
                ui.end_row();

                ui.label("Padding byte (hex):");
                ui.horizontal(|ui| {
                    let mut has = d.padding.is_some();
                    if ui.checkbox(&mut has, "").changed() {
                        d.padding = has.then_some(0xAA);
                        bufs.remove(&(F_PAD, 0));
                    }
                    if let Some(p) = d.padding.as_mut() {
                        let cur = *p;
                        let parse = |s: &str| parse_hex_u32(s).and_then(|v| u8::try_from(v).ok());
                        if let Some(v) =
                            parsed_edit(ui, bufs, (F_PAD, 0), || format!("{cur:02X}"), 40.0, parse)
                        {
                            *p = v;
                        }
                    }
                });
                ui.end_row();

                ui.label("Block size:");
                ui.add(egui::DragValue::new(&mut d.block_size).range(0..=255));
                ui.end_row();

                ui.label("STmin (ms):");
                ui.add(egui::DragValue::new(&mut d.st_min_ms).range(0..=127));
                ui.end_row();

                ui.label("P2 (ms):");
                ui.add(egui::DragValue::new(&mut d.p2_ms).range(0..=65_535));
                ui.end_row();

                ui.label("P2* (ms):");
                ui.add(egui::DragValue::new(&mut d.p2_star_ms).range(0..=655_350));
                ui.end_row();

                ui.label("Sessions (hex):");
                hex_bytes_edit(ui, bufs, (F_SESSIONS, 0), &mut d.sessions_supported, 120.0);
                ui.end_row();
            });
    }

    fn dids_ui(&mut self, ui: &mut egui::Ui, dids: &mut Vec<DidEntry>) {
        ui.label("Data identifiers:");
        let mut remove = None;
        for (i, e) in dids.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let cur = e.did;
                let parse = |s: &str| parse_hex_u32(s).and_then(|v| u16::try_from(v).ok());
                let init = || format!("{cur:04X}");
                if let Some(v) = parsed_edit(ui, &mut self.bufs, (F_DID, i), init, 48.0, parse) {
                    e.did = v;
                }
                ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(60.0));
                ui.checkbox(&mut e.writable, "Writable")
                    .on_hover_text("Writable with 0x2E (extended session)");
                if icons::icon_button(ui, icons::clear(), "Remove DID").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Data:");
                let w = (ui.available_width() - 8.0).max(120.0);
                hex_bytes_edit(ui, &mut self.bufs, (F_DID_DATA, i), &mut e.data, w);
            });
            ui.add_space(2.0);
        }
        if let Some(i) = remove {
            dids.remove(i);
            self.bufs.clear();
        }
        if ui.button("+ Add DID").clicked() {
            dids.push(DidEntry {
                did: 0xF100 + dids.len() as u16,
                name: format!("DID{}", dids.len()),
                data: vec![0],
                writable: false,
            });
        }
    }

    fn dtcs_ui(&mut self, ui: &mut egui::Ui, dtcs: &mut Vec<DtcEntry>) {
        ui.label("Trouble codes (status hex):");
        let mut remove = None;
        for (i, e) in dtcs.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let cur = e.code;
                let init = || {
                    let s = dtc_to_string(cur);
                    s.strip_suffix("-00").map_or(s.clone(), str::to_string)
                };
                if let Some(v) = parsed_edit(ui, &mut self.bufs, (F_DTC, i), init, 80.0, parse_dtc)
                {
                    e.code = v;
                }
                ui.label("Status");
                let st = e.status;
                let parse = |s: &str| parse_hex_u32(s).and_then(|v| u8::try_from(v).ok());
                let init = || format!("{st:02X}");
                if let Some(v) =
                    parsed_edit(ui, &mut self.bufs, (F_DTC_STATUS, i), init, 32.0, parse)
                {
                    e.status = v;
                }
                if icons::icon_button(ui, icons::clear(), "Remove DTC").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            dtcs.remove(i);
            self.bufs.clear();
        }
        if ui.button("+ Add DTC").clicked() {
            dtcs.push(DtcEntry {
                code: 0,
                status: 0x09,
            });
        }
    }

    fn security_ui(&mut self, ui: &mut egui::Ui, sel: FlowId, sec: &mut Option<SecurityConfig>) {
        let mut on = sec.is_some();
        if ui.checkbox(&mut on, "SecurityAccess (0x27)").changed() {
            *sec = on.then(|| SecurityConfig {
                level: 1,
                seed: vec![0x12, 0x34, 0x56, 0x78],
                key_algo: KeyAlgo::XorConst(vec![0xFF]),
            });
            self.bufs.clear();
        }
        let Some(s) = sec.as_mut() else { return };
        let bufs = &mut self.bufs;
        egui::Grid::new(("diag_sec", sel))
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Seed level (odd, hex):");
                let cur = s.level;
                let parse = |t: &str| {
                    parse_hex_u32(t)
                        .and_then(|v| u8::try_from(v).ok())
                        .filter(|v| v % 2 == 1 && *v < 0x7F)
                };
                if let Some(v) = parsed_edit(
                    ui,
                    bufs,
                    (F_SEC_LEVEL, 0),
                    || format!("{cur:02X}"),
                    40.0,
                    parse,
                ) {
                    s.level = v;
                }
                ui.end_row();

                ui.label("Seed (hex):");
                hex_bytes_edit(ui, bufs, (F_SEC_SEED, 0), &mut s.seed, 160.0);
                ui.end_row();

                ui.label("Key algorithm:");
                egui::ComboBox::from_id_salt(("diag_algo", sel))
                    .selected_text(algo_label(&s.key_algo))
                    .show_ui(ui, |ui| {
                        let options = [
                            KeyAlgo::XorConst(vec![0xFF]),
                            KeyAlgo::AddConst(0),
                            KeyAlgo::Script,
                        ];
                        for o in options {
                            let same =
                                std::mem::discriminant(&o) == std::mem::discriminant(&s.key_algo);
                            if ui.selectable_label(same, algo_label(&o)).clicked() && !same {
                                s.key_algo = o;
                                bufs.remove(&(F_SEC_CONST, 0));
                            }
                        }
                    });
                ui.end_row();

                match &mut s.key_algo {
                    KeyAlgo::XorConst(c) => {
                        ui.label("XOR bytes (hex):");
                        hex_bytes_edit(ui, bufs, (F_SEC_CONST, 0), c, 160.0);
                        ui.end_row();
                    }
                    KeyAlgo::AddConst(k) => {
                        ui.label("Constant (hex):");
                        hex_u32_edit(ui, bufs, (F_SEC_CONST, 0), k, 100.0);
                        ui.end_row();
                    }
                    KeyAlgo::Script => {}
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtc_text_round_trip() {
        assert_eq!(parse_dtc("P0123"), Some(0x012300));
        assert_eq!(parse_dtc(&dtc_to_string(0xC10001)), Some(0xC10001));
    }

    #[test]
    fn algo_labels_differ() {
        assert_ne!(
            algo_label(&KeyAlgo::AddConst(1)),
            algo_label(&KeyAlgo::Script)
        );
    }
}
