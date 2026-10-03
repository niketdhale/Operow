//! Run-time controls: node online/offline, per-message controls, the
//! fault-injection rule list and the node state badges.
//!
//! Everything here is runtime-only: nothing is saved in the project file
//! and the engine forgets it on Stop. The app mirrors what it has sent so
//! the windows can show it (the engine has no "list" API); the node and
//! message controls are reset whenever the simulation is loaded again, the
//! fault rules and the seed are re-applied at every start.

use std::collections::{HashMap, HashSet};

use operow_core::{BusId, CanErrorKind, EcuConfig, Link, NodeErrorState, NodeId};
use operow_engine::{Command, InjectMode, InjectSpec, MAX_MSG_DELAY_MS, MsgControl, NodeErrorInfo};

use crate::trace::NameLookup;

const RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x30, 0x30);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x90, 0x10);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x30, 0xa0, 0x50);
const GREY: egui::Color32 = egui::Color32::from_rgb(0x90, 0x90, 0x98);

/// What the app has told the engine about nodes and messages.
#[derive(Default)]
pub struct RuntimeState {
    offline: HashSet<(NodeId, BusId)>,
    msg: HashMap<(NodeId, u32, bool), MsgControl>,
    /// Bus-off entry count last seen per (node, bus) and when it grew.
    bus_off_seen: HashMap<(NodeId, BusId), (u32, std::time::Instant)>,
}

/// How long a badge keeps showing BUS-OFF after a bus-off event, so short
/// automatic bus-offs stay visible.
pub const BUS_OFF_HOLD: std::time::Duration = std::time::Duration::from_millis(1500);

impl RuntimeState {
    /// Forget everything (the engine does so on Stop and Load).
    pub fn reset(&mut self) {
        self.offline.clear();
        self.msg.clear();
        self.bus_off_seen.clear();
    }

    /// Note new bus-off events reported in `states`.
    pub fn observe(&mut self, states: &[NodeErrorInfo], now: std::time::Instant) {
        for s in states {
            let e = self.bus_off_seen.entry((s.node, s.bus)).or_insert((0, now));
            if s.bus_off_events > e.0 {
                *e = (s.bus_off_events, now);
            }
        }
    }

    /// `states` with every entry that went bus-off less than
    /// [`BUS_OFF_HOLD`] before `now` shown as bus-off.
    pub fn held_states(
        &self,
        states: &[NodeErrorInfo],
        now: std::time::Instant,
    ) -> Vec<NodeErrorInfo> {
        states
            .iter()
            .map(|s| {
                let mut s = *s;
                let recent = s.bus_off_events > 0
                    && self
                        .bus_off_seen
                        .get(&(s.node, s.bus))
                        .is_some_and(|(n, t)| {
                            *n == s.bus_off_events && now.duration_since(*t) < BUS_OFF_HOLD
                        });
                if recent {
                    s.state = NodeErrorState::BusOff;
                }
                s
            })
            .collect()
    }

    pub fn is_offline(&self, node: NodeId, bus: BusId) -> bool {
        self.offline.contains(&(node, bus))
    }

    pub fn offline_set(&self) -> &HashSet<(NodeId, BusId)> {
        &self.offline
    }

    /// Record and return the command for taking `node` online or offline on
    /// `bus` (`None` = every bus in `linked`).
    pub fn set_online(
        &mut self,
        node: NodeId,
        bus: Option<BusId>,
        linked: &[BusId],
        online: bool,
    ) -> Command {
        for b in linked.iter().filter(|b| bus.is_none_or(|x| x == **b)) {
            if online {
                self.offline.remove(&(node, *b));
            } else {
                self.offline.insert((node, *b));
            }
        }
        Command::SetNodeOnline { node, bus, online }
    }

    /// The control currently set for a message.
    pub fn msg_control(&self, node: NodeId, id: (u32, bool)) -> MsgControl {
        self.msg
            .get(&(node, id.0, id.1))
            .copied()
            .unwrap_or_default()
    }

    /// Record (clamped) and return the command setting a message control.
    pub fn set_msg_control(&mut self, node: NodeId, id: (u32, bool), c: MsgControl) -> Command {
        let control = c.sanitized();
        if control.is_noop() {
            self.msg.remove(&(node, id.0, id.1));
        } else {
            self.msg.insert((node, id.0, id.1), control);
        }
        Command::SetMsgControl { node, id, control }
    }
}

// --- node badges ------------------------------------------------------------

/// Worst condition of a node, for the badge on the canvas. Ordered from
/// best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeBadge {
    Active,
    Passive,
    BusOff,
    Offline,
}

impl NodeBadge {
    pub fn label(self) -> &'static str {
        match self {
            NodeBadge::Active => "ACTIVE",
            NodeBadge::Passive => "PASSIVE",
            NodeBadge::BusOff => "BUS-OFF",
            NodeBadge::Offline => "OFFLINE",
        }
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            NodeBadge::Active => GREEN,
            NodeBadge::Passive => AMBER,
            NodeBadge::BusOff => RED,
            NodeBadge::Offline => GREY,
        }
    }
}

/// Worst state of `node` across `buses`: offline when it is offline on all
/// of them, otherwise the worst fault-confinement state of the buses where
/// it is online. `None` for a node without buses.
pub fn node_badge(
    node: NodeId,
    buses: &[BusId],
    states: &[NodeErrorInfo],
    offline: &HashSet<(NodeId, BusId)>,
) -> Option<NodeBadge> {
    if buses.is_empty() {
        return None;
    }
    if buses.iter().all(|b| offline.contains(&(node, *b))) {
        return Some(NodeBadge::Offline);
    }
    let worst = states
        .iter()
        .filter(|s| s.node == node && buses.contains(&s.bus) && !offline.contains(&(node, s.bus)))
        .map(|s| match s.state {
            NodeErrorState::ErrorActive => NodeBadge::Active,
            NodeErrorState::ErrorPassive => NodeBadge::Passive,
            NodeErrorState::BusOff => NodeBadge::BusOff,
        })
        .max();
    Some(worst.unwrap_or(NodeBadge::Active))
}

/// Badge of every node that has a link.
pub fn badges(
    links: &[Link],
    states: &[NodeErrorInfo],
    offline: &HashSet<(NodeId, BusId)>,
) -> HashMap<NodeId, NodeBadge> {
    let mut by_node: HashMap<NodeId, Vec<BusId>> = HashMap::new();
    for l in links {
        by_node.entry(l.node).or_default().push(l.bus);
    }
    by_node
        .into_iter()
        .filter_map(|(n, buses)| node_badge(n, &buses, states, offline).map(|b| (n, b)))
        .collect()
}

/// Buses `node` is linked to.
pub fn buses_of(links: &[Link], node: NodeId) -> Vec<BusId> {
    links
        .iter()
        .filter(|l| l.node == node)
        .map(|l| l.bus)
        .collect()
}

fn state_color(s: NodeErrorState) -> egui::Color32 {
    match s {
        NodeErrorState::ErrorActive => GREEN,
        NodeErrorState::ErrorPassive => AMBER,
        NodeErrorState::BusOff => RED,
    }
}

// --- fault-injection rules --------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode {
    Count,
    EveryNth,
    Probability,
}

impl RuleMode {
    pub const ALL: [RuleMode; 3] = [RuleMode::Count, RuleMode::EveryNth, RuleMode::Probability];

    pub fn label(self) -> &'static str {
        match self {
            RuleMode::Count => "Count n",
            RuleMode::EveryNth => "Every Nth",
            RuleMode::Probability => "Probability %",
        }
    }
}

/// One row of the Faults window.
#[derive(Debug, Clone, PartialEq)]
pub struct FaultRule {
    pub bus: Option<BusId>,
    /// `None` = any transmitter.
    pub node: Option<NodeId>,
    /// Hex identifier; empty = any.
    pub id_text: String,
    pub extended: bool,
    pub kind: CanErrorKind,
    pub mode: RuleMode,
    /// `n` of `Count` and `Every Nth`.
    pub n: u32,
    pub pct: f32,
}

impl Default for FaultRule {
    fn default() -> Self {
        FaultRule {
            bus: None,
            node: None,
            id_text: String::new(),
            extended: false,
            kind: CanErrorKind::Crc,
            mode: RuleMode::Count,
            n: 10,
            pct: 10.0,
        }
    }
}

impl FaultRule {
    /// The engine rule, or why the row is invalid.
    pub fn to_spec(&self) -> Result<InjectSpec, String> {
        let bus = self.bus.ok_or("choose a bus")?;
        let id = match crate::inspector::parse_optional_hex(&self.id_text) {
            None => return Err("invalid ID (hex)".into()),
            Some(None) => None,
            Some(Some(id)) if id > 0x1FFF_FFFF => return Err("ID above 0x1FFFFFFF".into()),
            Some(Some(id)) => Some((id, self.extended || id > 0x7FF)),
        };
        let mode = match self.mode {
            RuleMode::Count if self.n == 0 => return Err("count must be at least 1".into()),
            RuleMode::EveryNth if self.n == 0 => return Err("N must be at least 1".into()),
            RuleMode::Count => InjectMode::Count(self.n),
            RuleMode::EveryNth => InjectMode::EveryNth(self.n),
            RuleMode::Probability => {
                if !(self.pct > 0.0 && self.pct <= 100.0) {
                    return Err("probability must be in (0, 100]".into());
                }
                InjectMode::Probability(f64::from(self.pct) / 100.0)
            }
        };
        Ok(InjectSpec {
            bus,
            node: self.node,
            id,
            kind: self.kind,
            mode,
            remaining: None,
        })
    }

    fn mode_text(&self) -> String {
        match self.mode {
            RuleMode::Count => format!("Count {}", self.n),
            RuleMode::EveryNth => format!("Every {}th", self.n),
            RuleMode::Probability => format!("{}% prob.", self.pct),
        }
    }
}

/// The valid rules as engine specs, in order.
pub fn rules_to_specs(rules: &[FaultRule]) -> Vec<InjectSpec> {
    rules.iter().filter_map(|r| r.to_spec().ok()).collect()
}

/// State of the Faults window.
pub struct FaultsState {
    pub rules: Vec<FaultRule>,
    draft: FaultRule,
    pub seed_text: String,
    error: Option<String>,
}

impl Default for FaultsState {
    fn default() -> Self {
        FaultsState {
            rules: Vec::new(),
            draft: FaultRule::default(),
            seed_text: "1".into(),
            error: None,
        }
    }
}

impl FaultsState {
    /// Commands that make a freshly loaded engine match this list: seed,
    /// then every valid rule. With `reset` the old rules are cleared first.
    pub fn sync_commands(&self, reset: bool) -> Vec<Command> {
        let mut cmds = Vec::new();
        if reset {
            cmds.push(Command::ClearInjections);
        }
        if let Ok(seed) = self.seed_text.trim().parse::<u64>() {
            cmds.push(Command::SetSeed(seed));
        }
        cmds.extend(
            rules_to_specs(&self.rules)
                .into_iter()
                .map(Command::InjectErrors),
        );
        cmds
    }

    /// Draw the window. `buses` and `nodes` are the choices; returns the
    /// engine commands to send (empty unless `running`).
    pub fn ui(&mut self, ui: &mut egui::Ui, names: &NameLookup, running: bool) -> Vec<Command> {
        let mut buses: Vec<(BusId, String)> = names
            .bus_names
            .iter()
            .map(|(b, n)| (*b, n.clone()))
            .collect();
        buses.sort_by(|a, b| a.1.cmp(&b.1));
        let mut nodes: Vec<(NodeId, String)> = names
            .node_names
            .iter()
            .map(|(n, s)| (*n, s.clone()))
            .collect();
        nodes.sort_by(|a, b| a.1.cmp(&b.1));
        if self
            .draft
            .bus
            .is_none_or(|b| !buses.iter().any(|x| x.0 == b))
        {
            self.draft.bus = buses.first().map(|b| b.0);
        }

        let mut changed = false;
        let mut remove = None;
        let mut clear = false;
        let mut add = false;
        let mut seed_set = false;

        ui.horizontal(|ui| {
            ui.label("Seed:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.seed_text).desired_width(90.0));
            let valid = self.seed_text.trim().parse::<u64>().is_ok();
            seed_set |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid;
            seed_set |= ui.add_enabled(valid, egui::Button::new("Set")).clicked();
            if !valid {
                ui.colored_label(RED, "not a number");
            }
            ui.separator();
            if ui
                .add_enabled(!self.rules.is_empty(), egui::Button::new("Clear all"))
                .clicked()
            {
                clear = true;
            }
            if !running {
                ui.weak("Rules are applied when the measurement starts.");
            }
        });
        ui.add_space(4.0);

        egui::ScrollArea::both()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Grid::new("fault_rules_grid")
                    .num_columns(6)
                    .spacing([12.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["Bus", "Transmitter", "ID", "Error", "Mode", ""] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for (i, r) in self.rules.iter().enumerate() {
                            ui.label(
                                r.bus
                                    .map(|b| names.bus_name(b))
                                    .unwrap_or_else(|| "?".into()),
                            );
                            ui.label(r.node.map_or("any".into(), |n| names.node_name(n)));
                            ui.label(if r.id_text.trim().is_empty() {
                                "any".to_string()
                            } else {
                                format!("0x{}", r.id_text.trim().trim_start_matches("0x"))
                            });
                            ui.label(r.kind.label());
                            ui.label(r.mode_text());
                            if ui.button("Remove").clicked() {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                        if self.rules.is_empty() {
                            ui.weak("none");
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.separator();
                ui.strong("New rule");
                let d = &mut self.draft;
                egui::Grid::new("fault_draft_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Bus");
                        egui::ComboBox::from_id_salt("fault_bus")
                            .selected_text(d.bus.map(|b| names.bus_name(b)).unwrap_or_default())
                            .show_ui(ui, |ui| {
                                for (id, name) in &buses {
                                    ui.selectable_value(&mut d.bus, Some(*id), name);
                                }
                            });
                        ui.end_row();
                        ui.label("Transmitter");
                        egui::ComboBox::from_id_salt("fault_node")
                            .selected_text(d.node.map_or("Any node".into(), |n| names.node_name(n)))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut d.node, None, "Any node");
                                for (id, name) in &nodes {
                                    ui.selectable_value(&mut d.node, Some(*id), name);
                                }
                            });
                        ui.end_row();
                        ui.label("ID (hex)");
                        ui.horizontal(|ui| {
                            let bad = d.to_spec().is_err() && d.bus.is_some() && {
                                crate::inspector::parse_optional_hex(&d.id_text).is_none()
                            };
                            let mut te = egui::TextEdit::singleline(&mut d.id_text)
                                .desired_width(90.0)
                                .hint_text("any");
                            if bad {
                                te = te.text_color(RED);
                            }
                            ui.add(te);
                            ui.checkbox(&mut d.extended, "Extended");
                        });
                        ui.end_row();
                        ui.label("Error");
                        egui::ComboBox::from_id_salt("fault_kind")
                            .selected_text(d.kind.label())
                            .show_ui(ui, |ui| {
                                for k in CanErrorKind::ALL {
                                    ui.selectable_value(&mut d.kind, k, k.label());
                                }
                            });
                        ui.end_row();
                        ui.label("Mode");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("fault_mode")
                                .selected_text(d.mode.label())
                                .show_ui(ui, |ui| {
                                    for m in RuleMode::ALL {
                                        ui.selectable_value(&mut d.mode, m, m.label());
                                    }
                                });
                            match d.mode {
                                RuleMode::Count | RuleMode::EveryNth => {
                                    ui.add(egui::DragValue::new(&mut d.n).range(1..=1_000_000));
                                }
                                RuleMode::Probability => {
                                    ui.add(
                                        egui::DragValue::new(&mut d.pct)
                                            .range(0.1..=100.0)
                                            .suffix(" %"),
                                    );
                                }
                            }
                        });
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        add = true;
                    }
                    if let Some(e) = &self.error {
                        ui.colored_label(RED, e);
                    }
                });
            });

        if add {
            match self.draft.to_spec() {
                Ok(_) => {
                    self.rules.push(self.draft.clone());
                    self.error = None;
                    changed = true;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(i) = remove {
            self.rules.remove(i);
            changed = true;
        }
        if clear {
            self.rules.clear();
            changed = true;
        }
        if !running {
            return Vec::new();
        }
        if changed {
            // The engine cannot list or remove single rules: start over.
            return self.sync_commands(true);
        }
        if seed_set && let Ok(seed) = self.seed_text.trim().parse::<u64>() {
            return vec![Command::SetSeed(seed)];
        }
        Vec::new()
    }
}

// --- Properties "Runtime" section -----------------------------------------------

/// Live controls of the selected node: online per bus, force bus-off, state
/// and per-message controls. Returns the engine commands to send.
pub fn runtime_section(
    ui: &mut egui::Ui,
    ecu: &EcuConfig,
    links: &[Link],
    rt: &mut RuntimeState,
    states: &[NodeErrorInfo],
    names: &NameLookup,
) -> Vec<Command> {
    let mut cmds = Vec::new();
    let buses = buses_of(links, ecu.id);
    ui.separator();
    ui.strong("Runtime");
    ui.weak("Live controls; not saved in the project, reset on Stop.");
    let all_online = buses.iter().all(|b| !rt.is_offline(ecu.id, *b));
    let mut all = all_online;
    if ui
        .add_enabled(
            !buses.is_empty(),
            egui::Checkbox::new(&mut all, "Online (all buses)"),
        )
        .changed()
    {
        cmds.push(rt.set_online(ecu.id, None, &buses, all));
    }
    egui::Grid::new(("runtime_buses", ecu.id))
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for bus in &buses {
                ui.label(names.bus_name(*bus));
                let mut on = !rt.is_offline(ecu.id, *bus);
                if ui.checkbox(&mut on, "Online").changed() {
                    cmds.push(rt.set_online(ecu.id, Some(*bus), &buses, on));
                }
                if ui
                    .button("Force bus-off")
                    .on_hover_text("Put the node bus-off on this bus now; it recovers by itself")
                    .clicked()
                {
                    cmds.push(Command::ForceBusOff(ecu.id, *bus));
                }
                let info = states.iter().find(|s| s.node == ecu.id && s.bus == *bus);
                ui.end_row();
                ui.label("");
                match info {
                    Some(s) => {
                        let off = if s.bus_off_events > 0 {
                            format!(" \u{b7} Bus-off \u{d7}{}", s.bus_off_events)
                        } else {
                            String::new()
                        };
                        ui.colored_label(
                            state_color(s.state),
                            format!(
                                "{} \u{b7} TEC {} \u{b7} REC {}{off}",
                                s.state.label(),
                                s.tec,
                                s.rec
                            ),
                        );
                    }
                    None => {
                        ui.weak("-");
                    }
                }
                if info.is_some_and(|s| s.state == NodeErrorState::BusOff)
                    && ui
                        .button("Recover")
                        .on_hover_text("Bring the node back error active now")
                        .clicked()
                {
                    cmds.push(Command::RecoverBusOff {
                        node: ecu.id,
                        bus: *bus,
                    });
                }
                ui.end_row();
            }
        });

    if !ecu.tx.is_empty() {
        ui.add_space(4.0);
        ui.label("Message controls (by ID):");
        egui::Grid::new(("runtime_msgs", ecu.id))
            .num_columns(5)
            .spacing([10.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                for h in ["Message", "Pause", "Drop %", "Delay ms", "Jitter ms"] {
                    ui.strong(h);
                }
                ui.end_row();
                for msg in &ecu.tx {
                    let id = (msg.frame.id, msg.frame.extended);
                    let before = rt.msg_control(ecu.id, id);
                    let mut c = before;
                    ui.label(format!("{} (0x{:X})", msg.name, msg.frame.id));
                    ui.checkbox(&mut c.paused, "");
                    ui.add(
                        egui::DragValue::new(&mut c.drop_pct)
                            .range(0.0..=100.0)
                            .speed(0.5),
                    );
                    ui.add(
                        egui::DragValue::new(&mut c.delay_ms)
                            .range(0.0..=MAX_MSG_DELAY_MS)
                            .speed(0.5),
                    );
                    ui.add(
                        egui::DragValue::new(&mut c.jitter_ms)
                            .range(0.0..=MAX_MSG_DELAY_MS)
                            .speed(0.5),
                    );
                    ui.end_row();
                    if c != before {
                        cmds.push(rt.set_msg_control(ecu.id, id, c));
                    }
                }
            });
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    const N1: NodeId = NodeId(1);
    const N2: NodeId = NodeId(2);
    const B1: BusId = BusId(1);
    const B2: BusId = BusId(2);

    fn st(node: NodeId, bus: BusId, state: NodeErrorState) -> NodeErrorInfo {
        NodeErrorInfo {
            node,
            bus,
            state,
            tec: 0,
            rec: 0,
            bus_off_events: 0,
            last_bus_off_ns: 0,
        }
    }

    #[test]
    fn rule_maps_to_inject_spec() {
        let r = FaultRule {
            bus: Some(B1),
            node: Some(N1),
            id_text: "100".into(),
            kind: CanErrorKind::Crc,
            mode: RuleMode::Probability,
            pct: 10.0,
            ..Default::default()
        };
        let s = r.to_spec().unwrap();
        assert_eq!(s.bus, B1);
        assert_eq!(s.node, Some(N1));
        assert_eq!(s.id, Some((0x100, false)));
        assert_eq!(s.kind, CanErrorKind::Crc);
        assert_eq!(s.remaining, None);
        match s.mode {
            InjectMode::Probability(p) => assert!((p - 0.1).abs() < 1e-9),
            m => panic!("{m:?}"),
        }
        let any = FaultRule {
            bus: Some(B2),
            mode: RuleMode::EveryNth,
            n: 3,
            ..Default::default()
        };
        let s = any.to_spec().unwrap();
        assert_eq!(
            (s.node, s.id, s.mode),
            (None, None, InjectMode::EveryNth(3))
        );
        // Large ids are extended.
        let ext = FaultRule {
            bus: Some(B1),
            id_text: "0x1ABCDEF".into(),
            ..Default::default()
        };
        assert_eq!(ext.to_spec().unwrap().id, Some((0x1AB_CDEF, true)));
    }

    #[test]
    fn invalid_rules_are_rejected_and_skipped() {
        let ok = FaultRule {
            bus: Some(B1),
            ..Default::default()
        };
        let no_bus = FaultRule::default();
        let bad_id = FaultRule {
            id_text: "zz".into(),
            ..ok.clone()
        };
        let too_big = FaultRule {
            id_text: "20000000".into(),
            ..ok.clone()
        };
        let zero = FaultRule { n: 0, ..ok.clone() };
        let p0 = FaultRule {
            mode: RuleMode::Probability,
            pct: 0.0,
            ..ok.clone()
        };
        let p101 = FaultRule {
            mode: RuleMode::Probability,
            pct: 101.0,
            ..ok.clone()
        };
        for r in [&no_bus, &bad_id, &too_big, &zero, &p0, &p101] {
            assert!(r.to_spec().is_err(), "{r:?}");
        }
        let all = [ok.clone(), no_bus, bad_id, ok];
        assert_eq!(rules_to_specs(&all).len(), 2);
    }

    #[test]
    fn sync_commands_order() {
        let mut f = FaultsState {
            seed_text: "42".into(),
            ..Default::default()
        };
        f.rules.push(FaultRule {
            bus: Some(B1),
            ..Default::default()
        });
        let c = f.sync_commands(true);
        assert!(matches!(c[0], Command::ClearInjections));
        assert!(matches!(c[1], Command::SetSeed(42)));
        assert!(matches!(c[2], Command::InjectErrors(_)));
        assert_eq!(f.sync_commands(false).len(), 2);
    }

    #[test]
    fn badge_is_worst_state_across_buses() {
        let off = HashSet::new();
        let states = [
            st(N1, B1, NodeErrorState::ErrorActive),
            st(N1, B2, NodeErrorState::ErrorPassive),
            st(N2, B1, NodeErrorState::ErrorPassive),
            st(N2, B2, NodeErrorState::BusOff),
        ];
        assert_eq!(
            node_badge(N1, &[B1, B2], &states, &off),
            Some(NodeBadge::Passive)
        );
        assert_eq!(
            node_badge(N2, &[B1, B2], &states, &off),
            Some(NodeBadge::BusOff)
        );
        assert_eq!(node_badge(N1, &[], &states, &off), None);
        // No state yet: active.
        assert_eq!(node_badge(N1, &[B1], &[], &off), Some(NodeBadge::Active));
        assert!(NodeBadge::Offline > NodeBadge::BusOff && NodeBadge::BusOff > NodeBadge::Passive);
    }

    #[test]
    fn badge_offline_needs_every_bus_offline() {
        let mut rt = RuntimeState::default();
        let states = [
            st(N2, B1, NodeErrorState::ErrorPassive),
            st(N2, B2, NodeErrorState::BusOff),
        ];
        rt.set_online(N2, Some(B2), &[B1, B2], false);
        // Offline on B2 only: judged by B1.
        assert_eq!(
            node_badge(N2, &[B1, B2], &states, rt.offline_set()),
            Some(NodeBadge::Passive)
        );
        rt.set_online(N2, None, &[B1, B2], false);
        assert_eq!(
            node_badge(N2, &[B1, B2], &states, rt.offline_set()),
            Some(NodeBadge::Offline)
        );
        let links = [Link { node: N2, bus: B1 }, Link { node: N2, bus: B2 }];
        assert_eq!(
            badges(&links, &states, rt.offline_set())[&N2],
            NodeBadge::Offline
        );
        rt.set_online(N2, None, &[B1, B2], true);
        assert!(rt.offline_set().is_empty());
    }

    #[test]
    fn bus_off_is_held_for_a_while_after_an_event() {
        let mut rt = RuntimeState::default();
        let t0 = std::time::Instant::now();
        let mut s = st(N1, B1, NodeErrorState::ErrorActive);
        assert_eq!(
            rt.held_states(&[s], t0)[0].state,
            NodeErrorState::ErrorActive
        );
        s.bus_off_events = 1;
        rt.observe(&[s], t0);
        let soon = t0 + std::time::Duration::from_millis(1000);
        let later = t0 + BUS_OFF_HOLD + std::time::Duration::from_millis(1);
        assert_eq!(rt.held_states(&[s], soon)[0].state, NodeErrorState::BusOff);
        assert_eq!(
            rt.held_states(&[s], later)[0].state,
            NodeErrorState::ErrorActive
        );
        // A new event restarts the hold.
        s.bus_off_events = 2;
        rt.observe(&[s], later);
        assert_eq!(rt.held_states(&[s], later)[0].state, NodeErrorState::BusOff);
        rt.reset();
        assert_eq!(
            rt.held_states(&[s], later)[0].state,
            NodeErrorState::ErrorActive
        );
    }

    #[test]
    fn msg_control_is_clamped_and_noop_removed() {
        let mut rt = RuntimeState::default();
        let c = MsgControl {
            paused: false,
            drop_pct: 500.0,
            delay_ms: -3.0,
            jitter_ms: f32::NAN,
        };
        let Command::SetMsgControl { control, .. } = rt.set_msg_control(N1, (0x100, false), c)
        else {
            panic!("wrong command");
        };
        assert_eq!(control.drop_pct, 100.0);
        assert_eq!(control.delay_ms, 0.0);
        assert_eq!(control.jitter_ms, 0.0);
        assert_eq!(rt.msg_control(N1, (0x100, false)).drop_pct, 100.0);
        rt.set_msg_control(N1, (0x100, false), MsgControl::default());
        assert!(rt.msg_control(N1, (0x100, false)).is_noop());
        rt.set_msg_control(
            N1,
            (0x100, false),
            MsgControl {
                paused: true,
                ..Default::default()
            },
        );
        rt.reset();
        assert!(rt.msg_control(N1, (0x100, false)).is_noop());
    }
}
