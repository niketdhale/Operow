//! Discrete-event CAN bus simulation engine for Operow.

mod diag;
mod ecu;
mod gateway;
mod hw;
mod replay;
mod runner;
mod script;
mod sim;
mod tester;
mod timing;

pub use diag::DiagEcu;
pub use ecu::{Ecu, EcuCommand, EcuCtx, FrameMeta, PeriodicEcu};
pub use gateway::GatewayEcu;
pub use hw::{HwBridge, HwNotice};
pub use replay::{LARGE_LOG_RECORDS, ReplayEcu};
pub use runner::{Command, Engine, EngineEvent, EngineHandle, RunState};
pub use script::{ScriptEcu, check_script};
pub use sim::{
    BusOffRecovery, BusStats, CanErrorCounts, GENERATOR_NODE_BASE, GeneratorId, HW_NODE_BASE,
    InjectMode, InjectSpec, MAX_HOPS, MAX_MSG_DELAY_MS, MsgControl, NodeErrorInfo, SimError,
    Simulation, hw_node,
};
pub use tester::{DiagRequestSpec, DiagResult, TESTER_NODE_BASE, TesterPresentSpec};
pub use timing::{
    fd_frame_phase_bits, frame_bits, frame_duration_ns, frame_duration_ns_any, frame_duration_ns_fd,
};

#[cfg(test)]
mod diag_tests;
#[cfg(test)]
mod hw_tests;
#[cfg(test)]
mod tests;
