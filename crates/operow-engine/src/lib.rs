//! Discrete-event CAN bus simulation engine for Operow.

mod ecu;
mod runner;
mod sim;
mod timing;

pub use ecu::{Ecu, EcuCtx, PeriodicEcu};
pub use runner::{Command, Engine, EngineEvent, EngineHandle, RunState};
pub use sim::{BusStats, Simulation};
pub use timing::{frame_bits, frame_duration_ns};

#[cfg(test)]
mod tests;
