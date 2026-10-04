/// A diagnostic trouble code with its status byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dtc {
    /// 24-bit code: two OBD-style bytes followed by the failure type byte.
    pub code: u32,
    /// ISO 14229 status-of-DTC byte.
    pub status: u8,
}

/// Names of the set bits of a DTC status byte, lowest bit first.
pub fn status_bit_names(status: u8) -> Vec<&'static str> {
    const NAMES: [&str; 8] = [
        "testFailed",
        "testFailedThisOperationCycle",
        "pendingDTC",
        "confirmedDTC",
        "testNotCompletedSinceLastClear",
        "testFailedSinceLastClear",
        "testNotCompletedThisOperationCycle",
        "warningIndicatorRequested",
    ];
    NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| status & (1 << i) != 0)
        .map(|(_, n)| *n)
        .collect()
}

/// Format a 24-bit DTC like `P0123-00`: the top two bits of the first byte
/// select the system letter (P, C, B, U), then four hex digits, then the
/// failure type byte.
pub fn dtc_to_string(code: u32) -> String {
    let code = code & 0xFF_FFFF;
    let letter = ['P', 'C', 'B', 'U'][(code >> 22) as usize & 3];
    let digit = (code >> 20) & 3;
    format!(
        "{letter}{digit}{:03X}-{:02X}",
        (code >> 8) & 0xFFF,
        code & 0xFF
    )
}

/// Parse `P0123`, `P0123-1A` (or without a dash) into a 24-bit code. The
/// failure type defaults to 0. Also accepts plain hex, e.g. `0x012300`.
pub fn parse_dtc(s: &str) -> Option<u32> {
    let s = s.trim();
    let first = s.chars().next()?;
    let sys = match first.to_ascii_uppercase() {
        'P' => Some(0u32),
        'C' => Some(1),
        'B' => Some(2),
        'U' => Some(3),
        _ => None,
    };
    let Some(sys) = sys else {
        let h = s.strip_prefix("0x").or(s.strip_prefix("0X")).unwrap_or(s);
        return u32::from_str_radix(h, 16).ok().filter(|v| *v <= 0xFF_FFFF);
    };
    let rest = &s[1..];
    let (body, ftb) = match rest.split_once('-') {
        Some((b, f)) => (b, u32::from_str_radix(f, 16).ok().filter(|v| *v <= 0xFF)?),
        None => (rest, 0),
    };
    if body.len() != 4 || !body.is_char_boundary(1) {
        return None;
    }
    let digit = u32::from_str_radix(&body[..1], 16)
        .ok()
        .filter(|d| *d <= 3)?;
    let low = u32::from_str_radix(&body[1..], 16).ok()?;
    Some((sys << 22) | (digit << 20) | (low << 8) | ftb)
}
