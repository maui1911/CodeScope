//! Vendor-neutral telemetry primitives shared across every agent
//! telemetry module (Claude / Copilot / OpenCode / Pi).
//!
//! Each agent module owns its own format-specific parser
//! (`claude_telemetry`, `copilot_telemetry`, etc.) but they all
//! produce the same observable shape: a [`TelemetrySnapshot`] with a
//! [`SessionState`], optionally tracked via a [`FileTail`] cursor and
//! sized against [`context_window_for_model`]. Previously these
//! lived under `claude_telemetry`, which made the Claude module the
//! de-facto base class for the other vendors; the move here breaks
//! that coupling without changing any behavior.

use std::time::{Duration, SystemTime};

/// Semantic activity of an agent session, derived from its transcript
/// tail or status file.
///
/// Mirrors `ClaudeActivityState` in the C# build — variants are
/// vendor-neutral so every per-agent tail maps onto the same set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionState {
    /// No information yet — no entries parsed, or session never
    /// produced a user prompt.
    #[default]
    Unknown,
    /// Last assistant turn ended with `stop_reason: end_turn` —
    /// waiting for next user prompt.
    Idle,
    /// Last assistant turn ended with `stop_reason: tool_use` —
    /// pending tool call (permission prompt in manual mode).
    PendingToolUse,
    /// Most recent entry is a user turn — agent is composing its
    /// response.
    Busy,
}

/// Snapshot of live telemetry for a single agent session.
///
/// Mirrors `ClaudeSessionTelemetry` in the C# build. `context_pct` is
/// computed from `tokens_used` and the context window looked up from
/// the model id; `None` when the model is unrecognised.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetrySnapshot {
    /// Most recent `message.model` seen on an assistant turn.
    pub model: Option<String>,
    /// Most recent assistant turn's total context:
    /// `input + cache_read + cache_creation + output`.
    ///
    /// Overwrites on each assistant turn — **not** a running sum.
    /// (Summing double-counts: Claude's `input_tokens` already covers
    /// the full prior conversation in every request.)
    pub tokens_used: u64,
    /// `tokens_used / context_window` in `[0.0, 1.0]`. `None` when
    /// the model id is unrecognised and no cap can be derived.
    pub context_pct: Option<f32>,
    /// Number of non-tool-result user messages — each fresh prompt
    /// increments this by one.
    pub turn_count: u32,
    /// Wall-clock duration of the most recent user→assistant pair
    /// (`assistant.timestamp − user.timestamp`). `None` until the
    /// first complete round-trip.
    pub last_turn_duration: Option<Duration>,
    /// Current activity state of the session.
    pub state: SessionState,
}

/// Nominal context-window capacity for a model id.
///
/// Mirrors `ClaudeModelCatalog` from the C# build. Rules are
/// deliberately loose — substring matching lets point-release
/// variants work without a code change. The `1m` marker upgrades
/// to 1M. Non-Claude model ids fall through to `None`; the agent
/// telemetry modules treat that as "no context-pct bar".
pub fn context_window_for_model(model_id: &str) -> Option<u64> {
    let id = model_id.to_lowercase();
    if id.contains("1m") {
        return Some(1_000_000);
    }
    if id.contains("sonnet") || id.contains("haiku") {
        return Some(200_000);
    }
    // Claude 3.x Opus — 200k; must not catch "claude-opus-4-*".
    if id.contains("claude-3") {
        return Some(200_000);
    }
    // Opus 4.x and later ship 1M context in Claude Code by default.
    if id.contains("claude") && id.contains("opus") {
        return Some(1_000_000);
    }
    None
}

/// Read a file's modification time, treating any stat error as "no
/// mtime yet". Every telemetry / discovery poller in this crate
/// reaches for an `Option<SystemTime>` so it can compare against the
/// cursor it stashed on the previous tick — when the platform fails
/// to expose mtime (rare; Linux without `statx`, exotic filesystems)
/// the right behaviour is to skip this tick and retry on the next,
/// not surface the error to the user. Centralising it here documents
/// the choice once instead of at eight call sites.
pub fn modified_or_none(meta: &std::fs::Metadata) -> Option<SystemTime> {
    meta.modified().ok()
}

/// Mutable state for a single watched JSONL/log file.
///
/// Cheap `stat`-first reads: callers only open the file when its
/// length has advanced past `last_pos`. Mirrors the `Watch` inner
/// class of `ClaudeTelemetryService` from the C# build but is shared
/// across every per-agent tail.
#[derive(Debug, Default)]
pub struct FileTail {
    /// Byte offset of the next read. Reset to 0 when the file shrinks
    /// (truncated / rewritten).
    pub last_pos: u64,
    /// mtime of the last `stat` that produced a read. Unused for
    /// correctness but handy for debugging; callers can use
    /// `metadata.len()` instead.
    pub last_mtime: Option<SystemTime>,
}
