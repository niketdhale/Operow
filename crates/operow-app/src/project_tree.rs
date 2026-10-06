//! The project tree panel: networks, databases and scripts.

use egui::{CollapsingHeader, RichText};
use egui_flow::NodeId as FlowId;
use operow_core::{NodeKind, UserSignalDef, UserSignalId};

use crate::dbcs::DbcStore;
use crate::graph::{Graph, GraphNode};
use crate::icons;
use crate::signals::SignalRef;

/// What the user did in the tree this frame.
#[derive(Default)]
pub struct TreeOutput {
    /// The canvas node that was clicked.
    pub picked: Option<FlowId>,
    /// "+ New signal..." was clicked.
    pub new_signal: bool,
    /// "Add to graph" was chosen for a signal.
    pub add_signal_to_graph: Option<SignalRef>,
    pub delete_signal: Option<UserSignalId>,
    /// "+ New test module..." was clicked.
    pub new_test_module: bool,
    /// Test module (project path) to run.
    pub run_test_module: Option<String>,
    /// Test module (project path) to open in an editor.
    pub open_test_module: Option<String>,
    /// Index into `topology.tests` to remove from the project.
    pub remove_test_module: Option<usize>,
}

/// Draws the tree.
pub fn ui(ui: &mut egui::Ui, graph: &Graph, dbcs: &DbcStore, tests_busy: bool) -> TreeOutput {
    let mut out = TreeOutput::default();
    let mut picked = None;
    let selected = graph.selected();
    let topo = graph.to_topology();
    let flow_of_ecu = |id| {
        graph.state.nodes.iter().find_map(|n| match &n.data {
            GraphNode::Ecu(e) if e.id == id => Some(n.id),
            _ => None,
        })
    };
    let flow_of_bus = |id| {
        graph.state.nodes.iter().find_map(|n| match &n.data {
            GraphNode::Bus(b) if b.id == id => Some(n.id),
            _ => None,
        })
    };

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            CollapsingHeader::new(RichText::new("Networks").strong())
                .icon(crate::icons::disclosure)
                .default_open(true)
                .show(ui, |ui| {
                    for bus in &topo.buses {
                        let flow = flow_of_bus(bus.id);
                        let resp = CollapsingHeader::new(format!(
                            "{}  ({})",
                            bus.name,
                            crate::graph::format_bitrate(bus.bitrate)
                        ))
                        .id_salt(("tree_bus", bus.id))
                        .default_open(true)
                        .show(ui, |ui| {
                            for link in topo.links.iter().filter(|l| l.bus == bus.id) {
                                let Some(node) = topo.nodes.iter().find(|n| n.id == link.node)
                                else {
                                    continue;
                                };
                                let flow = flow_of_ecu(node.id);
                                let icon = match node.kind {
                                    NodeKind::Gateway { .. } => icons::gateway(),
                                    NodeKind::Replay { .. } => icons::replay(),
                                    NodeKind::Ecu => icons::ecu(),
                                };
                                ui.horizontal(|ui| {
                                    ui.add(icons::icon_image(ui, icon));
                                    let on = flow.is_some() && flow == selected;
                                    if ui.selectable_label(on, &node.name).clicked() {
                                        picked = flow;
                                    }
                                });
                            }
                        });
                        if resp.header_response.clicked() {
                            picked = picked.or(flow);
                        }
                    }
                    let unlinked: Vec<_> = topo
                        .nodes
                        .iter()
                        .filter(|n| !topo.links.iter().any(|l| l.node == n.id))
                        .collect();
                    if !unlinked.is_empty() {
                        CollapsingHeader::new("Unconnected")
                            .icon(crate::icons::disclosure)
                            .default_open(true)
                            .show(ui, |ui| {
                                for node in unlinked {
                                    let flow = flow_of_ecu(node.id);
                                    let on = flow.is_some() && flow == selected;
                                    if ui.selectable_label(on, &node.name).clicked() {
                                        picked = flow;
                                    }
                                }
                            });
                    }
                });

            CollapsingHeader::new(RichText::new("Databases").strong())
                .icon(crate::icons::disclosure)
                .default_open(true)
                .show(ui, |ui| {
                    if topo.databases.is_empty() {
                        ui.weak("No databases");
                    }
                    for (i, d) in topo.databases.iter().enumerate() {
                        let file = std::path::Path::new(&d.path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(&d.path);
                        let bus = topo
                            .buses
                            .iter()
                            .find(|b| b.id == d.bus)
                            .map_or("?", |b| b.name.as_str());
                        CollapsingHeader::new(format!("{file}  \u{2192} {bus}"))
                            .icon(crate::icons::disclosure)
                            .id_salt(("tree_db", i))
                            .show(ui, |ui| {
                                let Some(db) = dbcs.by_bus.get(&d.bus) else {
                                    ui.weak("Not loaded");
                                    return;
                                };
                                for m in &db.messages {
                                    CollapsingHeader::new(format!("0x{:X}  {}", m.id, m.name))
                                        .icon(crate::icons::disclosure)
                                        .id_salt(("tree_msg", i, m.id, m.extended))
                                        .show(ui, |ui| {
                                            for s in &m.signals {
                                                let unit = if s.unit.is_empty() {
                                                    String::new()
                                                } else {
                                                    format!("  [{}]", s.unit)
                                                };
                                                let sig = SignalRef::Dbc {
                                                    bus: d.bus,
                                                    msg_id: m.id,
                                                    extended: m.extended,
                                                    signal_name: s.name.clone(),
                                                };
                                                let drag_id = egui::Id::new((
                                                    "tree_sig", i, m.id, m.extended, &s.name,
                                                ));
                                                let resp = ui
                                                    .dnd_drag_source(drag_id, sig.clone(), |ui| {
                                                        ui.label(format!("{}{unit}", s.name))
                                                    })
                                                    .response
                                                    .on_hover_text(
                                                        "Drag onto a graph window to plot",
                                                    );
                                                resp.context_menu(|ui| {
                                                    if ui.button("Add to graph").clicked() {
                                                        out.add_signal_to_graph = Some(sig);
                                                        ui.close();
                                                    }
                                                });
                                            }
                                        });
                                }
                            });
                    }
                });

            CollapsingHeader::new(RichText::new("User signals").strong())
                .icon(crate::icons::disclosure)
                .default_open(true)
                .show(ui, |ui| {
                    if graph.user_signals.is_empty() {
                        ui.weak("No user signals");
                    }
                    for u in &graph.user_signals {
                        let resp = ui
                            .dnd_drag_source(
                                egui::Id::new(("tree_user_sig", u.id)),
                                SignalRef::User(u.id),
                                |ui| ui.selectable_label(false, user_signal_label(u)),
                            )
                            .response
                            .on_hover_text(user_signal_tooltip(u, &topo));
                        resp.context_menu(|ui| {
                            if ui.button("Add to graph").clicked() {
                                out.add_signal_to_graph = Some(SignalRef::User(u.id));
                                ui.close();
                            }
                            if ui.button("Delete").clicked() {
                                out.delete_signal = Some(u.id);
                                ui.close();
                            }
                        });
                    }
                    if ui.button("+ New signal\u{2026}").clicked() {
                        out.new_signal = true;
                    }
                });

            CollapsingHeader::new(RichText::new("Scripts").strong())
                .icon(crate::icons::disclosure)
                .default_open(true)
                .show(ui, |ui| {
                    let mut any = false;
                    for node in topo.nodes.iter().filter(|n| n.script.is_some()) {
                        any = true;
                        let flow = flow_of_ecu(node.id);
                        let on = flow.is_some() && flow == selected;
                        let label = format!(
                            "{} {}",
                            node.name,
                            crate::script_editor::line_count_label(node.script.as_deref())
                        );
                        ui.horizontal(|ui| {
                            ui.add(icons::icon_image(ui, icons::script()));
                            if ui.selectable_label(on, label).clicked() {
                                picked = flow;
                            }
                        });
                    }
                    if !any {
                        ui.weak("No scripts");
                    }
                });

            CollapsingHeader::new(RichText::new("Tests").strong())
                .icon(crate::icons::disclosure)
                .default_open(true)
                .show(ui, |ui| {
                    if graph.tests.is_empty() {
                        ui.weak("No test modules");
                    }
                    for (i, path) in graph.tests.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(icons::icon_image(ui, icons::script()));
                            let resp = ui
                                .selectable_label(false, operow_test::module_name(path))
                                .on_hover_text(format!("{path}\nDouble-click to edit"));
                            if resp.double_clicked() {
                                out.open_test_module = Some(path.clone());
                            }
                            resp.context_menu(|ui| {
                                if ui
                                    .add_enabled(!tests_busy, egui::Button::new("Run"))
                                    .clicked()
                                {
                                    out.run_test_module = Some(path.clone());
                                    ui.close();
                                }
                                if ui.button("Open in editor").clicked() {
                                    out.open_test_module = Some(path.clone());
                                    ui.close();
                                }
                                if ui.button("Remove from project").clicked() {
                                    out.remove_test_module = Some(i);
                                    ui.close();
                                }
                            });
                        });
                    }
                    if ui.button("+ New test module\u{2026}").clicked() {
                        out.new_test_module = true;
                    }
                });
        });
    out.picked = picked;
    out
}

fn user_signal_label(u: &UserSignalDef) -> String {
    if u.unit.is_empty() {
        u.name.clone()
    } else {
        format!("{}  [{}]", u.name, u.unit)
    }
}

fn user_signal_tooltip(u: &UserSignalDef, topo: &operow_core::Topology) -> String {
    let bus = topo
        .buses
        .iter()
        .find(|b| b.id == u.bus)
        .map_or("?", |b| b.name.as_str());
    let order = match u.byte_order {
        operow_core::SignalByteOrder::Intel => 1,
        operow_core::SignalByteOrder::Motorola => 0,
    };
    format!(
        "{bus} 0x{:X}{}  {}|{}@{order}{}\nfactor {} offset {}",
        u.msg_id,
        if u.extended { "x" } else { "" },
        u.start_bit,
        u.size,
        if u.signed { "-" } else { "+" },
        u.factor,
        u.offset
    )
}
