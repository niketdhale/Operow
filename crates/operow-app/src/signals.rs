//! References to values that can be sampled from bus events: DBC signals,
//! user-defined signals and raw frame properties (bytes, DLC, rate, Δt).

use operow_core::{BusEvent, BusId, SignalByteOrder, UserSignalDef, UserSignalId};
use operow_dbc::{ByteOrder, Mux, SignalDef, ValueType};
use serde::{Deserialize, Serialize};

use crate::dbcs::DbcStore;
use crate::trace::NameLookup;

/// A property of a raw frame, with no DBC involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawKind {
    /// Data byte `n`.
    Byte(u8),
    /// The DLC code.
    Dlc,
    /// Instantaneous frame rate in Hz (needs the previous frame).
    Rate,
    /// Milliseconds since the previous frame (needs the previous frame).
    DeltaT,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalRef {
    Dbc {
        bus: BusId,
        msg_id: u32,
        extended: bool,
        signal_name: String,
    },
    User(UserSignalId),
    Raw {
        bus: BusId,
        id: u32,
        extended: bool,
        kind: RawKind,
    },
}

/// The decoder for a user signal.
pub fn user_signal_def(u: &UserSignalDef) -> SignalDef {
    SignalDef {
        name: u.name.clone(),
        start_bit: u.start_bit,
        size: u.size,
        byte_order: match u.byte_order {
            SignalByteOrder::Intel => ByteOrder::Intel,
            SignalByteOrder::Motorola => ByteOrder::Motorola,
        },
        value_type: if u.signed {
            ValueType::Signed
        } else {
            ValueType::Unsigned
        },
        factor: u.factor,
        offset: u.offset,
        min: 0.0,
        max: 0.0,
        unit: u.unit.clone(),
        receivers: Vec::new(),
        multiplexer: None,
        initial_raw: None,
        value_descriptions: Vec::new(),
        comment: None,
    }
}

impl SignalRef {
    /// The value of this signal in `ev`, or `None` when `ev` is not a frame
    /// that carries it. `Rate` and `DeltaT` need `sample_with_prev`.
    #[cfg(test)]
    pub fn sample(&self, ev: &BusEvent, dbcs: &DbcStore, users: &[UserSignalDef]) -> Option<f64> {
        self.sample_with_prev(ev, None, dbcs, users)
    }

    /// Like [`SignalRef::sample`], with the previous event of the same
    /// bus/ID/format for the timing kinds.
    pub fn sample_with_prev(
        &self,
        ev: &BusEvent,
        prev: Option<&BusEvent>,
        dbcs: &DbcStore,
        users: &[UserSignalDef],
    ) -> Option<f64> {
        if ev.is_error() {
            return None;
        }
        let f = &ev.frame;
        let data = f.payload();
        match self {
            SignalRef::Dbc {
                bus,
                msg_id,
                extended,
                signal_name,
            } => {
                if ev.bus != *bus || f.id != *msg_id || f.extended != *extended {
                    return None;
                }
                let msg = dbcs.by_bus.get(bus)?.message(*msg_id, *extended)?;
                let sig = msg.signals.iter().find(|s| s.name == *signal_name)?;
                if let Some(Mux::Multiplexed(n)) = sig.multiplexer {
                    let sel = msg
                        .signals
                        .iter()
                        .find(|s| s.multiplexer == Some(Mux::Multiplexor))?;
                    if sel.decode_raw(data) != n {
                        return None;
                    }
                }
                Some(sig.decode(data))
            }
            SignalRef::User(id) => {
                let u = users.iter().find(|u| u.id == *id)?;
                if ev.bus != u.bus || f.id != u.msg_id || f.extended != u.extended {
                    return None;
                }
                Some(user_signal_def(u).decode(data))
            }
            SignalRef::Raw {
                bus,
                id,
                extended,
                kind,
            } => {
                if ev.bus != *bus || f.id != *id || f.extended != *extended {
                    return None;
                }
                match kind {
                    RawKind::Byte(n) => data.get(*n as usize).map(|b| *b as f64),
                    RawKind::Dlc => Some(f.dlc_code() as f64),
                    RawKind::DeltaT | RawKind::Rate => {
                        let p = prev?;
                        let dt_ms = ev.time.0.checked_sub(p.time.0)? as f64 / 1e6;
                        if *kind == RawKind::DeltaT {
                            Some(dt_ms)
                        } else if dt_ms > 0.0 {
                            Some(1000.0 / dt_ms)
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }

    /// Where the signal comes from, as shown in graph signal lists.
    pub fn source(&self) -> &'static str {
        match self {
            SignalRef::Dbc { .. } => "DBC",
            SignalRef::User(_) => "User",
            SignalRef::Raw { .. } => "Raw",
        }
    }

    /// The decoder definition for DBC and user signals (`None` for raw
    /// kinds and unknown signals).
    pub fn signal_def(&self, dbcs: &DbcStore, users: &[UserSignalDef]) -> Option<SignalDef> {
        match self {
            SignalRef::Dbc {
                bus,
                msg_id,
                extended,
                signal_name,
            } => dbcs
                .by_bus
                .get(bus)?
                .message(*msg_id, *extended)?
                .signals
                .iter()
                .find(|s| s.name == *signal_name)
                .cloned(),
            SignalRef::User(id) => users.iter().find(|u| u.id == *id).map(user_signal_def),
            SignalRef::Raw { .. } => None,
        }
    }

    /// Human-readable name, e.g. `CAN1 · EngineSpeed` or `Powertrain 0x100 Byte 2`.
    pub fn label(&self, names: &NameLookup, users: &[UserSignalDef]) -> String {
        match self {
            SignalRef::Dbc {
                bus, signal_name, ..
            } => format!("{} \u{b7} {signal_name}", names.bus_name(*bus)),
            SignalRef::User(id) => users
                .iter()
                .find(|u| u.id == *id)
                .map_or_else(|| format!("User signal {}", id.0), |u| u.name.clone()),
            SignalRef::Raw {
                bus,
                id,
                extended,
                kind,
            } => {
                let what = match kind {
                    RawKind::Byte(n) => format!("Byte {n}"),
                    RawKind::Dlc => "DLC".to_string(),
                    RawKind::Rate => "Rate".to_string(),
                    RawKind::DeltaT => "\u{394}t".to_string(),
                };
                format!(
                    "{} 0x{id:X}{} {what}",
                    names.bus_name(*bus),
                    if *extended { "x" } else { "" }
                )
            }
        }
    }

    pub fn unit(&self, dbcs: &DbcStore, users: &[UserSignalDef]) -> String {
        match self {
            SignalRef::Dbc {
                bus,
                msg_id,
                extended,
                signal_name,
            } => dbcs
                .by_bus
                .get(bus)
                .and_then(|d| d.message(*msg_id, *extended))
                .and_then(|m| m.signals.iter().find(|s| s.name == *signal_name))
                .map(|s| s.unit.clone())
                .unwrap_or_default(),
            SignalRef::User(id) => users
                .iter()
                .find(|u| u.id == *id)
                .map(|u| u.unit.clone())
                .unwrap_or_default(),
            SignalRef::Raw { kind, .. } => match kind {
                RawKind::Rate => "Hz".into(),
                RawKind::DeltaT => "ms".into(),
                _ => String::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::{CanFrame, Direction, NodeId, Timestamp};
    use operow_dbc::Database;
    use std::sync::Arc;

    fn event(bus: u32, id: u32, t_ms: u64, data: &[u8]) -> BusEvent {
        BusEvent {
            time: Timestamp(t_ms * 1_000_000),
            bus: BusId(bus),
            sender: NodeId(1),
            origin: NodeId(1),
            dir: Direction::Tx,
            frame_uid: 0,
            hop: 0,
            frame: CanFrame::new(id, false, data).unwrap(),
            kind: Default::default(),
        }
    }

    const DBC: &str = r#"VERSION ""
NS_ :
BS_:
BU_: ECU
BO_ 256 Msg: 8 ECU
 SG_ Speed : 8|16@1+ (0.5,10) [0|0] "km/h" Vector__XXX
 SG_ Sel M : 0|8@1+ (1,0) [0|0] "" Vector__XXX
 SG_ A m1 : 24|8@1+ (1,0) [0|0] "V" Vector__XXX
"#;

    fn dbcs() -> DbcStore {
        let mut s = DbcStore::default();
        s.by_bus
            .insert(BusId(1), Arc::new(Database::parse(DBC).unwrap()));
        s
    }

    fn dbc_ref(name: &str) -> SignalRef {
        SignalRef::Dbc {
            bus: BusId(1),
            msg_id: 256,
            extended: false,
            signal_name: name.into(),
        }
    }

    #[test]
    fn dbc_signal_decodes_with_scaling_and_mux() {
        let d = dbcs();
        // Speed raw = 0x0102 = 258 -> 258 * 0.5 + 10.
        let ev = event(1, 256, 0, &[1, 0x02, 0x01, 7, 0, 0, 0, 0]);
        let v = dbc_ref("Speed").sample(&ev, &d, &[]).unwrap();
        assert!((v - 139.0).abs() < 1e-9);
        // Multiplexed signal only while the selector matches.
        assert_eq!(dbc_ref("A").sample(&ev, &d, &[]), Some(7.0));
        let other = event(1, 256, 0, &[2, 0, 0, 7, 0, 0, 0, 0]);
        assert_eq!(dbc_ref("A").sample(&other, &d, &[]), None);
        // Wrong bus / id / unknown name.
        assert_eq!(
            dbc_ref("Speed").sample(&event(2, 256, 0, &[0; 8]), &d, &[]),
            None
        );
        assert_eq!(
            dbc_ref("Speed").sample(&event(1, 257, 0, &[0; 8]), &d, &[]),
            None
        );
        assert_eq!(dbc_ref("Nope").sample(&ev, &d, &[]), None);
        assert_eq!(dbc_ref("Speed").unit(&d, &[]), "km/h");
    }

    fn user() -> UserSignalDef {
        UserSignalDef {
            id: UserSignalId(1),
            name: "Temp".into(),
            bus: BusId(1),
            msg_id: 0x200,
            extended: false,
            start_bit: 0,
            size: 8,
            byte_order: SignalByteOrder::Intel,
            signed: true,
            factor: 0.5,
            offset: 1.0,
            unit: "C".into(),
        }
    }

    #[test]
    fn user_signal_decodes_signed_and_motorola() {
        let u = user();
        let r = SignalRef::User(UserSignalId(1));
        // 0xFE = -2 -> -2 * 0.5 + 1 = 0.
        let ev = event(1, 0x200, 0, &[0xFE, 0]);
        assert_eq!(
            r.sample(&ev, &DbcStore::default(), std::slice::from_ref(&u)),
            Some(0.0)
        );
        assert_eq!(r.unit(&DbcStore::default(), std::slice::from_ref(&u)), "C");
        // Not our message, or unknown id.
        assert_eq!(
            r.sample(
                &event(1, 0x201, 0, &[0]),
                &DbcStore::default(),
                std::slice::from_ref(&u)
            ),
            None
        );
        assert_eq!(r.sample(&ev, &DbcStore::default(), &[]), None);
        // Motorola 16 bit unsigned starting at MSB bit 7.
        let m = UserSignalDef {
            start_bit: 7,
            size: 16,
            byte_order: SignalByteOrder::Motorola,
            signed: false,
            factor: 1.0,
            offset: 0.0,
            ..u
        };
        let ev = event(1, 0x200, 0, &[0x12, 0x34]);
        assert_eq!(
            r.sample(&ev, &DbcStore::default(), &[m]),
            Some(0x1234 as f64)
        );
    }

    fn raw(kind: RawKind) -> SignalRef {
        SignalRef::Raw {
            bus: BusId(1),
            id: 0x300,
            extended: false,
            kind,
        }
    }

    #[test]
    fn raw_byte_and_dlc() {
        let d = DbcStore::default();
        let ev = event(1, 0x300, 0, &[9, 8, 7]);
        assert_eq!(raw(RawKind::Byte(1)).sample(&ev, &d, &[]), Some(8.0));
        assert_eq!(raw(RawKind::Byte(3)).sample(&ev, &d, &[]), None);
        assert_eq!(raw(RawKind::Dlc).sample(&ev, &d, &[]), Some(3.0));
        assert_eq!(
            raw(RawKind::Dlc).sample(&event(2, 0x300, 0, &[1]), &d, &[]),
            None
        );
    }

    #[test]
    fn raw_rate_and_delta_t_need_previous_frame() {
        let d = DbcStore::default();
        let a = event(1, 0x300, 100, &[0]);
        let b = event(1, 0x300, 110, &[0]);
        assert_eq!(raw(RawKind::DeltaT).sample(&b, &d, &[]), None);
        let dt = raw(RawKind::DeltaT)
            .sample_with_prev(&b, Some(&a), &d, &[])
            .unwrap();
        assert!((dt - 10.0).abs() < 1e-9);
        let hz = raw(RawKind::Rate)
            .sample_with_prev(&b, Some(&a), &d, &[])
            .unwrap();
        assert!((hz - 100.0).abs() < 1e-9);
        // Same timestamp: no finite rate.
        assert_eq!(
            raw(RawKind::Rate).sample_with_prev(&a, Some(&a), &d, &[]),
            None
        );
        assert_eq!(raw(RawKind::Rate).unit(&d, &[]), "Hz");
        assert_eq!(raw(RawKind::DeltaT).unit(&d, &[]), "ms");
    }

    #[test]
    fn labels() {
        let mut names = NameLookup::default();
        names.bus_names.insert(BusId(1), "CAN1".into());
        let users = [user()];
        assert_eq!(dbc_ref("Speed").label(&names, &users), "CAN1 \u{b7} Speed");
        assert_eq!(
            SignalRef::User(UserSignalId(1)).label(&names, &users),
            "Temp"
        );
        assert_eq!(
            raw(RawKind::Byte(2)).label(&names, &users),
            "CAN1 0x300 Byte 2"
        );
    }
}
