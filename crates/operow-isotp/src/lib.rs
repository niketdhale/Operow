//! ISO 15765-2 (ISO-TP) transport layer, transport-agnostic and sans-IO.
//!
//! [`IsoTpChannel`] is a pure state machine: the host feeds it received CAN
//! frames ([`IsoTpChannel::on_frame`]) and the current time, and drains
//! [`IsoTpAction`]s from [`IsoTpChannel::poll`]. [`IsoTpChannel::next_deadline`]
//! tells the host when to call `poll` next. It depends only on `operow-core`
//! for [`operow_core::CanFrame`], never on the simulation engine.
//!
//! Simplifications relative to ISO 15765-2 are listed in the docs of
//! [`IsoTpChannel`].

mod channel;
mod config;
mod pdu;

#[cfg(test)]
mod tests;

pub use channel::{IsoTpAction, IsoTpChannel, IsoTpError};
pub use config::{Addressing, IsoTpConfig, StMin, Timeouts};
pub use pdu::{PciType, describe_pci, frame_pci_type};
