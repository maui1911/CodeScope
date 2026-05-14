//! OpenCode CLI message-file tail — per-session telemetry derived from
//! `~/.local/share/opencode/project/<slug>/storage/message/<sessionId>/msg_*.json`.
//!
//! Mirrors `OpenCodeTelemetryService` / `OpenCodeMessageParser` from
//! the C# build (`src/CodeScope.Core/Services/`). Data shapes match
//! [`crate::claude_telemetry`] so the status bar can render OpenCode
//! sessions through the same code paths.
//!
//! # Storage shape — different from Claude / Copilot
//!
//! Claude and Copilot both append to a single JSONL file at a known
//! path. OpenCode persists each message as its own JSON file under a
//! per-project, per-session directory. The slug between `project/` and
//! `storage/` is derived from the project path (git slug or `global`
//! for non-git), so the service searches **recursively** for the
//! `message/<sessionId>/` directory rather than predicting the slug.
//!
//! # Polling strategy — mtime watermark
//!
//! OpenCode never modifies a message file once written, so each file
//! is parsed at most once. Files are filtered by mtime against a
//! [`SessionWatch::mtime_watermark`] snapshotted at loop entry;
//! same-tick siblings are disambiguated by [`SessionWatch::seen_at_watermark`]
//! holding only files exactly at the watermark instant. The watermark
//! advances **after** the walk because directory enumeration is not
//! mtime-ordered: advancing mid-loop would skip an older sibling
//! returned later. Mirrors the C# implementation comments verbatim.
//!
//! Adaptive poll cadence: 250 ms while busy / pending tool, 2 s while
//! idle — same as Claude / Copilot.
//!
//! # Tokens
//!
//! `tokens_used` (mirrors C# `ContextTokens`) is the most-recent
//! assistant-with-usage's `input + output + reasoning + cache.read +
//! cache.write` — a snapshot of the last turn, not a running sum.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::claude_telemetry::{SessionState, TelemetrySnapshot, context_window_for_model};

// ---------------------------------------------------------------------------
// Per-message parser
// ---------------------------------------------------------------------------

/// One parsed OpenCode message file. Mirrors `OpenCodeMessageEntry`
/// in the C# build.
///
/// Unlike Claude / Copilot (append-only JSONL), OpenCode persists each
/// message as its own JSON file, so the unit of parse is one whole file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageEntry {
    pub id: Option<String>,
    pub session_id: Option<String>,
    /// `"user"` or `"assistant"`.
    pub role: Option<String>,
    /// `metadata.time.created` as Unix seconds (sub-second precision).
    pub created_at_secs: Option<f64>,
    /// `metadata.time.completed` as Unix seconds. Only present on
    /// finished assistant turns.
    pub completed_at_secs: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub model_id: Option<String>,
    pub provider_id: Option<String>,
    pub cwd: Option<String>,
    /// True when at least one `parts[]` entry is a tool-invocation in
    /// `state ∈ {"call","partial-call"}` — i.e. the agent is mid-call.
    pub has_pending_tool_call: bool,
}

impl MessageEntry {
    /// True when this is an assistant turn carrying token usage.
    /// Mirrors C# `OpenCodeMessageEntry.HasUsage`.
    pub fn has_usage(&self) -> bool {
        self.role.as_deref() == Some("assistant")
            && (self.input_tokens > 0
                || self.output_tokens > 0
                || self.reasoning_tokens > 0
                || self.cache_read_tokens > 0
                || self.cache_write_tokens > 0)
    }

    /// Total in-context tokens. Mirrors C# `OpenCodeMessageEntry.ContextTokens`:
    /// `input + output + reasoning + cache.read + cache.write`.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.reasoning_tokens
            + self.cache_read_tokens
            + self.cache_write_tokens
    }
}

/// Parse a single OpenCode `msg_*.json` file's contents. Returns
/// `None` for blank input or unparseable JSON. Mirrors
/// `OpenCodeMessageParser.ParseContent` in the C# build.
pub fn parse_content(content: &str) -> Option<MessageEntry> {
    if content.trim().is_empty() {
        return None;
    }
    let v: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(err) => {
            // OpenCode writes whole files atomically (rename-into-place),
            // so partial reads are rare but still possible during a save
            // race. Caller (`process_message_dir`) retries next tick.
            eprintln!("[opencode_telemetry] skipping malformed JSON: {err}");
            return None;
        }
    };
    let obj = v.as_object()?;

    let id = string_field(obj, "id");
    let role = string_field(obj, "role");

    let mut entry = MessageEntry {
        id,
        role: role.clone(),
        ..MessageEntry::default()
    };

    if let Some(meta) = obj.get("metadata").and_then(Value::as_object) {
        entry.session_id = string_field(meta, "sessionID");

        if let Some(time) = meta.get("time").and_then(Value::as_object) {
            entry.created_at_secs = unix_ms_field(time, "created");
            entry.completed_at_secs = unix_ms_field(time, "completed");
        }

        if let Some(asst) = meta.get("assistant").and_then(Value::as_object) {
            entry.model_id = string_field(asst, "modelID");
            entry.provider_id = string_field(asst, "providerID");
            if let Some(path) = asst.get("path").and_then(Value::as_object) {
                entry.cwd = string_field(path, "cwd");
            }
            if let Some(tokens) = asst.get("tokens").and_then(Value::as_object) {
                entry.input_tokens = u64_field(tokens, "input");
                entry.output_tokens = u64_field(tokens, "output");
                entry.reasoning_tokens = u64_field(tokens, "reasoning");
                if let Some(cache) = tokens.get("cache").and_then(Value::as_object) {
                    entry.cache_read_tokens = u64_field(cache, "read");
                    entry.cache_write_tokens = u64_field(cache, "write");
                }
            }
        }
    }

    // Pending tool detection: any `tool-invocation` part whose
    // `toolInvocation.state` is not `"result"` means the agent is mid-call.
    // Conservative: only flag pending if assistant role; user messages can
    // carry tool parts in some shapes but never wait. Mirrors C# logic.
    if role.as_deref() == Some("assistant") {
        if let Some(parts) = obj.get("parts").and_then(Value::as_array) {
            for part in parts {
                let Some(part_obj) = part.as_object() else {
                    continue;
                };
                if string_field(part_obj, "type").as_deref() != Some("tool-invocation") {
                    continue;
                }
                let Some(inv) = part_obj.get("toolInvocation").and_then(Value::as_object) else {
                    continue;
                };
                match string_field(inv, "state").as_deref() {
                    Some("call") | Some("partial-call") => {
                        entry.has_pending_tool_call = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    Some(entry)
}

/// Pull the message id out of a filename like `msg_<id>.json`.
/// Returns `None` for non-conforming names. Mirrors C#
/// `OpenCodeMessageParser.ExtractMessageIdFromFileName`.
pub fn extract_message_id_from_file_name(file_name: &str) -> Option<String> {
    let stem = Path::new(file_name).file_stem()?.to_str()?;
    if stem.len() <= 4 || !stem.starts_with("msg_") {
        return None;
    }
    Some(stem[4..].to_owned())
}

fn string_field(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_field(obj: &serde_json::Map<String, Value>, key: &str) -> u64 {
    obj.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Read a Unix-ms numeric field and convert to seconds (sub-second
/// precision). Mirrors `OpenCodeMessageParser.ReadUnixMs`.
fn unix_ms_field(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Option<f64> {
    let v = obj.get(key)?;
    if !v.is_number() {
        return None;
    }
    v.as_i64().map(|ms| ms as f64 / 1_000.0)
}

// ---------------------------------------------------------------------------
// Recursive locate: project/<slug>/storage/message/<sessionId>
// ---------------------------------------------------------------------------

/// Walk `data_root` recursively looking for a directory named `session_id`
/// whose **parent directory is named `message`** — guards against a
/// sibling project's directory that happens to embed the session id in
/// its name. Mirrors C# `OpenCodeTelemetryService.TryLocateMessageDir`.
///
/// Returns `None` when the directory doesn't exist (yet). Caller is
/// expected to throttle retries — the default service applies a 2 s TTL
/// on misses.
pub fn try_locate_message_dir(data_root: &Path, session_id: &str) -> Option<PathBuf> {
    if !data_root.is_dir() {
        return None;
    }
    locate_recursive(data_root, session_id).ok().flatten()
}

fn locate_recursive(dir: &Path, session_id: &str) -> std::io::Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // ACL / transient deletion mid-walk — same swallow-and-continue
        // posture as the C# version.
        Err(_) => return Ok(None),
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name == session_id {
            // Verify the parent segment is "message" — same path-segment
            // guard as the C# locate.
            if let Some(parent) = path.parent() {
                if parent
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.eq_ignore_ascii_case("message"))
                    .unwrap_or(false)
                {
                    return Ok(Some(path));
                }
            }
        }
        if let Some(found) = locate_recursive(&path, session_id)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Per-session watch state
// ---------------------------------------------------------------------------

/// Mutable state for a single OpenCode session being tailed.
///
/// Replaces the C# `Watch` inner class. Three running aggregates
/// (`last_entry`, `last_user`, `last_assistant_with_usage`) cover the
/// snapshot — no per-message retention. `turn_count` is incremented
/// once per newly-parsed `has_usage` entry.
#[derive(Debug, Default)]
pub struct SessionWatch {
    pub session_id: String,
    /// Resolved `message/<sessionId>/` directory once located.
    pub message_dir: Option<PathBuf>,
    /// mtime watermark — files at or below it are skipped on the next
    /// walk unless they're in `seen_at_watermark`.
    pub mtime_watermark: Option<SystemTime>,
    /// Files at exactly `mtime_watermark` (for same-tick disambiguation
    /// on coarse-resolution clocks). Cleared whenever the watermark
    /// advances.
    pub seen_at_watermark: HashSet<PathBuf>,
    /// Directory mtime captured at the start of the most recent
    /// successful walk. Quiet-tick short-circuit: if the dir mtime
    /// hasn't moved and we already have a snapshot, skip the
    /// per-file enumerate.
    pub last_walked_dir_mtime: Option<SystemTime>,

    pub last_entry: Option<MessageEntry>,
    pub last_user: Option<MessageEntry>,
    pub last_assistant_with_usage: Option<MessageEntry>,
    pub turn_count: u32,

    pub snapshot: Option<TelemetrySnapshot>,
}

impl SessionWatch {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Recompute
// ---------------------------------------------------------------------------

/// Walk the message directory and update `watch` in place. Returns
/// `true` when the snapshot mutated. Mirrors C# `Recompute`.
///
/// Read errors on individual files are skipped (the file is re-tried
/// next tick because it stays at or above the watermark). This matches
/// the C# `IOException` mid-write race handling.
pub fn process_message_dir(watch: &mut SessionWatch) -> bool {
    let Some(dir) = watch.message_dir.clone() else {
        return false;
    };
    let dir_mtime = std::fs::metadata(&dir).ok().and_then(|m| m.modified().ok());

    // Quiet-tick short-circuit. Capture dir mtime BEFORE the walk so a
    // file landing between this check and the enumerate is still picked
    // up on the next tick. Mirrors C# comment verbatim.
    if let (Some(dm), Some(last)) = (dir_mtime, watch.last_walked_dir_mtime) {
        if dm <= last && watch.snapshot.is_some() {
            return false;
        }
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return false,
    };

    let entry_watermark = watch.mtime_watermark;
    let mut max_mtime_seen = entry_watermark;
    let mut new_seen_at_max: Option<HashSet<PathBuf>> = None;
    let mut any_new_parsed = false;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        if !name.starts_with("msg_") || !name.ends_with(".json") {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let lwt = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if let Some(wm) = entry_watermark {
            if lwt < wm {
                continue;
            }
            if lwt == wm && watch.seen_at_watermark.contains(&path) {
                continue;
            }
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            // Mid-write race — leave for the next poll. The file's mtime
            // hasn't moved past the watermark yet (or we'd have seen it
            // here), so it'll be re-considered.
            Err(_) => continue,
        };

        let parsed = match parse_content(&content) {
            Some(p) => p,
            None => continue,
        };

        // Track post-loop watermark + same-instant set. Files strictly
        // between entry_watermark and max_mtime_seen don't need
        // tracking — the next tick filters them by the new watermark.
        match max_mtime_seen {
            None => {
                max_mtime_seen = Some(lwt);
                let mut s = HashSet::new();
                s.insert(path.clone());
                new_seen_at_max = Some(s);
            }
            Some(cur) if lwt > cur => {
                max_mtime_seen = Some(lwt);
                let mut s = HashSet::new();
                s.insert(path.clone());
                new_seen_at_max = Some(s);
            }
            Some(cur) if lwt == cur => {
                new_seen_at_max
                    .get_or_insert_with(HashSet::new)
                    .insert(path.clone());
            }
            _ => {}
        }

        // Update the three CreatedAt-keyed running aggregates. Files can
        // land out of disk-order, so each candidate is "largest CreatedAt
        // for its slice", not just "last seen".
        let ec = parsed.created_at_secs.unwrap_or(f64::MIN);
        let last_entry_ts = watch
            .last_entry
            .as_ref()
            .and_then(|e| e.created_at_secs)
            .unwrap_or(f64::MIN);
        if watch.last_entry.is_none() || ec > last_entry_ts {
            watch.last_entry = Some(parsed.clone());
        }

        if parsed.role.as_deref() == Some("user") {
            let last_user_ts = watch
                .last_user
                .as_ref()
                .and_then(|e| e.created_at_secs)
                .unwrap_or(f64::MIN);
            if watch.last_user.is_none() || ec > last_user_ts {
                watch.last_user = Some(parsed.clone());
            }
        }

        if parsed.has_usage() {
            let last_asst_ts = watch
                .last_assistant_with_usage
                .as_ref()
                .and_then(|e| e.created_at_secs)
                .unwrap_or(f64::MIN);
            if watch.last_assistant_with_usage.is_none() || ec > last_asst_ts {
                watch.last_assistant_with_usage = Some(parsed.clone());
            }
            watch.turn_count += 1;
        }

        any_new_parsed = true;
    }

    // Commit the watermark only after the walk so mid-loop advances
    // can't filter older siblings returned later. Mirrors C# comment.
    if let Some(new_set) = new_seen_at_max {
        if let Some(new_max) = max_mtime_seen {
            let advance = match entry_watermark {
                Some(wm) => new_max > wm,
                None => true,
            };
            if advance {
                watch.mtime_watermark = Some(new_max);
                watch.seen_at_watermark.clear();
            }
        }
        for f in new_set {
            watch.seen_at_watermark.insert(f);
        }
    }

    // Record dir mtime captured before the walk — next tick can
    // short-circuit unless the dir has changed again. Done
    // unconditionally so a no-change tick still advances the gate.
    if let Some(dm) = dir_mtime {
        watch.last_walked_dir_mtime = Some(dm);
    }

    if watch.last_entry.is_none() {
        return false;
    }
    if !any_new_parsed && watch.snapshot.is_some() {
        return false;
    }

    let last_entry = watch.last_entry.as_ref().unwrap();
    let last_assistant = watch.last_assistant_with_usage.as_ref();
    let last_user = watch.last_user.as_ref();

    // Activity FSM (mirrors C#):
    //   user is most recent                      → Busy (Composing)
    //   assistant + pending tool                  → PendingToolUse
    //   assistant + completed (no pending tools)  → Idle
    //   assistant + not yet completed             → Busy (streaming)
    let state = match last_entry.role.as_deref() {
        Some("user") => SessionState::Busy,
        Some("assistant") if last_entry.has_pending_tool_call => SessionState::PendingToolUse,
        Some("assistant") if last_entry.completed_at_secs.is_some() => SessionState::Idle,
        Some("assistant") => SessionState::Busy,
        _ => SessionState::Unknown,
    };

    let tokens_used = last_assistant.map_or(0, MessageEntry::context_tokens);
    let model = last_assistant.and_then(|e| e.model_id.clone());

    let last_turn_duration = match (last_assistant, last_user) {
        (Some(asst), Some(user)) => {
            match (user.created_at_secs, asst.created_at_secs) {
                (Some(u), Some(a)) if a > u => {
                    let end = asst.completed_at_secs.unwrap_or(a);
                    let secs = end - u;
                    if secs >= 0.0 {
                        Some(Duration::from_secs_f64(secs))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => None,
    };

    let context_window = model.as_deref().and_then(context_window_for_model);
    let context_pct = if tokens_used == 0 {
        None
    } else {
        context_window.map(|cap| (tokens_used as f32 / cap as f32).clamp(0.0, 1.0))
    };

    let snap = TelemetrySnapshot {
        model,
        tokens_used,
        context_pct,
        turn_count: watch.turn_count,
        last_turn_duration,
        state,
    };

    if Some(&snap) == watch.snapshot.as_ref() {
        return false;
    }
    watch.snapshot = Some(snap);
    true
}

// ---------------------------------------------------------------------------
// High-level tail handle
// ---------------------------------------------------------------------------

/// Handle to a watched OpenCode session. Tracks the resolved message
/// directory + watermark so repeated `poll()` calls are cheap.
///
/// Mirrors the `Watch` inner class of `OpenCodeTelemetryService` plus
/// its `TryLocateMessageDir` retry throttling.
#[derive(Debug)]
pub struct OpenCodeMessageTail {
    pub data_root: PathBuf,
    watch: SessionWatch,
    /// Last failed locate timestamp; used to throttle the recursive
    /// scan. Mirrors C# `LastLocateMissAtTicks`.
    last_locate_miss_at: Option<SystemTime>,
}

/// How long to skip `try_locate_message_dir` after a "not found"
/// result. Mirrors C# `LocateNotFoundTtl`.
pub const LOCATE_NOT_FOUND_TTL: Duration = Duration::from_secs(2);

impl OpenCodeMessageTail {
    /// Construct a tail for `(data_root, session_id)` and immediately
    /// attempt to locate + read the message directory so any existing
    /// content is consumed before the first poll interval fires.
    pub fn new(data_root: PathBuf, session_id: impl Into<String>) -> Self {
        let mut tail = Self {
            data_root,
            watch: SessionWatch::new(session_id),
            last_locate_miss_at: None,
        };
        tail.poll();
        tail
    }

    /// Read-only view of the resolved directory, if located.
    pub fn message_dir(&self) -> Option<&Path> {
        self.watch.message_dir.as_deref()
    }

    /// Latest snapshot, or `None` until at least one entry is parsed.
    pub fn snapshot(&self) -> Option<&TelemetrySnapshot> {
        self.watch.snapshot.as_ref()
    }

    /// Re-locate (if needed) and walk the message directory.
    /// Returns `true` when the snapshot changed.
    pub fn poll(&mut self) -> bool {
        if self.watch.message_dir.is_none() {
            // Throttle the recursive scan — the dir only appears once
            // OpenCode writes its first message file; a 250 ms recursive
            // walk over the whole data root every tick is wasteful.
            // Mirrors C# Issue #36.
            if let Some(missed) = self.last_locate_miss_at {
                if let Ok(elapsed) = SystemTime::now().duration_since(missed) {
                    if elapsed < LOCATE_NOT_FOUND_TTL {
                        return false;
                    }
                }
            }
            match try_locate_message_dir(&self.data_root, &self.watch.session_id) {
                Some(dir) => {
                    self.watch.message_dir = Some(dir);
                    self.last_locate_miss_at = None;
                }
                None => {
                    self.last_locate_miss_at = Some(SystemTime::now());
                    return false;
                }
            }
        }
        process_message_dir(&mut self.watch)
    }

    /// Suggested poll interval for the next wake-up. 250 ms while
    /// busy / pending tool, 2 s while idle / unknown. Same cadence as
    /// the Claude / Copilot telemetry tails.
    pub fn poll_interval(&self) -> Duration {
        match self.watch.snapshot.as_ref().map(|s| s.state) {
            Some(SessionState::Busy) | Some(SessionState::PendingToolUse) => {
                Duration::from_millis(250)
            }
            _ => Duration::from_secs(2),
        }
    }
}

/// Default OpenCode data root: `<home>/.local/share/opencode`.
/// Mirrors C# `OpenCodeTelemetryService.DefaultDataRoot`.
pub fn default_data_root() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("opencode"),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ---- parse_content ----

    fn user_msg_json(session: &str, created_ms: i64) -> String {
        format!(
            r#"{{
                "id":"u-1","role":"user",
                "metadata":{{
                    "sessionID":"{session}",
                    "time":{{"created":{created_ms}}}
                }},
                "parts":[{{"type":"text","text":"hi"}}]
            }}"#
        )
    }

    fn assistant_msg_json(
        session: &str,
        created_ms: i64,
        completed_ms: Option<i64>,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        model: &str,
        cwd: &str,
        pending_tool: bool,
    ) -> String {
        let completed = completed_ms
            .map(|t| format!(r#","completed":{t}"#))
            .unwrap_or_default();
        let parts = if pending_tool {
            r#"[{"type":"tool-invocation","toolInvocation":{"state":"call","toolName":"shell"}}]"#
        } else {
            r#"[{"type":"text","text":"ok"}]"#
        };
        format!(
            r#"{{
                "id":"a-1","role":"assistant",
                "metadata":{{
                    "sessionID":"{session}",
                    "time":{{"created":{created_ms}{completed}}},
                    "assistant":{{
                        "modelID":"{model}",
                        "providerID":"anthropic",
                        "path":{{"cwd":"{cwd}","root":"{cwd}"}},
                        "tokens":{{
                            "input":{input},"output":{output},"reasoning":0,
                            "cache":{{"read":{cache_read},"write":{cache_write}}}
                        }},
                        "cost":0
                    }}
                }},
                "parts":{parts}
            }}"#
        )
    }

    #[test]
    fn parse_user_message_extracts_role_and_session() {
        let entry = parse_content(&user_msg_json("sess-1", 1_700_000_000_000)).unwrap();
        assert_eq!(entry.role.as_deref(), Some("user"));
        assert_eq!(entry.session_id.as_deref(), Some("sess-1"));
        assert_eq!(entry.created_at_secs, Some(1_700_000_000.0));
        assert!(!entry.has_usage());
        assert!(!entry.has_pending_tool_call);
    }

    #[test]
    fn parse_assistant_extracts_tokens_and_model() {
        let entry = parse_content(&assistant_msg_json(
            "sess-1",
            1_700_000_010_000,
            Some(1_700_000_020_000),
            100,
            50,
            10,
            5,
            "claude-sonnet-4-6",
            "C:/dev/x",
            false,
        ))
        .unwrap();
        assert_eq!(entry.role.as_deref(), Some("assistant"));
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 50);
        assert_eq!(entry.cache_read_tokens, 10);
        assert_eq!(entry.cache_write_tokens, 5);
        assert_eq!(entry.context_tokens(), 165);
        assert!(entry.has_usage());
        assert_eq!(entry.model_id.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(entry.cwd.as_deref(), Some("C:/dev/x"));
        assert_eq!(entry.completed_at_secs, Some(1_700_000_020.0));
    }

    #[test]
    fn parse_assistant_pending_tool_call_detected() {
        let entry = parse_content(&assistant_msg_json(
            "s",
            1_700_000_000_000,
            None,
            10,
            0,
            0,
            0,
            "claude-sonnet-4-6",
            "C:/dev/x",
            true,
        ))
        .unwrap();
        assert!(entry.has_pending_tool_call);
    }

    #[test]
    fn parse_user_with_tool_part_does_not_flag_pending() {
        // Tool parts on user role must never flag pending — only
        // assistant role can be mid-call.
        let json = r#"{
            "id":"u-1","role":"user",
            "metadata":{"sessionID":"s","time":{"created":1700000000000}},
            "parts":[{"type":"tool-invocation","toolInvocation":{"state":"call"}}]
        }"#;
        let entry = parse_content(json).unwrap();
        assert!(!entry.has_pending_tool_call);
    }

    #[test]
    fn parse_partial_call_state_flags_pending() {
        let json = r#"{
            "id":"a-1","role":"assistant",
            "metadata":{"sessionID":"s","time":{"created":1700000000000}},
            "parts":[{"type":"tool-invocation","toolInvocation":{"state":"partial-call"}}]
        }"#;
        let entry = parse_content(json).unwrap();
        assert!(entry.has_pending_tool_call);
    }

    #[test]
    fn parse_result_state_does_not_flag_pending() {
        let json = r#"{
            "id":"a-1","role":"assistant",
            "metadata":{"sessionID":"s","time":{"created":1700000000000}},
            "parts":[{"type":"tool-invocation","toolInvocation":{"state":"result"}}]
        }"#;
        let entry = parse_content(json).unwrap();
        assert!(!entry.has_pending_tool_call);
    }

    #[test]
    fn parse_blank_input_returns_none() {
        assert!(parse_content("").is_none());
        assert!(parse_content("   \t").is_none());
    }

    #[test]
    fn parse_garbage_returns_none() {
        assert!(parse_content("{not json").is_none());
        assert!(parse_content("[]").is_none()); // root must be object
    }

    #[test]
    fn extract_message_id_from_well_formed_name() {
        assert_eq!(
            extract_message_id_from_file_name("msg_abc-123.json").as_deref(),
            Some("abc-123")
        );
    }

    #[test]
    fn extract_message_id_from_other_names_returns_none() {
        assert_eq!(extract_message_id_from_file_name("session.json"), None);
        assert_eq!(extract_message_id_from_file_name("msg_.json"), None);
        assert_eq!(extract_message_id_from_file_name(""), None);
    }

    // ---- try_locate_message_dir (recursive) ----

    fn make_session_dir(root: &Path, slug: &str, sid: &str) -> PathBuf {
        let dir = root
            .join("project")
            .join(slug)
            .join("storage")
            .join("message")
            .join(sid);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn locate_finds_session_under_project_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let target = make_session_dir(tmp.path(), "myproj", "sid-1");
        let found = try_locate_message_dir(tmp.path(), "sid-1").unwrap();
        assert_eq!(found, target);
    }

    #[test]
    fn locate_returns_none_when_session_missing() {
        let tmp = tempfile::tempdir().unwrap();
        make_session_dir(tmp.path(), "myproj", "sid-1");
        assert!(try_locate_message_dir(tmp.path(), "other-sid").is_none());
    }

    #[test]
    fn locate_ignores_dirs_whose_parent_is_not_message() {
        // A stray directory named like a session id but NOT under
        // `message/` must be ignored — protects against project slugs
        // that accidentally embed the session id.
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("decoy").join("sid-1")).unwrap();
        let real = make_session_dir(tmp.path(), "real", "sid-1");
        let found = try_locate_message_dir(tmp.path(), "sid-1").unwrap();
        assert_eq!(found, real);
    }

    #[test]
    fn locate_returns_none_for_missing_root() {
        let path = std::path::PathBuf::from("/nonexistent/opencode/root");
        assert!(try_locate_message_dir(&path, "sid-1").is_none());
    }

    // ---- process_message_dir / snapshot ----

    fn write_message(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn snapshot_after_user_assistant_pair_is_idle_with_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid",
                1_700_000_010_000,
                Some(1_700_000_020_000),
                100,
                50,
                10,
                5,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );

        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        let changed = process_message_dir(&mut watch);
        assert!(changed);

        let snap = watch.snapshot.as_ref().unwrap();
        assert_eq!(snap.state, SessionState::Idle);
        assert_eq!(snap.tokens_used, 165);
        assert_eq!(snap.turn_count, 1);
        assert_eq!(snap.model.as_deref(), Some("claude-sonnet-4-6"));
        // 20 s between user.created and assistant.completed.
        assert_eq!(snap.last_turn_duration, Some(Duration::from_secs(20)));
    }

    #[test]
    fn pending_tool_state_when_assistant_mid_call() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid",
                1_700_000_010_000,
                None, // not completed
                10,
                0,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                true,
            ),
        );

        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        process_message_dir(&mut watch);
        assert_eq!(watch.snapshot.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn streaming_assistant_without_completed_is_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid",
                1_700_000_010_000,
                None,
                10,
                0,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );
        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        process_message_dir(&mut watch);
        assert_eq!(watch.snapshot.unwrap().state, SessionState::Busy);
    }

    #[test]
    fn turn_count_advances_only_on_assistant_with_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        // Plain user message — must NOT bump the counter.
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );
        // Two assistant turns with usage — count = 2.
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid",
                1_700_000_010_000,
                Some(1_700_000_011_000),
                10,
                5,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );
        write_message(
            &dir,
            "msg_003.json",
            &assistant_msg_json(
                "sid",
                1_700_000_020_000,
                Some(1_700_000_021_000),
                20,
                7,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );

        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        process_message_dir(&mut watch);
        assert_eq!(watch.snapshot.as_ref().unwrap().turn_count, 2);
    }

    #[test]
    fn second_walk_with_no_new_files_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid",
                1_700_000_010_000,
                Some(1_700_000_011_000),
                10,
                5,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );

        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        let first = process_message_dir(&mut watch);
        assert!(first);
        let second = process_message_dir(&mut watch);
        assert!(!second);
        // Snapshot unchanged.
        assert_eq!(watch.snapshot.as_ref().unwrap().turn_count, 1);
    }

    #[test]
    fn malformed_message_file_is_skipped_without_panic() {
        // Mirrors C# IOException-on-mid-write tolerance: a partial /
        // garbage file leaves the watermark untouched (so the file is
        // re-tried next tick once it's flushed) and never crashes
        // the whole walk.
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );
        // Half-written file dropped between two valid ones.
        write_message(&dir, "msg_002.json", "{not json");
        write_message(
            &dir,
            "msg_003.json",
            &assistant_msg_json(
                "sid",
                1_700_000_010_000,
                Some(1_700_000_011_000),
                10,
                5,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );

        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        let changed = process_message_dir(&mut watch);
        assert!(changed);
        let snap = watch.snapshot.as_ref().unwrap();
        assert_eq!(snap.turn_count, 1);
        assert_eq!(snap.state, SessionState::Idle);
    }

    #[test]
    fn out_of_order_writes_pick_largest_created_at() {
        // Filesystem enumeration is not mtime-ordered; make sure the
        // running aggregates pick the entry with the largest CreatedAt
        // for each slice, regardless of disk order. Same property the
        // C# build relies on.
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid");
        // Older assistant turn written second (same model).
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid",
                1_700_000_005_000,
                Some(1_700_000_006_000),
                10,
                5,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );
        // Newer assistant turn (larger token total) — should win.
        write_message(
            &dir,
            "msg_003.json",
            &assistant_msg_json(
                "sid",
                1_700_000_020_000,
                Some(1_700_000_021_000),
                100,
                50,
                10,
                5,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );
        // User message in the middle.
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid", 1_700_000_000_000),
        );

        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(dir);
        process_message_dir(&mut watch);
        let snap = watch.snapshot.as_ref().unwrap();
        // tokens_used reflects the LAST assistant-with-usage by
        // CreatedAt, not the disk-order last write.
        assert_eq!(snap.tokens_used, 165);
        assert_eq!(snap.turn_count, 2);
    }

    #[test]
    fn missing_message_dir_returns_false() {
        let mut watch = SessionWatch::new("sid");
        watch.message_dir = Some(PathBuf::from("/nonexistent/opencode/sid"));
        assert!(!process_message_dir(&mut watch));
        assert!(watch.snapshot.is_none());
    }

    // ---- OpenCodeMessageTail (high-level handle) ----

    #[test]
    fn tail_resolves_dir_and_reads_initial_content() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "sid-x");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid-x", 1_700_000_000_000),
        );
        write_message(
            &dir,
            "msg_002.json",
            &assistant_msg_json(
                "sid-x",
                1_700_000_010_000,
                Some(1_700_000_011_000),
                10,
                5,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );

        let tail = OpenCodeMessageTail::new(tmp.path().to_path_buf(), "sid-x");
        assert_eq!(tail.message_dir(), Some(dir.as_path()));
        let snap = tail.snapshot().expect("initial poll should produce a snapshot");
        assert_eq!(snap.turn_count, 1);
        assert_eq!(snap.state, SessionState::Idle);
    }

    #[test]
    fn tail_locate_miss_does_not_panic_and_can_recover() {
        let tmp = tempfile::tempdir().unwrap();
        // Session dir doesn't exist yet.
        let mut tail = OpenCodeMessageTail::new(tmp.path().to_path_buf(), "sid-late");
        assert!(tail.message_dir().is_none());
        assert!(tail.snapshot().is_none());

        // Create the dir + a message and force a re-locate by clearing
        // the throttle (simulating a poll after the TTL window).
        let dir = make_session_dir(tmp.path(), "p", "sid-late");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("sid-late", 1_700_000_000_000),
        );
        tail.last_locate_miss_at = None;
        let changed = tail.poll();
        assert!(changed);
        assert_eq!(tail.message_dir(), Some(dir.as_path()));
    }

    #[test]
    fn tail_poll_interval_matches_state() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "p", "busy");
        write_message(
            &dir,
            "msg_001.json",
            &user_msg_json("busy", 1_700_000_000_000),
        );
        let busy_tail = OpenCodeMessageTail::new(tmp.path().to_path_buf(), "busy");
        assert_eq!(busy_tail.poll_interval(), Duration::from_millis(250));

        let idle_dir = make_session_dir(tmp.path(), "p", "idle");
        write_message(
            &idle_dir,
            "msg_001.json",
            &user_msg_json("idle", 1_700_000_000_000),
        );
        write_message(
            &idle_dir,
            "msg_002.json",
            &assistant_msg_json(
                "idle",
                1_700_000_010_000,
                Some(1_700_000_011_000),
                10,
                5,
                0,
                0,
                "claude-sonnet-4-6",
                "C:/dev/x",
                false,
            ),
        );
        let idle_tail = OpenCodeMessageTail::new(tmp.path().to_path_buf(), "idle");
        assert_eq!(idle_tail.poll_interval(), Duration::from_secs(2));
    }

    #[test]
    fn locate_throttle_skips_until_ttl_elapses() {
        let tmp = tempfile::tempdir().unwrap();
        // No session dir — first poll misses + sets the throttle.
        let mut tail = OpenCodeMessageTail::new(tmp.path().to_path_buf(), "missing");
        assert!(tail.message_dir().is_none());

        // Even after creating the dir, a follow-up poll within the TTL
        // must NOT re-walk the data root (the C# 2 s throttle).
        let _dir = make_session_dir(tmp.path(), "p", "missing");
        // Throttle still set — the poll early-returns.
        assert!(tail.last_locate_miss_at.is_some());
        let changed = tail.poll();
        assert!(!changed);
        assert!(tail.message_dir().is_none());

        // Clear throttle to simulate the TTL expiring.
        tail.last_locate_miss_at = None;
        let changed = tail.poll();
        // Now the dir is found (no message file written yet, so no
        // snapshot, but the directory resolves).
        assert!(!changed);
        assert!(tail.message_dir().is_some());
    }
}
