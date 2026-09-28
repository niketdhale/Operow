use serde::{Deserialize, Serialize};

/// Identifier for an ECU/node on the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

/// Identifier for a CAN bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BusId(pub u32);

/// Virtual simulation timestamp, in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const ZERO: Timestamp = Timestamp(0);

    /// Build a timestamp from a millisecond value.
    pub fn from_ms(ms: u64) -> Self {
        Timestamp(ms * 1_000_000)
    }

    /// Convert to fractional seconds.
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }
}

impl std::ops::Add<u64> for Timestamp {
    type Output = Timestamp;
    fn add(self, rhs: u64) -> Timestamp {
        Timestamp(self.0 + rhs)
    }
}
