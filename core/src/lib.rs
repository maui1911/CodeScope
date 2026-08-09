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
//! lives in the root `codescope` binary crate (`src/`). Anything
//! terminal-protocol-bound lives in `codescope-terminal`.

pub mod agent;
pub mod agent_registry;
pub mod agents;
pub mod attachments;
pub mod command_palette;
pub mod crash_log;
pub mod diff;
pub mod git;
pub mod layout;
pub mod memory_watchdog;
pub mod overview;
pub mod path_canon;
pub mod paths;
pub mod pr;
pub mod process;
pub mod projects;
pub mod session;
pub mod settings;
pub mod tab_drag;
pub mod tab_title;
pub mod telemetry;
pub mod theme;
pub mod time;
pub mod update_check;
pub mod window_state;

pub use agent::{AgentId, agent_id_from_auto_type};
pub use agent_registry::{
    AgentProfile, AgentRegistry, build_new_session_auto_type, build_resume_auto_type,
    built_in_defaults,
};
pub use agents::claude::discovery::POLL_INTERVAL_MS as CLAUDE_DISCOVERY_POLL_MS;
pub use agents::claude::telemetry::{ClaudeTranscriptTail, encode_cwd, model_display_name};
pub use agents::copilot::discovery::POLL_INTERVAL_MS as COPILOT_DISCOVERY_POLL_MS;
pub use agents::copilot::telemetry::CopilotTranscriptTail;
pub use agents::opencode::discovery::POLL_INTERVAL_MS as OPENCODE_DISCOVERY_POLL_MS;
pub use agents::opencode::telemetry::OpenCodeMessageTail;
pub use agents::pi::discovery::POLL_INTERVAL_MS as PI_DISCOVERY_POLL_MS;
pub use agents::pi::telemetry::PiTranscriptTail;
pub use attachments::{SavedAttachment, save_attachment_bytes};
pub use layout::{LayoutState, RestoreTab, SessionPlacement};
pub use overview::{
    LiveSessionLookup, OverviewLifecycle, OverviewRow,
    build_rows as build_overview_rows,
    build_rows_for_live as build_overview_rows_for_live,
};
pub use paths::AppPaths;
pub use projects::{Project, ProjectsConfig, Session, Worktree};
pub use session::{
    RetentionPolicy, SessionDescriptor, SessionManager, build_agent_shell_args, now_iso8601,
};
pub use settings::{CursorSettings, CursorShape, DEFAULT_AGENT_ID, FontSettings, Settings};
pub use tab_drag::{TabRect, compute_drop_index};
pub use tab_title::{TAB_TITLE_SEPARATOR, rebuild_title};
pub use telemetry::{
    SessionState, TelemetrySnapshot, context_window_for_model, format_context_pct, format_tokens,
};
pub use theme::{Rgb, Theme, ThemePalette, builtin};
pub use window_state::WindowState;
