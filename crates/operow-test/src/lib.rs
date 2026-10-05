//! Rhai test runner for Operow projects.
//!
//! A project lists `.rhai` test modules in `Topology::tests`. Every module
//! may define `module_setup`, `module_teardown`, `setup`, `teardown` and
//! any number of `test_*` functions (all without arguments). Each case runs
//! against a fresh [`operow_engine::Simulation`] in virtual time, with no
//! wall-clock waiting.
//!
//! # Script API
//!
//! Signal names are `"Message.Signal"` or `"Bus::Message.Signal"`, resolved
//! through the project's DBCs. Frames sent by a test come from a virtual
//! sender called `Test`. Blocking calls advance the simulation in
//! `step_ns` increments; waits only look at frames that appear after the
//! call, and return at the end of the step in which the frame occurred.
//!
//! * Timing: `wait(ms)`, `now_ms()`
//! * Waits (a timeout fails the test): `wait_for_message(id, timeout_ms)`,
//!   `wait_for_message_on(bus, id, timeout_ms)` return a frame map `{id,
//!   extended, data, dlc, bus, sender, time_ms}`;
//!   `wait_for_signal(sig, op, value, timeout_ms)` returns the value. A
//!   signal whose last seen value already satisfies the condition returns
//!   at once. `wait_any` is not implemented.
//! * Expectations: `expect_eq`, `expect_ne`, `expect_lt`, `expect_gt`,
//!   `expect_near(a, b, tol)`, `expect_true(cond[, msg])`,
//!   `expect_signal(sig, op, value)`, `expect_no_message(id, ms)`,
//!   `expect_no_message_on(bus, id, ms)`,
//!   `expect_cycle_time(id, period_ms, tol_pct, window_ms)`,
//!   `expect_cycle_time_on(bus, ...)`, `fail(msg)`, `skip(reason)`
//! * Stimulus: `send(bus, id, data)`, `set_signal(sig, value)`,
//!   `trigger(node, msg_name_or_index)`, `set_payload(node, msg, data)`
//! * Faults: `inject_errors(#{bus, node?, id?, kind?, count? | every? |
//!   probability?, limit?})`, `node_offline(node)`, `node_online(node)`,
//!   `force_bus_off(node, bus)`, `recover_bus_off(node, bus)`,
//!   `msg_control(node, id, #{paused?, drop_pct?, delay_ms?, jitter_ms?})`,
//!   `node_state(node, bus)`, `node_tec`, `node_rec`
//! * Diagnostics: `uds(node, bytes)` returns the response bytes,
//!   `uds_expect_nrc(node, bytes, nrc)`
//! * Data: `signal(sig)` (latest value or `()`), `last_frame(bus, id)`,
//!   `log(msg)`

mod api;
mod export;
mod project;
mod report;
mod runner;

pub use export::{to_html, to_json, to_junit};
pub use project::{Project, ProjectError, TestModule};
pub use report::{CaseResult, Failure, ModuleResult, RunReport, Status, Step, Totals, TraceRow};
pub use runner::{Progress, ProgressFn, RunOptions, TestRunner};

#[cfg(test)]
mod tests;
