//! The bottom trace panel: a virtualized, ring-buffered log of bus events.

use std::collections::{HashMap, VecDeque};

use egui_extras::{Column, TableBuilder};
use operow_core::{BusEvent, BusId, NodeId};

const MAX_ROWS: usize = 100_000;

pub struct TraceRow {
    pub time_s: f64,
    pub bus_name: String,
    pub sender_name: String,
    pub id: u32,
    pub extended: bool,
    pub fd: bool,
    pub brs: bool,
    pub dlc: u8,
    pub data: [u8; 64],
    pub msg_name: String,
}

impl TraceRow {
    pub fn frame_type(&self) -> &'static str {
        match (self.fd, self.brs) {
            (false, _) => "CAN",
            (true, false) => "CAN FD",
            (true, true) => "CAN FD BRS",
        }
    }
}

pub struct Trace {
    rows: VecDeque<TraceRow>,
    pub paused: bool,
    pub filter: String,
    pub autoscroll: bool,
}

impl Default for Trace {
    fn default() -> Self {
        Trace {
            rows: VecDeque::with_capacity(1024),
            paused: false,
            filter: String::new(),
            autoscroll: true,
        }
    }
}

impl Trace {
    pub fn clear(&mut self) {
        self.rows.clear();
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Push one bus event, resolving names via lookup closures. No-op while
    /// paused.
    pub fn push(&mut self, ev: &BusEvent, bus_name: &str, sender_name: &str, msg_name: &str) {
        if self.paused {
            return;
        }
        if self.rows.len() >= MAX_ROWS {
            self.rows.pop_front();
        }
        self.rows.push_back(TraceRow {
            time_s: ev.time.as_secs_f64(),
            bus_name: bus_name.to_string(),
            sender_name: sender_name.to_string(),
            id: ev.frame.id,
            extended: ev.frame.extended,
            fd: ev.frame.fd,
            brs: ev.frame.brs,
            dlc: ev.frame.dlc,
            data: ev.frame.data,
            msg_name: msg_name.to_string(),
        });
    }

    fn matches_filter(&self, row: &TraceRow) -> bool {
        if self.filter.trim().is_empty() {
            return true;
        }
        let f = self
            .filter
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        if let Ok(want) = u32::from_str_radix(f, 16) {
            row.id == want
        } else {
            row.msg_name
                .to_lowercase()
                .contains(&self.filter.to_lowercase())
                || row
                    .sender_name
                    .to_lowercase()
                    .contains(&self.filter.to_lowercase())
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Trace");
            ui.separator();
            ui.checkbox(&mut self.paused, "Pause");
            if ui.button("Clear").clicked() {
                self.clear();
            }
            ui.checkbox(&mut self.autoscroll, "Autoscroll");
            ui.separator();
            ui.label("Filter (ID hex or name):");
            ui.text_edit_singleline(&mut self.filter);
            ui.separator();
            ui.label(format!("{} rows", self.rows.len()));
        });
        ui.separator();

        let text_height = ui.text_style_height(&egui::TextStyle::Body);
        let filtered: Vec<&TraceRow> = self
            .rows
            .iter()
            .filter(|r| self.matches_filter(r))
            .collect();

        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(90.0)) // time
            .column(Column::exact(60.0)) // chn
            .column(Column::exact(70.0)) // id
            .column(Column::remainder().at_least(100.0)) // name
            .column(Column::exact(40.0)) // dir
            .column(Column::exact(80.0)) // type
            .column(Column::exact(40.0)) // dlc
            .column(Column::exact(36.0)) // len
            .column(Column::exact(220.0)) // data
            .column(Column::exact(120.0)); // sender

        if self.autoscroll {
            table = table.scroll_to_row(filtered.len(), Some(egui::Align::BOTTOM));
        }

        table
            .header(20.0, |mut header| {
                for label in [
                    "Time (s)", "Chn", "ID", "Name", "Dir", "Type", "DLC", "Len", "Data", "Sender",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|body| {
                body.rows(text_height, filtered.len(), |mut row| {
                    let r = filtered[row.index()];
                    row.col(|ui| {
                        ui.monospace(format!("{:.6}", r.time_s));
                    });
                    row.col(|ui| {
                        ui.label(&r.bus_name);
                    });
                    row.col(|ui| {
                        ui.monospace(format!("{:03X}{}", r.id, if r.extended { "x" } else { "" }));
                    });
                    row.col(|ui| {
                        ui.label(&r.msg_name);
                    });
                    row.col(|ui| {
                        ui.label("Tx");
                    });
                    row.col(|ui| {
                        ui.label(r.frame_type());
                    });
                    row.col(|ui| {
                        ui.label(
                            operow_core::len_to_dlc(r.dlc as usize)
                                .unwrap_or(0)
                                .to_string(),
                        );
                    });
                    row.col(|ui| {
                        ui.label(r.dlc.to_string());
                    });
                    row.col(|ui| {
                        let full: String = r.data[..r.dlc as usize]
                            .iter()
                            .enumerate()
                            .map(|(i, b)| {
                                if i > 0 {
                                    format!(" {b:02X}")
                                } else {
                                    format!("{b:02X}")
                                }
                            })
                            .collect();
                        const TRUNCATE_AT: usize = 8;
                        let shown = if r.dlc as usize > TRUNCATE_AT {
                            let mut s: String = r.data[..TRUNCATE_AT]
                                .iter()
                                .map(|b| format!("{b:02X} "))
                                .collect();
                            s.push('…');
                            s
                        } else {
                            full.clone()
                        };
                        ui.monospace(shown).on_hover_text(full);
                    });
                    row.col(|ui| {
                        ui.label(&r.sender_name);
                    });
                });
            });
    }
}

/// Helper lookups shared by the trace and inspector: node/bus names, and
/// tx-message names by (sender, id).
#[derive(Default)]
pub struct NameLookup {
    pub node_names: HashMap<NodeId, String>,
    pub bus_names: HashMap<BusId, String>,
    pub msg_names: HashMap<(NodeId, u32), String>,
}

impl NameLookup {
    pub fn rebuild(&mut self, topo: &operow_core::Topology) {
        self.node_names.clear();
        self.bus_names.clear();
        self.msg_names.clear();
        for n in &topo.nodes {
            self.node_names.insert(n.id, n.name.clone());
            for tx in &n.tx {
                self.msg_names.insert((n.id, tx.frame.id), tx.name.clone());
            }
        }
        for b in &topo.buses {
            self.bus_names.insert(b.id, b.name.clone());
        }
    }

    pub fn node_name(&self, id: NodeId) -> String {
        self.node_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Node{}", id.0))
    }

    pub fn bus_name(&self, id: BusId) -> String {
        self.bus_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Bus{}", id.0))
    }

    pub fn msg_name(&self, sender: NodeId, id: u32) -> String {
        self.msg_names
            .get(&(sender, id))
            .cloned()
            .unwrap_or_default()
    }
}
