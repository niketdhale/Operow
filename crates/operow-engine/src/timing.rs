use operow_core::CanFrame;

/// Worst-case on-wire bit length of a classic CAN frame, including bit
/// stuffing and the inter-frame space.
///
/// Standard (11-bit id) frames: 44 fixed bits (SOF, arbitration+control
/// overhead, CRC, ACK, EOF) + 8 bits per data byte. Extended (29-bit id)
/// frames: 64 fixed bits + 8 bits per data byte.
///
/// Of those fixed+data bits, only the portion up to (but not including) the
/// fixed-form CRC delimiter/ACK/EOF/IFS fields can ever be stuffed: 34 bits
/// for standard frames, 54 for extended, plus 8 bits per data byte. Worst
/// case one stuff bit is inserted every 5 bits, i.e. `(stuffable - 1) / 4`
/// extra bits (integer division). Finally 3 bits of inter-frame space (IFS)
/// are added after every frame.
///
/// This function only applies to classic (non-FD) frames; use
/// [`fd_frame_phase_bits`]/[`frame_duration_ns_fd`] for CAN FD frames.
pub fn frame_bits(frame: &CanFrame) -> u32 {
    let dlc = frame.dlc as u32;
    let (base, stuffable) = if frame.extended {
        (64 + 8 * dlc, 54 + 8 * dlc)
    } else {
        (44 + 8 * dlc, 34 + 8 * dlc)
    };
    let stuff = (stuffable - 1) / 4;
    base + stuff + 3
}

/// Worst-case time, in nanoseconds, that a classic `frame` occupies the bus
/// at `bitrate` bits/second.
pub fn frame_duration_ns(frame: &CanFrame, bitrate: u32) -> u64 {
    let bits = frame_bits(frame) as u64;
    ceil_div(bits * 1_000_000_000, bitrate as u64)
}

/// Worst-case bit breakdown of a CAN FD frame, split into the bits sent
/// during the arbitration phase (always at the bus's nominal bitrate) and
/// the bits sent during the data phase (at the data bitrate when BRS is
/// set, otherwise also at the nominal bitrate — see
/// [`frame_duration_ns_fd`]).
///
/// Arbitration-phase fixed fields (nominal bitrate):
/// - standard (11-bit id): SOF(1) + ID(11) + RRS(1) + IDE(1) + FDF(1) +
///   res(1) + BRS(1) = 17 bits.
/// - extended (29-bit id): SOF(1) + ID(11) + SRR(1) + IDE(1) + ID(18) +
///   RRS(1) + FDF(1) + res(1) + BRS(1) = 36 bits.
///
/// Data-phase fields: ESI(1) + DLC(4) + 8*len (payload) + stuff-count(4) +
/// CRC(17 bits if len<=16, else 21 bits).
///
/// Bit stuffing, worst case one stuff bit every 4 bits of the *dynamically
/// stuffed* portion (`(n - 1) / 4`, floor, minimum 0), is attributed
/// separately to each phase:
/// - nominal phase: stuffing over the arbitration-phase fields themselves,
///   `(arb_bits - 1) / 4`.
/// - data phase: stuffing over ESI+DLC+payload (the CRC and stuff-count
///   fields are *fixed*-stuffed instead, one fixed stuff bit inserted every
///   4 bits of the CRC field: `ceil((crc_len + 4) / 4)`).
///
/// Finally a fixed tail is sent at the nominal bitrate after the data
/// phase: CRC delimiter(1) + ACK(2) + EOF(7) + IFS(3) = 13 bits.
pub fn fd_frame_phase_bits(frame: &CanFrame) -> (u32, u32) {
    let len = frame.dlc as u32;
    let arb_bits: u32 = if frame.extended { 36 } else { 17 };
    let crc_len: u32 = if len <= 16 { 17 } else { 21 };
    const TAIL: u32 = 13;

    let nominal_stuff = (arb_bits - 1) / 4;
    let nominal_bits = arb_bits + nominal_stuff + TAIL;

    let data_dynamic_bits = 1 /* ESI */ + 4 /* DLC */ + 8 * len;
    let data_stuff = (data_dynamic_bits - 1) / 4;
    let fixed_stuff = (crc_len + 4).div_ceil(4);
    let data_phase_fields = data_dynamic_bits + 4 /* stuff-count */ + crc_len;
    let data_bits = data_phase_fields + fixed_stuff + data_stuff;

    (nominal_bits, data_bits)
}

/// Worst-case time, in nanoseconds, that a CAN FD `frame` occupies the bus,
/// given the bus's nominal and data bitrates (bit/s). When `frame.brs` is
/// unset, the data-phase bits are also sent at `nominal_bitrate`.
pub fn frame_duration_ns_fd(frame: &CanFrame, nominal_bitrate: u32, data_bitrate: u32) -> u64 {
    let (nominal_bits, data_bits) = fd_frame_phase_bits(frame);
    let data_rate = if frame.brs {
        data_bitrate
    } else {
        nominal_bitrate
    };
    ceil_div(nominal_bits as u64 * 1_000_000_000, nominal_bitrate as u64)
        + ceil_div(data_bits as u64 * 1_000_000_000, data_rate as u64)
}

/// Worst-case bus-occupation time, in nanoseconds, of any `frame` (classic
/// or FD), given the bus's nominal and data bitrates.
pub fn frame_duration_ns_any(frame: &CanFrame, nominal_bitrate: u32, data_bitrate: u32) -> u64 {
    if frame.fd {
        frame_duration_ns_fd(frame, nominal_bitrate, data_bitrate)
    } else {
        frame_duration_ns(frame, nominal_bitrate)
    }
}

fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::CanFrame;

    #[test]
    fn std_8byte_frame_at_500k() {
        let frame = CanFrame::new(0x100, false, &[0u8; 8]).unwrap();
        // base = 44 + 64 = 108, stuffable = 34 + 64 = 98, stuff = 97/4 = 24
        // total = 108 + 24 + 3 = 135
        assert_eq!(frame_bits(&frame), 135);
        assert_eq!(frame_duration_ns(&frame, 500_000), 135 * 2000);
    }

    #[test]
    fn fd_std_64byte_brs_500k_2m() {
        let frame = CanFrame::new_fd(0x100, false, true, &[0u8; 64]).unwrap();
        let (nominal_bits, data_bits) = fd_frame_phase_bits(&frame);
        // arb_bits = 17, nominal_stuff = 16/4 = 4, tail = 13 -> 34
        assert_eq!(nominal_bits, 34);
        // crc_len = 21 (len > 16); data_dynamic = 1+4+512 = 517;
        // data_stuff = 516/4 = 129; fixed_stuff = ceil(25/4) = 7;
        // data_phase_fields = 517 + 4 + 21 = 542; data_bits = 542+7+129 = 678
        assert_eq!(data_bits, 678);

        let duration = frame_duration_ns_fd(&frame, 500_000, 2_000_000);
        // nominal: 34 bits @ 500k = 68_000 ns; data: 678 bits @ 2M = 339_000 ns
        assert_eq!(duration, 68_000 + 339_000);
        assert_eq!(duration, 407_000);
    }

    #[test]
    fn fd_std_64byte_without_brs_uses_nominal_for_data_phase_too() {
        let frame = CanFrame::new_fd(0x100, false, false, &[0u8; 64]).unwrap();
        assert!(!frame.brs);
        let duration = frame_duration_ns_fd(&frame, 500_000, 2_000_000);
        // data phase (678 bits) now also at 500k: 678*2000 = 1_356_000 ns
        assert_eq!(duration, 68_000 + 1_356_000);
        assert_eq!(duration, 1_424_000);
    }

    #[test]
    fn fd_extended_8byte_brs_500k_2m() {
        let frame = CanFrame::new_fd(0x1ABCDE, true, true, &[0u8; 8]).unwrap();
        let (nominal_bits, data_bits) = fd_frame_phase_bits(&frame);
        // arb_bits = 36, nominal_stuff = 35/4 = 8, tail = 13 -> 57
        assert_eq!(nominal_bits, 57);
        // crc_len = 17 (len<=16); data_dynamic = 1+4+64=69; data_stuff=68/4=17;
        // fixed_stuff = ceil(21/4) = 6; data_phase_fields = 69+4+17=90;
        // data_bits = 90+6+17 = 113
        assert_eq!(data_bits, 113);

        let duration = frame_duration_ns_fd(&frame, 500_000, 2_000_000);
        // nominal: 57*2000 = 114_000; data: 113 bits @ 2M -> ceil(113*500)=56_500
        assert_eq!(duration, 114_000 + 56_500);
    }
}
