//! Pi coding-agent transcript tail — per-session telemetry derived
//! from `~/.pi/agent/sessions/**/<timestamp>_<session-id>.jsonl`.
//!
//! Mirrors `PiTelemetryService` / `PiTranscriptParser` from the C#
//! build (`legacy:CodeScope.Core/Services/`). Data shapes match
//! [`crate::claude_telemetry`] so the status bar can render Pi
//! sessions through the same code paths.
//!
//! # Discovery model
//!
//! Pi names every session file `<timestamp>_<uuid>.jsonl` under
//! `~/.pi/agent/sessions/--<encoded-cwd>--/`. The timestamp prefix
//! isn't recoverable from the session id alone, and Pi's cwd→dir
//! encoding isn't reliably round-trippable on Windows — so the
//! locate step scans the configured root for any file whose stem
//! ends with `_<sid>` (UUIDs are unique). See [`locate_transcript`].
//!
//! # Polling strategy
//!
//! Same `stat`-first strategy as Claude / Copilot: when
//! `metadata.len()` matches `last_pos` there is nothing new to read.
//! The caller drives the poll cadence (250 ms while busy, 2 s while
//! idle); this module is pure logic with no background thread.
//!
//! # Differences from Claude transcripts
//!
//! Pi's on-disk schema is richer than Claude's. Top-level event types
//! we model:
//!
//! * `session` — header with the canonical session id + cwd. Used
//!   only by discovery (header peek); the parser drops it.
//! * `model_change` — root-level `modelId` + `provider`. Latches
//!   the model id and context-window cap.
//! * `message` — carries `message.role` + `message.stopReason` +
//!   `message.model` + `message.usage.{input,output,cacheRead,cacheWrite}`.
//!
//! Activity FSM (mirrors C# `PiTelemetryService.TryRead`):
//!
//! * `user` / `toolResult` → Busy. Only fresh `user` messages anchor
//!   `last_user_ts` — `toolResult` is mid-turn, resetting would cut
//!   the measured turn duration short.
//! * `assistant` + `stopReason: "tool_use"` → PendingToolUse.
//! * `assistant` + `stopReason: "stop"` / `"end_turn"` → Idle.
//!
//! Token total mirrors Claude: `input + cacheRead + cacheWrite +
//! output` overwrites on each assistant turn (not a running sum —
//! Pi's `input` already covers the full prior conversation).

use std::io::{BufRead as _, BufReader, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::telemetry::{FileTail, SessionState, TelemetrySnapshot, context_window_for_model};

// ---------------------------------------------------------------------------
// JSONL parser
// ---------------------------------------------------------------------------

/// One parsed line from a Pi `<ts>_<uuid>.jsonl` transcript. Mirrors
/// `PiTranscriptEntry` in the C# build.
#[derive(Debug, Default)]
struct Entry {
    /// Top-level `type`: `"session"`, `"message"`, `"model_change"`,
    /// or another extension-defined value we ignore.
    event_type: Option<String>,
    /// `message.role` for `message` events.
    role: Option<String>,
    timestamp_secs: Option<f64>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    /// `message.stopReason` (`"tool_use"`, `"stop"`, `"end_turn"`).
    stop_reason: Option<String>,
    /// `message.model` (on `message`) or root `modelId` (on
    /// `model_change`).
    model: Option<String>,
}

impl Entry {
    /// True when this entry carries assistant token usage. Mirrors
    /// `PiTranscriptEntry.HasUsage` in the C# build.
    fn has_usage(&self) -> bool {
        self.event_type.as_deref() == Some("message")
            && self.role.as_deref() == Some("assistant")
            && (self.input_tokens > 0
                || self.output_tokens > 0
                || self.cache_creation_tokens > 0
                || self.cache_read_tokens > 0)
    }
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
            eprintln!("[pi_telemetry] skipping malformed JSONL line: {err}");
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

    match entry.event_type.as_deref() {
        Some("model_change") => {
            entry.model = obj
                .get("modelId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
        }
        Some("message") => {
            if let Some(msg) = obj.get("message").and_then(Value::as_object) {
                entry.role = msg
                    .get("role")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                entry.stop_reason = msg
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                entry.model = msg
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                if let Some(usage) = msg.get("usage").and_then(Value::as_object) {
                    entry.input_tokens =
                        usage.get("input").and_then(Value::as_u64).unwrap_or(0);
                    entry.output_tokens =
                        usage.get("output").and_then(Value::as_u64).unwrap_or(0);
                    entry.cache_read_tokens =
                        usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0);
                    entry.cache_creation_tokens =
                        usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
                }
            }
        }
        _ => {}
    }

    Some(entry)
}

/// Pull the trailing UUID out of a Pi session-file name like
/// `2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.jsonl`.
/// Pi names every session file with a timestamp prefix and an
/// underscore separator before the UUID; the portion after the last
/// `_` minus the extension IS the session id. Returns `None` when the
/// file name doesn't match the convention or the trailing token isn't
/// a valid UUID — that lets discovery skip extension-created sidecar
/// files without picking up false positives.
///
/// Mirrors `PiTranscriptParser.ExtractSessionIdFromFileName` in the
/// C# build.
pub fn extract_session_id_from_file_name(file_name: &str) -> Option<String> {
    let trimmed = file_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Require a `.jsonl` extension explicitly (case-insensitive). The C#
    // version relies on its callers always passing pre-filtered
    // `*.jsonl` paths from `Directory.EnumerateFiles(..., "*.jsonl", ...)`,
    // so the parser itself is naive about extensions; callers in this
    // crate go through `locate_transcript` which is `.jsonl`-bounded too,
    // but exposing the function publicly means a non-JSONL sidecar
    // ending in `_<uuid>.<ext>` could otherwise yield a false positive.
    // Validating the suffix here keeps the public contract honest.
    let stem = strip_jsonl_extension(trimmed)?;
    let underscore = stem.rfind('_')?;
    if underscore == stem.len() - 1 {
        return None;
    }
    let id = &stem[underscore + 1..];
    if is_canonical_uuid(id) {
        Some(id.to_owned())
    } else {
        None
    }
}

/// Return the stem when `file_name` ends with `.jsonl` (case-insensitive),
/// otherwise `None`. Splits on the literal byte boundary so we don't have
/// to allocate a lowercase copy of the whole string.
fn strip_jsonl_extension(file_name: &str) -> Option<&str> {
    const EXT_LEN: usize = 6; // ".jsonl"
    if file_name.len() <= EXT_LEN {
        return None;
    }
    let (stem, ext) = file_name.split_at(file_name.len() - EXT_LEN);
    if ext.eq_ignore_ascii_case(".jsonl") {
        Some(stem)
    } else {
        None
    }
}

/// Validate the canonical 8-4-4-4-12 UUID form (hex digits + dashes).
/// Mirrors `Guid.TryParseExact("D")` in the C# build — we accept only
/// the exact dashed form Pi writes, no braces / parentheses.
fn is_canonical_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        let is_dash = matches!(i, 8 | 13 | 18 | 23);
        if is_dash {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Locate a Pi transcript by suffix-matching `*_<session_id>.jsonl`
/// under `sessions_root` (recursive). Returns the first matching path
/// or `None` when no file matches.
///
/// UUIDs are globally unique so the suffix match is unambiguous. The
/// scan is bounded to `.jsonl` files; sidecar / extension files with
/// other extensions are ignored.
///
/// Mirrors `PiTelemetryService.TryLocate` in the C# build (which uses
/// `Directory.EnumerateFiles(root, "*_<sid>.jsonl", AllDirectories)`).
pub fn locate_transcript(sessions_root: &Path, session_id: &str) -> Option<PathBuf> {
    if !sessions_root.exists() {
        return None;
    }
    let suffix = format!("_{session_id}.jsonl");
    locate_recursive(sessions_root, &suffix)
}

fn locate_recursive(dir: &Path, suffix: &str) -> Option<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return None,
    };
    // Walk children: collect dirs to recurse into after files (so a
    // shallow match wins over a deeper duplicate).
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str())
                && name.ends_with(suffix) {
                    return Some(path);
                }
        } else if file_type.is_dir() {
            subdirs.push(path);
        }
    }
    for sub in subdirs {
        if let Some(found) = locate_recursive(&sub, suffix) {
            return Some(found);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Incremental reader
// ---------------------------------------------------------------------------

/// Process new bytes appended to a Pi transcript file, updating
/// `snapshot` in place.
///
/// Returns `true` when at least one event drove a snapshot mutation.
///
/// Mirrors `PiTelemetryService.TryRead` from the C# build, with the
/// same `last_pos` retry semantics as
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
            eprintln!("[pi_telemetry] cannot open {path:?}: {err}");
            return false;
        }
    };

    if file_len < tail.last_pos {
        tail.last_pos = 0;
        *last_user_ts = None;
        *snapshot = None;
    }

    if let Err(err) = f.seek(SeekFrom::Start(tail.last_pos)) {
        eprintln!("[pi_telemetry] seek failed for {path:?}: {err}");
        return false;
    }

    let mut reader = BufReader::new(&mut f);

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
                eprintln!("[pi_telemetry] read error for {path:?}: {err}");
                break;
            }
        }
        let entry = match parse_line(&line) {
            Some(e) => e,
            None => continue,
        };

        // model_change: latch the model + cap as they arrive. Mirrors
        // C# `PiTelemetryService.TryRead` shortcut for this event type.
        if entry.event_type.as_deref() == Some("model_change") {
            if let Some(m) = entry.model.as_deref()
                && !m.is_empty() {
                    model = Some(m.to_owned());
                    changed = true;
                }
            continue;
        }

        if entry.event_type.as_deref() != Some("message") {
            continue;
        }

        match entry.role.as_deref() {
            Some("user") | Some("toolResult") => {
                state = SessionState::Busy;
                // Only fresh user turns anchor `last_user_ts` —
                // toolResult is mid-turn, resetting would cut the
                // measured turn duration short.
                if entry.role.as_deref() == Some("user")
                    && let Some(ts) = entry.timestamp_secs {
                        *last_user_ts = Some(ts);
                    }
                changed = true;
                continue;
            }
            Some("assistant") => {
                state = match entry.stop_reason.as_deref() {
                    Some("tool_use") => SessionState::PendingToolUse,
                    Some("stop") | Some("end_turn") => SessionState::Idle,
                    _ => state,
                };
                changed = true;
            }
            _ => continue,
        }

        if !entry.has_usage() {
            continue;
        }

        if let Some(ref m) = entry.model
            && Some(m.as_str()) != model.as_deref() {
                model = Some(m.clone());
            }

        // Overwrite, not accumulate — Pi's `input` already covers the
        // full prior conversation, mirroring Claude semantics.
        tokens_used = entry.input_tokens
            + entry.cache_read_tokens
            + entry.cache_creation_tokens
            + entry.output_tokens;
        turn_count += 1;

        if let (Some(user_ts), Some(asst_ts)) = (*last_user_ts, entry.timestamp_secs)
            && asst_ts > user_ts {
                let secs = asst_ts - user_ts;
                if secs >= 0.0 {
                    last_turn_duration = Some(Duration::from_secs_f64(secs));
                }
            }
    }

    if clean_eof {
        // Use the reader's stream position rather than the pre-read
        // `file_len` — append-only logs can grow during the read, and
        // pinning to a stale `file_len` would re-read (and double-count)
        // bytes that were already parsed. Mirrors the Copilot fix
        // already shipped in `copilot_telemetry`.
        let advanced_to = reader
            .stream_position()
            .unwrap_or(file_len)
            .max(file_len);
        tail.last_pos = advanced_to;
        tail.last_mtime = crate::telemetry::modified_or_none(&meta);
    }

    if changed {
        let context_window = model.as_deref().and_then(context_window_for_model);
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

/// Handle to a watched Pi transcript. Tracks read position so
/// repeated `poll()` calls only read new bytes.
///
/// Mirrors the `Watch` inner class of `PiTelemetryService`.
#[derive(Debug)]
pub struct PiTranscriptTail {
    /// Absolute path to the JSONL file.
    pub path: PathBuf,
    tail: FileTail,
    last_user_ts: Option<f64>,
    pub snapshot: Option<TelemetrySnapshot>,
}

impl PiTranscriptTail {
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

    /// Locate the transcript for `session_id` under `sessions_root`
    /// (suffix-match `*_<sid>.jsonl` recursively) and build a tail
    /// for it. Returns `None` when no matching file exists yet — the
    /// caller should retry on the next poll tick (covers the "session
    /// header not yet flushed" race).
    pub fn for_session(sessions_root: &Path, session_id: &str) -> Option<Self> {
        let path = locate_transcript(sessions_root, session_id)?;
        Some(Self::new(path))
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

    /// Suggested poll interval for the next wake-up: 250 ms while
    /// Busy / PendingToolUse, 2 s while Idle / Unknown. Mirrors the
    /// cadence used by Claude / Copilot tails.
    pub fn poll_interval(&self) -> Duration {
        match self.snapshot.as_ref().map(|s| s.state) {
            Some(SessionState::Busy) | Some(SessionState::PendingToolUse) => {
                Duration::from_millis(250)
            }
            _ => Duration::from_secs(2),
        }
    }
}

/// Default Pi sessions root: `<home>/.pi/agent/sessions`.
/// Mirrors `PiTelemetryService.DefaultSessionsRoot` from the C# build.
pub fn default_sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".pi").join("agent").join("sessions"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_session_id_from_file_name ---

    #[test]
    fn extract_sid_from_canonical_pi_file_name() {
        let name = "2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.jsonl";
        assert_eq!(
            extract_session_id_from_file_name(name).as_deref(),
            Some("f1e2d3c4-aaaa-bbbb-cccc-1234567890ab")
        );
    }

    #[test]
    fn extract_sid_rejects_non_uuid_suffix() {
        // Trailing token isn't a UUID → returns None so discovery
        // skips extension-created sidecar files.
        assert!(extract_session_id_from_file_name("2026-04-22T08-00-00-000Z_summary.jsonl").is_none());
    }

    #[test]
    fn extract_sid_rejects_missing_underscore() {
        assert!(extract_session_id_from_file_name("nounderscore.jsonl").is_none());
    }

    #[test]
    fn extract_sid_rejects_empty_and_blank() {
        assert!(extract_session_id_from_file_name("").is_none());
        assert!(extract_session_id_from_file_name("   ").is_none());
    }

    #[test]
    fn extract_sid_rejects_trailing_underscore() {
        assert!(extract_session_id_from_file_name("ts_.jsonl").is_none());
    }

    #[test]
    fn extract_sid_rejects_non_jsonl_extension() {
        // Sidecar with a UUID-shaped suffix but a non-JSONL extension
        // must NOT be adopted as a Pi transcript. Mirrors the C#
        // discovery pipeline's `*.jsonl` enumeration filter — keeps
        // the public contract honest without requiring callers to
        // pre-filter.
        let txt = "2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.txt";
        assert!(extract_session_id_from_file_name(txt).is_none());
        let log = "2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.log";
        assert!(extract_session_id_from_file_name(log).is_none());
    }

    #[test]
    fn extract_sid_jsonl_extension_is_case_insensitive() {
        // Windows-style upper/mixed case `.JSONL` / `.JsonL` must
        // still be accepted (FAT32-derived case insensitivity).
        let upper = "ts_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.JSONL";
        assert_eq!(
            extract_session_id_from_file_name(upper).as_deref(),
            Some("f1e2d3c4-aaaa-bbbb-cccc-1234567890ab")
        );
    }

    #[test]
    fn extract_sid_rejects_braced_uuid() {
        // Pi writes the dashed form only; braced/N forms are not
        // accepted — mirrors `Guid.TryParseExact("D")`.
        let braced = "ts_{f1e2d3c4-aaaa-bbbb-cccc-1234567890ab}.jsonl";
        assert!(extract_session_id_from_file_name(braced).is_none());
    }

    // --- locate_transcript ---

    #[test]
    fn locate_transcript_finds_suffix_match_in_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("--c-dev-codescope--");
        std::fs::create_dir_all(&sub).unwrap();
        let target = sub.join("2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.jsonl");
        std::fs::write(&target, b"").unwrap();
        // Distractor file in the same dir with a different suffix.
        std::fs::write(sub.join("note.jsonl"), b"").unwrap();

        let found = locate_transcript(root, "f1e2d3c4-aaaa-bbbb-cccc-1234567890ab").unwrap();
        assert_eq!(found, target);
    }

    #[test]
    fn locate_transcript_ignores_other_files_in_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("ts_aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl"),
            b"",
        )
        .unwrap();
        std::fs::write(root.join("ts_other-uuid.jsonl"), b"").unwrap();
        std::fs::write(root.join("random.txt"), b"").unwrap();

        // Suffix `_<sid>.jsonl` only matches the first file.
        let found =
            locate_transcript(root, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        assert!(found.file_name().unwrap().to_string_lossy().ends_with(
            "_aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl"
        ));
    }

    #[test]
    fn locate_transcript_returns_none_when_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("ts_unrelated.jsonl"), b"").unwrap();
        let result =
            locate_transcript(tmp.path(), "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert!(result.is_none());
    }

    #[test]
    fn locate_transcript_returns_none_when_root_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let result =
            locate_transcript(&nonexistent, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert!(result.is_none());
    }

    // --- parse_line ---

    #[test]
    fn parse_session_header() {
        let line = r#"{"type":"session","id":"f1e2d3c4-aaaa-bbbb-cccc-1234567890ab","cwd":"/tmp/x","timestamp":"2026-04-22T08:00:00Z"}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("session"));
        assert!(entry.timestamp_secs.is_some());
    }

    #[test]
    fn parse_model_change_extracts_model_id() {
        let line = r#"{"type":"model_change","timestamp":"2026-04-22T08:00:01Z","modelId":"claude-opus-4-7","provider":"anthropic"}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("model_change"));
        assert_eq!(entry.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn parse_message_assistant_with_usage() {
        let line = r#"{"type":"message","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stopReason":"end_turn","model":"claude-sonnet-4-6","usage":{"input":10,"output":20,"cacheRead":5,"cacheWrite":3}}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.event_type.as_deref(), Some("message"));
        assert_eq!(entry.role.as_deref(), Some("assistant"));
        assert_eq!(entry.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(entry.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(entry.input_tokens, 10);
        assert_eq!(entry.output_tokens, 20);
        assert_eq!(entry.cache_read_tokens, 5);
        assert_eq!(entry.cache_creation_tokens, 3);
        assert!(entry.has_usage());
    }

    #[test]
    fn parse_message_user_role() {
        let line = r#"{"type":"message","timestamp":"2026-04-22T08:00:30Z","message":{"role":"user"}}"#;
        let entry = parse_line(line).expect("should parse");
        assert_eq!(entry.role.as_deref(), Some("user"));
        assert!(!entry.has_usage());
    }

    #[test]
    fn parse_message_assistant_without_usage_returns_no_usage() {
        let line = r#"{"type":"message","timestamp":"2026-04-22T08:00:30Z","message":{"role":"assistant","stopReason":"tool_use"}}"#;
        let entry = parse_line(line).expect("should parse");
        assert!(!entry.has_usage());
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
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"session","id":"sid","cwd":"/tmp/x","timestamp":"2026-04-22T08:00:00Z"}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:00:10Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:01:10Z","message":{"role":"assistant","stopReason":"end_turn","model":"claude-sonnet-4-6","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert!(changed);
        let s = snap.as_ref().unwrap();
        assert_eq!(s.turn_count, 1);
        // tokens_used = input + cacheRead + cacheWrite + output.
        assert_eq!(s.tokens_used, 100 + 10 + 5 + 50);
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.last_turn_duration, Some(Duration::from_secs(60)));
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn pending_tool_use_when_assistant_stop_reason_is_tool_use() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:00:01Z","message":{"role":"assistant","stopReason":"tool_use","usage":{"input":5,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn tool_result_returns_to_busy_without_resetting_user_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:00:01Z","message":{"role":"assistant","stopReason":"tool_use","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:00:02Z","message":{"role":"toolResult"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":2,"output":2,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        let s = snap.unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.turn_count, 2);
        // last_turn_duration is from the original user prompt
        // (08:00:00) to the final assistant (08:01:00) — toolResult
        // must NOT reset the user anchor.
        assert_eq!(s.last_turn_duration, Some(Duration::from_secs(60)));
    }

    #[test]
    fn turn_count_only_advances_on_assistant_message_with_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#,
                // Assistant with no usage — must NOT bump the counter.
                r#"{"type":"message","timestamp":"2026-04-22T08:00:01Z","message":{"role":"assistant","stopReason":"tool_use"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":5,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().turn_count, 1);
    }

    #[test]
    fn model_change_event_latches_model() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"model_change","timestamp":"2026-04-22T08:00:00Z","modelId":"claude-opus-4-7","provider":"anthropic"}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        let s = snap.unwrap();
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn incremental_read_only_processes_new_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"session","id":"sid","cwd":"/tmp/x","timestamp":"2026-04-22T08:00:00Z"}"#,
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
                r#"{"type":"message","timestamp":"2026-04-22T08:01:00Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        assert_eq!(snap.as_ref().unwrap().turn_count, 1);
    }

    #[test]
    fn truncated_file_resets_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().turn_count, 1);

        // Truncate to a single fresh model_change event.
        std::fs::write(
            &path,
            r#"{"type":"model_change","timestamp":"2026-04-22T09:00:00Z","modelId":"claude-sonnet-4-6"}
"#,
        )
        .unwrap();

        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        let s = snap.unwrap();
        assert_eq!(s.turn_count, 0);
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn missing_file_returns_false_and_leaves_snapshot_unchanged() {
        let path = std::path::PathBuf::from("/nonexistent/pi/ts_sid.jsonl");
        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);
        assert!(snap.is_none());
    }

    /// Read partial / malformed lines must NOT advance the cursor
    /// past the unread bytes — mirrors the parity guarantee in
    /// `claude_telemetry::process_new_lines`. JSON parse errors on a
    /// fully-read line are skipped (line-by-line basis); IO errors
    /// during the read leave `last_pos` unchanged.
    #[test]
    fn parser_skips_malformed_lines_without_losing_subsequent_events() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#,
                r#"{not a json line"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:00:01Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(changed);
        assert_eq!(snap.as_ref().unwrap().turn_count, 1);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(tail.last_pos, len);
    }

    /// `last_pos` advances to the reader's actual stream position,
    /// not the pre-read `metadata.len()`. Mirrors the parity guarantee
    /// in `copilot_telemetry::process_new_lines`.
    #[test]
    fn cursor_advances_past_pre_read_file_len_when_log_grew_during_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-04-22T08:00:01Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );
        let len = std::fs::metadata(&path).unwrap().len();

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert!(
            tail.last_pos >= len,
            "last_pos {} should be >= file_len {}",
            tail.last_pos,
            len
        );

        let changed = process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);
    }

    // --- DST / timestamp formatting ---

    /// Cross-DST timestamps stay in UTC — same parity guarantee as
    /// Claude / Copilot.
    #[test]
    fn dst_boundary_durations_are_utc_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ts_sid.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"message","timestamp":"2026-03-29T00:55:00Z","message":{"role":"user"}}"#,
                r#"{"type":"message","timestamp":"2026-03-29T01:05:00Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(
            snap.unwrap().last_turn_duration,
            Some(Duration::from_secs(600))
        );
    }

    // --- PiTranscriptTail ---

    #[test]
    fn for_session_locates_and_builds_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("--c-dev-x--");
        std::fs::create_dir_all(&sub).unwrap();
        let sid = "f1e2d3c4-aaaa-bbbb-cccc-1234567890ab";
        let path = sub.join(format!("2026-04-22T08-00-00-000Z_{sid}.jsonl"));
        write_lines(
            &path,
            &[r#"{"type":"model_change","timestamp":"2026-04-22T08:00:00Z","modelId":"claude-sonnet-4-6"}"#],
        );

        let tail = PiTranscriptTail::for_session(root, sid).expect("should locate");
        assert_eq!(tail.path, path);
        assert_eq!(
            tail.snapshot.as_ref().unwrap().model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn for_session_returns_none_when_file_not_yet_written() {
        let tmp = tempfile::tempdir().unwrap();
        let result =
            PiTranscriptTail::for_session(tmp.path(), "f1e2d3c4-aaaa-bbbb-cccc-1234567890ab");
        assert!(result.is_none());
    }

    #[test]
    fn poll_interval_matches_state() {
        let tmp = tempfile::tempdir().unwrap();
        let busy_path = tmp.path().join("ts_busy.jsonl");
        write_lines(
            &busy_path,
            &[r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user"}}"#],
        );
        let tail = PiTranscriptTail::new(busy_path);
        assert_eq!(tail.poll_interval(), Duration::from_millis(250));

        let idle_path = tmp.path().join("ts_idle.jsonl");
        write_lines(
            &idle_path,
            &[r#"{"type":"message","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","stopReason":"end_turn","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0}}}"#],
        );
        let tail = PiTranscriptTail::new(idle_path);
        assert_eq!(tail.poll_interval(), Duration::from_secs(2));
    }
}
