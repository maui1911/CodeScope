//! Codex rollout tail — per-session telemetry derived from
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<thread-id>.jsonl`
//! plus the thread's name in `$CODEX_HOME/session_index.jsonl`.
//!
//! Formats confirmed against codex-cli 0.160 rollouts on disk and the
//! `openai/codex` sources (`codex-rs/protocol/src/protocol.rs`,
//! `codex-rs/rollout/src/policy.rs`, `.../session_index.rs`).
//!
//! # Rollout lines
//!
//! Every line is `{"timestamp", "type", "payload"}`. What each one
//! drives:
//!
//! | line                                              | effect |
//! |---------------------------------------------------|--------|
//! | `event_msg` `task_started` (alias `turn_started`) | Busy; `model_context_window` sizes the context bar; anchors the turn timer |
//! | `event_msg` `user_message` (legacy history)       | a prompt: turn count + 1, `Prompt` title |
//! | `event_msg` `item_completed`, item `UserMessage` (paginated history) | the same, from `item.content[].text` |
//! | `response_item` `function_call` / `custom_tool_call` / `local_shell_call` / `tool_search_call` | PendingToolUse until every `call_id` is answered |
//! | `response_item` `*_call_output`                   | answers its `call_id`; Busy once none is left |
//! | `event_msg` `token_count`                         | `info.last_token_usage.total_tokens` → tokens (overwrites, like Claude); `info.model_context_window` |
//! | `turn_context`                                    | `model` |
//! | `event_msg` `task_complete` (alias `turn_complete`) | Idle; `duration_ms` → last turn duration |
//! | `event_msg` `turn_aborted`                        | Idle (interrupted) |
//!
//! Everything else (`session_meta`, `world_state`, reasoning, messages,
//! `token_usage_record`, …) is skipped. Codex does not persist approval
//! requests, so a call waiting for the user and a call that is running
//! both read as PendingToolUse — the same as Claude's `tool_use`.
//!
//! Like Claude's tail, a Busy session whose rollout stays untouched for
//! [`BUSY_QUIET_TIMEOUT`] is shown Idle with `quiet_timeout` set — a
//! `codex` killed mid-turn never writes `task_complete`.
//!
//! # Titles
//!
//! `session_index.jsonl` is an append-only `{id, thread_name,
//! updated_at}` log shared by every thread; the newest line for an id
//! wins. Both the TUI's generated titles and `/rename` land there with
//! nothing saying which is which, and a saved name is what Codex itself
//! shows — so every name is offered as [`TitleSource::Generated`],
//! which lets the newest one replace the last. The first prompt is the
//! [`TitleSource::Prompt`] fallback.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::Value;

use crate::session_title::{SessionTitle, TitleSource};
use crate::telemetry::{
    FileTail, LineRead, SessionState, TelemetrySnapshot, context_window_for_model, read_new_lines,
};

/// How long a Busy session may go without its rollout changing before
/// it is shown Idle (with `quiet_timeout`). Same reasoning and value as
/// the Claude tail's (issue #351).
pub const BUSY_QUIET_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How often a tail looks for its rollout file again: until it first
/// appears, then for a `thread/revert` moving the thread to a new one.
const LOCATE_RETRY: Duration = Duration::from_secs(2);

/// How often the shared `session_index.jsonl` is checked for a new
/// thread name. A title can lag a second; the rollout cannot.
const INDEX_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// `$CODEX_HOME`, else `<home>/.codex` — where Codex keeps its state.
pub fn default_codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".codex"))
}

/// `<codex_home>/sessions`, the root of the dated rollout tree.
pub fn sessions_root(codex_home: &Path) -> PathBuf {
    codex_home.join("sessions")
}

/// `<codex_home>/session_index.jsonl`, the thread-name log.
pub fn session_index_path(codex_home: &Path) -> PathBuf {
    codex_home.join("session_index.jsonl")
}

// ---------------------------------------------------------------------------
// Line parser
// ---------------------------------------------------------------------------

/// One rollout line, with only the fields the parser reads declared —
/// serde skips the rest (tool output, base instructions, encrypted
/// reasoning) without allocating. Fields are loose `Value`s so a shape
/// change in one of them drops that field, not the line.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<Value>,
    model: Option<Value>,
    call_id: Option<Value>,
    model_context_window: Option<Value>,
    duration_ms: Option<Value>,
    info: Option<Value>,
    message: Option<Value>,
    item: Option<Value>,
}

const TOOL_CALLS: &[&str] =
    &["function_call", "custom_tool_call", "local_shell_call", "tool_search_call"];
const TOOL_OUTPUTS: &[&str] =
    &["function_call_output", "custom_tool_call_output", "tool_search_output"];

/// Running state folded from rollout lines and index entries.
#[derive(Debug, Default)]
struct RolloutParser {
    state: SessionState,
    pending_calls: HashSet<String>,
    model: Option<String>,
    context_window: Option<u64>,
    tokens_used: u64,
    turn_count: u32,
    last_turn_duration: Option<Duration>,
    turn_started_secs: Option<f64>,
    title: Option<SessionTitle>,
}

impl RolloutParser {
    /// Fold one rollout line in. Returns `true` when it changed
    /// anything the snapshot shows.
    fn apply_line(&mut self, line: &str) -> bool {
        let Ok(line) = serde_json::from_str::<Line>(line) else {
            return false;
        };
        let Some(p) = line.payload else { return false };
        let ts = line.timestamp.as_deref().and_then(crate::time::parse_iso8601_secs);
        let sub = p.kind.as_ref().and_then(Value::as_str).unwrap_or_default();
        match (line.kind.as_deref(), sub) {
            (Some("event_msg"), "task_started" | "turn_started") => {
                self.state = SessionState::Busy;
                self.pending_calls.clear();
                self.turn_started_secs = ts;
                if let Some(w) = p.model_context_window.as_ref().and_then(Value::as_u64) {
                    self.context_window = Some(w);
                }
                true
            }
            (Some("event_msg"), "task_complete" | "turn_complete") => {
                self.state = SessionState::Idle;
                self.pending_calls.clear();
                let measured = match (self.turn_started_secs, ts) {
                    (Some(start), Some(end)) if end >= start => {
                        Some(Duration::from_secs_f64(end - start))
                    }
                    _ => None,
                };
                self.last_turn_duration = p
                    .duration_ms
                    .as_ref()
                    .and_then(Value::as_u64)
                    .map(Duration::from_millis)
                    .or(measured);
                true
            }
            (Some("event_msg"), "turn_aborted") => {
                self.state = SessionState::Idle;
                self.pending_calls.clear();
                true
            }
            (Some("event_msg"), "token_count") => {
                let Some(info) = p.info.as_ref() else { return false };
                if let Some(w) = info.get("model_context_window").and_then(Value::as_u64) {
                    self.context_window = Some(w);
                }
                let Some(total) = info
                    .get("last_token_usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(Value::as_u64)
                else {
                    return false;
                };
                self.tokens_used = total;
                true
            }
            (Some("event_msg"), "user_message") => {
                let text = p.message.as_ref().and_then(Value::as_str).unwrap_or_default();
                self.prompt(text);
                true
            }
            (Some("event_msg"), "item_completed") => {
                let Some(item) = p.item.as_ref() else { return false };
                if item.get("type").and_then(Value::as_str) != Some("UserMessage") {
                    return false;
                }
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                self.prompt(&text);
                true
            }
            (Some("response_item"), kind) if TOOL_CALLS.contains(&kind) => {
                let Some(id) = p.call_id.as_ref().and_then(Value::as_str) else {
                    return false;
                };
                self.pending_calls.insert(id.to_owned());
                self.state = SessionState::PendingToolUse;
                true
            }
            (Some("response_item"), kind) if TOOL_OUTPUTS.contains(&kind) => {
                if let Some(id) = p.call_id.as_ref().and_then(Value::as_str) {
                    self.pending_calls.remove(id);
                }
                if self.pending_calls.is_empty() && self.state == SessionState::PendingToolUse {
                    self.state = SessionState::Busy;
                    return true;
                }
                false
            }
            (Some("turn_context"), _) => {
                let Some(model) = p.model.as_ref().and_then(Value::as_str) else {
                    return false;
                };
                if model.is_empty() || self.model.as_deref() == Some(model) {
                    return false;
                }
                self.model = Some(model.to_owned());
                true
            }
            _ => false,
        }
    }

    fn prompt(&mut self, text: &str) {
        self.state = SessionState::Busy;
        self.turn_count += 1;
        SessionTitle::offer(&mut self.title, text, TitleSource::Prompt);
    }

    /// The snapshot this state shows, or `None` before the rollout has
    /// said anything worth showing.
    fn snapshot(&self) -> Option<TelemetrySnapshot> {
        if self.state == SessionState::Unknown && self.title.is_none() && self.model.is_none() {
            return None;
        }
        let window = self
            .context_window
            .or_else(|| self.model.as_deref().and_then(context_window_for_model))
            .filter(|w| *w > 0);
        Some(TelemetrySnapshot {
            model: self.model.clone(),
            tokens_used: self.tokens_used,
            context_pct: window.map(|w| (self.tokens_used as f32 / w as f32).clamp(0.0, 1.0)),
            turn_count: self.turn_count,
            last_turn_duration: self.last_turn_duration,
            state: self.state,
            quiet_timeout: false,
            title: self.title.clone(),
        })
    }
}

/// One `session_index.jsonl` line.
#[derive(Deserialize)]
struct IndexEntry {
    id: String,
    thread_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Tail handle
// ---------------------------------------------------------------------------

/// Handle to one Codex thread's rollout and index entries. Repeated
/// `poll()` calls only read what was appended since the last one.
#[derive(Debug)]
pub struct CodexRolloutTail {
    /// The Codex thread id this tail follows.
    pub session_id: String,
    /// The rollout file, once located.
    pub path: Option<PathBuf>,
    sessions_root: PathBuf,
    index_path: PathBuf,
    tail: FileTail,
    index_tail: FileTail,
    parser: RolloutParser,
    /// Latest computed snapshot, or `None` until the rollout says
    /// something worth showing.
    pub snapshot: Option<TelemetrySnapshot>,
    last_activity: Instant,
    observed_mtime: Option<SystemTime>,
    next_locate: Instant,
    next_index_check: Instant,
}

impl CodexRolloutTail {
    /// Follow thread `session_id` under `codex_home`. The rollout is
    /// located through [`super::discovery::locate_rollout`]; until it
    /// is found, polls retry every couple of seconds.
    pub fn for_session(codex_home: &Path, session_id: &str) -> Self {
        let now = Instant::now();
        let mut tail = Self {
            session_id: session_id.to_owned(),
            path: None,
            sessions_root: sessions_root(codex_home),
            index_path: session_index_path(codex_home),
            tail: FileTail::default(),
            index_tail: FileTail::default(),
            parser: RolloutParser::default(),
            snapshot: None,
            last_activity: now,
            observed_mtime: None,
            next_locate: now,
            next_index_check: now,
        };
        tail.poll_at(now);
        tail
    }

    /// Read what was appended and update `snapshot`. Returns `true`
    /// when the snapshot changed.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    /// [`Self::poll`] with the clock passed in, for the quiet-window
    /// tests.
    pub fn poll_at(&mut self, now: Instant) -> bool {
        let mut dirty = self.relocate(now);
        if let Some(path) = self.path.as_deref() {
            let parser = &mut self.parser;
            let read = read_new_lines(path, &mut self.tail, |read| match read {
                // Keep the title: the index entries that set it are
                // not re-read.
                LineRead::Rewritten => {
                    *parser = RolloutParser { title: parser.title.take(), ..Default::default() }
                }
                LineRead::Line(line) => {
                    parser.apply_line(line);
                }
            });
            if read.consumed || read.mtime != self.observed_mtime {
                self.last_activity = now;
            }
            self.observed_mtime = read.mtime;
            dirty |= read.consumed;
        }
        if now >= self.next_index_check {
            self.next_index_check = now + INDEX_CHECK_INTERVAL;
            let id = self.session_id.as_str();
            let parser = &mut self.parser;
            dirty |= read_new_lines(&self.index_path, &mut self.index_tail, |read| {
                // A rewrite re-reads every entry; the newest still wins.
                let LineRead::Line(line) = read else { return };
                // Cheap filter first: the index holds every thread's names.
                if !line.contains(id) {
                    return;
                }
                if let Ok(entry) = serde_json::from_str::<IndexEntry>(line)
                    && entry.id == id
                    && let Some(name) = entry.thread_name.as_deref()
                {
                    SessionTitle::offer(&mut parser.title, name, TitleSource::Generated);
                }
            })
            .consumed;
        }

        let quiet = self.parser.state == SessionState::Busy
            && now.saturating_duration_since(self.last_activity) >= BUSY_QUIET_TIMEOUT;
        let shown_quiet = self.snapshot.as_ref().is_some_and(|s| s.quiet_timeout);
        if !dirty && quiet == shown_quiet {
            return false;
        }
        let mut next = self.parser.snapshot();
        if quiet && let Some(s) = next.as_mut() {
            // Not a finished turn: the app stays quiet instead of
            // announcing "Turn complete".
            s.state = SessionState::Idle;
            s.quiet_timeout = true;
        }
        if next == self.snapshot {
            return false;
        }
        self.snapshot = next;
        true
    }

    /// Every [`LOCATE_RETRY`], find the thread's rollout again: the
    /// first time, and after a `thread/revert` moved the thread on to a
    /// new file. A switch starts the new file from the top, keeping the
    /// title (the index entries that set it are not re-read). Returns
    /// whether the file changed.
    fn relocate(&mut self, now: Instant) -> bool {
        if now < self.next_locate {
            return false;
        }
        self.next_locate = now + LOCATE_RETRY;
        let found = super::discovery::locate_rollout(&self.sessions_root, &self.session_id);
        if found.is_none() || found == self.path {
            return false;
        }
        let moved = self.path.is_some();
        self.path = found;
        self.tail = FileTail::default();
        self.parser = RolloutParser { title: self.parser.title.take(), ..Default::default() };
        self.observed_mtime = None;
        moved
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
    use crate::agents::codex::discovery::tests::{ID_A, meta_line};
    use tempfile::TempDir;

    fn ev(kind: &str, extra: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-30T10:36:55.000Z","type":"event_msg","payload":{{"type":"{kind}"{extra}}}}}"#
        )
    }

    fn ev_at(ts: &str, kind: &str, extra: &str) -> String {
        ev(kind, extra).replace("2026-10-30T10:36:55.000Z", ts)
    }

    fn item(kind: &str, call_id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-30T10:36:56.000Z","type":"response_item","payload":{{"type":"{kind}","id":"x","call_id":"{call_id}","output":[{{"type":"input_text","text":"\"call_id\":\"zzz\""}}]}}}}"#
        )
    }

    fn token_count(total: u64, window: u64) -> String {
        ev(
            "token_count",
            &format!(
                r#","info":{{"total_token_usage":{{"total_tokens":999999}},"last_token_usage":{{"input_tokens":1,"total_tokens":{total}}},"model_context_window":{window}}}"#
            ),
        )
    }

    fn turn_context(model: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-30T10:36:55.100Z","type":"turn_context","payload":{{"turn_id":"t","cwd":"C:/repo/alpha","model":"{model}","effort":"high"}}}}"#
        )
    }

    fn paginated_prompt(text: &str) -> String {
        ev(
            "item_completed",
            &format!(r#","item":{{"type":"UserMessage","id":"u","content":[{{"type":"text","text":"{text}"}}]}}"#),
        )
    }

    fn parse(lines: &[String]) -> RolloutParser {
        let mut p = RolloutParser::default();
        for l in lines {
            p.apply_line(l);
        }
        p
    }

    // --- parser: state transitions ---

    #[test]
    fn nothing_but_meta_shows_no_snapshot() {
        let p = parse(&[meta_line(ID_A, "C:/repo/alpha", "cli")]);
        assert_eq!(p.snapshot(), None);
    }

    #[test]
    fn task_started_is_busy() {
        let p = parse(&[ev("task_started", r#","model_context_window":475000"#)]);
        assert_eq!(p.snapshot().unwrap().state, SessionState::Busy);
        assert_eq!(p.context_window, Some(475_000));
    }

    #[test]
    fn turn_started_alias_is_busy() {
        let p = parse(&[ev("turn_started", "")]);
        assert_eq!(p.state, SessionState::Busy);
    }

    #[test]
    fn tool_call_is_pending_until_its_output() {
        let mut p = parse(&[ev("task_started", ""), item("function_call", "c1")]);
        assert_eq!(p.state, SessionState::PendingToolUse);
        p.apply_line(&item("function_call_output", "c1"));
        assert_eq!(p.state, SessionState::Busy);
    }

    #[test]
    fn every_call_kind_is_pending() {
        for kind in TOOL_CALLS {
            let p = parse(&[ev("task_started", ""), item(kind, "c1")]);
            assert_eq!(p.state, SessionState::PendingToolUse, "{kind}");
        }
    }

    #[test]
    fn parallel_calls_stay_pending_until_all_answered() {
        let mut p = parse(&[
            ev("task_started", ""),
            item("custom_tool_call", "c1"),
            item("function_call", "c2"),
            item("custom_tool_call_output", "c1"),
        ]);
        assert_eq!(p.state, SessionState::PendingToolUse);
        p.apply_line(&item("function_call_output", "c2"));
        assert_eq!(p.state, SessionState::Busy);
    }

    #[test]
    fn task_complete_is_idle_with_reported_duration() {
        let p = parse(&[
            ev("task_started", ""),
            item("function_call", "c1"),
            ev("task_complete", r#","duration_ms":101656"#),
        ]);
        let s = p.snapshot().unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.last_turn_duration, Some(Duration::from_millis(101_656)));
        assert!(p.pending_calls.is_empty());
    }

    #[test]
    fn task_complete_without_duration_measures_from_turn_start() {
        let p = parse(&[
            ev_at("2026-10-30T10:00:00.000Z", "task_started", ""),
            ev_at("2026-10-30T10:00:42.000Z", "turn_complete", ""),
        ]);
        assert_eq!(p.last_turn_duration, Some(Duration::from_secs(42)));
    }

    #[test]
    fn turn_aborted_is_idle() {
        let p = parse(&[ev("task_started", ""), item("function_call", "c1"), ev("turn_aborted", "")]);
        assert_eq!(p.state, SessionState::Idle);
    }

    #[test]
    fn next_turn_after_idle_is_busy_again() {
        let p = parse(&[
            ev("task_started", ""),
            ev("task_complete", ""),
            paginated_prompt("again"),
            ev("task_started", ""),
        ]);
        assert_eq!(p.state, SessionState::Busy);
    }

    #[test]
    fn call_id_inside_output_text_is_not_the_payloads() {
        // The fixture's output text contains `"call_id":"zzz"`.
        let mut p = parse(&[ev("task_started", ""), item("function_call", "c1")]);
        p.apply_line(&item("function_call_output", "c1"));
        assert_eq!(p.state, SessionState::Busy);
    }

    #[test]
    fn malformed_and_unknown_lines_change_nothing() {
        let mut p = parse(&[ev("task_started", "")]);
        assert!(!p.apply_line("{not json"));
        assert!(!p.apply_line(r#"{"type":"world_state","payload":{"full":true}}"#));
        assert!(!p.apply_line(&ev("exec_command_end", "")));
        assert_eq!(p.state, SessionState::Busy);
    }

    // --- parser: tokens, model, prompts ---

    #[test]
    fn token_count_overwrites_tokens_and_sizes_the_window() {
        let p = parse(&[
            ev("task_started", ""),
            token_count(17_813, 475_000),
            token_count(95_000, 475_000),
        ]);
        let s = p.snapshot().unwrap();
        assert_eq!(s.tokens_used, 95_000);
        assert_eq!(s.context_pct, Some(0.2));
    }

    #[test]
    fn turn_context_sets_the_model() {
        let p = parse(&[turn_context("gpt-6.1-sol"), ev("task_started", "")]);
        let s = p.snapshot().unwrap();
        assert_eq!(s.model.as_deref(), Some("gpt-6.1-sol"));
        // No window reported and none known for the id.
        assert_eq!(s.context_pct, None);
    }

    #[test]
    fn prompts_count_turns_and_the_first_titles_the_session() {
        let p = parse(&[
            ev("user_message", r#","message":"fix the resize bug\nwith details""#),
            ev("task_started", ""),
            ev("task_complete", ""),
            paginated_prompt("second prompt"),
        ]);
        let s = p.snapshot().unwrap();
        assert_eq!(s.turn_count, 2);
        assert_eq!(
            s.title,
            Some(SessionTitle { text: "fix the resize bug".into(), source: TitleSource::Prompt })
        );
    }

    #[test]
    fn non_user_completed_items_are_not_prompts() {
        let p = parse(&[ev(
            "item_completed",
            r#","item":{"type":"AgentMessage","content":[{"type":"Text","text":"hi"}]}"#,
        )]);
        assert_eq!(p.turn_count, 0);
        assert_eq!(p.snapshot(), None);
    }

    // --- tail: files, titles, cadence ---

    struct Fixture {
        _tmp: TempDir,
        home: PathBuf,
        rollout: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        // The UTC day of ID_A's v7 timestamp.
        let c = crate::time::unix_secs_to_civil(1_793_356_614);
        let dir = sessions_root(&home)
            .join(format!("{:04}", c.year))
            .join(format!("{:02}", c.month))
            .join(format!("{:02}", c.day));
        std::fs::create_dir_all(&dir).unwrap();
        let rollout = dir.join(format!("rollout-2026-10-30T11-36-54-{ID_A}.jsonl"));
        std::fs::write(&rollout, meta_line(ID_A, "C:/repo/alpha", "cli")).unwrap();
        Fixture { _tmp: tmp, home, rollout }
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    fn index_line(id: &str, name: &str) -> String {
        format!(r#"{{"id":"{id}","thread_name":"{name}","updated_at":"2026-10-30T10:40:00Z"}}"#)
            + "\n"
    }

    #[test]
    fn tail_locates_the_rollout_and_follows_appends() {
        let f = fixture();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert_eq!(tail.path.as_deref(), Some(f.rollout.as_path()));
        assert!(tail.snapshot.is_none());
        append(&f.rollout, &(ev("task_started", "") + "\n"));
        assert!(tail.poll());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        append(&f.rollout, &(ev("task_complete", "") + "\n"));
        assert!(tail.poll());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);
        assert!(!tail.poll(), "nothing new");
    }

    #[test]
    fn tail_leaves_a_partial_line_for_the_next_poll() {
        let f = fixture();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        let line = ev("task_started", "");
        append(&f.rollout, &line[..10]);
        tail.poll();
        assert!(tail.snapshot.is_none());
        append(&f.rollout, &(line[10..].to_owned() + "\n"));
        assert!(tail.poll());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
    }

    #[test]
    fn tail_titles_from_the_session_index_newest_entry_wins() {
        let f = fixture();
        let index = session_index_path(&f.home);
        append(&f.rollout, &(paginated_prompt("first prompt") + "\n"));
        append(&index, &index_line("01a18c5a-3b93-7e73-b7a8-32a4dd2b8b80", "Other thread"));
        let start = Instant::now();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert_eq!(tail.snapshot.as_ref().unwrap().title.as_ref().unwrap().text, "first prompt");
        append(&index, &index_line(ID_A, "Resize bug"));
        assert!(!tail.poll_at(start), "the index is checked once a second");
        assert!(tail.poll_at(start + INDEX_CHECK_INTERVAL * 2));
        let title = tail.snapshot.as_ref().unwrap().title.clone().unwrap();
        assert_eq!(title, SessionTitle { text: "Resize bug".into(), source: TitleSource::Generated });
        append(&index, &index_line(ID_A, "Renamed by user"));
        assert!(tail.poll_at(start + INDEX_CHECK_INTERVAL * 4));
        assert_eq!(tail.snapshot.as_ref().unwrap().title.as_ref().unwrap().text, "Renamed by user");
    }

    #[test]
    fn tail_rereads_a_rewritten_index() {
        // Codex rewrites the index when it drops a thread's names.
        let f = fixture();
        let index = session_index_path(&f.home);
        append(&index, &index_line("01a18c5a-3b93-7e73-b7a8-32a4dd2b8b80", "Other thread name"));
        append(&f.rollout, &(ev("task_started", "") + "\n"));
        let start = Instant::now();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        std::fs::write(&index, index_line(ID_A, "Named")).unwrap();
        assert!(tail.poll_at(start + INDEX_CHECK_INTERVAL * 2));
        assert_eq!(tail.snapshot.as_ref().unwrap().title.as_ref().unwrap().text, "Named");
    }

    #[test]
    fn tail_waits_for_a_rollout_that_appears_later() {
        let f = fixture();
        std::fs::remove_file(&f.rollout).unwrap();
        let start = Instant::now();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert!(tail.path.is_none());
        std::fs::write(&f.rollout, meta_line(ID_A, "C:/repo/alpha", "cli") + &ev("task_started", "") + "\n")
            .unwrap();
        crate::agents::dir_listing::forget(f.rollout.parent().unwrap());
        assert!(!tail.poll_at(start + Duration::from_millis(100)), "retry is throttled");
        assert!(tail.poll_at(start + LOCATE_RETRY * 2));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
    }

    #[test]
    fn tail_follows_a_reverted_thread_to_its_new_rollout() {
        // `thread/revert` keeps the thread id and moves on to a new file.
        let f = fixture();
        append(&f.rollout, &(paginated_prompt("first prompt") + "\n" + &ev("task_started", "") + "\n"));
        let start = Instant::now();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        let old = std::fs::File::options().write(true).open(&f.rollout).unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(600)).unwrap();
        let reverted = f.rollout.with_file_name(format!(
            "rollout-2026-10-30T12-00-00-{ID_A}_01a18c62-0000-7000-8000-000000000001.jsonl"
        ));
        let body = meta_line(ID_A, "C:/repo/alpha", "cli") + &ev("task_complete", "") + "\n";
        std::fs::write(&reverted, body).unwrap();
        crate::agents::dir_listing::forget(f.rollout.parent().unwrap());
        assert!(!tail.poll_at(start + Duration::from_millis(100)), "re-locating is throttled");
        assert!(tail.poll_at(start + LOCATE_RETRY * 2));
        assert_eq!(tail.path.as_deref(), Some(reverted.as_path()));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.title.unwrap().text, "first prompt", "the title survives the switch");
    }

    #[test]
    fn quiet_busy_session_goes_idle_and_comes_back_on_activity() {
        let f = fixture();
        append(&f.rollout, &(ev("task_started", "") + "\n"));
        let start = Instant::now();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert!(!tail.poll_at(start + BUSY_QUIET_TIMEOUT / 2));
        assert!(tail.poll_at(start + BUSY_QUIET_TIMEOUT + Duration::from_secs(1)));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!((s.state, s.quiet_timeout), (SessionState::Idle, true));
        append(&f.rollout, &(item("function_call", "c9") + "\n"));
        assert!(tail.poll_at(start + BUSY_QUIET_TIMEOUT + Duration::from_secs(2)));
        let s = tail.snapshot.clone().unwrap();
        assert_eq!((s.state, s.quiet_timeout), (SessionState::PendingToolUse, false));
    }

    #[test]
    fn pending_tool_use_never_times_out() {
        // An approval prompt writes nothing until the user answers.
        let f = fixture();
        append(&f.rollout, &(ev("task_started", "") + "\n" + &item("function_call", "c1") + "\n"));
        let start = Instant::now();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert!(!tail.poll_at(start + BUSY_QUIET_TIMEOUT * 3));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn poll_interval_tracks_state() {
        let f = fixture();
        let mut tail = CodexRolloutTail::for_session(&f.home, ID_A);
        assert_eq!(tail.poll_interval(), Duration::from_secs(2));
        append(&f.rollout, &(ev("task_started", "") + "\n"));
        tail.poll();
        assert_eq!(tail.poll_interval(), Duration::from_millis(250));
    }
}
