//! The "New user signal" dialog: define a signal on a raw message, with a
//! bit-layout preview and the live value from the frame store.

use operow_core::{BusEvent, BusId, CanBusConfig, SignalByteOrder, UserSignalDef, UserSignalId};
use operow_dbc::SignalDef;

use crate::dbcs::format_value;
use crate::signals::user_signal_def;
use crate::store::FrameStore;

/// How far back in the store the live value looks for a matching frame.
const LIVE_SCAN: usize = 50_000;
/// Bytes shown in the layout preview at most.
const MAX_PREVIEW_BYTES: usize = 8;

pub struct NewSignalDialog {
    pub name: String,
    pub bus: Option<BusId>,
    pub id_hex: String,
    pub extended: bool,
    pub start_bit: u16,
    pub size: u16,
    pub motorola: bool,
    pub signed: bool,
    pub factor: f64,
    pub offset: f64,
    pub unit: String,
}

pub enum DialogOutcome {
    Open,
    Save(UserSignalDef),
    Cancel,
}

/// Bitmask of the data bits a signal occupies (64 bytes).
pub fn signal_mask(def: &SignalDef) -> [u8; 64] {
    let mut data = [0u8; 64];
    def.encode_raw(&mut data, u64::MAX);
    data
}

/// Number of preview byte rows: enough to show every signal bit, at least
/// two, at most [`MAX_PREVIEW_BYTES`].
pub fn preview_bytes(mask: &[u8; 64]) -> usize {
    let last = mask.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    last.clamp(2, MAX_PREVIEW_BYTES)
}

impl NewSignalDialog {
    pub fn new(bus: Option<BusId>) -> Self {
        NewSignalDialog {
            name: String::new(),
            bus,
            id_hex: String::new(),
            extended: false,
            start_bit: 0,
            size: 8,
            motorola: false,
            signed: false,
            factor: 1.0,
            offset: 0.0,
            unit: String::new(),
        }
    }

    fn msg_id(&self) -> Result<u32, String> {
        let t = self.id_hex.trim();
        let t = t.trim_start_matches("0x").trim_start_matches("0X");
        let id =
            u32::from_str_radix(t, 16).map_err(|_| "Enter the message ID in hex".to_string())?;
        if self.extended && id > 0x1FFF_FFFF {
            Err("ID exceeds 29 bits".into())
        } else if !self.extended && id > 0x7FF {
            Err("ID exceeds 11 bits (tick Extended)".into())
        } else {
            Ok(id)
        }
    }

    /// The signal as entered, ignoring the name. Errors say what is wrong.
    pub fn def(&self, id: UserSignalId) -> Result<UserSignalDef, String> {
        let bus = self.bus.ok_or("Choose a bus")?;
        let msg_id = self.msg_id()?;
        if !(1..=64).contains(&self.size) {
            return Err("Length must be 1 to 64 bits".into());
        }
        if !self.factor.is_finite() || self.factor == 0.0 || !self.offset.is_finite() {
            return Err("Factor must be a non-zero number".into());
        }
        let def = UserSignalDef {
            id,
            name: self.name.trim().to_string(),
            bus,
            msg_id,
            extended: self.extended,
            start_bit: self.start_bit,
            size: self.size,
            byte_order: if self.motorola {
                SignalByteOrder::Motorola
            } else {
                SignalByteOrder::Intel
            },
            signed: self.signed,
            factor: self.factor,
            offset: self.offset,
            unit: self.unit.trim().to_string(),
        };
        let bits: u32 = signal_mask(&user_signal_def(&def))
            .iter()
            .map(|b| b.count_ones())
            .sum();
        if bits != self.size as u32 {
            return Err("Signal does not fit into 64 data bytes".into());
        }
        Ok(def)
    }

    fn name_error(&self, existing: &[UserSignalDef]) -> Option<String> {
        let n = self.name.trim();
        if n.is_empty() {
            Some("Enter a name".into())
        } else if existing.iter().any(|u| u.name == n) {
            Some(format!("A user signal called \"{n}\" already exists"))
        } else {
            None
        }
    }

    /// Validated signal ready to save.
    pub fn build(
        &self,
        id: UserSignalId,
        existing: &[UserSignalDef],
    ) -> Result<UserSignalDef, String> {
        if let Some(e) = self.name_error(existing) {
            return Err(e);
        }
        self.def(id)
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        buses: &[CanBusConfig],
        existing: &[UserSignalDef],
        store: &FrameStore,
        next_id: UserSignalId,
    ) -> DialogOutcome {
        let mut outcome = DialogOutcome::Open;
        egui::Window::new("New user signal")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                self.form(ui, buses);
                ui.add_space(8.0);
                let def = self.def(next_id);
                self.preview(ui, def.as_ref().ok(), store);
                ui.add_space(8.0);
                let built = self.build(next_id, existing);
                if let Err(e) = &built {
                    ui.colored_label(egui::Color32::from_rgb(0xd0, 0x50, 0x50), e);
                }
                ui.horizontal(|ui| {
                    let ok = built.is_ok();
                    if ui
                        .add_enabled(ok, egui::Button::new("Save"))
                        .on_disabled_hover_text("Fix the problem shown above")
                        .clicked()
                        && let Ok(def) = built
                    {
                        outcome = DialogOutcome::Save(def);
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Cancel;
                    }
                });
            });
        outcome
    }

    fn form(&mut self, ui: &mut egui::Ui, buses: &[CanBusConfig]) {
        egui::Grid::new("new_signal_grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(220.0));
                ui.end_row();

                ui.label("Bus:");
                let current = buses
                    .iter()
                    .find(|b| Some(b.id) == self.bus)
                    .map_or("Select\u{2026}", |b| b.name.as_str());
                egui::ComboBox::from_id_salt("new_signal_bus")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for b in buses {
                            ui.selectable_value(&mut self.bus, Some(b.id), &b.name);
                        }
                    });
                ui.end_row();

                ui.label("Message ID (hex):");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.id_hex)
                            .desired_width(90.0)
                            .hint_text("100")
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.checkbox(&mut self.extended, "Extended (29-bit)");
                });
                ui.end_row();

                ui.label("Start bit / length:");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.start_bit).range(0..=511));
                    ui.label("/");
                    ui.add(
                        egui::DragValue::new(&mut self.size)
                            .range(1..=64)
                            .suffix(" bits"),
                    );
                });
                ui.end_row();

                ui.label("Byte order:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.motorola, false, "Intel");
                    ui.selectable_value(&mut self.motorola, true, "Motorola");
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.signed, "Signed");
                });
                ui.end_row();

                ui.label("Factor / offset:");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.factor).speed(0.01));
                    ui.label("/");
                    ui.add(egui::DragValue::new(&mut self.offset).speed(0.1));
                });
                ui.end_row();

                ui.label("Unit:");
                ui.add(egui::TextEdit::singleline(&mut self.unit).desired_width(90.0));
                ui.end_row();
            });
    }

    /// Bit grid (bit 7 to bit 0 per byte) with the signal's bits
    /// highlighted, and the value decoded from the latest matching frame.
    fn preview(&self, ui: &mut egui::Ui, def: Option<&UserSignalDef>, store: &FrameStore) {
        let sig = def.map(user_signal_def);
        let mask = sig.as_ref().map_or([0u8; 64], signal_mask);
        let rows = preview_bytes(&mask);
        let live: Option<&BusEvent> = def.and_then(|d| {
            store.find_latest(LIVE_SCAN, |e| {
                e.bus == d.bus && e.frame.id == d.msg_id && e.frame.extended == d.extended
            })
        });

        ui.strong("Bit layout");
        let (cw, ch, label_w) = (30.0, 22.0, 34.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(label_w + 8.0 * cw, (rows + 1) as f32 * ch),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        let on_fill = visuals.selection.bg_fill;
        let off_fill = visuals.faint_bg_color;
        let text = visuals.text_color();
        let weak = visuals.weak_text_color();
        let grid = visuals.widgets.noninteractive.bg_stroke.color;
        let font = egui::FontId::monospace(12.0);
        for bit in 0..8 {
            painter.text(
                rect.min + egui::vec2(label_w + (7 - bit) as f32 * cw + cw / 2.0, ch / 2.0),
                egui::Align2::CENTER_CENTER,
                bit.to_string(),
                font.clone(),
                weak,
            );
        }
        let start = def.map(|d| d.start_bit as usize);
        for (byte, mask_byte) in mask.iter().enumerate().take(rows) {
            let y = rect.min.y + (byte + 1) as f32 * ch;
            painter.text(
                egui::pos2(rect.min.x + 4.0, y + ch / 2.0),
                egui::Align2::LEFT_CENTER,
                format!("B{byte}"),
                font.clone(),
                weak,
            );
            for bit in 0..8usize {
                let x = rect.min.x + label_w + (7 - bit) as f32 * cw;
                let cell = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cw, ch));
                let in_sig = mask_byte >> bit & 1 == 1;
                painter.rect_filled(
                    cell.shrink(1.0),
                    3.0,
                    if in_sig { on_fill } else { off_fill },
                );
                if start == Some(byte * 8 + bit) {
                    painter.rect_stroke(
                        cell.shrink(1.0),
                        3.0,
                        egui::Stroke::new(2.0_f32, text),
                        egui::StrokeKind::Inside,
                    );
                }
                let (label, color) = match live {
                    Some(e) => {
                        let v = e
                            .frame
                            .payload()
                            .get(byte)
                            .is_some_and(|b| b >> bit & 1 == 1);
                        (if v { "1" } else { "0" }.to_string(), text)
                    }
                    None => (
                        (byte * 8 + bit).to_string(),
                        if in_sig { text } else { weak },
                    ),
                };
                painter.text(
                    cell.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    font.clone(),
                    color,
                );
            }
        }
        painter.rect_stroke(
            egui::Rect::from_min_max(rect.min + egui::vec2(label_w, ch), rect.max),
            0.0,
            egui::Stroke::new(1.0_f32, grid),
            egui::StrokeKind::Inside,
        );
        ui.weak(match def {
            Some(d) if d.byte_order == SignalByteOrder::Motorola => {
                "Outlined bit: start bit (MSB for Motorola). Cells show bit numbers."
            }
            _ => "Outlined bit: start bit (LSB for Intel). Cells show bit numbers.",
        });
        if mask[MAX_PREVIEW_BYTES..].iter().any(|b| *b != 0) {
            ui.weak("Some signal bits lie beyond byte 7 and are not shown.");
        }

        ui.add_space(4.0);
        match (def, sig, live) {
            (Some(d), Some(sig), Some(e)) => {
                let v = sig.decode(e.frame.payload());
                let raw = e
                    .frame
                    .payload()
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Live value:");
                    ui.monospace(format!("{} {}", format_value(v, d.factor), d.unit));
                    ui.weak(format!("(frame {raw})"));
                });
            }
            (Some(_), _, None) => {
                ui.weak("Live value: no matching frame in the buffer yet.");
            }
            _ => {
                ui.weak("Live value: complete the fields above.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> NewSignalDialog {
        NewSignalDialog {
            name: "Speed".into(),
            bus: Some(BusId(1)),
            id_hex: "100".into(),
            size: 16,
            ..NewSignalDialog::new(None)
        }
    }

    fn bits(m: &[u8; 64]) -> Vec<usize> {
        (0..512).filter(|i| m[i / 8] >> (i % 8) & 1 == 1).collect()
    }

    #[test]
    fn mask_intel_and_motorola() {
        let mut d = dialog();
        d.start_bit = 4;
        d.size = 8;
        let def = d.def(UserSignalId(1)).unwrap();
        assert_eq!(
            bits(&signal_mask(&user_signal_def(&def))),
            (4..12).collect::<Vec<_>>()
        );

        d.motorola = true;
        d.start_bit = 7;
        d.size = 16;
        let def = d.def(UserSignalId(1)).unwrap();
        let m = signal_mask(&user_signal_def(&def));
        assert_eq!(bits(&m), (0..16).collect::<Vec<_>>());
        assert_eq!(preview_bytes(&m), 2);
    }

    #[test]
    fn preview_rows_grow_with_signal() {
        let mut d = dialog();
        d.size = 4;
        let m = signal_mask(&user_signal_def(&d.def(UserSignalId(1)).unwrap()));
        assert_eq!(preview_bytes(&m), 2, "at least two bytes");
        d.start_bit = 40;
        let m = signal_mask(&user_signal_def(&d.def(UserSignalId(1)).unwrap()));
        assert_eq!(preview_bytes(&m), 6);
        d.start_bit = 200;
        let m = signal_mask(&user_signal_def(&d.def(UserSignalId(1)).unwrap()));
        assert_eq!(preview_bytes(&m), MAX_PREVIEW_BYTES);
    }

    #[test]
    fn validation() {
        let ok = dialog().build(UserSignalId(4), &[]).unwrap();
        assert_eq!((ok.id, ok.msg_id, ok.size), (UserSignalId(4), 0x100, 16));
        let bad = |f: fn(&mut NewSignalDialog)| {
            let mut d = dialog();
            f(&mut d);
            d.build(UserSignalId(1), std::slice::from_ref(&ok)).is_err()
        };
        assert!(bad(|d| d.name = "  ".into()));
        assert!(bad(|d| d.name = "Speed".into()), "duplicate name");
        assert!(bad(|d| d.bus = None));
        assert!(bad(|d| d.id_hex = "zz".into()));
        assert!(bad(|d| d.id_hex = "800".into()), "11-bit limit");
        assert!(bad(|d| d.size = 0));
        assert!(bad(|d| d.size = 65));
        assert!(bad(|d| d.factor = 0.0));
        assert!(bad(|d| d.start_bit = 511), "runs past byte 63");
        let mut d = dialog();
        d.extended = true;
        d.id_hex = "0x18DAF110".into();
        assert_eq!(d.def(UserSignalId(1)).unwrap().msg_id, 0x18DA_F110);
        d.id_hex = "20000000".into();
        assert!(d.def(UserSignalId(1)).is_err());
    }
}
