//! Per-agent discovery + telemetry modules.
//!
//! Each sub-module owns the agent-specific transcript / state file
//! parsing logic for one CLI. Shared vendor-neutral primitives
//! (`SessionState`, `TelemetrySnapshot`, `FileTail`, `context_window_for_model`)
//! live in [`crate::telemetry`] so no agent module imports another.

pub mod claude;
pub mod copilot;
pub mod opencode;
pub mod pi;
