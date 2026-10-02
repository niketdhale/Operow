use operow_core::CanFrame;

use crate::model::{ByteOrder, MessageDef, Mux, SignalDef, ValueType};

impl SignalDef {
    /// Absolute bit positions of the signal, from its MSB to its LSB.
    fn bit_positions(&self) -> impl Iterator<Item = usize> + use<> {
        let size = (self.size as usize).min(64);
        let start = self.start_bit as usize;
        let order = self.byte_order;
        let mut pos = start;
        (0..size).map(move |i| match order {
            // Intel: iterate LSB first, so reverse by index.
            ByteOrder::Intel => start + (size - 1 - i),
            ByteOrder::Motorola => {
                let cur = pos;
                pos = if pos.is_multiple_of(8) {
                    pos + 15
                } else {
                    pos - 1
                };
                cur
            }
        })
    }

    /// Extract the raw (unsigned, unscaled) value from `data`. Bits beyond
    /// the end of `data` read as zero.
    pub fn decode_raw(&self, data: &[u8]) -> u64 {
        let mut raw = 0u64;
        for pos in self.bit_positions() {
            let bit = data.get(pos / 8).is_some_and(|b| b >> (pos % 8) & 1 == 1);
            raw = raw << 1 | bit as u64;
        }
        raw
    }

    /// Decode the physical value: sign-extend if signed, then apply
    /// `factor` and `offset`.
    pub fn decode(&self, data: &[u8]) -> f64 {
        let raw = self.decode_raw(data);
        let n = self.size.min(64) as u32;
        let value = if self.value_type == ValueType::Signed && n > 0 {
            let shift = 64 - n;
            (((raw << shift) as i64) >> shift) as f64
        } else {
            raw as f64
        };
        value * self.factor + self.offset
    }

    /// Store the low `size` bits of `raw` into `data`. Bits beyond the end
    /// of `data` are dropped.
    pub fn encode_raw(&self, data: &mut [u8], raw: u64) {
        let size = self.size.min(64) as usize;
        for (i, pos) in self.bit_positions().enumerate() {
            let bit = raw >> (size - 1 - i) & 1 == 1;
            if let Some(byte) = data.get_mut(pos / 8) {
                if bit {
                    *byte |= 1 << (pos % 8);
                } else {
                    *byte &= !(1 << (pos % 8));
                }
            }
        }
    }

    /// Inverse of [`SignalDef::decode`]: remove offset and factor, round,
    /// clamp to the signal's bit width and store.
    pub fn encode(&self, data: &mut [u8], value: f64) {
        let n = self.size.min(64) as u32;
        if n == 0 {
            return;
        }
        let scaled = if self.factor == 0.0 {
            0.0
        } else {
            ((value - self.offset) / self.factor).round()
        };
        let raw = match self.value_type {
            ValueType::Unsigned => {
                let max = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
                // `as` saturates and maps NaN to 0.
                (scaled as u64).min(max)
            }
            ValueType::Signed => {
                let max = if n == 64 {
                    i64::MAX
                } else {
                    (1i64 << (n - 1)) - 1
                };
                let min = if n == 64 {
                    i64::MIN
                } else {
                    -(1i64 << (n - 1))
                };
                (scaled as i64).clamp(min, max) as u64
            }
        };
        self.encode_raw(data, raw);
    }
}

impl MessageDef {
    /// Decode every signal present in `frame`: all non-multiplexed signals,
    /// the multiplexor, and only the multiplexed signals of the active group.
    pub fn decode(&self, frame: &CanFrame) -> Vec<(String, f64)> {
        let data = frame.payload();
        let selector = self
            .signals
            .iter()
            .find(|s| s.multiplexer == Some(Mux::Multiplexor))
            .map(|s| s.decode_raw(data));
        self.signals
            .iter()
            .filter(|s| match s.multiplexer {
                None | Some(Mux::Multiplexor) => true,
                Some(Mux::Multiplexed(n)) => selector == Some(n),
            })
            .map(|s| (s.name.clone(), s.decode(data)))
            .collect()
    }
}
