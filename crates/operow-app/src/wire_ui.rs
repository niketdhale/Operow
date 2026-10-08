//! Editing a [`WireStyle`]: the Wire section of the Properties panel and
//! the project default in the Network toolbar's Wires menu.

use operow_core::{WireArrow, WireKind, WireLine, WireStyle};

const KINDS: [(WireKind, &str); 4] = [
    (WireKind::Bezier, "Curved"),
    (WireKind::Straight, "Straight"),
    (WireKind::Step, "Right-angled"),
    (WireKind::SmoothStep, "Rounded"),
];
const LINES: [(WireLine, &str); 3] = [
    (WireLine::Solid, "Solid"),
    (WireLine::Dashed, "Dashed"),
    (WireLine::Dotted, "Dotted"),
];
const ARROWS: [(WireArrow, &str); 5] = [
    (WireArrow::None, "None"),
    (WireArrow::Triangle, "Triangle"),
    (WireArrow::Open, "Open"),
    (WireArrow::Circle, "Circle"),
    (WireArrow::Diamond, "Diamond"),
];
const SWITCH: [(bool, &str); 2] = [(true, "On"), (false, "Off")];

/// A row with an "Auto" entry (unset) and the given options.
fn choice<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<T>,
    options: &[(T, &str)],
) -> bool {
    let before = *value;
    ui.horizontal(|ui| {
        ui.label(label);
        let text = options
            .iter()
            .find(|(t, _)| Some(*t) == *value)
            .map_or("Auto", |(_, n)| n);
        egui::ComboBox::from_id_salt(("wire_choice", label))
            .selected_text(text)
            .show_ui(ui, |ui| {
                ui.selectable_value(value, None, "Auto");
                for (t, name) in options {
                    ui.selectable_value(value, Some(*t), *name);
                }
            });
    });
    *value != before
}

/// Edit `style`; with `full`, also the arrows and the label (per-wire only).
/// Returns whether anything changed.
pub fn wire_style_ui(ui: &mut egui::Ui, style: &mut WireStyle, full: bool) -> bool {
    let mut changed = choice(ui, "Routing:", &mut style.kind, &KINDS);
    changed |= choice(ui, "Line:", &mut style.line, &LINES);
    ui.horizontal(|ui| {
        ui.label("Colour:");
        let mut custom = style.color.is_some();
        if ui.checkbox(&mut custom, "").changed() {
            style.color = custom.then_some([120, 160, 255]);
            changed = true;
        }
        if let Some(c) = &mut style.color {
            changed |= ui.color_edit_button_srgb(c).changed();
        }
    });
    ui.horizontal(|ui| {
        ui.label("Width:");
        let mut custom = style.width.is_some();
        if ui.checkbox(&mut custom, "").changed() {
            style.width = custom.then_some(1.5);
            changed = true;
        }
        if let Some(w) = &mut style.width {
            changed |= ui
                .add(egui::DragValue::new(w).range(0.5..=8.0).speed(0.1))
                .changed();
        }
    });
    changed |= choice(ui, "Animated:", &mut style.animated, &SWITCH);
    if full {
        changed |= choice(ui, "Arrow:", &mut style.arrow, &ARROWS);
        changed |= choice(ui, "Arrow at node:", &mut style.arrow_at_source, &SWITCH);
        ui.horizontal(|ui| {
            ui.label("Label:");
            let mut custom = style.label.is_some();
            if ui.checkbox(&mut custom, "").changed() {
                style.label = custom.then(String::new);
                changed = true;
            }
            if let Some(l) = &mut style.label {
                changed |= ui.text_edit_singleline(l).changed();
            }
        });
    }
    changed
}

/// Whether an edit that `changed` this frame or earlier is over (no mouse
/// button held), so a drag or colour pick makes one undo step, not one per frame.
pub fn edit_finished(ui: &egui::Ui, changed: bool) -> bool {
    let id = egui::Id::new("wire_edit_pending");
    let pending = ui.data_mut(|d| {
        let p = d.get_temp_mut_or_default::<bool>(id);
        *p |= changed;
        *p
    });
    let done = pending && !ui.input(|i| i.pointer.any_down());
    if done {
        ui.data_mut(|d| d.insert_temp(id, false));
    }
    done
}
