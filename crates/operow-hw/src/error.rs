use thiserror::Error;

/// Errors from drivers and channels.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HwError {
    #[error("bad interface name {0:?}: expected driver:channel, e.g. socketcan:can0")]
    BadInterface(String),
    #[error("unknown hardware driver {0:?}")]
    UnknownDriver(String),
    #[error("driver not available: {0}")]
    NotAvailable(String),
    #[error("cannot open {interface}: {msg}")]
    Open { interface: String, msg: String },
    #[error("channel is listen-only; transmitting is disabled")]
    ListenOnly,
    #[error("channel is closed")]
    Closed,
    #[error("transmit queue full")]
    TxQueueFull,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("I/O error: {0}")]
    Io(String),
}
