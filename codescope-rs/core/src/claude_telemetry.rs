//! Claude Code transcript tail — per-session telemetry derived from
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
//!
//! Mirrors `ClaudeTelemetryService` / `ClaudeTranscriptParser` /
//! `ClaudeModelCatalog` from the C# build
//! (`src/CodeScope.Core/Services/`). Data shapes and field names are
//! intentionally kept 1:1 so the status bar can display the same
//! information regardless of which runtime processes the transcript.
//!
//! # Polling strategy
//!
//! We avoid the `notify` crate (not in Cargo.toml) and use
//! `std::fs::metadata` to stat the file cheaply. When
//! `metadata.len() == last_pos` there is nothing new to read.
//! The caller drives the poll interval (250 ms while busy, 2 s
//! while idle) — this module is pure logic; there is no background
//! thread inside it.

use std::io::{BufRead as _, BufReader, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Semantic activity of a Claude session, derived from the transcript tail.
///
/// Mirrors `ClaudeActivityState` in the C# build.
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

/// Snapshot of live telemetry for a single Claude Code session.
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

// ---------------------------------------------------------------------------
// Model catalog
// ---------------------------------------------------------------------------

/// Nominal context-window capacity for a Claude model id.
///
/// Mirrors `ClaudeModelCatalog` from the C# build. Rules are
/// deliberately loose — substring matching lets point-release
/// variants work without a code change. The `1m` marker upgrades
/// to 1M.
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

/// Encode an absolute path to the `~/.claude/projects/<name>` directory
/// name used by Claude Code.
///
/// Claude replaces `:`, `\`, `/`, and `.` with `-`.
/// E.g. `C:\dev\codescope` → `C--dev-codescope`.
pub fn encode_cwd(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            ':' | '\\' | '/' | '.' => '-',
            other => other,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Per-file tail state
// ---------------------------------------------------------------------------

/// Mutable state for a single watched JSONL file.
///
/// Cheap `stat`-first reads: we only open the file when its length
/// has advanced past `last_pos`. Mirrors the `Watch` inner class of
/// `ClaudeTelemetryService`.
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
// JSONL parser
// ---------------------------------------------------------------------------

/// One parsed line from a Claude Code JSONL transcript.
#[derive(Debug, Default)]
struct Entry {
    kind: EntryKind,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    stop_reason: Option<String>,
    model: Option<String>,
    /// Unix seconds extracted from `timestamp` field (ISO 8601 via
    /// `parse_iso8601`).
    timestamp_secs: Option<f64>,
    /// True when this is a user entry whose content array contains at
    /// least one `{"type":"tool_result",...}` item — i.e. a tool-call
    /// answer, not a fresh user prompt.
    user_carries_tool_result: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum EntryKind {
    #[default]
    Other,
    User,
    Assistant,
}

/// Parse a single JSONL line. Returns `None` for whitespace-only lines
/// and lines that fail JSON parsing (logged to stderr).
fn parse_line(line: &str) -> Option<Entry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("[claude_telemetry] skipping malformed JSONL line: {err}");
            return None;
        }
    };
    let obj = v.as_object()?;

    let kind = match obj.get("type").and_then(Value::as_str) {
        Some("user") => EntryKind::User,
        Some("assistant") => EntryKind::Assistant,
        _ => EntryKind::Other,
    };

    let timestamp_secs = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso8601);

    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cache_creation_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut stop_reason: Option<String> = None;
    let mut model: Option<String> = None;
    let mut user_carries_tool_result = false;

    if let Some(msg) = obj.get("message").and_then(Value::as_object) {
        if let Some(usage) = msg.get("usage").and_then(Value::as_object) {
            input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            cache_creation_tokens = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            cache_read_tokens = usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }

        stop_reason = msg
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_owned);

        model = msg
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        // Detect tool-result user messages: content is an array
        // with at least one `{"type":"tool_result",...}` item.
        if kind == EntryKind::User {
            if let Some(content) = msg.get("content").and_then(Value::as_array) {
                user_carries_tool_result = content.iter().any(|item| {
                    item.as_object()
                        .and_then(|o| o.get("type"))
                        .and_then(Value::as_str)
                        == Some("tool_result")
                });
            }
        }
    }

    Some(Entry {
        kind,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        stop_reason,
        model,
        timestamp_secs,
        user_carries_tool_result,
    })
}

/// Parse an ISO 8601 timestamp string (e.g. `2026-04-22T08:59:44.811Z`)
/// into seconds since the Unix epoch as `f64`.
///
/// We only handle the subset Claude Code emits: UTC with a `Z` or
/// `+00:00` suffix, optional sub-second precision. A full RFC 3339
/// parser would be nicer but would need an extra crate.
fn parse_iso8601(s: &str) -> Option<f64> {
    // Strip trailing 'Z' or '+00:00' and split at 'T'.
    let s = s.trim_end_matches('Z').trim_end_matches("+00:00");
    let t_pos = s.find('T')?;
    let date = &s[..t_pos];
    let time = &s[t_pos + 1..];

    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;

    // Split time on '.' to separate seconds from sub-seconds.
    let (hms, frac) = if let Some(dot) = time.find('.') {
        (&time[..dot], &time[dot + 1..])
    } else {
        (time, "")
    };

    let mut parts = hms.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let sec: i64 = parts.next()?.parse().ok()?;

    // Days since Unix epoch using the proleptic Gregorian calendar.
    // Algorithm from https://www.tondering.dk/claus/cal/julperiod.php
    // adjusted for Unix epoch (1970-01-01).
    let days = days_since_epoch(year, month, day)?;

    let total_secs: f64 = days as f64 * 86_400.0
        + hour as f64 * 3_600.0
        + minute as f64 * 60.0
        + sec as f64;

    // Sub-second fraction.
    let subsec: f64 = if frac.is_empty() {
        0.0
    } else {
        let n: f64 = frac.parse().ok()?;
        n / 10f64.powi(frac.len() as i32)
    };

    Some(total_secs + subsec)
}

/// Days between 1970-01-01 and the given date (UTC). Returns `None`
/// for obviously invalid dates.
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Shift Jan/Feb to previous year so the leap-day always falls at
    // the end of the adjusted year.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * m + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let jd = era * 146_097 + doe - 719_468; // days since 1970-01-01
    Some(jd)
}

// ---------------------------------------------------------------------------
// Incremental reader
// ---------------------------------------------------------------------------

/// Process new bytes appended to a JSONL file, updating `snapshot`
/// in place.
///
/// Returns `true` when at least one parseable entry was found and the
/// snapshot was mutated.
///
/// `tail` tracks the read position across calls so re-reads are
/// cheap — only newly-appended bytes are processed. If the file
/// shrinks (truncated / replaced) `tail.last_pos` is reset to 0 and
/// the file is re-read from the beginning.
///
/// Mirrors `ClaudeTelemetryService.TryRead` from the C# build.
pub fn process_new_lines(
    path: &Path,
    tail: &mut FileTail,
    snapshot: &mut Option<TelemetrySnapshot>,
    last_user_ts: &mut Option<f64>,
) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let file_len = meta.len();
    if file_len == tail.last_pos {
        return false;
    }

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("[claude_telemetry] cannot open {path:?}: {err}");
            return false;
        }
    };

    // Reset on file shrink (truncated / rewritten).
    if file_len < tail.last_pos {
        tail.last_pos = 0;
        *last_user_ts = None;
        *snapshot = None;
    }

    if let Err(err) = f.seek(SeekFrom::Start(tail.last_pos)) {
        eprintln!("[claude_telemetry] seek failed for {path:?}: {err}");
        return false;
    }

    let mut reader = BufReader::new(&mut f);

    // Pull the existing snapshot fields to mutate.
    let mut tokens_used = snapshot.as_ref().map_or(0, |s| s.tokens_used);
    let mut turn_count = snapshot.as_ref().map_or(0, |s| s.turn_count);
    let mut last_turn_duration = snapshot.as_ref().and_then(|s| s.last_turn_duration);
    let mut state = snapshot.as_ref().map_or(SessionState::Unknown, |s| s.state);
    let mut model: Option<String> = snapshot.as_ref().and_then(|s| s.model.clone());
    let mut changed = false;

    let mut line = String::new();
    let mut clean_eof = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                clean_eof = true;
                break;
            }
            Ok(_) => {}
            Err(err) => {
                // Read failure (transient mid-flush, sharing race,
                // truncation between stat and read…). Bail out
                // *without* advancing `last_pos` — a future poll
                // will retry from the same offset rather than
                // permanently skipping unread bytes.
                eprintln!("[claude_telemetry] read error for {path:?}: {err}");
                break;
            }
        }
        let entry = match parse_line(&line) {
            Some(e) => e,
            None => continue,
        };

        match entry.kind {
            EntryKind::User => {
                state = SessionState::Busy;
                // Anchor for the last-turn duration; tool-result
                // entries don't reset the anchor (they're internal
                // to a turn the user already kicked off).
                if !entry.user_carries_tool_result {
                    *last_user_ts = entry.timestamp_secs;
                }
                changed = true;
            }
            EntryKind::Assistant => {
                state = match entry.stop_reason.as_deref() {
                    Some("tool_use") => SessionState::PendingToolUse,
                    Some("end_turn") => SessionState::Idle,
                    _ => state,
                };

                let has_usage = entry.input_tokens > 0
                    || entry.output_tokens > 0
                    || entry.cache_creation_tokens > 0
                    || entry.cache_read_tokens > 0;

                if has_usage {
                    // Latch most-recent model; update context window.
                    if let Some(ref m) = entry.model {
                        if Some(m.as_str()) != model.as_deref() {
                            model = Some(m.clone());
                        }
                    }

                    // Overwrite, not accumulate — see doc comment on
                    // `TelemetrySnapshot::tokens_used`.
                    tokens_used = entry.input_tokens
                        + entry.cache_read_tokens
                        + entry.cache_creation_tokens
                        + entry.output_tokens;

                    // Mirror C# `ClaudeTelemetryService`: a *turn* is
                    // a completed assistant reply with usage, not a
                    // user prompt. Counting on user prompts would
                    // make the badge tick up the moment the user
                    // hits enter (even mid-compose in some clients)
                    // and would diverge from the Claude transcript's
                    // own definition.
                    turn_count += 1;

                    // Compute turn duration from last fresh user prompt.
                    if let (Some(user_ts), Some(asst_ts)) = (*last_user_ts, entry.timestamp_secs) {
                        if asst_ts > user_ts {
                            let secs = asst_ts - user_ts;
                            if secs >= 0.0 {
                                last_turn_duration =
                                    Some(Duration::from_secs_f64(secs));
                            }
                        }
                    }
                }

                changed = true;
            }
            EntryKind::Other => {}
        }
    }

    // Only advance `last_pos` on a clean walk to EOF. Aborted
    // mid-stream (read error) leaves the cursor where it was so the
    // next poll re-reads from the same offset.
    if clean_eof {
        tail.last_pos = file_len;
        tail.last_mtime = meta.modified().ok();
    }

    if changed {
        let context_window = model.as_deref().and_then(context_window_for_model);
        // Clamp to [0.0, 1.0] — the doc-comment promises the value
        // is a fraction in that range, and a token count above the
        // window cap (possible while the agent winds down a long
        // conversation) would otherwise leak >100% into the UI.
        let context_pct =
            context_window.map(|cap| (tokens_used as f32 / cap as f32).clamp(0.0, 1.0));
        *snapshot = Some(TelemetrySnapshot {
            model,
            tokens_used,
            context_pct,
            turn_count,
            last_turn_duration,
            state,
        });
    }

    changed
}

// ---------------------------------------------------------------------------
// High-level tail handle
// ---------------------------------------------------------------------------

/// Handle to a watched JSONL transcript. Tracks read position so
/// repeated `poll()` calls only read new bytes.
///
/// Mirrors the `Watch` inner class of `ClaudeTelemetryService`.
#[derive(Debug)]
pub struct TranscriptTail {
    /// Absolute path to the JSONL file.
    pub path: PathBuf,
    tail: FileTail,
    /// Timestamp of the most recent non-tool-result user entry, in
    /// seconds since the Unix epoch (from the transcript, not wall
    /// clock). Used to compute `last_turn_duration`.
    last_user_ts: Option<f64>,
    /// Latest computed snapshot, or `None` if no entries have been
    /// parsed yet.
    pub snapshot: Option<TelemetrySnapshot>,
}

impl TranscriptTail {
    /// Construct a tail for `path` and immediately do an initial read
    /// so existing transcript content is consumed before the first
    /// poll interval fires.
    pub fn new(path: PathBuf) -> Self {
        let mut tail = Self {
            path,
            tail: FileTail::default(),
            last_user_ts: None,
            snapshot: None,
        };
        tail.poll();
        tail
    }

    /// Build the transcript path from an absolute working directory
    /// and a Claude session id. Returns the path regardless of whether
    /// the file exists — the caller should handle missing-file
    /// gracefully via `poll()`.
    pub fn for_session(projects_root: &Path, working_directory: &str, session_id: &str) -> Self {
        let encoded = encode_cwd(working_directory);
        let path = projects_root
            .join(encoded)
            .join(format!("{session_id}.jsonl"));
        Self::new(path)
    }

    /// Check for new bytes in the file and update `snapshot`.
    ///
    /// Returns `true` when the snapshot changed.
    pub fn poll(&mut self) -> bool {
        process_new_lines(
            &self.path,
            &mut self.tail,
            &mut self.snapshot,
            &mut self.last_user_ts,
        )
    }

    /// Suggested poll interval for the next wake-up: 250 ms while the
    /// session is busy (Busy / PendingToolUse), 2 s while Idle or
    /// Unknown.
    pub fn poll_interval(&self) -> Duration {
        match self.snapshot.as_ref().map(|s| s.state) {
            Some(SessionState::Busy) | Some(SessionState::PendingToolUse) => {
                Duration::from_millis(250)
            }
            _ => Duration::from_secs(2),
        }
    }

    /// Format `last_turn_duration` as `m:ss` or `Xs` (mirrors the
    /// C# status-bar formatting).
    pub fn format_duration(d: Duration) -> String {
        let total = d.as_secs();
        if total < 60 {
            format!("{total}s")
        } else {
            format!("{}:{:02}", total / 60, total % 60)
        }
    }
}

/// Drop the `claude-` prefix and the `[1m]` extended-context suffix
/// so the status-bar model column reads `opus-4-7` rather than
/// `claude-opus-4-7[1m]`. Empty input falls back to "claude" so the
/// column never goes blank.
///
/// Mirrors the human-readable output of C#
/// `AgentProfile.DisplayName` for Claude profiles.
pub fn model_display_name(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return "claude".into();
    }
    let stripped = trimmed.strip_prefix("claude-").unwrap_or(trimmed);
    let bracket = stripped.find('[').unwrap_or(stripped.len());
    stripped[..bracket].trim_end_matches('-').to_owned()
}

/// Format a token count as `N`, `N.Mk`, `Mk`, `N.MM`, or `N.MM` so
/// the status bar stays readable at large context windows. Mirrors
/// C# `MainViewModel.FormatTokens`.
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

/// Format `0.0..=1.0` as `"3%"` / `"6.2%"` / `"87%"`. Mirrors C#
/// `MainViewModel.StatusTokensPercentText` rounding rules: one
/// decimal under 10%, integer rounding above.
pub fn format_context_pct(pct: f32) -> String {
    let p = (pct * 100.0).clamp(0.0, 999.0);
    if p >= 10.0 {
        format!("{:.0}%", p.round())
    } else {
        format!("{p:.1}%")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_line ---

    #[test]
    fn parse_assistant_with_usage_extracts_tokens() {
        let line = r#"{"type":"assistant","sessionId":"abc-123","timestamp":"2026-04-22T08:59:44.811Z","message":{"role":"assistant","usage":{"input_tokens":6,"cache_creation_input_tokens":20667,"cache_read_input_tokens":16850,"output_tokens":521}}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.kind, EntryKind::Assistant);
        assert_eq!(entry.input_tokens, 6);
        assert_eq!(entry.output_tokens, 521);
        assert_eq!(entry.cache_creation_tokens, 20667);
        assert_eq!(entry.cache_read_tokens, 16850);
        assert!(entry.timestamp_secs.is_some());
    }

    #[test]
    fn parse_user_plain_prompt_no_tool_result() {
        let line = r#"{"type":"user","sessionId":"abc-123","timestamp":"2026-04-22T08:59:44.811Z","message":{"role":"user","content":"hi"}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.kind, EntryKind::User);
        assert!(!entry.user_carries_tool_result);
        assert_eq!(entry.input_tokens, 0);
    }

    #[test]
    fn parse_user_tool_result_detected() {
        let line = r#"{"type":"user","sessionId":"abc-123","timestamp":"2026-04-22T08:59:44.811Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}}"#;
        let entry = parse_line(line).expect("should parse");
        assert!(entry.user_carries_tool_result);
    }

    #[test]
    fn parse_file_history_snapshot_returns_other_kind() {
        let line = r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.kind, EntryKind::Other);
    }

    #[test]
    fn parse_invalid_json_returns_none() {
        assert!(parse_line("{not json").is_none());
    }

    #[test]
    fn parse_empty_and_whitespace_returns_none() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   \t\n").is_none());
    }

    #[test]
    fn parse_model_extracted() {
        let line = r#"{"type":"assistant","sessionId":"m","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","model":"claude-opus-4-7[1m]","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.model.as_deref(), Some("claude-opus-4-7[1m]"));
        assert_eq!(entry.stop_reason.as_deref(), Some("end_turn"));
    }

    // --- encode_cwd ---

    #[test]
    fn encode_cwd_windows_path() {
        assert_eq!(encode_cwd(r"C:\dev\codescope"), "C--dev-codescope");
    }

    #[test]
    fn encode_cwd_with_dots() {
        assert_eq!(
            encode_cwd(r"C:\dev\codescope.worktrees\feat-x"),
            "C--dev-codescope-worktrees-feat-x"
        );
    }

    #[test]
    fn encode_cwd_unix_path() {
        assert_eq!(
            encode_cwd("/home/user/myrepo"),
            "-home-user-myrepo"
        );
    }

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

    // --- parse_iso8601 ---

    #[test]
    fn parse_iso8601_utc_z() {
        // 2026-04-22T08:59:44Z → known epoch offset
        // Verified: python3 -c "import datetime; print(int(datetime.datetime(2026,4,22,8,59,44,tzinfo=datetime.timezone.utc).timestamp()))"
        // → 1776848384
        let secs = parse_iso8601("2026-04-22T08:59:44Z");
        assert!(secs.is_some());
        assert_eq!(secs.unwrap() as u64, 1_776_848_384);
    }

    #[test]
    fn parse_iso8601_with_millis() {
        let secs = parse_iso8601("2026-04-22T08:59:44.811Z");
        assert!(secs.is_some());
        let s = secs.unwrap();
        assert!((s - 1_776_848_384.811).abs() < 0.001);
    }

    // --- process_new_lines / TelemetrySnapshot ---

    fn write_lines(path: &std::path::Path, lines: &[&str]) {
        let content = lines.join("\n") + "\n";
        std::fs::write(path, content).unwrap();
    }

    fn append_lines(path: &std::path::Path, lines: &[&str]) {
        use std::io::Write as _;
        let content = lines.join("\n") + "\n";
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn snapshot_after_user_assistant_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","sessionId":"s1","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"hi"}}"#,
                r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":50}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert!(changed);
        let s = snap.as_ref().unwrap();
        assert_eq!(s.turn_count, 1);
        // tokens_used = 10 + 50 + 100 + 20 = 180
        assert_eq!(s.tokens_used, 180);
        assert_eq!(s.state, SessionState::Idle);
        // duration = 60 seconds
        assert_eq!(
            s.last_turn_duration,
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn snapshot_state_pending_tool_use() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"run it"}}"#,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn snapshot_state_busy_when_last_entry_is_user() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"user","content":"follow-up"}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::Busy);
    }

    #[test]
    fn turn_count_counts_assistant_entries_with_usage() {
        // Mirrors the C# `ClaudeTelemetryService` rule: a *turn* is a
        // completed assistant reply with usage, not a user prompt.
        // The fixture below contains two assistant entries with
        // usage (one ending in tool_use, one in end_turn) and one
        // tool-result user entry that must NOT count as its own
        // turn — the latter is verified indirectly via
        // `last_turn_duration` anchoring on the last fresh user
        // prompt rather than the tool-result entry.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"do it"}}"#,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
                // Tool result — does not advance the
                // last-turn anchor and does not bump the counter.
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:01:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}}"#,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(path.as_path(), &mut tail, &mut snap, &mut last_user_ts);

        let s = snap.unwrap();
        // Two assistant-with-usage entries → two turns.
        assert_eq!(s.turn_count, 2);
        // Last turn duration is computed from the *fresh* user prompt
        // at 08:00:00, not the tool-result user entry at 08:01:05.
        // 08:02:00 − 08:00:00 = 120 s.
        assert_eq!(s.last_turn_duration, Some(Duration::from_secs(120)));
    }

    #[test]
    fn model_and_context_pct_extracted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[r#"{"type":"assistant","sessionId":"m","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","model":"claude-opus-4-7[1m]","stop_reason":"end_turn","usage":{"input_tokens":1000,"output_tokens":1000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        let s = snap.unwrap();
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-7[1m]"));
        assert_eq!(s.tokens_used, 2000);
        // 2000 / 1_000_000 = 0.002
        let pct = s.context_pct.unwrap();
        assert!((pct - 0.002).abs() < 1e-5);
    }

    #[test]
    fn incremental_read_only_processes_new_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        let snap1_tokens = snap.as_ref().unwrap().tokens_used;
        let pos_after_first = tail.last_pos;
        assert!(pos_after_first > 0);

        // Second poll — nothing new, should not change.
        let changed =
            process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);
        assert_eq!(snap.as_ref().unwrap().tokens_used, snap1_tokens);

        // Append a new assistant turn.
        append_lines(
            &path,
            &[r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"user","content":"more"}}"#,
              r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );

        let changed =
            process_new_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        // New tokens_used = 100 + 200 (overwrite, not accumulate).
        assert_eq!(snap.as_ref().unwrap().tokens_used, 300);
    }

    #[test]
    fn missing_file_returns_false_and_leaves_snapshot_unchanged() {
        let path = std::path::PathBuf::from("/nonexistent/path/session.jsonl");
        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);
        assert!(snap.is_none());
    }

    #[test]
    fn format_duration_under_minute() {
        assert_eq!(TranscriptTail::format_duration(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(
            TranscriptTail::format_duration(Duration::from_secs(134)),
            "2:14"
        );
    }

    #[test]
    fn format_duration_exactly_one_minute() {
        assert_eq!(
            TranscriptTail::format_duration(Duration::from_secs(60)),
            "1:00"
        );
    }

    // --- model_display_name ---

    #[test]
    fn model_display_name_strips_claude_prefix_and_bracket_tag() {
        assert_eq!(model_display_name("claude-opus-4-7[1m]"), "opus-4-7");
        assert_eq!(model_display_name("claude-sonnet-4-6"), "sonnet-4-6");
    }

    #[test]
    fn model_display_name_handles_no_prefix() {
        assert_eq!(model_display_name("custom-model"), "custom-model");
    }

    #[test]
    fn model_display_name_empty_falls_back_to_claude() {
        assert_eq!(model_display_name(""), "claude");
        assert_eq!(model_display_name("   "), "claude");
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

    // --- format_context_pct ---

    #[test]
    fn format_context_pct_under_10_uses_one_decimal() {
        assert_eq!(format_context_pct(0.0), "0.0%");
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
