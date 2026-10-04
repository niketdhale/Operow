//! ISO 14229 (UDS) message encoding and decoding, independent of any
//! transport or simulation engine.
//!
//! [`Request`] and [`Response`] model the services Operow simulates and
//! fall back to `Raw` for everything else. Sub-function bytes keep the
//! suppress-positive-response bit (0x80) exactly as on the wire; use
//! [`Request::suppress_positive_response`] and [`Request::sub_function`] to
//! interpret them.

mod dtc;
mod message;
mod nrc;

#[cfg(test)]
mod tests;

pub use dtc::{Dtc, dtc_to_string, parse_dtc, status_bit_names};
pub use message::{Request, Response, RoutineSub, UdsError, describe, service_name};
pub use nrc::Nrc;
