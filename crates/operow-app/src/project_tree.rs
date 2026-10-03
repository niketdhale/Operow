//! The project tree panel: networks, databases and scripts.

use egui::{CollapsingHeader, RichText};
use egui_flow::NodeId as FlowId;
use operow_core::NodeKind;

use crate::dbcs::DbcStore;
use crate::graph::{Graph, GraphNode};
use crate::icons;

/// Draws the tree; returns the canvas node the user clicked, if any.
pub fn ui(ui: &mut egui::Ui, graph: &Graph, dbcs: &DbcStore) -> Option<FlowId> {
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
                            .id_salt(("tree_db", i))
                            .show(ui, |ui| {
                                let Some(db) = dbcs.by_bus.get(&d.bus) else {
                                    ui.weak("Not loaded");
                                    return;
                                };
                                for m in &db.messages {
                                    CollapsingHeader::new(format!("0x{:X}  {}", m.id, m.name))
                                        .id_salt(("tree_msg", i, m.id, m.extended))
                                        .show(ui, |ui| {
                                            for s in &m.signals {
                                                let unit = if s.unit.is_empty() {
                                                    String::new()
                                                } else {
                                                    format!("  [{}]", s.unit)
                                                };
                                                ui.label(format!("{}{unit}", s.name));
                                            }
                                        });
                                }
                            });
                    }
                });

            CollapsingHeader::new(RichText::new("Scripts").strong())
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
        });
    picked
}
