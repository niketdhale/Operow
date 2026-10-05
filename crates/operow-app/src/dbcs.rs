//! DBC databases attached to buses: path handling, loading, and the
//! formatting used by the trace and inspector to show decoded signals.

use operow_core::CanFrame;
use operow_dbc::{MessageDef, SignalDef};
pub use operow_project::{DbcStore, load_all, load_file, resolve_path, stored_path};

/// Format a physical value: integers without decimals when the factor is
/// integral, otherwise up to 3 decimals with trailing zeros trimmed.
pub fn format_value(value: f64, factor: f64) -> String {
    if factor.fract() == 0.0 && value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let s = format!("{value:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// `Name = value unit (Description)`, with the description taken from
/// `VAL_` when the raw value matches.
pub fn signal_line(sig: &SignalDef, value: f64) -> String {
    let mut s = format!("{} = {}", sig.name, format_value(value, sig.factor));
    if !sig.unit.is_empty() {
        s.push(' ');
        s.push_str(&sig.unit);
    }
    if sig.factor != 0.0 {
        let raw = ((value - sig.offset) / sig.factor).round() as i64;
        if let Some((_, text)) = sig.value_descriptions.iter().find(|(v, _)| *v == raw) {
            s.push_str(&format!(" ({text})"));
        }
    }
    s
}

/// One line per decodable signal of `msg` in `frame` (mux aware).
pub fn decode_lines(msg: &MessageDef, frame: &CanFrame) -> Vec<String> {
    msg.decode(frame)
        .into_iter()
        .filter_map(|(name, v)| {
            let sig = msg.signals.iter().find(|s| s.name == name)?;
            Some(signal_line(sig, v))
        })
        .collect()
}

/// `start|size@order` as in DBC, e.g. `7|16@0`.
pub fn bit_layout(sig: &SignalDef) -> String {
    let order = match sig.byte_order {
        operow_dbc::ByteOrder::Intel => 1,
        operow_dbc::ByteOrder::Motorola => 0,
    };
    format!("{}|{}@{order}", sig.start_bit, sig.size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::BusId;
    use operow_dbc::Database;
    use std::sync::Arc;

    #[test]
    fn value_formatting() {
        assert_eq!(format_value(3.0, 1.0), "3");
        assert_eq!(format_value(-40.0, 1.0), "-40");
        assert_eq!(format_value(812.5, 0.25), "812.5");
        assert_eq!(format_value(12.0, 0.1), "12");
        assert_eq!(format_value(12.3456, 0.0001), "12.346");
        assert_eq!(format_value(0.1 * 3.0, 0.1), "0.3");
        assert_eq!(format_value(-0.0001, 0.1), "0");
    }

    fn sample() -> Database {
        Database::parse(include_str!("../../operow-dbc/tests/fixtures/sample.dbc")).unwrap()
    }

    #[test]
    fn signal_lines_with_units_and_descriptions() {
        let db = sample();
        let msg = db.message(256, false).unwrap();
        let mut data = [0u8; 8];
        for s in &msg.signals {
            match s.name.as_str() {
                "EngineSpeed" => s.encode(&mut data, 3000.0),
                "CoolantTemp" => s.encode(&mut data, 50.0),
                "Running" => s.encode(&mut data, 1.0),
                _ => {}
            }
        }
        let frame = CanFrame::new(256, false, &data).unwrap();
        let lines = decode_lines(msg, &frame);
        assert_eq!(lines[0], "EngineSpeed = 3000 rpm");
        assert_eq!(lines[1], "CoolantTemp = 50 degC");
        assert_eq!(lines[3], "Running = 1 (On)");
    }

    #[test]
    fn name_lookup_prefers_dbc_for_bus() {
        use crate::trace::NameLookup;
        let db = Arc::new(sample());
        let mut names = NameLookup::default();
        names.bus_names.insert(BusId(1), "PT".into());
        assert_eq!(
            names.msg_name(BusId(1), operow_core::NodeId(9), 256, false),
            ""
        );
        names.dbcs.by_bus.insert(BusId(1), db);
        assert_eq!(
            names.msg_name(BusId(1), operow_core::NodeId(9), 256, false),
            "EngineData"
        );
        assert_eq!(
            names.msg_name(BusId(2), operow_core::NodeId(9), 256, false),
            ""
        );
        assert_eq!(
            names.dbc_for_bus_name("PT").map(|d| d.messages.len()),
            Some(3)
        );
    }
}
