//! Per-agent discovery + telemetry modules.
//!
//! Each sub-module owns the agent-specific transcript / state file
//! parsing logic for one CLI. Shared vendor-neutral primitives
//! (`SessionState`, `TelemetrySnapshot`, `FileTail`, `context_window_for_model`)
//! live in [`crate::telemetry`] so no agent module imports another;
//! `dir_listing` is the shared directory cache discovery scans use.

pub mod claude;
pub mod codex;
pub mod copilot;
pub(crate) mod dir_listing;
pub mod gemini;
pub mod opencode;
pub mod pi;
