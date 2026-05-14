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

// ---------------------------------------------------------------------------
// Numeric formatters — vendor-neutral display helpers used by every
// agent telemetry tail's status-bar rendering. Their thresholds are
// intentionally Claude-tuned (per the C# `MainViewModel.FormatTokens`
// shape) but the maths is generic.
// ---------------------------------------------------------------------------

/// Format a token count for the status bar: raw integer below 10k,
/// `.1f k` between 10k–100k, `Nk` between 100k–1M, `.2f M` between
/// 1M–10M, `.1f M` above. Independent thresholds from the C# build
/// (`MainViewModel.FormatTokens` switches to `k` at 1k); the Rust
/// version keeps small values raw because session telemetry stays
/// well under 10k for short sessions and an `8.5k` rendering reads
/// worse than `8500` at that scale.
pub fn format_tokens(n: u64) -> String {
    if n < 10_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        if k < 100.0 {
            return format!("{k:.1}k");
        }
        return format!("{}k", k.round() as u64);
    }
    let m = n as f64 / 1_000_000.0;
    if m < 10.0 {
        format!("{m:.2}M")
    } else {
        format!("{m:.1}M")
    }
}

/// Format `0.0..=1.0` as `"0%"` / `"3%"` / `"6.2%"` / `"87%"`.
/// Mirrors C# `MainViewModel.StatusTokensPercentText` rounding:
/// integer rounding at and above 10%, otherwise C#'s `0.#` format
/// (one decimal *only when non-zero*; whole numbers under 10%
/// drop the trailing `.0`).
pub fn format_context_pct(pct: f32) -> String {
    let p = (pct * 100.0).clamp(0.0, 999.0);
    if p >= 10.0 {
        return format!("{:.0}%", p.round());
    }
    // C#'s `0.#` keeps one decimal only when it isn't zero — i.e.
    // `6.2%`, `9.9%`, but `7%` (not `7.0%`).
    let one_dp = (p * 10.0).round() / 10.0;
    if (one_dp - one_dp.trunc()).abs() < f32::EPSILON {
        format!("{:.0}%", one_dp)
    } else {
        format!("{one_dp:.1}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- context_window_for_model ---

    #[test]
    fn context_window_opus_4_is_1m() {
        assert_eq!(context_window_for_model("claude-opus-4-7"), Some(1_000_000));
    }

    #[test]
    fn context_window_opus_1m_tag_is_1m() {
        assert_eq!(
            context_window_for_model("claude-opus-4-7[1m]"),
            Some(1_000_000)
        );
    }

    #[test]
    fn context_window_sonnet_is_200k() {
        assert_eq!(
            context_window_for_model("claude-sonnet-4-6"),
            Some(200_000)
        );
    }

    #[test]
    fn context_window_haiku_is_200k() {
        assert_eq!(
            context_window_for_model("claude-haiku-3-5"),
            Some(200_000)
        );
    }

    #[test]
    fn context_window_claude_3_opus_is_200k() {
        assert_eq!(
            context_window_for_model("claude-3-opus-20240229"),
            Some(200_000)
        );
    }

    #[test]
    fn context_window_unknown_is_none() {
        assert_eq!(context_window_for_model("unknown-model"), None);
    }

    // --- format_tokens ---

    #[test]
    fn format_tokens_under_10k_uses_raw() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(123), "123");
        assert_eq!(format_tokens(9_999), "9999");
    }

    #[test]
    fn format_tokens_10k_to_100k_uses_one_decimal_k() {
        assert_eq!(format_tokens(10_000), "10.0k");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(99_999), "100.0k");
    }

    #[test]
    fn format_tokens_100k_to_1m_uses_integer_k() {
        assert_eq!(format_tokens(123_456), "123k");
        assert_eq!(format_tokens(999_999), "1000k");
    }

    #[test]
    fn format_tokens_over_1m_uses_decimal_m() {
        assert_eq!(format_tokens(1_000_000), "1.00M");
        assert_eq!(format_tokens(1_234_567), "1.23M");
        assert_eq!(format_tokens(12_345_678), "12.3M");
    }

    #[test]
    fn format_tokens_at_thousands_boundary_stays_raw() {
        // The `k` switchover is at 10k, *not* 1k like the C# build
        // (documented deviation in the function doc comment).
        assert_eq!(format_tokens(1_000), "1000");
        assert_eq!(format_tokens(5_500), "5500");
    }

    #[test]
    fn format_tokens_extremely_large_uses_one_decimal_m() {
        // Far above the largest known context window — the function
        // must still produce a non-panicking, consistent `<value>M`
        // string with a single fractional digit (no scientific
        // notation, no trailing zero stripping). Length isn't bounded.
        assert_eq!(format_tokens(999_999_999), "1000.0M");
    }

    // --- format_context_pct ---

    #[test]
    fn format_context_pct_under_10_uses_one_decimal_when_non_zero() {
        // Whole numbers drop the trailing `.0` (matches C# `0.#`).
        assert_eq!(format_context_pct(0.0), "0%");
        assert_eq!(format_context_pct(0.07), "7%");
        // Non-zero fractional digit keeps one decimal.
        assert_eq!(format_context_pct(0.062), "6.2%");
        assert_eq!(format_context_pct(0.099), "9.9%");
    }

    #[test]
    fn format_context_pct_at_or_above_10_rounds_to_int() {
        assert_eq!(format_context_pct(0.1), "10%");
        assert_eq!(format_context_pct(0.873), "87%");
        assert_eq!(format_context_pct(0.999), "100%");
    }

    #[test]
    fn format_context_pct_clamps_above_one() {
        // Defensive: TelemetrySnapshot already clamps but the formatter
        // shouldn't blow up if a caller hands it 1.5.
        assert_eq!(format_context_pct(1.5), "150%");
        assert_eq!(format_context_pct(99.0), "999%");
    }
}
