//! Import of Vector DBC CAN databases: a hand-written parser, a signal
//! codec and a helper that turns a database into an Operow [`Topology`].
//!
//! [`Topology`]: operow_core::Topology

mod model;
mod parser;
mod signal;
mod topology;

pub use model::{ByteOrder, Database, MessageDef, Mux, SignalDef, ValueType};
pub use parser::DbcError;
