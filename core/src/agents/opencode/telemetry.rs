//! OpenCode CLI session tail — per-session telemetry read from
//! `~/.local/share/opencode/opencode.db`.
//!
//! Data shapes match [`crate::agents::claude::telemetry`] so the status
//! bar can render OpenCode sessions through the same code paths.
//!
//! # Storage shape
//!
//! OpenCode 1.x keeps sessions in one SQLite database (see
//! [`super::db`]). A tail reads three tables:
//!
//! * `session` — the generated `title`;
//! * `message` — one row per user prompt and one per assistant *step*
//!   (a turn that calls tools is several assistant rows), with the
//!   message JSON in `data`: `role`, `time.created` /
//!   `time.completed` (Unix ms), `tokens`, `modelID`, `finish`, `error`;
//! * `part` — the pieces of a message; a tool call is
//!   `data.type == "tool"` with `data.state.status`.
//!
//! # Polling — stamp gate
//!
//! [`OpenCodeMessageTail::poll`] stats the database and its WAL and
//! returns at once when neither moved. Only then does it run its
//! handful of indexed queries, on a connection it keeps open.
//!
//! Adaptive poll cadence: 250 ms while busy / pending tool, 2 s while
//! idle — same as Claude / Copilot.
//!
//! # Tokens
//!
//! `tokens_used` is the most recent assistant-with-usage's `input +
//! output + reasoning + cache.read + cache.write` — a snapshot of the
//! last step, not a running sum.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::agents::opencode::db::{self, DbStamp};
use crate::session_title::{SessionTitle, TitleSource};
use crate::telemetry::{SessionState, TelemetrySnapshot, context_window_for_model};

// ---------------------------------------------------------------------------
// Message parser
// ---------------------------------------------------------------------------

/// The fields of one `message.data` blob the snapshot needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageEntry {
    /// `"user"` or `"assistant"`.
    pub role: Option<String>,
    /// `time.created`, Unix ms.
    pub created_ms: Option<i64>,
    /// `time.completed`, Unix ms. Only set on finished assistant steps.
    pub completed_ms: Option<i64>,
    /// Why the step ended: `"stop"`, `"tool-calls"`, `"length"`, …
    pub finish: Option<String>,
    /// The step ended in an error (aborted, provider failure).
    pub has_error: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub model_id: Option<String>,
}

impl MessageEntry {
    /// True when this is an assistant step carrying token usage.
    pub fn has_usage(&self) -> bool {
        self.role.as_deref() == Some("assistant") && self.context_tokens() > 0
    }

    /// Total in-context tokens: `input + output + reasoning +
    /// cache.read + cache.write`.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.reasoning_tokens
            + self.cache_read_tokens
            + self.cache_write_tokens
    }
}

/// Parse one `message.data` JSON blob. `None` for blank input,
/// unparseable JSON or a non-object root.
pub fn parse_message(data: &str) -> Option<MessageEntry> {
    let v: Value = serde_json::from_str(data).ok()?;
    let obj = v.as_object()?;
    let mut entry = MessageEntry {
        role: string_field(obj, "role"),
        finish: string_field(obj, "finish"),
        has_error: obj.get("error").is_some_and(|e| !e.is_null()),
        model_id: string_field(obj, "modelID"),
        ..MessageEntry::default()
    };
    if let Some(time) = obj.get("time").and_then(Value::as_object) {
        entry.created_ms = time.get("created").and_then(Value::as_i64);
        entry.completed_ms = time.get("completed").and_then(Value::as_i64);
    }
    if let Some(tokens) = obj.get("tokens").and_then(Value::as_object) {
        entry.input_tokens = u64_field(tokens, "input");
        entry.output_tokens = u64_field(tokens, "output");
        entry.reasoning_tokens = u64_field(tokens, "reasoning");
        if let Some(cache) = tokens.get("cache").and_then(Value::as_object) {
            entry.cache_read_tokens = u64_field(cache, "read");
            entry.cache_write_tokens = u64_field(cache, "write");
        }
    }
    Some(entry)
}

fn string_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_field(obj: &serde_json::Map<String, Value>, key: &str) -> u64 {
    obj.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Activity state from the session's latest message:
///
/// * a user message → Busy (OpenCode is composing the reply);
/// * an assistant step with a `pending` / `running` tool part →
///   PendingToolUse;
/// * a completed step that ended in `tool-calls` → Busy. OpenCode's
///   prompt loop starts the next step right after (its loop only exits
///   on a finish other than `tool-calls`), so reporting Idle in that gap
///   would flash a finished turn between every tool call;
/// * any other completed step → Idle;
/// * a step still streaming → Busy.
pub fn derive_state(latest: &MessageEntry, pending_tool: bool) -> SessionState {
    match latest.role.as_deref() {
        Some("user") => SessionState::Busy,
        Some("assistant") if pending_tool => SessionState::PendingToolUse,
        Some("assistant") if latest.completed_ms.is_some() => {
            if latest.finish.as_deref() == Some("tool-calls") && !latest.has_error {
                SessionState::Busy
            } else {
                SessionState::Idle
            }
        }
        Some("assistant") => SessionState::Busy,
        _ => SessionState::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// What one read of the database says about a session.
#[derive(Debug, Default)]
struct SessionRead {
    title: Option<String>,
    latest: Option<MessageEntry>,
    pending_tool: bool,
    last_usage: Option<MessageEntry>,
    prompts: PromptTally,
}

/// Running count of the session's user messages, advanced from a
/// cursor so a re-read only parses messages it hasn't counted yet. A
/// message's role never changes, so counting each one once is exact.
#[derive(Debug, Clone, Default, PartialEq)]
struct PromptTally {
    /// `(time_created, id)` of the last message counted.
    cursor: Option<(i64, String)>,
    count: u32,
    /// `time_created` of the newest user message.
    last_user_ms: Option<i64>,
}

impl PromptTally {
    fn advance(&self, conn: &Connection, session_id: &str) -> rusqlite::Result<PromptTally> {
        let mut tally = self.clone();
        // OpenCode's `/undo` deletes the newest messages. When the
        // cursor's message is gone, count again from the start.
        if let Some((_, id)) = &tally.cursor {
            let still_there: bool = conn
                .prepare_cached("SELECT EXISTS (SELECT 1 FROM message WHERE id = ?1)")?
                .query_row([id], |r| r.get(0))?;
            if !still_there {
                tally = PromptTally::default();
            }
        }
        let (after_ms, after_id) = tally.cursor.clone().unwrap_or((i64::MIN, String::new()));
        let mut stmt = conn.prepare_cached(
            "SELECT time_created, id, json_extract(data, '$.role') = 'user' FROM message
             WHERE session_id = ?1 AND (time_created, id) > (?2, ?3)
             ORDER BY time_created, id",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, after_ms, after_id])?;
        while let Some(row) = rows.next()? {
            let created: i64 = row.get(0)?;
            if row.get::<_, Option<bool>>(2)? == Some(true) {
                tally.count += 1;
                tally.last_user_ms = Some(tally.last_user_ms.map_or(created, |m| m.max(created)));
            }
            tally.cursor = Some((created, row.get(1)?));
        }
        Ok(tally)
    }
}

/// Every query rides the `(session_id, time_created, id)` index on
/// `message` or the `(message_id, id)` index on `part`; the `json_extract`
/// filters only touch the rows those ranges return.
fn read_session(
    conn: &Connection,
    session_id: &str,
    prompts: &PromptTally,
) -> rusqlite::Result<SessionRead> {
    let title: Option<String> = conn
        .prepare_cached("SELECT title FROM session WHERE id = ?1")?
        .query_row([session_id], |r| r.get(0))
        .optional()?;
    let latest: Option<(String, String)> = conn
        .prepare_cached(
            "SELECT id, data FROM message WHERE session_id = ?1
             ORDER BY time_created DESC, id DESC LIMIT 1",
        )?
        .query_row([session_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((latest_id, latest_data)) = latest else {
        return Ok(SessionRead { title, ..SessionRead::default() });
    };
    let latest = parse_message(&latest_data);
    let pending_tool = latest.as_ref().is_some_and(|m| m.role.as_deref() == Some("assistant"))
        && conn
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM part WHERE message_id = ?1
                    AND json_extract(data, '$.type') = 'tool'
                    AND json_extract(data, '$.state.status') IN ('pending', 'running'))",
            )?
            .query_row([&latest_id], |r| r.get::<_, bool>(0))?;
    let last_usage: Option<String> = conn
        .prepare_cached(
            "SELECT data FROM message WHERE session_id = ?1
                AND json_extract(data, '$.role') = 'assistant'
                AND coalesce(json_extract(data, '$.tokens.input'), 0)
                  + coalesce(json_extract(data, '$.tokens.output'), 0)
                  + coalesce(json_extract(data, '$.tokens.reasoning'), 0)
                  + coalesce(json_extract(data, '$.tokens.cache.read'), 0)
                  + coalesce(json_extract(data, '$.tokens.cache.write'), 0) > 0
             ORDER BY time_created DESC, id DESC LIMIT 1",
        )?
        .query_row([session_id], |r| r.get(0))
        .optional()?;
    Ok(SessionRead {
        title,
        latest,
        pending_tool,
        last_usage: last_usage.as_deref().and_then(parse_message),
        prompts: prompts.advance(conn, session_id)?,
    })
}

fn build_snapshot(read: &SessionRead, title: Option<SessionTitle>) -> Option<TelemetrySnapshot> {
    let latest = read.latest.as_ref()?;
    let state = derive_state(latest, read.pending_tool);
    let last_usage = read.last_usage.as_ref();
    let tokens_used = last_usage.map_or(0, MessageEntry::context_tokens);
    let model = last_usage.and_then(|e| e.model_id.clone());

    // Prompt → end of the latest step that answered it.
    let last_turn_duration = match (last_usage, read.prompts.last_user_ms) {
        (Some(asst), Some(user_ms)) => match asst.created_ms {
            Some(created) if created > user_ms => {
                let end = asst.completed_ms.unwrap_or(created);
                u64::try_from(end - user_ms).ok().map(Duration::from_millis)
            }
            _ => None,
        },
        _ => None,
    };

    let context_window = model.as_deref().and_then(context_window_for_model);
    let context_pct = if tokens_used == 0 {
        None
    } else {
        context_window.map(|cap| (tokens_used as f32 / cap as f32).clamp(0.0, 1.0))
    };

    Some(TelemetrySnapshot {
        model,
        tokens_used,
        context_pct,
        turn_count: read.prompts.count,
        last_turn_duration,
        state,
        quiet_timeout: false,
        title,
    })
}

// ---------------------------------------------------------------------------
// High-level tail handle
// ---------------------------------------------------------------------------

/// Handle to a watched OpenCode session. Keeps its own read-only
/// connection and the last [`DbStamp`] it read at, so a `poll()` with
/// nothing new is a stamp comparison (two `stat`s, shared with the
/// other tabs polled in the same tick — see [`db::shared_stamp`]).
#[derive(Debug)]
pub struct OpenCodeMessageTail {
    pub data_root: PathBuf,
    session_id: String,
    db_path: PathBuf,
    conn: Option<Connection>,
    /// Stamp of the last completed read; `None` before the first.
    last_stamp: Option<DbStamp>,
    title: Option<SessionTitle>,
    prompts: PromptTally,
    /// Reads run against the database — lets the tests prove a quiet
    /// poll never touches SQLite.
    queries: u64,
    /// Latest snapshot, or `None` until the session has a message.
    /// Public to match the sibling tails (`ClaudeTranscriptTail`,
    /// `CopilotTranscriptTail`, `PiTranscriptTail`).
    pub snapshot: Option<TelemetrySnapshot>,
}

impl OpenCodeMessageTail {
    /// Construct a tail for `(data_root, session_id)` and poll once so
    /// existing content is read before the first poll interval fires.
    pub fn for_session(data_root: &Path, session_id: impl Into<String>) -> Self {
        let mut tail = Self {
            data_root: data_root.to_path_buf(),
            session_id: session_id.into(),
            db_path: db::db_path(data_root),
            conn: None,
            last_stamp: None,
            title: None,
            prompts: PromptTally::default(),
            queries: 0,
            snapshot: None,
        };
        tail.poll();
        tail
    }

    /// Re-read the session if the database moved since the last read.
    /// Returns `true` when the snapshot changed.
    pub fn poll(&mut self) -> bool {
        // Stat before reading: a commit landing between the stat and the
        // queries is read now *and* moves the stamp, so the next poll
        // reads again rather than missing it.
        let stamp = db::shared_stamp(&self.db_path);
        if self.last_stamp == Some(stamp) {
            return false;
        }
        if !stamp.db_exists() {
            self.conn = None;
            self.last_stamp = Some(stamp);
            return false;
        }
        self.queries += 1;
        let read = match self.read() {
            Ok(read) => read,
            // Locked mid-write: retry next poll, stamp unrecorded.
            Err(err) if db::is_busy(&err) => return false,
            Err(err) => {
                eprintln!("[opencode_telemetry] reading {}: {err}", self.db_path.display());
                self.conn = None;
                self.last_stamp = Some(stamp);
                return false;
            }
        };
        self.last_stamp = Some(stamp);
        self.prompts = read.prompts.clone();
        if let Some(raw) = read.title.as_deref().filter(|t| !db::is_placeholder_title(t)) {
            SessionTitle::offer(&mut self.title, raw, TitleSource::Generated);
        }
        let snap = build_snapshot(&read, self.title.clone());
        if snap == self.snapshot {
            return false;
        }
        self.snapshot = snap;
        true
    }

    fn read(&mut self) -> rusqlite::Result<SessionRead> {
        let conn = match self.conn.take() {
            Some(conn) => conn,
            None => db::open_read_only(&self.db_path)?,
        };
        let read = read_session(&conn, &self.session_id, &self.prompts);
        self.conn = Some(conn);
        read
    }

    /// Suggested poll interval for the next wake-up. 250 ms while
    /// busy / pending tool, 2 s while idle / unknown. Same cadence as
    /// the Claude / Copilot telemetry tails.
    pub fn poll_interval(&self) -> Duration {
        match self.snapshot.as_ref().map(|s| s.state) {
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
    use crate::agents::opencode::db::fixture::{Fixture, assistant, user};

    const SID: &str = "ses_alpha";
    const MODEL: &str = "claude-sonnet-4-6";

    fn fixture() -> Fixture {
        let fx = Fixture::new();
        fx.add_session(SID, None, "C:/repo/alpha", "New session - 2026-04-17T09:25:15.466Z", 1);
        fx
    }

    fn tail(fx: &Fixture) -> OpenCodeMessageTail {
        OpenCodeMessageTail::for_session(fx.root(), SID)
    }

    fn state(t: &OpenCodeMessageTail) -> SessionState {
        t.snapshot.as_ref().expect("snapshot").state
    }

    // ---- parse_message ----

    #[test]
    fn parse_assistant_reads_top_level_fields() {
        let data = assistant(10_000, Some((20_000, "stop")), (100, 50, 10, 5), MODEL);
        let entry = parse_message(&data.to_string()).unwrap();
        assert_eq!(entry.role.as_deref(), Some("assistant"));
        assert_eq!(entry.created_ms, Some(10_000));
        assert_eq!(entry.completed_ms, Some(20_000));
        assert_eq!(entry.finish.as_deref(), Some("stop"));
        assert_eq!(entry.context_tokens(), 165);
        assert!(entry.has_usage());
        assert!(!entry.has_error);
        assert_eq!(entry.model_id.as_deref(), Some(MODEL));
    }

    #[test]
    fn parse_user_has_no_usage() {
        let entry = parse_message(&user(1_000).to_string()).unwrap();
        assert_eq!(entry.role.as_deref(), Some("user"));
        assert!(!entry.has_usage());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_message("").is_none());
        assert!(parse_message("{not json").is_none());
        assert!(parse_message("[]").is_none());
    }

    // ---- state machine ----

    #[test]
    fn no_messages_yet_means_no_snapshot() {
        let fx = fixture();
        assert!(tail(&fx).snapshot.is_none());
    }

    #[test]
    fn missing_db_means_no_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = OpenCodeMessageTail::for_session(tmp.path(), SID);
        assert!(t.snapshot.is_none());
        assert!(!t.poll());
    }

    #[test]
    fn user_message_is_busy() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        assert_eq!(state(&tail(&fx)), SessionState::Busy);
    }

    #[test]
    fn streaming_assistant_is_busy() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        fx.put_message("msg_2", SID, &assistant(2_000, None, (0, 0, 0, 0), MODEL));
        assert_eq!(state(&tail(&fx)), SessionState::Busy);
    }

    #[test]
    fn running_tool_is_pending_tool_use() {
        for status in ["pending", "running"] {
            let fx = fixture();
            fx.put_message("msg_1", SID, &user(1_000));
            fx.put_message("msg_2", SID, &assistant(2_000, None, (0, 0, 0, 0), MODEL));
            fx.put_tool_part("prt_1", "msg_2", SID, status);
            assert_eq!(state(&tail(&fx)), SessionState::PendingToolUse, "{status}");
        }
    }

    #[test]
    fn finished_tool_is_not_pending() {
        for status in ["completed", "error"] {
            let fx = fixture();
            fx.put_message("msg_1", SID, &user(1_000));
            fx.put_message("msg_2", SID, &assistant(2_000, None, (0, 0, 0, 0), MODEL));
            fx.put_tool_part("prt_1", "msg_2", SID, status);
            assert_eq!(state(&tail(&fx)), SessionState::Busy, "{status}");
        }
    }

    #[test]
    fn completed_step_that_stopped_is_idle() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        fx.put_message("msg_2", SID, &assistant(2_000, Some((3_000, "stop")), (1, 1, 0, 0), MODEL));
        assert_eq!(state(&tail(&fx)), SessionState::Idle);
    }

    #[test]
    fn completed_step_that_called_tools_stays_busy() {
        // OpenCode's loop starts the next step after a `tool-calls`
        // finish; the gap must not read as a finished turn.
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        fx.put_message(
            "msg_2",
            SID,
            &assistant(2_000, Some((3_000, "tool-calls")), (1, 1, 0, 0), MODEL),
        );
        assert_eq!(state(&tail(&fx)), SessionState::Busy);
    }

    #[test]
    fn errored_step_is_idle_even_after_tool_calls() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        let mut data = assistant(2_000, Some((3_000, "tool-calls")), (1, 1, 0, 0), MODEL);
        data["error"] = serde_json::json!({"name": "MessageAbortedError", "data": {}});
        fx.put_message("msg_2", SID, &data);
        assert_eq!(state(&tail(&fx)), SessionState::Idle);
    }

    #[test]
    fn state_follows_the_session_through_a_turn() {
        let fx = fixture();
        let mut t = tail(&fx);
        assert!(t.snapshot.is_none());

        fx.put_message("msg_1", SID, &user(1_000));
        assert!(t.poll());
        assert_eq!(state(&t), SessionState::Busy);

        fx.put_message("msg_2", SID, &assistant(2_000, None, (0, 0, 0, 0), MODEL));
        fx.put_tool_part("prt_1", "msg_2", SID, "running");
        assert!(t.poll());
        assert_eq!(state(&t), SessionState::PendingToolUse);

        fx.put_tool_part("prt_1", "msg_2", SID, "completed");
        fx.put_message(
            "msg_2",
            SID,
            &assistant(2_000, Some((3_000, "tool-calls")), (10, 5, 0, 0), MODEL),
        );
        assert!(t.poll());
        assert_eq!(state(&t), SessionState::Busy);

        let stop = assistant(4_000, Some((5_000, "stop")), (20, 7, 0, 0), MODEL);
        fx.put_message("msg_3", SID, &stop);
        assert!(t.poll());
        assert_eq!(state(&t), SessionState::Idle);
        assert_eq!(t.poll_interval(), Duration::from_secs(2));

        fx.put_message("msg_4", SID, &user(6_000));
        assert!(t.poll());
        assert_eq!(state(&t), SessionState::Busy);
        assert_eq!(t.poll_interval(), Duration::from_millis(250));
    }

    // ---- tokens / model / turns ----

    #[test]
    fn tokens_and_model_come_from_latest_step_with_usage() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        let step = assistant(2_000, Some((3_000, "tool-calls")), (10, 5, 0, 0), "gpt-5.4");
        fx.put_message("msg_2", SID, &step);
        let last = assistant(4_000, Some((21_000, "stop")), (100, 50, 10, 5), MODEL);
        fx.put_message("msg_3", SID, &last);
        // A new step with no usage yet must not zero the counter.
        fx.put_message("msg_4", SID, &assistant(22_000, None, (0, 0, 0, 0), "gpt-5.4"));
        let t = tail(&fx);
        let snap = t.snapshot.as_ref().unwrap();
        assert_eq!(snap.tokens_used, 165);
        assert_eq!(snap.model.as_deref(), Some(MODEL));
        let window = context_window_for_model(MODEL).expect("known model");
        assert_eq!(snap.context_pct, Some(165.0 / window as f32));
        // Prompt at 1 s → latest step with usage completed at 21 s.
        assert_eq!(snap.last_turn_duration, Some(Duration::from_secs(20)));
    }

    #[test]
    fn turn_count_counts_prompts_not_steps() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        let step = assistant(2_000, Some((3_000, "tool-calls")), (1, 1, 0, 0), MODEL);
        fx.put_message("msg_2", SID, &step);
        fx.put_message("msg_3", SID, &assistant(4_000, Some((5_000, "stop")), (1, 1, 0, 0), MODEL));
        fx.put_message("msg_4", SID, &user(6_000));
        fx.put_message("msg_5", SID, &assistant(7_000, Some((8_000, "stop")), (1, 1, 0, 0), MODEL));
        assert_eq!(tail(&fx).snapshot.unwrap().turn_count, 2);
    }

    #[test]
    fn turn_count_keeps_up_across_polls() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        let mut t = tail(&fx);
        assert_eq!(t.snapshot.as_ref().unwrap().turn_count, 1);
        fx.put_message("msg_2", SID, &assistant(2_000, Some((3_000, "stop")), (1, 1, 0, 0), MODEL));
        t.poll();
        assert_eq!(t.snapshot.as_ref().unwrap().turn_count, 1);
        // Same millisecond as the counted message, later id.
        fx.put_message("msg_3", SID, &user(3_000));
        fx.put_message("msg_4", SID, &user(3_000));
        t.poll();
        assert_eq!(t.snapshot.as_ref().unwrap().turn_count, 3);
    }

    #[test]
    fn undo_recounts_turns() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        fx.put_message("msg_2", SID, &assistant(2_000, Some((3_000, "stop")), (1, 1, 0, 0), MODEL));
        fx.put_message("msg_3", SID, &user(4_000));
        fx.put_message("msg_4", SID, &assistant(5_000, Some((6_000, "stop")), (1, 1, 0, 0), MODEL));
        let mut t = tail(&fx);
        assert_eq!(t.snapshot.as_ref().unwrap().turn_count, 2);
        fx.delete_message("msg_4");
        fx.delete_message("msg_3");
        assert!(t.poll());
        let snap = t.snapshot.as_ref().unwrap();
        assert_eq!(snap.turn_count, 1);
        assert_eq!(snap.state, SessionState::Idle);
    }

    #[test]
    fn messages_of_other_sessions_are_ignored() {
        let fx = fixture();
        fx.add_session("ses_other", None, "C:/repo/alpha", "Other", 1);
        fx.put_message("msg_1", SID, &user(1_000));
        fx.put_message("msg_2", SID, &assistant(2_000, Some((3_000, "stop")), (1, 1, 0, 0), MODEL));
        fx.put_message("msg_3", "ses_other", &user(9_000));
        fx.put_tool_part("prt_1", "msg_3", "ses_other", "running");
        let snap = tail(&fx).snapshot.unwrap();
        assert_eq!(snap.state, SessionState::Idle);
        assert_eq!(snap.turn_count, 1);
    }

    // ---- title ----

    #[test]
    fn placeholder_title_never_surfaces() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        assert_eq!(tail(&fx).snapshot.unwrap().title, None);
    }

    #[test]
    fn generated_title_is_offered_once_opencode_writes_it() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        let mut t = tail(&fx);
        assert_eq!(t.snapshot.as_ref().unwrap().title, None);

        fx.set_title(SID, "Fix the resize bug");
        assert!(t.poll());
        assert_eq!(
            t.snapshot.as_ref().unwrap().title,
            Some(SessionTitle {
                text: "Fix the resize bug".into(),
                source: TitleSource::Generated,
            })
        );
    }

    // ---- stamp gate ----

    #[test]
    fn quiet_polls_do_not_query() {
        let fx = fixture();
        fx.put_message("msg_1", SID, &user(1_000));
        let mut t = tail(&fx);
        assert_eq!(t.queries, 1);
        for _ in 0..5 {
            assert!(!t.poll());
        }
        assert_eq!(t.queries, 1, "nothing moved, nothing read");

        fx.put_message("msg_2", SID, &assistant(2_000, Some((3_000, "stop")), (1, 1, 0, 0), MODEL));
        assert!(t.poll());
        assert_eq!(t.queries, 2);
        assert!(!t.poll());
        assert_eq!(t.queries, 2);
    }

    #[test]
    fn a_write_to_another_session_reads_but_reports_no_change() {
        let fx = fixture();
        fx.add_session("ses_other", None, "C:/repo/beta", "Other", 1);
        fx.put_message("msg_1", SID, &user(1_000));
        let mut t = tail(&fx);
        fx.put_message("msg_2", "ses_other", &user(2_000));
        assert!(!t.poll());
        assert_eq!(t.queries, 2);
    }

    #[test]
    fn missing_db_is_not_reopened_every_poll() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = OpenCodeMessageTail::for_session(tmp.path(), SID);
        for _ in 0..3 {
            t.poll();
        }
        assert_eq!(t.queries, 0);
    }

    #[test]
    fn tail_picks_up_a_db_created_after_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = OpenCodeMessageTail::for_session(tmp.path(), SID);
        assert!(t.snapshot.is_none());
        // OpenCode creates its database on first launch.
        let fx = Fixture::in_dir(tmp);
        fx.add_session(SID, None, "C:/repo/alpha", "Title", 1);
        fx.put_message("msg_1", SID, &user(1_000));
        assert!(t.poll());
        assert_eq!(state(&t), SessionState::Busy);
    }

    /// Per-tick cost of 10 OpenCode tabs: each tab's tail `poll()` plus
    /// its discovery `scan()`, against 10 sessions of 300 messages.
    /// Run with `cargo test -p codescope-core --release opencode_tick_cost
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark; prints timings"]
    fn opencode_tick_cost() {
        use crate::agents::opencode::discovery;
        use std::time::{Instant, SystemTime};

        const TABS: usize = 10;
        let fx = Fixture::new();
        let sid = |i: usize| format!("ses_{i:02}");
        let cwd = |i: usize| format!("C:/repo/p{i:02}");
        for i in 0..TABS {
            fx.add_session(&sid(i), None, &cwd(i), "A generated title", 1);
            for m in 0..300i64 {
                let id = format!("msg_{i:02}_{m:04}");
                let created = 1_000 + m * 1_000;
                let data = if m % 10 == 0 {
                    user(created)
                } else {
                    let done = Some((created + 500, "tool-calls"));
                    assistant(created, done, (300, 50, 9_000, 0), MODEL)
                };
                fx.put_message(&id, &sid(i), &data);
                fx.put_tool_part(&format!("prt_{i:02}_{m:04}"), &id, &sid(i), "completed");
            }
        }
        let mut tails: Vec<_> =
            (0..TABS).map(|i| OpenCodeMessageTail::for_session(fx.root(), sid(i))).collect();
        // One telemetry tick (every tail polls) plus one adoption tick
        // (every tab scans). Ticks are >= 250 ms apart in the app, so
        // each starts with the shared stamp expired.
        let tick = |tails: &mut Vec<OpenCodeMessageTail>| {
            db::forget_shared_stamps();
            let start = Instant::now();
            for (i, t) in tails.iter_mut().enumerate() {
                t.poll();
                discovery::scan(fx.root(), &cwd(i), SystemTime::UNIX_EPOCH);
            }
            start.elapsed()
        };
        let measure = |tails: &mut Vec<OpenCodeMessageTail>, label: &str| {
            tick(tails);
            const QUIET: u32 = 2_000;
            let quiet = (0..QUIET).map(|_| tick(tails)).sum::<Duration>() / QUIET;
            // One session streaming: a commit before every tick, so
            // every tail and the discovery cache re-read once.
            const ACTIVE: u32 = 200;
            let mut ticking = Duration::ZERO;
            for n in 0..ACTIVE {
                let created = 400_000 + i64::from(n);
                let data = assistant(created, None, (1, 1, 0, 0), MODEL);
                fx.put_message(&format!("msg_live_{n:04}"), &sid(0), &data);
                ticking += tick(tails);
            }
            let active = ticking / ACTIVE;
            println!("{TABS} tabs, {label}: quiet tick {quiet:?}; tick after a commit {active:?}");
        };
        measure(&mut tails, "stat per call");
        db::TEST_STAMP_REUSE.with(|w| w.set(db::STAMP_REUSE_WINDOW));
        measure(&mut tails, "shared stamp (production)");
    }
}
