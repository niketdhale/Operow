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

/// Worst-case time, in nanoseconds, that `frame` occupies the bus at
/// `bitrate` bits/second.
pub fn frame_duration_ns(frame: &CanFrame, bitrate: u32) -> u64 {
    let bits = frame_bits(frame) as u64;
    bits * 1_000_000_000 / bitrate as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_8byte_frame_at_500k() {
        let frame = CanFrame::new(0x100, false, &[0u8; 8]).unwrap();
        // base = 44 + 64 = 108, stuffable = 34 + 64 = 98, stuff = 97/4 = 24
        // total = 108 + 24 + 3 = 135
        assert_eq!(frame_bits(&frame), 135);
        assert_eq!(frame_duration_ns(&frame, 500_000), 135 * 2000);
    }
}
