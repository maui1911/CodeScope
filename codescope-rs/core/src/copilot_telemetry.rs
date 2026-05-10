//! Copilot CLI transcript tail — per-session telemetry derived from
//! `~/.copilot/session-state/<session-id>/events.jsonl`.
//!
//! Mirrors `CopilotTelemetryService` / `CopilotTranscriptParser` from
//! the C# build (`src/CodeScope.Core/Services/`). Data shapes match
//! [`crate::claude_telemetry`] so the status bar can render Copilot
//! sessions through the same code paths.
//!
//! # Polling strategy
//!
//! Same `stat`-first strategy as Claude: when `metadata.len()` matches
//! `last_pos` there is nothing new to read. Caller drives the poll
//! cadence (250 ms while the agent is busy, 2 s while idle); this
//! module is pure logic with no background thread.
//!
//! # Differences from Claude transcripts
//!
//! Copilot's events.jsonl uses event-typed records keyed by a top-level
//! `type`:
//!
//! * `session.start` — carries the selected model + cwd. Latches the
//!   model id and context window cap.
//! * `user.message` — anchors the start of a turn for duration
//!   accounting. Sets activity to Composing.
//! * `assistant.turn_start` / `tool.execution_start` — keep activity at
//!   Composing during multi-tool turns.
//! * `assistant.message` — bumps the turn counter when it carries
//!   `outputTokens > 0`. `toolRequests: [...]` flips activity to
//!   PendingToolUse.
//! * `tool.execution_complete` — back to Composing while the agent
//!   resumes streaming.
//! * `assistant.turn_end` / `session.shutdown` — Idle.
//!
//! Token-context handling: Copilot only reports per-turn `outputTokens`
//! during the session and a cumulative breakdown in `session.shutdown`.
//! The C# build deliberately leaves `tokens_used` at 0 for live
//! sessions because a running sum of just `outputTokens` is misleading
//! as a "context used" indicator. We mirror that — the status-bar
//! token cell stays hidden for Copilot sessions until shutdown lands.

use std::io::{BufRead as _, BufReader, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::claude_telemetry::{FileTail, SessionState, TelemetrySnapshot, context_window_for_model};

// ---------------------------------------------------------------------------
// JSONL parser
// ---------------------------------------------------------------------------

/// One parsed event from a Copilot `events.jsonl` transcript. Mirrors
/// `CopilotTranscriptEntry` in the C# build.
#[derive(Debug, Default)]
struct Entry {
    event_type: Option<String>,
    timestamp_secs: Option<f64>,
    /// `data.outputTokens` — only set on `assistant.message`.
    output_tokens: u64,
    /// `data.selectedModel` — only set on `session.start`.
    model: Option<String>,
    /// `true` when `data.toolRequests` is a non-empty array.
    has_tool_requests: bool,
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
            eprintln!("[copilot_telemetry] skipping malformed JSONL line: {err}");
            return None;
        }
    };
    let obj = v.as_object()?;

    let event_type = obj
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let timestamp_secs = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(crate::time::parse_iso8601_secs);

    let mut entry = Entry {
        event_type,
        timestamp_secs,
        ..Entry::default()
    };

    let data = match obj.get("data").and_then(Value::as_object) {
        Some(d) => d,
        None => return Some(entry),
    };

    match entry.event_type.as_deref() {
        Some("session.start") => {
            entry.model = data
                .get("selectedModel")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
        }
        Some("assistant.message") => {
            entry.output_tokens = data
                .get("outputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            entry.has_tool_requests = data
                .get("toolRequests")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty());
        }
        _ => {}
    }

    Some(entry)
}

// ---------------------------------------------------------------------------
// Incremental reader
// ---------------------------------------------------------------------------

/// Process new bytes appended to a Copilot `events.jsonl` file,
/// updating `snapshot` in place.
///
/// Returns `true` when at least one event drove a snapshot mutation.
///
/// Mirrors `CopilotTelemetryService.TryRead` from the C# build, with
/// the same `last_pos` retry semantics as
/// [`crate::claude_telemetry::process_new_lines`]: read failures bail
/// without advancing the cursor, so the next poll re-reads from the
/// same offset rather than permanently skipping bytes.
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
            eprintln!("[copilot_telemetry] cannot open {path:?}: {err}");
            return false;
        }
    };

    if file_len < tail.last_pos {
        tail.last_pos = 0;
        *last_user_ts = None;
        *snapshot = None;
    }

    if let Err(err) = f.seek(SeekFrom::Start(tail.last_pos)) {
        eprintln!("[copilot_telemetry] seek failed for {path:?}: {err}");
        return false;
    }

    let mut reader = BufReader::new(&mut f);

    // Tokens stay at 0 for live Copilot sessions — see module docs.
    let tokens_used = snapshot.as_ref().map_or(0, |s| s.tokens_used);
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
                eprintln!("[copilot_telemetry] read error for {path:?}: {err}");
                break;
            }
        }
        let entry = match parse_line(&line) {
            Some(e) => e,
            None => continue,
        };

        match entry.event_type.as_deref() {
            Some("session.start") => {
                if let Some(m) = entry.model.as_deref() {
                    model = Some(m.to_owned());
                }
                changed = true;
            }
            Some("user.message") => {
                state = SessionState::Busy;
                if let Some(ts) = entry.timestamp_secs {
                    *last_user_ts = Some(ts);
                }
                changed = true;
            }
            Some("assistant.turn_start") => {
                if state != SessionState::Busy {
                    state = SessionState::Busy;
                    changed = true;
                }
            }
            Some("assistant.message") => {
                if entry.has_tool_requests {
                    state = SessionState::PendingToolUse;
                }
                // Don't flip to Idle here — assistant.turn_end does that.
                changed = true;

                if entry.output_tokens > 0 {
                    turn_count += 1;
                    if let Some(ts) = entry.timestamp_secs {
                        if let Some(user_ts) = *last_user_ts {
                            if ts > user_ts {
                                let secs = ts - user_ts;
                                if secs >= 0.0 {
                                    last_turn_duration = Some(Duration::from_secs_f64(secs));
                                }
                            }
                        }
                    }
                }
            }
            Some("tool.execution_start") => {
                if state == SessionState::PendingToolUse {
                    // Tool now executing — still pending more tools or composing.
                    changed = true;
                }
            }
            Some("tool.execution_complete") => {
                state = SessionState::Busy;
                changed = true;
            }
            Some("assistant.turn_end") | Some("session.shutdown") => {
                state = SessionState::Idle;
                changed = true;
            }
            _ => {}
        }
    }

    if clean_eof {
        tail.last_pos = file_len;
        tail.last_mtime = meta.modified().ok();
    }

    if changed {
        let context_window = model.as_deref().and_then(context_window_for_model);
        // tokens_used stays at 0 for live Copilot sessions — the status
        // bar hides the cell when the value is 0. See module docs.
        let context_pct = context_window
            .map(|cap| (tokens_used as f32 / cap as f32).clamp(0.0, 1.0));
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

/// Handle to a watched Copilot `events.jsonl` transcript. Tracks read
/// position so repeated `poll()` calls only read new bytes.
///
/// Mirrors the `Watch` inner class of `CopilotTelemetryService`.
#[derive(Debug)]
pub struct CopilotTranscriptTail {
    pub path: PathBuf,
    tail: FileTail,
    last_user_ts: Option<f64>,
    pub snapshot: Option<TelemetrySnapshot>,
}

impl CopilotTranscriptTail {
    /// Construct a tail for `path` and immediately do an initial read
    /// so any existing transcript content is consumed before the first
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

    /// Build the transcript path from a session-state root and a
    /// session id. Returns the path regardless of whether the file
    /// exists — the caller should handle missing-file gracefully via
    /// `poll()`.
    ///
    /// Default root is `~/.copilot/session-state/`; use [`default_session_state_root`].
    pub fn for_session(session_state_root: &Path, session_id: &str) -> Self {
        let path = session_state_root.join(session_id).join("events.jsonl");
        Self::new(path)
    }

    /// Check for new bytes in the file and update `snapshot`.
    /// Returns `true` when the snapshot changed.
    pub fn poll(&mut self) -> bool {
        process_new_lines(
            &self.path,
            &mut self.tail,
            &mut self.snapshot,
            &mut self.last_user_ts,
        )
    }

    /// Suggested poll interval for the next wake-up. 250 ms while the
    /// session is Busy / PendingToolUse, 2 s while Idle / Unknown.
    /// Mirrors the cadence used by [`crate::claude_telemetry::TranscriptTail`].
    pub fn poll_interval(&self) -> Duration {
        match self.snapshot.as_ref().map(|s| s.state) {
            Some(SessionState::Busy) | Some(SessionState::PendingToolUse) => {
                Duration::from_millis(250)
            }
            _ => Duration::from_secs(2),
        }
    }
}

/// Default Copilot session-state root: `<home>/.copilot/session-state`.
/// Mirrors `CopilotTelemetryService.DefaultSessionStateRoot` from the C# build.
pub fn default_session_state_root() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".copilot").join("session-state"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::iso8601_from_unix_secs;

    // --- parse_line ---

    #[test]
    fn parse_session_start_extracts_model() {
        let line = r#"{"type":"session.start","timestamp":"2026-04-22T08:00:00Z","data":{"sessionId":"abc","selectedModel":"gpt-5","context":{"cwd":"/tmp/x"}}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("session.start"));
        assert_eq!(entry.model.as_deref(), Some("gpt-5"));
        assert!(entry.timestamp_secs.is_some());
    }

    #[test]
    fn parse_assistant_message_extracts_output_tokens_and_tool_requests() {
        let line = r#"{"type":"assistant.message","timestamp":"2026-04-22T08:01:00Z","data":{"outputTokens":42,"toolRequests":[{"name":"shell"}]}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("assistant.message"));
        assert_eq!(entry.output_tokens, 42);
        assert!(entry.has_tool_requests);
    }

    #[test]
    fn parse_assistant_message_without_tool_requests() {
        let line = r#"{"type":"assistant.message","timestamp":"2026-04-22T08:01:00Z","data":{"outputTokens":10}}"#;
        let entry = parse_line(line).expect("should parse");
        assert!(!entry.has_tool_requests);
        assert_eq!(entry.output_tokens, 10);
    }

    #[test]
    fn parse_user_message_returns_event_type_only() {
        let line = r#"{"type":"user.message","timestamp":"2026-04-22T08:00:30Z","data":{"text":"hi"}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("user.message"));
        assert_eq!(entry.output_tokens, 0);
        assert!(!entry.has_tool_requests);
    }

    #[test]
    fn parse_invalid_json_returns_none() {
        assert!(parse_line("{not json").is_none());
    }

    #[test]
    fn parse_empty_and_whitespace_returns_none() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   \t").is_none());
    }

    #[test]
    fn parse_event_without_data_object_returns_event_type() {
        let line = r#"{"type":"assistant.turn_end","timestamp":"2026-04-22T08:02:00Z"}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("assistant.turn_end"));
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
    fn snapshot_after_user_assistant_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"session.start","timestamp":"2026-04-22T08:00:00Z","data":{"sessionId":"s","selectedModel":"claude-sonnet-4-6"}}"#,
                r#"{"type":"user.message","timestamp":"2026-04-22T08:00:10Z","data":{"text":"hi"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:01:10Z","data":{"outputTokens":15}}"#,
                r#"{"type":"assistant.turn_end","timestamp":"2026-04-22T08:01:11Z"}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert!(changed);
        let s = snap.as_ref().unwrap();
        assert_eq!(s.turn_count, 1);
        // Live Copilot sessions: tokens_used stays 0 by design.
        assert_eq!(s.tokens_used, 0);
        assert_eq!(s.state, SessionState::Idle);
        // 60 seconds between user.message and assistant.message.
        assert_eq!(s.last_turn_duration, Some(Duration::from_secs(60)));
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn pending_tool_use_when_assistant_message_carries_tool_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-04-22T08:00:00Z","data":{"text":"go"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:00:01Z","data":{"outputTokens":5,"toolRequests":[{"name":"shell"}]}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn tool_execution_complete_returns_to_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-04-22T08:00:00Z","data":{"text":"go"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:00:01Z","data":{"outputTokens":5,"toolRequests":[{"name":"shell"}]}}"#,
                r#"{"type":"tool.execution_complete","timestamp":"2026-04-22T08:00:02Z"}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::Busy);
    }

    #[test]
    fn turn_count_only_advances_on_assistant_message_with_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-04-22T08:00:00Z","data":{"text":"a"}}"#,
                // outputTokens 0 — must NOT bump the counter.
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:00:01Z","data":{"outputTokens":0}}"#,
                r#"{"type":"user.message","timestamp":"2026-04-22T08:01:00Z","data":{"text":"b"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:01:01Z","data":{"outputTokens":7}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:01:02Z","data":{"outputTokens":3}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        let s = snap.unwrap();
        assert_eq!(s.turn_count, 2);
    }

    #[test]
    fn incremental_read_only_processes_new_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"session.start","timestamp":"2026-04-22T08:00:00Z","data":{"sessionId":"s","selectedModel":"claude-sonnet-4-6"}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        let pos_after_first = tail.last_pos;
        assert!(pos_after_first > 0);

        // Idle-tick: no growth, no work.
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);

        append_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-04-22T08:01:00Z","data":{"text":"go"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:02:00Z","data":{"outputTokens":1}}"#,
            ],
        );
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        assert_eq!(snap.as_ref().unwrap().turn_count, 1);
    }

    #[test]
    fn truncated_file_resets_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-04-22T08:00:00Z","data":{"text":"go"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:01:00Z","data":{"outputTokens":1}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().turn_count, 1);

        // Truncate file to a single fresh start event.
        std::fs::write(
            &path,
            r#"{"type":"session.start","timestamp":"2026-04-22T09:00:00Z","data":{"selectedModel":"claude-sonnet-4-6"}}
"#,
        )
        .unwrap();

        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        // Snapshot was reset before reapplying the new file.
        let s = snap.unwrap();
        assert_eq!(s.turn_count, 0);
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn missing_file_returns_false_and_leaves_snapshot_unchanged() {
        let path = std::path::PathBuf::from("/nonexistent/copilot/events.jsonl");
        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);
        assert!(snap.is_none());
    }

    /// Read errors must leave `last_pos` unchanged so the next poll
    /// retries from the same offset rather than skipping bytes.
    /// Mirrors the parity guarantee in `claude_telemetry::process_new_lines`.
    #[test]
    fn parser_skips_malformed_lines_without_advancing_past_them() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        // Mix a malformed line in between two valid ones — the
        // malformed line is skipped via parse_line returning None
        // (NOT a read error), so last_pos still advances to EOF after
        // a clean walk. This is the expected behaviour: a read error
        // (e.g. mid-flush IO) is what holds the cursor; a JSON parse
        // failure is logged and skipped.
        write_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-04-22T08:00:00Z","data":{"text":"go"}}"#,
                r#"{not a json line"#,
                r#"{"type":"assistant.message","timestamp":"2026-04-22T08:00:01Z","data":{"outputTokens":1}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        assert_eq!(snap.as_ref().unwrap().turn_count, 1);
        // Cursor advanced fully past EOF.
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(tail.last_pos, len);
    }

    // --- DST / timestamp formatting ---

    /// Cross-DST timestamps in the same JSONL are parsed as raw UTC,
    /// so a session that crosses a DST boundary computes
    /// last_turn_duration purely from UTC seconds (no local-zone shift).
    /// Mirrors how Claude telemetry handles transcript timestamps.
    #[test]
    fn dst_boundary_durations_are_utc_only() {
        // Europe/Amsterdam DST starts 2026-03-29 02:00 local → 03:00 local
        // (UTC wallclock 01:00 → unchanged). The transcript writes UTC,
        // so a turn that brackets the boundary should still report the
        // raw UTC delta.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user.message","timestamp":"2026-03-29T00:55:00Z","data":{"text":"go"}}"#,
                r#"{"type":"assistant.message","timestamp":"2026-03-29T01:05:00Z","data":{"outputTokens":1}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        // 10 minutes — not 70 minutes (would be the wrong direction
        // if local-zone arithmetic crept in).
        assert_eq!(
            snap.unwrap().last_turn_duration,
            Some(Duration::from_secs(600))
        );
    }

    /// Round-trip a wall-clock timestamp through the shared time
    /// helpers — keeps Copilot telemetry's date math tied to the same
    /// DST-correct path used by `claude_telemetry`.
    #[test]
    fn timestamp_round_trip_is_stable() {
        let secs = crate::time::parse_iso8601_secs("2026-04-22T08:01:00Z").unwrap() as i64;
        let printed = iso8601_from_unix_secs(secs);
        assert_eq!(printed, "2026-04-22T08:01:00Z");
    }

    // --- CopilotTranscriptTail ---

    #[test]
    fn for_session_builds_expected_path() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "abc-123";
        let session_dir = tmp.path().join(session_id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let events_path = session_dir.join("events.jsonl");
        write_lines(
            &events_path,
            &[r#"{"type":"session.start","timestamp":"2026-04-22T08:00:00Z","data":{"selectedModel":"claude-sonnet-4-6"}}"#],
        );

        let tail = CopilotTranscriptTail::for_session(tmp.path(), session_id);
        assert_eq!(tail.path, events_path);
        assert_eq!(
            tail.snapshot.as_ref().unwrap().model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn poll_interval_matches_state() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = tmp.path().join("s");
        std::fs::create_dir_all(&session_dir).unwrap();
        let events_path = session_dir.join("events.jsonl");
        write_lines(
            &events_path,
            &[r#"{"type":"user.message","timestamp":"2026-04-22T08:00:00Z","data":{"text":"go"}}"#],
        );

        let tail = CopilotTranscriptTail::for_session(tmp.path(), "s");
        assert_eq!(tail.poll_interval(), Duration::from_millis(250));

        let idle_dir = tmp.path().join("idle");
        std::fs::create_dir_all(&idle_dir).unwrap();
        let idle_path = idle_dir.join("events.jsonl");
        write_lines(
            &idle_path,
            &[r#"{"type":"assistant.turn_end","timestamp":"2026-04-22T08:00:00Z"}"#],
        );
        let tail = CopilotTranscriptTail::for_session(tmp.path(), "idle");
        assert_eq!(tail.poll_interval(), Duration::from_secs(2));
    }
}
