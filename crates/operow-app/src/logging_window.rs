//! The Logging window: all logging settings, the trigger editor, the
//! current state and the files written this session.

use std::path::Path;

use egui::{Color32, RichText};
use operow_core::{BusId, UserSignalDef};

use crate::dbcs::DbcStore;
use crate::logging::{
    CmpOp, Condition, LogFile, LogState, LogStatus, LoggingConfig, SigCmp, StartTrigger,
    StopTrigger, expand_pattern, output_dir,
};
use crate::signals::{RawKind, SignalRef};
use crate::trace::NameLookup;

const REC_RED: Color32 = Color32::from_rgb(0xd0, 0x30, 0x30);
const ARMED_AMBER: Color32 = Color32::from_rgb(0xd0, 0x90, 0x1a);

/// What the window shows besides the config it edits.
pub struct LoggingInput<'a> {
    /// Every bus in channel order, with its name.
    pub buses: &'a [(BusId, String)],
    pub dbcs: &'a DbcStore,
    pub users: &'a [UserSignalDef],
    pub names: &'a NameLookup,
    pub status: LogStatus,
    pub files: &'a [LogFile],
    pub project: &'a str,
    pub project_dir: Option<&'a Path>,
}

/// Color of the state dot: red while recording, amber while armed.
pub fn state_color(state: LogState) -> Color32 {
    match state {
        LogState::Recording | LogState::PostTrigger => REC_RED,
        LogState::Armed => ARMED_AMBER,
        LogState::Idle => Color32::GRAY,
    }
}

/// `01:23.4` / `1:02:03`.
pub fn format_elapsed(s: f64) -> String {
    let total = s.max(0.0);
    let (h, m) = ((total / 3600.0) as u64, (total / 60.0) as u64 % 60);
    if h > 0 {
        format!("{h}:{m:02}:{:02}", (total % 60.0) as u64)
    } else {
        format!("{m:02}:{:04.1}", total % 60.0)
    }
}

pub fn open_folder(path: &Path) {
    let cmd = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

/// Show the window; returns whether the config changed.
pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    cfg: &mut LoggingConfig,
    input: &LoggingInput<'_>,
) -> bool {
    let mut changed = false;
    egui::Window::new("Logging")
        .open(open)
        .collapsible(false)
        .default_size([600.0, 680.0])
        .default_pos(egui::pos2(300.0, 50.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    changed |= contents(ui, cfg, input);
                });
        });
    changed
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    egui::Frame::group(ui.style())
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn contents(ui: &mut egui::Ui, cfg: &mut LoggingConfig, input: &LoggingInput<'_>) -> bool {
    let mut changed = false;
    let st = input.status;

    ui.horizontal(|ui| {
        changed |= ui.checkbox(&mut cfg.enabled, "Enable logging").changed();
        ui.separator();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 5.0, state_color(st.state));
        ui.label(RichText::new(st.state.label()).strong());
        if matches!(st.state, LogState::Recording | LogState::PostTrigger) {
            ui.label(format!(
                "{} \u{b7} {} \u{b7} {} frames",
                format_elapsed(st.elapsed_s),
                format_size(st.bytes),
                st.frames
            ));
        }
    });
    ui.weak("Logging starts and stops with the measurement.");

    section(ui, "Output", |ui| {
        egui::Grid::new("log_output")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("File name:");
                ui.vertical(|ui| {
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut cfg.pattern)
                                .desired_width(320.0)
                                .hint_text(crate::logging::DEFAULT_PATTERN),
                        )
                        .changed();
                    let next =
                        expand_pattern(&cfg.pattern, input.project, &operow_log::AscDate::now(), 1);
                    ui.weak("Tokens: {project} {date} {time} {n}");
                    ui.weak(format!("Next file: {next}"));
                });
                ui.end_row();

                ui.label("Folder:");
                let default_dir = output_dir(&LoggingConfig::default(), input.project_dir);
                let mut folder = cfg
                    .folder
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut folder)
                                .desired_width((ui.available_width() - 90.0).max(120.0))
                                .hint_text(default_dir.display().to_string()),
                        )
                        .changed()
                    {
                        cfg.folder = (!folder.trim().is_empty()).then(|| folder.trim().into());
                        changed = true;
                    }
                    if ui.button("Browse...").clicked()
                        && let Some(p) = rfd::FileDialog::new().pick_folder()
                    {
                        cfg.folder = Some(p);
                        changed = true;
                    }
                });
                ui.end_row();
            });
        ui.weak("Empty folder: the project folder, else the home folder.");
    });

    section(ui, "Trigger", |ui| {
        changed |= trigger_ui(ui, cfg, input);
    });

    section(ui, "Buses and channels", |ui| {
        if input.buses.is_empty() {
            ui.weak("No buses.");
            return;
        }
        egui::Grid::new("log_buses")
            .num_columns(3)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.strong("Log");
                ui.strong("Bus");
                ui.strong("ASC channel");
                ui.end_row();
                for (i, (bus, name)) in input.buses.iter().enumerate() {
                    let mut included = cfg.includes(*bus);
                    if ui.checkbox(&mut included, "").changed() {
                        let all: Vec<BusId> = input.buses.iter().map(|(b, _)| *b).collect();
                        let mut sel = cfg.buses.take().unwrap_or_else(|| all.clone());
                        sel.retain(|b| b != bus);
                        if included {
                            sel.push(*bus);
                        }
                        sel.sort();
                        cfg.buses = (sel.len() != all.len()).then_some(sel);
                        changed = true;
                    }
                    ui.label(name);
                    let mut ch = cfg
                        .channels
                        .iter()
                        .find(|(b, _)| b == bus)
                        .map_or((i + 1).min(255) as u8, |(_, c)| *c);
                    if ui
                        .add(egui::DragValue::new(&mut ch).range(1..=255))
                        .changed()
                    {
                        cfg.channels.retain(|(b, _)| b != bus);
                        if usize::from(ch) != i + 1 {
                            cfg.channels.push((*bus, ch));
                        }
                        changed = true;
                    }
                    ui.end_row();
                }
            });
        ui.weak("Default channels follow the bus order. Frames are written with their Tx/Rx direction on that bus.");
    });

    section(ui, "Split files", |ui| {
        egui::Grid::new("log_split")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                changed |= optional_drag(ui, "By size", &mut cfg.split_mb, 100, 1..=100_000, " MB");
                ui.end_row();
                changed |= optional_drag(
                    ui,
                    "By time",
                    &mut cfg.split_minutes,
                    10,
                    1..=10_000,
                    " min",
                );
                ui.end_row();
            });
    });

    section(ui, "Files this session", |ui| {
        if input.files.is_empty() {
            ui.weak("Nothing written yet.");
        } else {
            egui::ScrollArea::vertical()
                .id_salt("log_files")
                .max_height(110.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new("log_files_grid")
                        .num_columns(3)
                        .spacing([16.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for f in input.files {
                                let name = f
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                ui.label(RichText::new(name).monospace())
                                    .on_hover_text(f.path.display().to_string());
                                ui.label(format_size(f.size()));
                                ui.label(format!("{} frames", f.frames));
                                ui.end_row();
                            }
                        });
                });
        }
        let dir = input
            .files
            .last()
            .and_then(|f| f.path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| output_dir(cfg, input.project_dir));
        if ui
            .add_enabled(dir.is_dir(), egui::Button::new("Open folder"))
            .on_hover_text(dir.display().to_string())
            .clicked()
        {
            open_folder(&dir);
        }
    });
    changed
}

pub fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} kB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// A checkbox that enables a numeric limit.
fn optional_drag(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<u32>,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
    suffix: &str,
) -> bool {
    let mut changed = false;
    let mut on = value.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *value = on.then_some(default);
        changed = true;
    }
    ui.add_enabled_ui(on, |ui| {
        let mut v = value.unwrap_or(default);
        if ui
            .add(egui::DragValue::new(&mut v).range(range).suffix(suffix))
            .changed()
        {
            *value = Some(v);
            changed = true;
        }
    });
    changed
}

fn trigger_ui(ui: &mut egui::Ui, cfg: &mut LoggingConfig, input: &LoggingInput<'_>) -> bool {
    let mut changed = false;
    let default_cond = Condition::IdSeen {
        bus: None,
        id: 0x100,
        ext: false,
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new("Start").strong());
        let t = &mut cfg.trigger;
        let on_cond = matches!(t.start, StartTrigger::OnCondition(_));
        egui::ComboBox::from_id_salt("log_start")
            .selected_text(if on_cond {
                "On condition"
            } else {
                "Immediately"
            })
            .show_ui(ui, |ui| {
                if ui.selectable_label(!on_cond, "Immediately").clicked() && on_cond {
                    t.start = StartTrigger::Immediate;
                    changed = true;
                }
                if ui.selectable_label(on_cond, "On condition").clicked() && !on_cond {
                    t.start = StartTrigger::OnCondition(default_cond.clone());
                    changed = true;
                }
            });
    });
    if let StartTrigger::OnCondition(c) = &mut cfg.trigger.start {
        ui.indent("log_start_cond", |ui| {
            changed |= condition_ui(ui, "start", c, input);
        });
    }
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label(RichText::new("Stop").strong());
        let t = &mut cfg.trigger;
        let text = match t.stop {
            StopTrigger::OnMeasurementStop => "On measurement stop",
            StopTrigger::OnCondition(_) => "On condition",
            StopTrigger::AfterSeconds(_) => "After time",
        };
        egui::ComboBox::from_id_salt("log_stop")
            .selected_text(text)
            .show_ui(ui, |ui| {
                let is_m = matches!(t.stop, StopTrigger::OnMeasurementStop);
                let is_c = matches!(t.stop, StopTrigger::OnCondition(_));
                let is_a = matches!(t.stop, StopTrigger::AfterSeconds(_));
                if ui.selectable_label(is_m, "On measurement stop").clicked() && !is_m {
                    t.stop = StopTrigger::OnMeasurementStop;
                    changed = true;
                }
                if ui.selectable_label(is_c, "On condition").clicked() && !is_c {
                    t.stop = StopTrigger::OnCondition(Condition::Key('E'));
                    changed = true;
                }
                if ui.selectable_label(is_a, "After time").clicked() && !is_a {
                    t.stop = StopTrigger::AfterSeconds(10.0);
                    changed = true;
                }
            });
        if let StopTrigger::AfterSeconds(s) = &mut t.stop {
            changed |= ui
                .add(
                    egui::DragValue::new(s)
                        .range(0.1..=86_400.0)
                        .speed(0.1)
                        .suffix(" s"),
                )
                .changed();
        }
    });
    if let StopTrigger::OnCondition(c) = &mut cfg.trigger.stop {
        ui.indent("log_stop_cond", |ui| {
            changed |= condition_ui(ui, "stop", c, input);
        });
    }
    ui.add_space(6.0);

    egui::Grid::new("log_prepost")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Pre-trigger:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.trigger.pre_trigger_s)
                        .range(0.0..=3600.0)
                        .speed(0.05)
                        .suffix(" s"),
                )
                .on_hover_text("Frames from the buffer written before the start condition")
                .changed();
            ui.end_row();
            ui.label("Post-trigger:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.trigger.post_trigger_s)
                        .range(0.0..=3600.0)
                        .speed(0.05)
                        .suffix(" s"),
                )
                .on_hover_text("Keep writing this long after the stop condition")
                .changed();
            ui.end_row();
        });
    ui.weak(
        "A recording is single-shot per measurement. Key conditions need no text field focused.",
    );
    changed
}

/// Every DBC and user signal that can be picked for a condition.
fn signal_candidates(input: &LoggingInput<'_>) -> Vec<(String, SignalRef)> {
    let mut v = Vec::new();
    let mut dbc_buses: Vec<_> = input.dbcs.by_bus.iter().collect();
    dbc_buses.sort_by_key(|(b, _)| **b);
    for (bus, db) in dbc_buses {
        for m in &db.messages {
            for s in &m.signals {
                let sig = SignalRef::Dbc {
                    bus: *bus,
                    msg_id: m.id,
                    extended: m.extended,
                    signal_name: s.name.clone(),
                };
                v.push((sig.label(input.names, input.users), sig));
            }
        }
    }
    for u in input.users {
        let sig = SignalRef::User(u.id);
        v.push((sig.label(input.names, input.users), sig));
    }
    v
}

fn bus_combo(
    ui: &mut egui::Ui,
    salt: &str,
    bus: &mut Option<BusId>,
    buses: &[(BusId, String)],
    allow_any: bool,
) -> bool {
    let mut changed = false;
    let text = bus
        .and_then(|b| {
            buses
                .iter()
                .find(|(id, _)| *id == b)
                .map(|(_, n)| n.as_str())
        })
        .unwrap_or("Any bus");
    egui::ComboBox::from_id_salt(salt)
        .selected_text(text)
        .show_ui(ui, |ui| {
            if allow_any && ui.selectable_label(bus.is_none(), "Any bus").clicked() {
                *bus = None;
                changed = true;
            }
            for (id, name) in buses {
                if ui.selectable_label(*bus == Some(*id), name).clicked() {
                    *bus = Some(*id);
                    changed = true;
                }
            }
        });
    changed
}

fn id_drag(ui: &mut egui::Ui, id: &mut u32, ext: bool) -> bool {
    let max = if ext { 0x1FFF_FFFF } else { 0x7FF };
    *id = (*id).min(max);
    ui.add(
        egui::DragValue::new(id)
            .range(0..=max)
            .hexadecimal(3, true, false)
            .prefix("0x"),
    )
    .changed()
}

fn condition_ui(
    ui: &mut egui::Ui,
    salt: &str,
    cond: &mut Condition,
    input: &LoggingInput<'_>,
) -> bool {
    let mut changed = false;
    let first_bus = input.buses.first().map(|(b, _)| *b);
    let kind = match cond {
        Condition::IdSeen { .. } => 0,
        Condition::Signal { .. } => 1,
        Condition::Key(_) => 2,
    };
    ui.horizontal_wrapped(|ui| {
        let mut new_kind = kind;
        egui::ComboBox::from_id_salt(format!("log_cond_kind_{salt}"))
            .selected_text(["ID seen", "Signal", "Key"][kind])
            .show_ui(ui, |ui| {
                for (i, l) in ["ID seen", "Signal", "Key"].into_iter().enumerate() {
                    ui.selectable_value(&mut new_kind, i, l);
                }
            });
        if new_kind != kind {
            *cond = match new_kind {
                0 => Condition::IdSeen {
                    bus: None,
                    id: 0x100,
                    ext: false,
                },
                1 => Condition::Signal {
                    signal: signal_candidates(input).into_iter().next().map_or(
                        SignalRef::Raw {
                            bus: first_bus.unwrap_or(BusId(1)),
                            id: 0x100,
                            extended: false,
                            kind: RawKind::Byte(0),
                        },
                        |(_, s)| s,
                    ),
                    cmp: SigCmp {
                        op: CmpOp::Gt,
                        value: 0.0,
                    },
                },
                _ => Condition::Key('T'),
            };
            changed = true;
        }
        match cond {
            Condition::IdSeen { bus, id, ext } => {
                changed |= bus_combo(ui, &format!("log_cond_bus_{salt}"), bus, input.buses, true);
                changed |= id_drag(ui, id, *ext);
                changed |= ui.checkbox(ext, "Extended").changed();
            }
            Condition::Signal { signal, cmp } => {
                changed |= signal_ui(ui, salt, signal, input);
                egui::ComboBox::from_id_salt(format!("log_cond_op_{salt}"))
                    .selected_text(cmp.op.label())
                    .width(52.0)
                    .show_ui(ui, |ui| {
                        for op in CmpOp::ALL {
                            changed |= ui.selectable_value(&mut cmp.op, op, op.label()).changed();
                        }
                    });
                changed |= ui
                    .add(egui::DragValue::new(&mut cmp.value).speed(0.1))
                    .changed();
            }
            Condition::Key(k) => {
                egui::ComboBox::from_id_salt(format!("log_cond_key_{salt}"))
                    .selected_text(k.to_string())
                    .width(52.0)
                    .show_ui(ui, |ui| {
                        for c in ('A'..='Z').chain('0'..='9') {
                            changed |= ui.selectable_value(k, c, c.to_string()).changed();
                        }
                    });
                ui.weak("pressed");
            }
        }
    });
    changed
}

/// A DBC/user signal from a list, or a raw frame byte.
fn signal_ui(
    ui: &mut egui::Ui,
    salt: &str,
    signal: &mut SignalRef,
    input: &LoggingInput<'_>,
) -> bool {
    let mut changed = false;
    let is_raw = matches!(signal, SignalRef::Raw { .. });
    let text = if is_raw {
        "Raw byte".to_string()
    } else {
        signal.label(input.names, input.users)
    };
    egui::ComboBox::from_id_salt(format!("log_cond_sig_{salt}"))
        .selected_text(text)
        .show_ui(ui, |ui| {
            for (label, sig) in signal_candidates(input) {
                if ui.selectable_label(*signal == sig, label).clicked() {
                    *signal = sig;
                    changed = true;
                }
            }
            if ui.selectable_label(is_raw, "Raw byte").clicked() && !is_raw {
                *signal = SignalRef::Raw {
                    bus: input.buses.first().map_or(BusId(1), |(b, _)| *b),
                    id: 0x100,
                    extended: false,
                    kind: RawKind::Byte(0),
                };
                changed = true;
            }
        });
    if let SignalRef::Raw {
        bus,
        id,
        extended,
        kind,
    } = signal
    {
        let mut b = Some(*bus);
        if bus_combo(
            ui,
            &format!("log_cond_rawbus_{salt}"),
            &mut b,
            input.buses,
            false,
        ) {
            *bus = b.unwrap_or(*bus);
            changed = true;
        }
        changed |= id_drag(ui, id, *extended);
        changed |= ui.checkbox(extended, "Ext").changed();
        let mut byte = match kind {
            RawKind::Byte(n) => *n,
            _ => 0,
        };
        if ui
            .add(
                egui::DragValue::new(&mut byte)
                    .range(0..=63)
                    .prefix("byte "),
            )
            .changed()
        {
            *kind = RawKind::Byte(byte);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_and_size_formatting() {
        assert_eq!(format_elapsed(3.26), "00:03.3");
        assert_eq!(format_elapsed(75.0), "01:15.0");
        assert_eq!(format_elapsed(3723.0), "1:02:03");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(12_345), "12.3 kB");
        assert_eq!(format_size(2_500_000), "2.5 MB");
    }
}
