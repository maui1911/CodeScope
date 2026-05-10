//! Pure-Rust shared models for the CodeScope app.
//!
//! Lives outside the gpui binary so:
//!
//! * tests run without spinning up a window;
//! * future CLI tools (settings linter, theme validator) can re-use
//!   the same types without dragging gpui through their dep graph;
//! * the C# `Designs/Tokens/` work has a clear "translation target"
//!   on the Rust side — when the design team moves a hex value, we
//!   only have to change it here, not in two places.
//!
//! No gpui, alacritty, or windows-rs imports here. Anything UI-bound
//! lives in the `app` crate. Anything terminal-protocol-bound lives in
//! `codescope-terminal`.

pub mod claude_discovery;
pub mod claude_telemetry;
pub mod copilot_telemetry;
pub mod crash_log;
pub mod git;
pub mod layout;
pub mod paths;
pub mod pi_telemetry;
pub mod pr;
pub mod projects;
pub mod session;
pub mod settings;
pub mod theme;
pub mod time;
pub mod window_state;

pub use claude_discovery::{AdoptionCandidate, POLL_INTERVAL_MS as CLAUDE_DISCOVERY_POLL_MS};
pub use claude_telemetry::{
    SessionState, TelemetrySnapshot, TranscriptTail, context_window_for_model, encode_cwd,
    format_context_pct, format_tokens, model_display_name,
};
pub use layout::{LayoutState, RestoreTab};
pub use paths::AppPaths;
pub use projects::{Project, ProjectsConfig, Session, Worktree};
pub use settings::{CursorSettings, FontSettings, Settings};
pub use theme::{Rgb, Theme, ThemePalette, builtin};
pub use window_state::WindowState;
