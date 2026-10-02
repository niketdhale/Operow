//! Discrete-event CAN bus simulation engine for Operow.

mod ecu;
mod gateway;
mod runner;
mod sim;
mod timing;

pub use ecu::{Ecu, EcuCtx, FrameMeta, PeriodicEcu};
pub use gateway::GatewayEcu;
pub use runner::{Command, Engine, EngineEvent, EngineHandle, RunState};
pub use sim::{BusStats, MAX_HOPS, Simulation};
pub use timing::{
    fd_frame_phase_bits, frame_bits, frame_duration_ns, frame_duration_ns_any, frame_duration_ns_fd,
};

#[cfg(test)]
mod tests;
