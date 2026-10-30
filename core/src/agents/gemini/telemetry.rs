//! Gemini CLI chat tail — per-session telemetry derived from
//! `<gemini-home>/tmp/<project>/chats/session-*.json[l]`.
//!
//! Formats confirmed against gemini-cli 0.30 (chats on disk, and its
//! bundled `chatRecordingService.js` / `geminiChat.js`) and the 0.67
//! sources (`packages/core/src/services/chatRecordingService.ts`).
//!
//! # Two formats, one message list
//!
//! * `.json` (≤ 0.3x): one document `{sessionId, projectHash,
//!   startTime, lastUpdated, messages[], summary?}`, rewritten whole on
//!   every change. Re-parsed whenever its size or mtime moves — parsing
//!   skips tool arguments and results without allocating, and a large
//!   chat is re-parsed at most every [`LEGACY_REPARSE_DUTY`] × its parse
//!   time, so a multi-megabyte chat cannot pin the UI thread.
//! * `.jsonl` (later releases): an append-only log, read incrementally.
//!   A line is a metadata record (`sessionId`, …), a message (has an
//!   `id`; a repeated id replaces the earlier copy in place), `{"$set":
//!   {…}}` (metadata update — `summary`, or a legacy full `messages`
//!   checkpoint), `{"$rewindTo": id}` (drops that message and all
//!   after it) or `{"$patch": {…}}` (content updates, removals,
//!   re-ordering).
//!
//! Both fold into one ordered list of message summaries.
//!
//! # Fields
//!
//! | file says                                         | snapshot |
//! |---------------------------------------------------|----------|
//! | last message `user`                               | Busy |
//! | last message `gemini` with `toolCalls`, all `success`/`error` | Busy (results go back to the model) |
//! | … with a call still `awaiting_approval`/`scheduled`/`validating`/`executing` | PendingToolUse |
//! | … with a `cancelled` call                         | Idle (the turn was cancelled) |
//! | last message `gemini`, no text, no tool calls     | PendingToolUse (0.6x records a tool-call-only reply before the calls run) |
//! | last message `gemini` with text, no tool calls    | Idle once the file has been quiet for [`IDLE_SETTLE`], Busy until then |
//! | last message `info` / `error`                     | Idle (`Request cancelled.`, API errors) |
//! | `tokens.total` of the last `gemini` message       | tokens (overwrites, like Claude) |
//! | `model` of the last `gemini` message              | model; `gemini-*` ids get a 1,048,576-token window |
//! | user messages (not `/…` or `?…` commands)         | turn count; the first one is the [`TitleSource::Prompt`] title |
//! | `summary`                                         | [`TitleSource::Generated`] title |
//! | user → last `gemini` message timestamps           | last turn duration |
//!
//! Why the settle: Gemini records a reply's text when its stream ends,
//! and attaches the tool calls that reply asked for only once they have
//! *finished* — after any approval prompt. A narrated "I will read the
//! file…" followed by a call is indistinguishable from a final answer
//! until then, so a text reply only counts as the end of the turn once
//! nothing has been written for a few seconds. A call that waits for
//! approval or runs longer than that shows Idle meanwhile.
//!
//! Gemini has no in-CLI rename, so there is no [`TitleSource::User`]
//! title. `summary` is written when a *later* session starts in the
//! same project, so a live session is usually titled by its prompt.
//!
//! A Busy session whose chat stays untouched for [`BUSY_QUIET_TIMEOUT`]
//! is shown Idle with `quiet_timeout` set, as for Claude (issue #351).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::Value;

use crate::session_title::{SessionTitle, TitleSource};
use crate::telemetry::{FileTail, LineRead, SessionState, TelemetrySnapshot, read_new_lines};

/// How long a chat must stay unwritten before a text-only reply counts
/// as the end of the turn. See the module docs.
pub const IDLE_SETTLE: Duration = Duration::from_secs(5);

/// How long a Busy session may go without its chat changing before it
/// is shown Idle (with `quiet_timeout`). Same value as the Claude tail's.
pub const BUSY_QUIET_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A legacy `.json` chat is re-parsed at most once per this many times
/// the duration of its last parse (≤ 2% of the UI thread).
pub const LEGACY_REPARSE_DUTY: u32 = 50;

/// How often a tail without a located chat retries, and how often a
/// legacy tail checks whether its chat moved on to a `.jsonl`.
const LOCATE_RETRY: Duration = Duration::from_secs(2);

/// Context window of Gemini 2.5 / 3 models, as gemini-cli's own
/// `tokenLimit` reports it.
const GEMINI_CONTEXT_WINDOW: u64 = 1_048_576;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Any record: a JSONL line, a `$set` body, a legacy document, or a
/// message inside one. Only the fields read are declared — serde skips
/// the rest (tool arguments and results, thoughts) without allocating.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Record {
    #[serde(rename = "$rewindTo")]
    rewind_to: Option<Value>,
    #[serde(rename = "$set")]
    set: Option<Box<Record>>,
    #[serde(rename = "$patch")]
    patch: Option<Patch>,
    id: Option<Value>,
    #[serde(rename = "type")]
    kind: Option<Value>,
    timestamp: Option<Value>,
    content: Option<Value>,
    #[serde(rename = "displayContent")]
    display_content: Option<Value>,
    model: Option<Value>,
    tokens: Option<Value>,
    #[serde(rename = "toolCalls")]
    tool_calls: Option<Vec<ToolCall>>,
    summary: Option<Value>,
    messages: Option<Vec<Record>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolCall {
    status: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Patch {
    id: Option<Value>,
    content: Option<Value>,
    updates: Option<Vec<Patch>>,
    #[serde(rename = "removeIds")]
    remove_ids: Option<Vec<Value>>,
    #[serde(rename = "orderIds")]
    order_ids: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Gemini,
    /// `info` / `error`: the turn ended (cancelled, failed).
    Notice,
    /// `warning` and anything newer: no bearing on the turn.
    Other,
}

/// What the state needs from one message.
#[derive(Debug, Clone)]
struct Msg {
    id: String,
    kind: Kind,
    ts: Option<f64>,
    /// User: a real prompt (not a `/` or `?` command). Gemini: has text.
    text: bool,
    tool_calls: usize,
    tool_open: bool,
    tool_cancelled: bool,
    tokens: Option<u64>,
    model: Option<String>,
}

/// Text of a `content` / `displayContent` value: a string, or the
/// `text` parts of a part list.
fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Gemini's own rule (`isIgnoredUserContent`) for user entries that are
/// commands rather than prompts.
fn is_prompt(text: &str) -> bool {
    let t = text.trim();
    !(t.is_empty()
        || t.starts_with('/')
        || t.starts_with('?')
        || t.starts_with("<session_context>")
        || t.starts_with("<hook_context>"))
}

fn tokens_total(v: &Value) -> Option<u64> {
    let field = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let total = field("total");
    if total > 0 {
        return Some(total);
    }
    let sum = field("input") + field("output") + field("thoughts") + field("tool");
    (sum > 0).then_some(sum)
}

// ---------------------------------------------------------------------------
// Chat state
// ---------------------------------------------------------------------------

/// The message list and titles folded from a chat.
#[derive(Debug, Default)]
struct Chat {
    messages: Vec<Msg>,
    title: Option<SessionTitle>,
}

/// The state a chat's last message implies, before the settle window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Raw {
    State(SessionState),
    /// A text-only reply: Idle if nothing follows within the settle
    /// window, Busy until then.
    TextReply,
}

impl Chat {
    /// Fold one record in.
    fn apply(&mut self, r: Record) {
        if let Some(id) = r.rewind_to.as_ref().and_then(Value::as_str) {
            match self.messages.iter().position(|m| m.id == id) {
                Some(i) => self.messages.truncate(i),
                None => self.messages.clear(),
            }
            return;
        }
        if let Some(p) = r.patch {
            self.apply_patch(&p);
            for u in p.updates.iter().flatten() {
                self.apply_patch(u);
            }
            for id in p.remove_ids.iter().flatten().filter_map(Value::as_str) {
                self.messages.retain(|m| m.id != id);
            }
            if let Some(order) = p.order_ids.as_ref() {
                let ids: Vec<&str> = order.iter().filter_map(Value::as_str).collect();
                let (mut moved, rest): (Vec<Msg>, Vec<Msg>) =
                    self.messages.drain(..).partition(|m| ids.contains(&m.id.as_str()));
                moved.sort_by_key(|m| ids.iter().position(|id| *id == m.id));
                self.messages = rest;
                self.messages.extend(moved);
            }
            return;
        }
        if r.id.as_ref().is_some_and(Value::is_string) {
            self.upsert(r);
            return;
        }
        // `$set` or a metadata record (legacy documents included).
        let body = match r.set {
            Some(set) => *set,
            None => r,
        };
        if let Some(summary) = body.summary.as_ref().and_then(Value::as_str) {
            SessionTitle::offer(&mut self.title, summary, TitleSource::Generated);
        }
        if let Some(messages) = body.messages {
            self.messages.clear();
            for m in messages {
                if m.id.as_ref().is_some_and(Value::is_string) {
                    self.upsert(m);
                }
            }
        }
    }

    fn upsert(&mut self, r: Record) {
        let Some(id) = r.id.as_ref().and_then(Value::as_str).map(str::to_owned) else { return };
        let kind = match r.kind.as_ref().and_then(Value::as_str) {
            Some("user") => Kind::User,
            Some("gemini") => Kind::Gemini,
            Some("info") | Some("error") => Kind::Notice,
            _ => Kind::Other,
        };
        let shown = r.display_content.as_ref().or(r.content.as_ref()).map(content_text);
        let text = match kind {
            Kind::User => shown.as_deref().is_some_and(is_prompt),
            _ => shown.as_deref().is_some_and(|t| !t.trim().is_empty()),
        };
        if kind == Kind::User
            && text
            && let Some(prompt) = shown.as_deref()
        {
            SessionTitle::offer(&mut self.title, prompt, TitleSource::Prompt);
        }
        let statuses: Vec<&str> = r
            .tool_calls
            .iter()
            .flatten()
            .map(|c| c.status.as_ref().and_then(Value::as_str).unwrap_or_default())
            .collect();
        let open = |s: &&str| {
            matches!(*s, "awaiting_approval" | "scheduled" | "validating" | "executing")
        };
        let msg = Msg {
            id,
            kind,
            ts: r
                .timestamp
                .as_ref()
                .and_then(Value::as_str)
                .and_then(crate::time::parse_iso8601_secs),
            text,
            tool_calls: statuses.len(),
            tool_open: statuses.iter().any(open),
            tool_cancelled: statuses.contains(&"cancelled"),
            tokens: r.tokens.as_ref().and_then(tokens_total),
            model: r
                .model
                .as_ref()
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned),
        };
        match self.messages.iter_mut().rev().find(|m| m.id == msg.id) {
            Some(slot) => *slot = msg,
            None => self.messages.push(msg),
        }
    }

    fn apply_patch(&mut self, p: &Patch) {
        let (Some(id), Some(content)) = (p.id.as_ref().and_then(Value::as_str), p.content.as_ref())
        else {
            return;
        };
        let Some(m) = self.messages.iter_mut().rev().find(|m| m.id == id) else { return };
        let text = content_text(content);
        m.text = match m.kind {
            Kind::User => is_prompt(&text),
            _ => !text.trim().is_empty(),
        };
    }

    /// The state the last message that matters implies — warnings and
    /// `/` commands don't move a turn either way.
    fn raw_state(&self) -> Raw {
        let last = self
            .messages
            .iter()
            .rev()
            .find(|m| m.kind != Kind::Other && (m.kind != Kind::User || m.text));
        let Some(m) = last else { return Raw::State(SessionState::Unknown) };
        match m.kind {
            Kind::User => Raw::State(SessionState::Busy),
            Kind::Notice => Raw::State(SessionState::Idle),
            Kind::Gemini if m.tool_cancelled => Raw::State(SessionState::Idle),
            Kind::Gemini if m.tool_open => Raw::State(SessionState::PendingToolUse),
            Kind::Gemini if m.tool_calls > 0 => Raw::State(SessionState::Busy),
            Kind::Gemini if !m.text => Raw::State(SessionState::PendingToolUse),
            Kind::Gemini => Raw::TextReply,
            Kind::Other => Raw::State(SessionState::Unknown),
        }
    }

    /// The snapshot for `state`, or `None` before the chat holds
    /// anything worth showing.
    fn snapshot(&self, state: SessionState) -> Option<TelemetrySnapshot> {
        if state == SessionState::Unknown && self.title.is_none() {
            return None;
        }
        let gemini = || self.messages.iter().rev().filter(|m| m.kind == Kind::Gemini);
        let model = gemini().find_map(|m| m.model.clone());
        let tokens_used = gemini().find_map(|m| m.tokens).unwrap_or(0);
        let last_prompt = self.messages.iter().rposition(|m| m.kind == Kind::User && m.text);
        let last_turn_duration = last_prompt.and_then(|i| {
            let start = self.messages[i].ts?;
            let end = self.messages[i..].iter().rev().find(|m| m.kind == Kind::Gemini)?.ts?;
            (end >= start).then(|| Duration::from_secs_f64(end - start))
        });
        let window =
            model.as_deref().filter(|m| m.contains("gemini")).map(|_| GEMINI_CONTEXT_WINDOW);
        let prompts = self.messages.iter().filter(|m| m.kind == Kind::User && m.text).count();
        Some(TelemetrySnapshot {
            context_pct: window.map(|w| (tokens_used as f32 / w as f32).clamp(0.0, 1.0)),
            model,
            tokens_used,
            turn_count: prompts as u32,
            last_turn_duration,
            state,
            quiet_timeout: false,
            title: self.title.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tail handle
// ---------------------------------------------------------------------------

/// Handle to one Gemini chat file.
#[derive(Debug)]
pub struct GeminiChatTail {
    /// The Gemini session id this tail follows.
    pub session_id: String,
    /// The chat file, once located.
    pub path: Option<PathBuf>,
    gemini_home: PathBuf,
    working_directory: String,
    /// `.jsonl` cursor.
    tail: FileTail,
    /// `.json`: the `(len, mtime)` last parsed successfully.
    legacy_seen: Option<(u64, Option<SystemTime>)>,
    next_legacy_parse: Instant,
    chat: Chat,
    /// Latest computed snapshot, or `None` until the chat says
    /// something worth showing.
    pub snapshot: Option<TelemetrySnapshot>,
    last_activity: Instant,
    observed_mtime: Option<SystemTime>,
    next_locate: Instant,
}

impl GeminiChatTail {
    /// Follow session `session_id` of the project at
    /// `working_directory`. The chat is located through
    /// [`super::discovery::locate_chat`]; until it is found, polls retry
    /// every couple of seconds.
    pub fn for_session(gemini_home: &Path, working_directory: &str, session_id: &str) -> Self {
        let now = Instant::now();
        let mut tail = Self {
            session_id: session_id.to_owned(),
            path: None,
            gemini_home: gemini_home.to_path_buf(),
            working_directory: working_directory.to_owned(),
            tail: FileTail::default(),
            legacy_seen: None,
            next_legacy_parse: now,
            chat: Chat::default(),
            snapshot: None,
            last_activity: now,
            observed_mtime: None,
            next_locate: now,
        };
        tail.poll_at(now);
        tail
    }

    /// Read what changed and update `snapshot`. Returns `true` when the
    /// snapshot changed.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    /// [`Self::poll`] with the tail clock passed in, for tests.
    pub fn poll_at(&mut self, now: Instant) -> bool {
        self.poll_with(now, SystemTime::now())
    }

    fn poll_with(&mut self, now: Instant, wall: SystemTime) -> bool {
        self.relocate(now);
        let Some(path) = self.path.clone() else { return false };
        let legacy = path.extension().is_some_and(|e| e == "json");
        let (dirty, mtime) = if legacy {
            self.read_legacy(&path, now)
        } else {
            let chat = &mut self.chat;
            let read = read_new_lines(&path, &mut self.tail, |read| match read {
                LineRead::Rewritten => *chat = Chat { title: chat.title.take(), ..Chat::default() },
                LineRead::Line(line) => {
                    if let Ok(record) = serde_json::from_str::<Record>(line) {
                        chat.apply(record);
                    }
                }
            });
            (read.consumed, read.mtime)
        };
        if dirty || mtime != self.observed_mtime {
            self.last_activity = now;
        }
        self.observed_mtime = mtime;

        let raw = self.chat.raw_state();
        let mut quiet = false;
        let state = match raw {
            Raw::TextReply => {
                let age = mtime.and_then(|t| wall.duration_since(t).ok()).unwrap_or_default();
                if age >= IDLE_SETTLE { SessionState::Idle } else { SessionState::Busy }
            }
            Raw::State(SessionState::Busy)
                if now.saturating_duration_since(self.last_activity) >= BUSY_QUIET_TIMEOUT =>
            {
                quiet = true;
                SessionState::Idle
            }
            Raw::State(s) => s,
        };
        let shown = self.snapshot.as_ref().map(|s| (s.state, s.quiet_timeout));
        if !dirty && shown == Some((state, quiet)) {
            return false;
        }
        let mut next = self.chat.snapshot(state);
        if let Some(s) = next.as_mut() {
            s.quiet_timeout = quiet;
        }
        if next == self.snapshot {
            return false;
        }
        self.snapshot = next;
        true
    }

    /// Find the chat if it isn't known yet, and follow a legacy chat
    /// that a newer CLI continued in a `.jsonl`.
    fn relocate(&mut self, now: Instant) {
        let legacy = self.path.as_ref().is_some_and(|p| p.extension().is_some_and(|e| e == "json"));
        if (self.path.is_some() && !legacy) || now < self.next_locate {
            return;
        }
        self.next_locate = now + LOCATE_RETRY;
        let found =
            super::discovery::locate_chat(&self.gemini_home, &self.working_directory, &self.session_id);
        if found.is_some() && found != self.path {
            self.path = found;
            self.tail = FileTail::default();
            self.legacy_seen = None;
            self.chat = Chat { title: self.chat.title.take(), ..Chat::default() };
        }
    }

    /// Re-parse a legacy `.json` chat when it changed. Returns whether
    /// the chat state was replaced, and the file's mtime.
    fn read_legacy(&mut self, path: &Path, now: Instant) -> (bool, Option<SystemTime>) {
        let Ok(meta) = std::fs::metadata(path) else { return (false, None) };
        let mtime = crate::telemetry::modified_or_none(&meta);
        let stamp = (meta.len(), mtime);
        if self.legacy_seen == Some(stamp) || now < self.next_legacy_parse {
            return (false, mtime);
        }
        let started = Instant::now();
        let Ok(bytes) = std::fs::read(path) else { return (false, mtime) };
        // A torn read (Gemini truncates, then writes) fails to parse;
        // the next poll tries again.
        let Ok(doc) = serde_json::from_slice::<Record>(&bytes) else { return (false, mtime) };
        let mut chat = Chat { title: self.chat.title.take(), ..Chat::default() };
        chat.apply(doc);
        self.chat = chat;
        self.legacy_seen = Some(stamp);
        self.next_legacy_parse = now + started.elapsed() * LEGACY_REPARSE_DUTY;
        (true, mtime)
    }

    /// Suggested poll interval: 250 ms while Busy / PendingToolUse,
    /// 2 s otherwise — the same cadence as the other tails.
    pub fn poll_interval(&self) -> Duration {
        match self.snapshot.as_ref().map(|s| s.state) {
            Some(SessionState::Busy) | Some(SessionState::PendingToolUse) => {
                Duration::from_millis(250)
            }
            _ => Duration::from_secs(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::gemini::discovery::tests::{SID, home_with_project, jsonl_head};
    use serde_json::json;
    use std::collections::HashMap;

    fn kinds(chat: &Chat) -> HashMap<&'static str, usize> {
        let mut out = HashMap::new();
        for m in &chat.messages {
            let k = match m.kind {
                Kind::User => "user",
                Kind::Gemini => "gemini",
                Kind::Notice => "notice",
                Kind::Other => "other",
            };
            *out.entry(k).or_insert(0) += 1;
        }
        out
    }

    /// A tail clock past any re-parse throttle the last poll set.
    fn later() -> Instant {
        Instant::now() + Duration::from_secs(1)
    }

    fn user(id: &str, text: &str, ts: &str) -> Value {
        json!({"id": id, "timestamp": ts, "type": "user", "content": [{"text": text}]})
    }

    fn reply(id: &str, text: &str, ts: &str) -> Value {
        json!({"id": id, "timestamp": ts, "type": "gemini", "content": text,
               "thoughts": [{"subject": "s", "description": "d"}],
               "tokens": {"input": 8698, "output": 35, "cached": 3081, "thoughts": 238, "tool": 0, "total": 8971},
               "model": "gemini-3-flash-preview"})
    }

    fn with_calls(mut msg: Value, statuses: &[&str]) -> Value {
        let calls: Vec<Value> = statuses
            .iter()
            .enumerate()
            .map(|(i, s)| json!({"id": format!("c{i}"), "name": "read_file", "args": {"path": "x"},
                                 "result": [{"functionResponse": {"response": {"output": "…"}}}],
                                 "status": s}))
            .collect();
        msg["toolCalls"] = Value::Array(calls);
        msg
    }

    fn chat_of(records: &[Value]) -> Chat {
        let mut chat = Chat::default();
        for r in records {
            chat.apply(serde_json::from_value(r.clone()).unwrap());
        }
        chat
    }

    fn doc(messages: &[Value], summary: Option<&str>) -> Value {
        let mut d = json!({"sessionId": SID, "projectHash": "6e31", "startTime": "2026-02-27T12:00:00.000Z",
                           "lastUpdated": "2026-02-27T12:00:00.000Z", "messages": messages});
        if let Some(s) = summary {
            d["summary"] = json!(s);
        }
        d
    }

    const T0: &str = "2026-02-27T12:00:00.000Z";
    const T1: &str = "2026-02-27T12:00:05.000Z";
    const T2: &str = "2026-02-27T12:00:12.000Z";

    // --- state derivation ---

    #[test]
    fn empty_chat_has_no_snapshot() {
        let chat = Chat::default();
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Unknown));
        assert_eq!(chat.snapshot(SessionState::Unknown), None);
    }

    #[test]
    fn prompt_is_busy() {
        let chat = chat_of(&[user("u1", "fix the bug", T0)]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Busy));
    }

    #[test]
    fn finished_tool_calls_are_busy() {
        let chat = chat_of(&[
            user("u1", "fix", T0),
            with_calls(reply("g1", "I will read the file.", T1), &["success", "error"]),
        ]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Busy));
    }

    #[test]
    fn open_tool_call_is_pending() {
        let chat = chat_of(&[user("u1", "fix", T0), with_calls(reply("g1", "", T1), &["awaiting_approval"])]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::PendingToolUse));
    }

    #[test]
    fn tool_call_only_reply_is_pending() {
        // 0.6x records a reply with no text before its calls run.
        let chat = chat_of(&[user("u1", "fix", T0), reply("g1", "", T1)]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::PendingToolUse));
    }

    #[test]
    fn cancelled_call_ends_the_turn() {
        let chat = chat_of(&[
            user("u1", "fix", T0),
            with_calls(reply("g1", "I will run it.", T1), &["cancelled"]),
        ]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Idle));
    }

    #[test]
    fn text_reply_waits_for_the_settle_window() {
        let chat = chat_of(&[user("u1", "fix", T0), reply("g1", "Done.", T1)]);
        assert_eq!(chat.raw_state(), Raw::TextReply);
    }

    #[test]
    fn notices_end_the_turn_and_warnings_are_ignored() {
        let cancelled = json!({"id": "i1", "timestamp": T2, "type": "info", "content": "Request cancelled."});
        let chat = chat_of(&[user("u1", "fix", T0), cancelled]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Idle));
        let warning = json!({"id": "w1", "timestamp": T2, "type": "warning", "content": "slow"});
        let chat = chat_of(&[user("u1", "fix", T0), warning]);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Busy));
    }

    #[test]
    fn commands_are_not_prompts() {
        let chat = chat_of(&[
            user("u1", "fix", T0),
            reply("g1", "Done.", T1),
            user("u2", "/model", T2),
        ]);
        assert_eq!(chat.raw_state(), Raw::TextReply);
        let s = chat.snapshot(SessionState::Idle).unwrap();
        assert_eq!(s.turn_count, 1);
    }

    // --- tokens, model, duration, titles ---

    #[test]
    fn tokens_model_and_duration_come_from_the_last_reply() {
        let mut second = reply("g2", "Done.", T2);
        second["tokens"]["total"] = json!(104_857);
        let chat = chat_of(&[
            user("u1", "fix the resize bug", T0),
            with_calls(reply("g1", "Reading.", T1), &["success"]),
            second,
        ]);
        let s = chat.snapshot(SessionState::Idle).unwrap();
        assert_eq!(s.model.as_deref(), Some("gemini-3-flash-preview"));
        assert_eq!(s.tokens_used, 104_857);
        assert_eq!(s.context_pct, Some(104_857.0 / 1_048_576.0));
        assert_eq!(s.last_turn_duration, Some(Duration::from_secs(12)));
        assert_eq!(s.turn_count, 1);
    }

    #[test]
    fn tokens_fall_back_to_the_parts_when_total_is_missing() {
        assert_eq!(tokens_total(&json!({"input": 10, "output": 5, "thoughts": 2})), Some(17));
        assert_eq!(tokens_total(&json!({})), None);
    }

    #[test]
    fn first_prompt_titles_then_summary_replaces_it() {
        let mut chat = chat_of(&[
            user("u1", "fix the resize bug\nmore", T0),
            user("u2", "second prompt", T1),
        ]);
        assert_eq!(
            chat.title,
            Some(SessionTitle { text: "fix the resize bug".into(), source: TitleSource::Prompt })
        );
        chat.apply(serde_json::from_value(json!({"$set": {"summary": "Resize bug fix"}})).unwrap());
        assert_eq!(
            chat.title,
            Some(SessionTitle { text: "Resize bug fix".into(), source: TitleSource::Generated })
        );
    }

    #[test]
    fn display_content_is_the_prompt_shown() {
        let mut u = user("u1", "expanded @file contents", T0);
        u["displayContent"] = json!([{"text": "look at @file"}]);
        let chat = chat_of(&[u]);
        assert_eq!(chat.title.unwrap().text, "look at @file");
    }

    // --- JSONL records ---

    #[test]
    fn repeated_id_replaces_in_place() {
        let chat = chat_of(&[
            user("u1", "fix", T0),
            reply("g1", "I will read it.", T1),
            with_calls(reply("g1", "I will read it.", T1), &["success"]),
        ]);
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Busy));
    }

    #[test]
    fn rewind_drops_the_message_and_everything_after() {
        let mut chat = chat_of(&[user("u1", "a", T0), reply("g1", "b", T1), user("u2", "c", T2)]);
        chat.apply(serde_json::from_value(json!({"$rewindTo": "g1"})).unwrap());
        assert_eq!(chat.messages.len(), 1);
        chat.apply(serde_json::from_value(json!({"$rewindTo": "unknown"})).unwrap());
        assert!(chat.messages.is_empty());
    }

    #[test]
    fn patch_updates_content_removes_and_reorders() {
        let mut chat = chat_of(&[user("u1", "a", T0), reply("g1", "", T1), reply("g2", "x", T2)]);
        chat.apply(
            serde_json::from_value(json!({"$patch": {"updates": [{"id": "g1", "content": "now text"}],
                                                     "removeIds": ["g2"]}}))
            .unwrap(),
        );
        assert_eq!(chat.raw_state(), Raw::TextReply);
        chat.apply(serde_json::from_value(json!({"$patch": {"orderIds": ["u1"]}})).unwrap());
        assert_eq!(chat.messages.last().unwrap().id, "u1");
        assert_eq!(chat.raw_state(), Raw::State(SessionState::Busy));
    }

    #[test]
    fn set_messages_checkpoint_replaces_the_list() {
        let mut chat = chat_of(&[user("u1", "a", T0)]);
        chat.apply(
            serde_json::from_value(json!({"$set": {"messages": [user("u9", "z", T0), reply("g9", "y", T1)]}}))
                .unwrap(),
        );
        assert_eq!(kinds(&chat), HashMap::from([("user", 1), ("gemini", 1)]));
    }

    #[test]
    fn legacy_document_parses_as_one_record() {
        let d = doc(&[user("u1", "fix", T0), reply("g1", "Done.", T1)], Some("Fix it"));
        let chat = chat_of(&[d]);
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.title.unwrap().text, "Fix it");
    }

    // --- tail ---

    fn write_json(path: &Path, v: &Value) {
        std::fs::write(path, serde_json::to_string_pretty(v).unwrap()).unwrap();
    }

    fn append_line(path: &Path, v: &Value) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{v}").unwrap();
    }

    fn home_of(chats: &Path) -> PathBuf {
        chats.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
    }

    const CWD: &str = "C:/repo/alpha";

    #[test]
    fn legacy_tail_follows_rewrites_and_settles_to_idle() {
        let (_tmp, chats) = home_with_project(CWD, "alpha");
        let path = chats.join("session-2026-02-27T12-00-d1c96a3f.json");
        write_json(&path, &doc(&[user("u1", "fix it", T0)], None));
        let mut tail = GeminiChatTail::for_session(&home_of(&chats), CWD, SID);
        assert_eq!(tail.path.as_deref(), Some(path.as_path()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);

        write_json(&path, &doc(&[user("u1", "fix it", T0), reply("g1", "Done.", T1)], None));
        let wall = SystemTime::now();
        tail.poll_with(later(), wall);
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy, "just written");
        assert!(tail.poll_with(later(), wall + IDLE_SETTLE));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.last_turn_duration, Some(Duration::from_secs(5)));
        assert_eq!(s.title.unwrap().text, "fix it");
    }

    #[test]
    fn legacy_tail_retries_a_torn_document() {
        let (_tmp, chats) = home_with_project(CWD, "alpha");
        let path = chats.join("session-2026-02-27T12-00-d1c96a3f.json");
        write_json(&path, &doc(&[user("u1", "fix it", T0)], None));
        let mut tail = GeminiChatTail::for_session(&home_of(&chats), CWD, SID);
        let full = serde_json::to_string_pretty(&doc(
            &[user("u1", "fix it", T0), with_calls(reply("g1", "", T1), &["awaiting_approval"])],
            None,
        ))
        .unwrap();
        std::fs::write(&path, &full[..full.len() / 2]).unwrap();
        tail.poll_at(later());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        std::fs::write(&path, &full).unwrap();
        assert!(tail.poll_at(later()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn jsonl_tail_reads_records_incrementally() {
        let (_tmp, chats) = home_with_project(CWD, "alpha");
        let path = chats.join("session-2026-10-30T10-00-d1c96a3f.jsonl");
        std::fs::write(&path, jsonl_head(SID, "main")).unwrap();
        let start = Instant::now();
        let mut tail = GeminiChatTail::for_session(&home_of(&chats), CWD, SID);
        assert!(tail.snapshot.is_none());
        append_line(&path, &user("u1", "fix it", T0));
        assert!(tail.poll_with(start, SystemTime::now()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        append_line(&path, &reply("g1", "", T1));
        assert!(tail.poll_with(start, SystemTime::now()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::PendingToolUse);
        append_line(&path, &with_calls(reply("g1", "", T1), &["success"]));
        append_line(&path, &json!({"$set": {"lastUpdated": T2}}));
        assert!(tail.poll_with(start, SystemTime::now()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        append_line(&path, &reply("g2", "All done.", T2));
        append_line(&path, &json!({"$set": {"summary": "Fix it"}}));
        let wall = SystemTime::now();
        tail.poll_with(start, wall);
        assert!(tail.poll_with(start, wall + IDLE_SETTLE));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.title.unwrap(), SessionTitle { text: "Fix it".into(), source: TitleSource::Generated });
    }

    #[test]
    fn tail_moves_from_legacy_json_to_its_jsonl_continuation() {
        let (_tmp, chats) = home_with_project(CWD, "alpha");
        let legacy = chats.join("session-2026-02-27T12-00-d1c96a3f.json");
        write_json(&legacy, &doc(&[user("u1", "fix it", T0), reply("g1", "Done.", T1)], None));
        let old = SystemTime::now() - Duration::from_secs(600);
        std::fs::File::options().write(true).open(&legacy).unwrap().set_modified(old).unwrap();
        let start = Instant::now();
        let mut tail = GeminiChatTail::for_session(&home_of(&chats), CWD, SID);
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);
        let log = chats.join("session-2026-10-30T10-00-d1c96a3f.jsonl");
        std::fs::write(&log, jsonl_head(SID, "main")).unwrap();
        append_line(&log, &user("u1", "fix it", T0));
        append_line(&log, &reply("g1", "Done.", T1));
        append_line(&log, &user("u2", "and the tests", T2));
        crate::agents::dir_listing::forget(&chats);
        assert!(tail.poll_with(start + LOCATE_RETRY * 2, SystemTime::now()));
        assert_eq!(tail.path.as_deref(), Some(log.as_path()));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!((s.state, s.turn_count), (SessionState::Busy, 2));
    }

    #[test]
    fn quiet_busy_session_goes_idle() {
        let (_tmp, chats) = home_with_project(CWD, "alpha");
        let path = chats.join("session-2026-10-30T10-00-d1c96a3f.jsonl");
        std::fs::write(&path, jsonl_head(SID, "main")).unwrap();
        append_line(&path, &user("u1", "fix it", T0));
        let start = Instant::now();
        let mut tail = GeminiChatTail::for_session(&home_of(&chats), CWD, SID);
        assert!(!tail.poll_at(start + BUSY_QUIET_TIMEOUT / 2));
        assert!(tail.poll_at(start + BUSY_QUIET_TIMEOUT + Duration::from_secs(1)));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!((s.state, s.quiet_timeout), (SessionState::Idle, true));
    }

    #[test]
    fn poll_interval_tracks_state() {
        let (_tmp, chats) = home_with_project(CWD, "alpha");
        let path = chats.join("session-2026-10-30T10-00-d1c96a3f.jsonl");
        std::fs::write(&path, jsonl_head(SID, "main")).unwrap();
        let mut tail = GeminiChatTail::for_session(&home_of(&chats), CWD, SID);
        assert_eq!(tail.poll_interval(), Duration::from_secs(2));
        append_line(&path, &user("u1", "fix it", T0));
        tail.poll();
        assert_eq!(tail.poll_interval(), Duration::from_millis(250));
    }
}
