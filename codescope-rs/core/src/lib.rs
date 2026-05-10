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

pub mod claude_telemetry;
pub mod git;
pub mod layout;
pub mod paths;
pub mod projects;
pub mod settings;
pub mod theme;
pub mod window_state;

pub use claude_telemetry::{
    SessionState, TelemetrySnapshot, TranscriptTail, context_window_for_model, encode_cwd,
};
pub use layout::{LayoutState, RestoreTab};
pub use paths::AppPaths;
pub use projects::{Project, ProjectsConfig, Session, Worktree};
pub use settings::{CursorSettings, FontSettings, Settings};
pub use theme::{Rgb, Theme, ThemePalette, builtin};
pub use window_state::WindowState;
