//! Claude Code transcript tail — per-session telemetry derived from
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
//!
//! Mirrors `ClaudeTelemetryService` / `ClaudeTranscriptParser` /
//! `ClaudeModelCatalog` from the C# build
//! (`legacy:CodeScope.Core/Services/`). Data shapes and field names are
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
use std::time::Duration;

use serde_json::Value;

pub use crate::telemetry::{FileTail, SessionState, TelemetrySnapshot, context_window_for_model};

/// Default Claude Code projects root: `<home>/.claude/projects`.
/// Mirrors the sibling agents' `default_*_root` helpers so callers
/// that need a transcript path don't hand-roll the join.
pub fn default_projects_root() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

/// Absolute path of the transcript Claude Code writes for
/// `session_id` while running in `working_directory`:
/// `<projects_root>/<encoded-cwd>/<session_id>.jsonl`.
///
/// The single definition of that layout — the tail constructor and the
/// retention probe both go through it, so neither can drift.
pub fn transcript_path(projects_root: &Path, working_directory: &str, session_id: &str) -> PathBuf {
    projects_root
        .join(encode_cwd(working_directory))
        .join(format!("{session_id}.jsonl"))
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
        if kind == EntryKind::User
            && let Some(content) = msg.get("content").and_then(Value::as_array) {
                user_carries_tool_result = content.iter().any(|item| {
                    item.as_object()
                        .and_then(|o| o.get("type"))
                        .and_then(Value::as_str)
                        == Some("tool_result")
                });
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

/// Thin re-export of [`crate::time::parse_iso8601_secs`] kept under
/// the original name so existing call sites and tests don't churn.
/// The shared helper covers the same Claude-Code ISO-8601 subset
/// (UTC with `Z` / `+00:00` suffix, optional fractional seconds) and
/// is also used by [`crate::session`]; collapsing the two avoided
/// the drift risk Copilot flagged on PR #114.
fn parse_iso8601(s: &str) -> Option<f64> {
    crate::time::parse_iso8601_secs(s)
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
                    if let Some(ref m) = entry.model
                        && Some(m.as_str()) != model.as_deref() {
                            model = Some(m.clone());
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
                    if let (Some(user_ts), Some(asst_ts)) = (*last_user_ts, entry.timestamp_secs)
                        && asst_ts > user_ts {
                            let secs = asst_ts - user_ts;
                            if secs >= 0.0 {
                                last_turn_duration =
                                    Some(Duration::from_secs_f64(secs));
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
        tail.last_mtime = crate::telemetry::modified_or_none(&meta);
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
pub struct ClaudeTranscriptTail {
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

impl ClaudeTranscriptTail {
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
        Self::new(transcript_path(projects_root, working_directory, session_id))
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

    /// Format `last_turn_duration` for the status-bar turn-time
    /// segment. Mirrors C# `MainViewModel.FormatDuration`:
    /// - `< 10s` → one decimal, e.g. `"3.1s"`
    /// - `< 60s` → integer seconds, e.g. `"42s"`
    /// - `< 1h`  → `"2m 12s"`
    /// - else   → `"1h 4m"`
    pub fn format_duration(d: Duration) -> String {
        let secs = d.as_secs_f64();
        if secs < 10.0 {
            // C#'s `0.0` format uses banker's rounding but at this scale
            // the difference is invisible — keep it simple.
            return format!("{:.1}s", secs);
        }
        if secs < 60.0 {
            return format!("{}s", secs.floor() as u64);
        }
        if secs < 3600.0 {
            let m = (secs / 60.0).floor() as u64;
            let s = (secs % 60.0).floor() as u64;
            return format!("{m}m {s}s");
        }
        let h = (secs / 3600.0).floor() as u64;
        let m = ((secs % 3600.0) / 60.0).floor() as u64;
        format!("{h}h {m}m")
    }
}

/// Drop the `claude-` prefix and the `[1m]` extended-context suffix
/// so the status-bar model column reads `opus-4-7` rather than
/// `claude-opus-4-7[1m]`. Empty input falls back to "claude" so the
/// column never goes blank.
///
/// This is *not* a parity port of C# `AgentProfile.DisplayName` —
/// that returns a registry-supplied label like "Claude Code". This
/// function specifically shortens the JSONL `message.model` id for
/// the status-bar's right-cluster model slot; the C# build does the
/// same inline shortening when no agent profile is registered.
pub fn model_display_name(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return "claude".into();
    }
    let stripped = trimmed.strip_prefix("claude-").unwrap_or(trimmed);
    let bracket = stripped.find('[').unwrap_or(stripped.len());
    stripped[..bracket].trim_end_matches('-').to_owned()
}

pub use crate::telemetry::{format_context_pct, format_tokens};

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
    fn format_duration_under_10s_keeps_one_decimal() {
        // Mirrors C# `FormatDuration` "{seconds:0.0}s" branch.
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_millis(0)),
            "0.0s"
        );
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_millis(3_100)),
            "3.1s"
        );
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_millis(9_999)),
            "10.0s"
        );
    }

    #[test]
    fn format_duration_under_minute_uses_integer_seconds() {
        assert_eq!(ClaudeTranscriptTail::format_duration(Duration::from_secs(10)), "10s");
        assert_eq!(ClaudeTranscriptTail::format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(ClaudeTranscriptTail::format_duration(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn format_duration_minutes_use_m_s_format() {
        // C# emits `"{m}m {s}s"` — no leading zeros on seconds.
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_secs(60)),
            "1m 0s"
        );
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_secs(134)),
            "2m 14s"
        );
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_secs(3_599)),
            "59m 59s"
        );
    }

    #[test]
    fn format_duration_hours_use_h_m_format() {
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_secs(3_600)),
            "1h 0m"
        );
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_secs(3_900)),
            "1h 5m"
        );
        assert_eq!(
            ClaudeTranscriptTail::format_duration(Duration::from_secs(7_265)),
            "2h 1m"
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

    #[test]
    fn model_display_name_trims_trailing_dash_before_bracket() {
        // A trailing `-` directly before the `[1m]` tag would otherwise
        // produce `"opus-"` — guard against that for any future ids
        // that hyphenate before the context-window suffix.
        assert_eq!(model_display_name("claude-opus-[1m]"), "opus");
    }

    // format_tokens / format_context_pct / context_window_for_model
    // tests live next to the functions in `crate::telemetry`.
}
