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

use std::collections::HashSet;
use std::io::{BufRead as _, BufReader, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

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
/// **Every character outside `[a-zA-Z0-9]` becomes `-`** — not just the
/// separators. Claude Code's own encoder, lifted from its bundle:
///
/// ```js
/// function Uso(e){ return e.replace(/[^a-zA-Z0-9]/g,"-") }
/// function vv(e){ let t=Uso(e); if(t.length<=BZ) return t;
///                 return `${t.slice(0,BZ)}-${VNg(e)}` }   // BZ = 200
/// function Vz(e){ return join(join(claudeDir(),"projects"), vv(e)) }
/// ```
///
/// This used to list only `:`, `\`, `/`, `.` — an under-generalisation
/// from paths that happened to contain nothing else. **Spaces were the
/// one that bit:** a session in
/// `D:\Dev\...\Web Object Projects\Profit Connector` had us looking in
/// `D--Dev-...-Web Object Projects-Profit Connector` while Claude wrote
/// to `…-Web-Object-Projects-Profit-Connector`. The directory never
/// resolved, so transcript discovery found nothing, no
/// `agent_session_id` was ever persisted, and reopening the session
/// gave a blank terminal even though `claude -r` could still find the
/// conversation. Telemetry (model / tokens / turns / busy) was dark for
/// the same reason.
///
/// Verified against the user's real `~/.claude/projects/` by aligning
/// each transcript's `cwd` field with its containing directory name,
/// character for character: space, `.`, `:` and `\` all map to `-`,
/// alphanumerics and `-` map to themselves.
///
/// Note the regex carries **no `u` flag**, so it matches per UTF-16
/// code unit — a non-BMP character becomes *two* dashes. See the body.
///
/// **Known gap:** the `BZ = 200` truncate-and-append-hash branch is not
/// implemented — `VNg` is a base36 string hash we'd have to reproduce
/// bit-exactly to be useful, and guessing it wrong is worse than not
/// having it. Encoded names over 200 chars therefore still miss. No
/// path in the user's store comes close (longest is 68), so this is
/// latent, not live; a directory scan comparing each candidate's
/// `cwd` would be the robust fix if it ever bites.
pub fn encode_cwd(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            // One dash per UTF-16 code unit, not per code point. JS
            // strings are UTF-16 and the regex carries no `u` flag, so
            // the negated class matches each half of a surrogate pair
            // separately: `"🚀".replace(/[^a-zA-Z0-9]/g,"-")` is `"--"`.
            // A `chars()`-shaped one-dash-per-char version silently
            // under-counts for any non-BMP character and lands us back
            // on a directory Claude never wrote.
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
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
    /// True when this is an assistant `stop_reason == "tool_use"`
    /// entry whose tool calls are *all* user-interaction prompts
    /// (`AskUserQuestion` / `ExitPlanMode`). The agent is blocked on
    /// the human, not working — the busy dot must not show red
    /// (issue #293). A mixed batch (a real tool racing a question)
    /// still counts as working.
    awaiting_user_input: bool,
    /// True when this entry is the CLI's own answer to a client-side
    /// slash command (`/model`, `/clear`, …): its content *is* a
    /// `<local-command-stdout>` envelope, written either as a `user`
    /// entry or as a `system` entry with `subtype: "local_command"`.
    /// No model turn follows, so nothing else would unlatch the `Busy`
    /// its invocation entry set (issue #343).
    local_command_answer: bool,
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

    // Client-side slash command answers. See the field doc on
    // `Entry::local_command_answer`. A `user` entry carries the
    // envelope in `message.content`; a `system` entry carries it at the
    // top level, and only the `local_command` subtype counts — the
    // other system subtypes (`turn_duration`, `compact_boundary`, …)
    // stay ignored.
    let local_command_content = match (
        obj.get("type").and_then(Value::as_str),
        obj.get("subtype").and_then(Value::as_str),
    ) {
        (Some("user"), _) => obj.get("message").and_then(|m| m.get("content")),
        (Some("system"), Some("local_command")) => obj.get("content"),
        _ => None,
    };
    let local_command_answer = local_command_content
        .and_then(Value::as_str)
        .is_some_and(is_local_command_stdout);

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
    let mut awaiting_user_input = false;

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

        // Detect assistant entries blocked on the human: every
        // tool_use block is a user-interaction prompt. See the
        // field doc on `Entry::awaiting_user_input`. Single pass, no
        // allocation; a tool_use block with a missing or non-string
        // `name` counts as a real tool, so a malformed batch never
        // classifies as idle (Copilot review on PR #299).
        if kind == EntryKind::Assistant
            && stop_reason.as_deref() == Some("tool_use")
            && let Some(content) = msg.get("content").and_then(Value::as_array) {
                let mut any_tool_use = false;
                let mut all_user_prompts = true;
                for item in content {
                    let Some(o) = item.as_object() else { continue };
                    if o.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    any_tool_use = true;
                    if !matches!(
                        o.get("name").and_then(Value::as_str),
                        Some("AskUserQuestion" | "ExitPlanMode")
                    ) {
                        all_user_prompts = false;
                        break;
                    }
                }
                awaiting_user_input = any_tool_use && all_user_prompts;
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
        awaiting_user_input,
        local_command_answer,
    })
}

/// True when `content` *is* a `<local-command-stdout>` envelope, not
/// merely contains one: an ordinary prompt may quote the tags, and
/// reading that quotation as an answer would paint real work as idle.
fn is_local_command_stdout(content: &str) -> bool {
    content.starts_with("<local-command-stdout>") && content.ends_with("</local-command-stdout>")
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
// Background subagents
// ---------------------------------------------------------------------------

/// Text Claude Code writes into the `tool_result` of an `Agent` call
/// that was launched in the background. The agent id follows on the
/// next line as `agentId: <id>`.
const AGENT_LAUNCH_MARKER: &str = "Async agent launched successfully";

/// Wrapper element of the message Claude Code injects when a
/// background task stops. Carries `<task-id>` plus a `<status>` that
/// is always terminal (`completed` / `failed` / `killed` / `stopped`),
/// so any notification for an id retires it.
const TASK_NOTIFICATION_MARKER: &str = "<task-notification>";

/// Update `pending` from one **user** transcript line.
///
/// Both markers only ever reach the transcript inside user entries
/// (the launch as a `tool_result`, the completion as an injected
/// `<task-notification>` message), so the caller gates on
/// [`EntryKind::User`] and we can scan the raw JSON text instead of
/// walking a `content` shape that differs between the two.
///
/// One line can carry several markers — two `Agent` calls in a single
/// assistant message produce two `tool_result` blocks in one user
/// entry, and queued notifications batch the same way.
fn apply_agent_markers(line: &str, pending: &mut HashSet<String>) {
    for (idx, _) in line.match_indices(AGENT_LAUNCH_MARKER) {
        if let Some(id) = tagged_value(&line[idx..], "agentId: ", |c| c.is_ascii_alphanumeric()) {
            pending.insert(id);
        }
    }
    if line.contains(TASK_NOTIFICATION_MARKER) {
        for (idx, _) in line.match_indices("<task-id>") {
            if let Some(id) = tagged_value(&line[idx..], "<task-id>", |c| c != '<') {
                pending.remove(&id);
            }
        }
    }
}

/// First `prefix`-introduced run of `accept` characters in `hay`, or
/// `None` when the prefix is absent or immediately followed by a
/// rejected character.
fn tagged_value(hay: &str, prefix: &str, accept: impl Fn(char) -> bool) -> Option<String> {
    let start = hay.find(prefix)? + prefix.len();
    let value: String = hay[start..].chars().take_while(|c| accept(*c)).collect();
    (!value.is_empty()).then_some(value)
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
/// `pending_agents` accumulates the ids of background subagents that
/// were launched but have not reported back yet; while it is
/// non-empty an otherwise-`Idle` session stays [`SessionState::Busy`]
/// (issue #284 — the main agent finishes its turn while its
/// background agents keep working, and the green dot lied).
///
/// Mirrors `ClaudeTelemetryService.TryRead` from the C# build.
pub fn process_new_lines(
    path: &Path,
    tail: &mut FileTail,
    snapshot: &mut Option<TelemetrySnapshot>,
    last_user_ts: &mut Option<f64>,
    pending_agents: &mut HashSet<String>,
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

    // A read that starts at byte 0 is replaying history: either the
    // tail was just created (the agent process behind this transcript
    // cannot be older than the tail — a resumed session gets a fresh
    // CLI) or the file was rewritten. Background agents launched in
    // that history are therefore all dead, so the pending set is
    // dropped once the replay finishes rather than pinning the
    // session to `Busy` forever.
    //
    // ponytail: the cost is a launch that lands in the very first
    // read (adoption racing the first `Agent` call) being missed —
    // that degrades to the pre-#284 behaviour for one agent, where
    // keeping it would mean a permanently wrong red dot after every
    // resume. Needs the agent to report liveness to do better.
    let catch_up = tail.last_pos == 0;

    if let Err(err) = f.seek(SeekFrom::Start(tail.last_pos)) {
        eprintln!("[claude_telemetry] seek failed for {path:?}: {err}");
        return false;
    }

    let mut reader = BufReader::new(&mut f);

    // Pull the existing snapshot fields to mutate.
    let mut tokens_used = snapshot.as_ref().map_or(0, |s| s.tokens_used);
    let mut turn_count = snapshot.as_ref().map_or(0, |s| s.turn_count);
    let mut last_turn_duration = snapshot.as_ref().and_then(|s| s.last_turn_duration);
    // An idle the quiet-window fallback produced (#351) is the tail's
    // guess, not something the transcript said: the parser's own state
    // was still `Busy`. Seed from that, so output that resumes without a
    // terminal `stop_reason` reads as work again instead of staying idle,
    // while an `end_turn` still resolves to `Idle` the normal way.
    let mut state = snapshot.as_ref().map_or(SessionState::Unknown, |s| {
        if s.quiet_timeout { SessionState::Busy } else { s.state }
    });
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
            // The CLI answered a client-side slash command by itself
            // (issue #343): no model turn follows, so this is the only
            // entry that will ever release the `Busy` its invocation
            // set. Not a fresh prompt either, so `last_user_ts` stays.
            _ if entry.local_command_answer => {
                state = SessionState::Idle;
                changed = true;
            }
            EntryKind::User => {
                state = SessionState::Busy;
                // Anchor for the last-turn duration; tool-result
                // entries don't reset the anchor (they're internal
                // to a turn the user already kicked off).
                if !entry.user_carries_tool_result {
                    *last_user_ts = entry.timestamp_secs;
                }
                apply_agent_markers(&line, pending_agents);
                changed = true;
            }
            EntryKind::Assistant => {
                state = match entry.stop_reason.as_deref() {
                    // A "tool call" that is really a question to the
                    // human (AskUserQuestion / plan approval) blocks
                    // until they answer — that is idle time, not work
                    // (issue #293). The answer arrives as a
                    // tool-result user entry, which flips the state
                    // back to `Busy` on its own.
                    Some("tool_use") if entry.awaiting_user_input => SessionState::Idle,
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

    if catch_up {
        pending_agents.clear();
    }

    // The main agent ending its turn does not mean the session is
    // done — background subagents outlive it, and their completion
    // arrives later as a `<task-notification>` user entry (which
    // flips the state back to `Busy` on its own).
    if state == SessionState::Idle && !pending_agents.is_empty() {
        state = SessionState::Busy;
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
            quiet_timeout: false,
        });
    }

    changed
}

// ---------------------------------------------------------------------------
// High-level tail handle
// ---------------------------------------------------------------------------

/// How long a [`SessionState::Busy`] transcript may stay unchanged — no
/// new bytes, same mtime — before the tail drops it to
/// [`SessionState::Idle`] (issue #351). A fallback for a turn that ends
/// without an assistant entry: the CLI was killed, the machine slept, or
/// a CLI writes a shape the parser does not recognise.
///
/// Deliberately generous. A dead session shown busy for ten minutes
/// costs a glance; a working session shown idle invites the user to type
/// into it or close it. And a working session can be silent for a long
/// time: an assistant entry is written per finished content block, so a
/// long thinking or generation step writes nothing until it ends.
///
/// Measured on [`Instant`], which is monotonic. On Linux and macOS it
/// does not advance while the machine is suspended, so a session that
/// was busy when the machine went to sleep goes idle ten minutes after
/// wake rather than on wake. That is the safe direction to be late in,
/// and a wall-clock timestamp would bring clock changes and rollback
/// into a check that currently cannot be fooled by them.
pub const BUSY_QUIET_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
    /// Ids of background subagents launched from this session that
    /// have not reported back yet. See [`process_new_lines`].
    pending_agents: HashSet<String>,
    /// Latest computed snapshot, or `None` if no entries have been
    /// parsed yet.
    pub snapshot: Option<TelemetrySnapshot>,
    /// Tail clock at the last poll that saw the transcript change. Starts
    /// at construction. See [`BUSY_QUIET_TIMEOUT`].
    last_activity: Instant,
    /// File mtime seen by the last poll, whether or not bytes were read.
    /// `FileTail::last_mtime` only moves on a clean read, so a same-length
    /// rewrite would not show up there.
    observed_mtime: Option<SystemTime>,
}

impl ClaudeTranscriptTail {
    /// Construct a tail for `path` and immediately do an initial read
    /// so existing transcript content is consumed before the first
    /// poll interval fires.
    pub fn new(path: PathBuf) -> Self {
        let now = Instant::now();
        let mut tail = Self {
            path,
            tail: FileTail::default(),
            last_user_ts: None,
            pending_agents: HashSet::new(),
            snapshot: None,
            last_activity: now,
            observed_mtime: None,
        };
        tail.poll_at(now);
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
        self.poll_at(Instant::now())
    }

    /// [`Self::poll`] with the tail clock passed in, so the quiet-window
    /// fallback can be tested without waiting for it.
    ///
    /// Any sign the file is being written — bytes read (entries, a
    /// partial line, lines that change nothing) or a moved mtime —
    /// restarts the window. Once it lapses a `Busy` snapshot becomes
    /// `Idle`; `PendingToolUse` is exempt (a running tool or a permission
    /// prompt writes nothing until it resolves), and so is a session with
    /// background agents pending (they write to their own transcripts).
    pub fn poll_at(&mut self, now: Instant) -> bool {
        let pos_before = self.tail.last_pos;
        let mtime = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| crate::telemetry::modified_or_none(&m));
        let changed = process_new_lines(
            &self.path,
            &mut self.tail,
            &mut self.snapshot,
            &mut self.last_user_ts,
            &mut self.pending_agents,
        );
        if self.tail.last_pos != pos_before || mtime != self.observed_mtime {
            self.last_activity = now;
            self.observed_mtime = mtime;
        }

        // Only the state moves (and says why): tokens, turns, model and
        // the turn anchor still describe what the transcript said.
        if let Some(snapshot) = self.snapshot.as_mut()
            && snapshot.state == SessionState::Busy
            && self.pending_agents.is_empty()
            && now.saturating_duration_since(self.last_activity) >= BUSY_QUIET_TIMEOUT
        {
            snapshot.state = SessionState::Idle;
            // Not a finished turn: the app reads this to stay quiet
            // instead of announcing "Turn complete".
            snapshot.quiet_timeout = true;
            return true;
        }
        changed
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

    /// [`process_new_lines`] without the background-subagent set —
    /// the tests that predate issue #284 don't exercise it and a
    /// fresh set per call keeps their signatures unchanged.
    fn read_lines(
        path: &Path,
        tail: &mut FileTail,
        snapshot: &mut Option<TelemetrySnapshot>,
        last_user_ts: &mut Option<f64>,
    ) -> bool {
        process_new_lines(path, tail, snapshot, last_user_ts, &mut HashSet::new())
    }

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

    #[test]
    fn encode_cwd_replaces_spaces() {
        // The regression this rule was widened for. Path and expected
        // directory name are both copied from the user's real machine:
        // the session was live, its transcripts existed, and CodeScope
        // could not see them.
        assert_eq!(
            encode_cwd(r"D:\Dev\profit\src\Anta\Projects\Web Object Projects\Profit Connector"),
            "D--Dev-profit-src-Anta-Projects-Web-Object-Projects-Profit-Connector"
        );
    }

    #[test]
    fn encode_cwd_replaces_every_non_alphanumeric() {
        // Claude's encoder is `[^a-zA-Z0-9] -> '-'`, so underscores,
        // parens and `#` go too — a hyphen is the only punctuation that
        // survives, and only because replacing it yields itself.
        assert_eq!(
            encode_cwd(r"C:\dev\my_repo (copy)\v#2"),
            "C--dev-my-repo--copy--v-2"
        );
        // Non-ASCII is outside the class as well: JS `[a-zA-Z0-9]` does
        // not match `é`, so neither does `is_ascii_alphanumeric`.
        assert_eq!(encode_cwd("/tmp/café"), "-tmp-caf-");
        // Hyphens and digits are preserved verbatim.
        assert_eq!(encode_cwd(r"D:\Dev\online.worktrees\worktree-2"), "D--Dev-online-worktrees-worktree-2");
    }

    #[test]
    fn encode_cwd_counts_utf16_code_units_not_code_points() {
        // Claude's regex has no `u` flag, so it matches per UTF-16 code
        // unit and a surrogate pair yields *two* dashes. Every vector
        // here was produced by running the real encoder under node:
        //
        //   s.replace(/[^a-zA-Z0-9]/g, "-")
        //
        //   "🚀"        -> "--"
        //   "a🚀b"      -> "a--b"
        //   "café"      -> "caf-"
        //   "𝔘nicode"   -> "--nicode"
        assert_eq!(encode_cwd("🚀"), "--");
        assert_eq!(encode_cwd("a🚀b"), "a--b");
        assert_eq!(encode_cwd("𝔘nicode"), "--nicode");
        // BMP non-ASCII is a single code unit, so a single dash — the
        // distinction only bites above U+FFFF.
        assert_eq!(encode_cwd("café"), "caf-");
        assert_eq!(encode_cwd(r"D:\x\🚀 dir"), "D--x----dir");
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
        let changed = read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

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
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn ask_user_question_is_idle_not_busy() {
        // Issue #293: the agent asking the human a question blocks
        // until they answer — the red dot lied for the whole wait.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"do it"}}"#,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"AskUserQuestion","input":{}}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::Idle);
    }

    #[test]
    fn plan_approval_is_idle_not_busy() {
        // ExitPlanMode blocks on the user the same way.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"ExitPlanMode","input":{}}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::Idle);
    }

    #[test]
    fn question_racing_a_real_tool_stays_pending() {
        // A batch mixing AskUserQuestion with a real tool call means
        // work is still running — that is not idle.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}},{"type":"tool_use","id":"t2","name":"AskUserQuestion","input":{}}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn unnamed_tool_use_block_never_classifies_as_idle() {
        // A tool_use block missing its name must count as a real
        // tool — a malformed batch must not read as "waiting on the
        // user" (Copilot review on PR #299).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1"},{"type":"tool_use","id":"t2","name":"AskUserQuestion","input":{}}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn answering_the_question_goes_back_to_busy() {
        // The answer arrives as a tool-result user entry.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"AskUserQuestion","input":{}}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:05:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"picked option A"}]}}"#,
            ],
        );

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        assert_eq!(snap.unwrap().state, SessionState::Busy);
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
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

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
        read_lines(path.as_path(), &mut tail, &mut snap, &mut last_user_ts);

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
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

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
        read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);

        let snap1_tokens = snap.as_ref().unwrap().tokens_used;
        let pos_after_first = tail.last_pos;
        assert!(pos_after_first > 0);

        // Second poll — nothing new, should not change.
        let changed = read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);
        assert!(!changed);
        assert_eq!(snap.as_ref().unwrap().tokens_used, snap1_tokens);

        // Append a new assistant turn.
        append_lines(
            &path,
            &[r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"user","content":"more"}}"#,
              r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );

        let changed = read_lines(&path.to_path_buf(), &mut tail, &mut snap, &mut last_user_ts);
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
        let changed = read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
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

    // --- background subagents (issue #284) ---

    const PROMPT: &str = r#"{"type":"user","sessionId":"s","timestamp":"2026-08-09T08:00:00Z","message":{"role":"user","content":"go"}}"#;
    const LAUNCH: &str = r#"{"type":"user","sessionId":"s","timestamp":"2026-08-09T08:00:10Z","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":[{"type":"text","text":"Async agent launched successfully. (internal metadata)\nagentId: a02e36f821d048123 (internal ID - do not mention to user.)"}]}]}}"#;
    const NOTIFICATION: &str = r#"{"type":"user","sessionId":"s","timestamp":"2026-08-09T08:05:00Z","message":{"role":"user","content":"<task-notification>\n<task-id>a02e36f821d048123</task-id>\n<status>completed</status>\n</task-notification>"}}"#;
    const END_TURN: &str = r#"{"type":"assistant","sessionId":"s","timestamp":"2026-08-09T08:00:20Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;

    #[test]
    fn background_subagent_keeps_session_busy_after_end_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let mut pending = HashSet::new();
        // Catch-up read first, so the launch below is seen live.
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts, &mut pending);

        // Main agent spawns a background subagent and then wraps up
        // its own turn — the state must not go green yet.
        append_lines(&path, &[LAUNCH, END_TURN]);
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts, &mut pending);
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Busy);

        // Subagent reports back; the main agent's next end_turn is
        // the real idle.
        append_lines(&path, &[NOTIFICATION, END_TURN]);
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts, &mut pending);
        assert!(pending.is_empty());
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn subagent_launched_before_the_tail_existed_is_not_pending() {
        // Replaying history (fresh tail / resumed session): the agent
        // process that launched this subagent is gone, so it must not
        // pin the session to Busy.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT, LAUNCH, END_TURN]);

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        let mut pending = HashSet::new();
        process_new_lines(&path, &mut tail, &mut snap, &mut last_user_ts, &mut pending);

        assert!(pending.is_empty());
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn agent_markers_handle_several_ids_per_line() {
        let mut pending = HashSet::new();
        // Two `Agent` calls in one assistant message answer with two
        // tool_result blocks inside a single user entry.
        apply_agent_markers(
            r#"Async agent launched successfully.\nagentId: aaa111 (internal ID) … Async agent launched successfully.\nagentId: bbb222 (internal ID)"#,
            &mut pending,
        );
        assert_eq!(pending.len(), 2);

        apply_agent_markers(
            r#"<task-notification>\n<task-id>aaa111</task-id>\n<status>killed</status>"#,
            &mut pending,
        );
        assert_eq!(pending.len(), 1);
        assert!(pending.contains("bbb222"));
    }

    #[test]
    fn task_id_outside_a_notification_is_ignored() {
        // Guard the gate: a bare `<task-id>` (e.g. quoted in a prompt)
        // must not retire a live agent.
        let mut pending = HashSet::new();
        pending.insert("aaa111".to_string());
        apply_agent_markers("<task-id>aaa111</task-id>", &mut pending);
        assert!(pending.contains("aaa111"));
    }

    // --- client-side slash commands (issue #343) ---
    //
    // Captured from `~/.claude/projects/C--dev-codescope-public/*.jsonl`.
    // Long content is elided with `…`; nothing else is edited.

    /// `/model` invocation. A prompt-expanding command writes the same
    /// shape, so this alone must not decide anything.
    const MODEL_INVOCATION: &str = r#"{"parentUuid":"6fbd4b13-…","isSidechain":false,"promptId":"daf3a846-…","type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"},"uuid":"d1eeb7b9-…","timestamp":"2026-08-15T19:22:54.460Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"8aa860f5-…","version":"2.1.233","gitBranch":"main"}"#;
    /// `/model` answer: a `user` entry whose content is the envelope.
    const MODEL_STDOUT: &str = r#"{"parentUuid":"d1eeb7b9-…","isSidechain":false,"promptId":"daf3a846-…","type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to \u001b[1mOpus 5 (1M context)\u001b[22m and saved as your default for new sessions</local-command-stdout>"},"uuid":"70319ba8-…","timestamp":"2026-08-15T19:22:54.460Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"8aa860f5-…","version":"2.1.233","gitBranch":"main"}"#;
    /// `/clear` answer: nothing printed, so a `system` entry instead.
    const CLEAR_STDOUT: &str = r#"{"parentUuid":"3ff4f733-…","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout></local-command-stdout>","level":"info","timestamp":"2026-09-11T06:51:27.219Z","uuid":"d5e8e2ca-…","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268"}"#;
    /// `/effort high` — the command in the issue's own repro. It was
    /// missing from the transcripts this rule was derived from, so it
    /// was covered by argument rather than by evidence until somebody
    /// ran it. Same shape as `/model`, which is the point: the rule is
    /// keyed on the answer, not on a list of command names.
    const EFFORT_INVOCATION: &str = r#"{"parentUuid":"7c11eae0-…","isSidechain":false,"promptId":"13b90a4f-…","type":"user","message":{"role":"user","content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>high</command-args>"},"uuid":"b72e1cda-…","timestamp":"2026-09-12T18:41:24.562Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268","gitBranch":"fix/telemetry-slash-command-busy"}"#;
    /// `/effort high` answer, captured 2026-09-12.
    const EFFORT_STDOUT: &str = r#"{"parentUuid":"b72e1cda-…","isSidechain":false,"promptId":"13b90a4f-…","type":"user","message":{"role":"user","content":"<local-command-stdout>Set effort level to high (saved as your default for new sessions): Comprehensive implementation with extensive testing and documentation</local-command-stdout>"},"uuid":"0882b682-…","timestamp":"2026-09-12T18:41:24.562Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268","gitBranch":"fix/telemetry-slash-command-busy"}"#;

    /// `/m` invocation — a project command that expands into a prompt.
    const M_INVOCATION: &str = r#"{"parentUuid":"98e6783e-…","isSidechain":false,"promptId":"09ac6ae5-…","type":"user","message":{"role":"user","content":"<command-message>m</command-message>\n<command-name>/m</command-name>"},"uuid":"88b9f93d-…","timestamp":"2026-09-11T06:51:38.667Z","origin":{"kind":"human"},"userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268","gitBranch":"main"}"#;
    /// `/m` expansion (`isMeta: true`). A model turn follows this one.
    const M_EXPANSION: &str = r##"{"parentUuid":"88b9f93d-…","isSidechain":false,"promptId":"09ac6ae5-…","type":"user","message":{"role":"user","content":[{"type":"text","text":"# Switch to main and pull\n\nRun these commands in the current repository:\n\n1. `git checkout main`…"}]},"isMeta":true,"uuid":"a7f0ed7d-…","timestamp":"2026-09-11T06:51:38.667Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268","gitBranch":"main"}"##;

    #[test]
    fn parse_local_command_answer_detects_both_captured_shapes() {
        let user = parse_line(MODEL_STDOUT).expect("should parse");
        assert_eq!(user.kind, EntryKind::User);
        assert!(user.local_command_answer);

        let system = parse_line(CLEAR_STDOUT).expect("should parse");
        assert_eq!(system.kind, EntryKind::Other);
        assert!(system.local_command_answer);

        // The repro's own command, captured after the rule was written.
        let effort = parse_line(EFFORT_STDOUT).expect("should parse");
        assert_eq!(effort.kind, EntryKind::User);
        assert!(effort.local_command_answer);

        for line in [MODEL_INVOCATION, EFFORT_INVOCATION, M_INVOCATION, M_EXPANSION] {
            assert!(!parse_line(line).expect("should parse").local_command_answer);
        }
    }

    #[test]
    fn parse_local_command_answer_needs_the_whole_envelope() {
        // A prompt that merely quotes the tags is real work. Derived from
        // the captured answer by moving the envelope off either edge.
        let quoted_after = MODEL_STDOUT.replace(
            r#""content":"<local-command-stdout>"#,
            r#""content":"why did <local-command-stdout>"#,
        );
        let quoted_before = MODEL_STDOUT.replace(
            r#"</local-command-stdout>""#,
            r#"</local-command-stdout> mean that?""#,
        );
        for line in [quoted_after, quoted_before] {
            let entry = parse_line(&line).expect("should parse");
            assert!(!entry.local_command_answer, "{line}");
        }
    }

    #[test]
    fn parse_local_command_answer_needs_the_local_command_subtype() {
        // Other `system` subtypes stay ignored, even carrying the same
        // content as the captured `/clear` answer.
        for subtype in
            ["turn_duration", "stop_hook_summary", "compact_boundary", "model_refusal_fallback"]
        {
            let line = CLEAR_STDOUT.replace(
                r#""subtype":"local_command""#,
                &format!(r#""subtype":"{subtype}""#),
            );
            let entry = parse_line(&line).expect("should parse");
            assert!(!entry.local_command_answer, "{subtype}");
        }
    }

    #[test]
    fn client_side_command_with_output_is_idle_not_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[MODEL_INVOCATION]);

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Busy);

        append_lines(&path, &[MODEL_STDOUT]);
        read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn client_side_command_without_output_is_idle_not_busy() {
        // The `/clear` invocation was not captured; the `/model` one is
        // the same shape and sets the same latch.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[MODEL_INVOCATION]);

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Busy);

        // Arrives alone in its own poll, so it must mark the snapshot
        // changed by itself.
        append_lines(&path, &[CLEAR_STDOUT]);
        assert!(read_lines(&path, &mut tail, &mut snap, &mut last_user_ts));
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn the_repro_command_goes_idle() {
        // `/effort high`, invocation then answer, exactly as issue #343
        // describes typing it.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[EFFORT_INVOCATION]);

        let mut tail = FileTail::default();
        let mut snap: Option<TelemetrySnapshot> = None;
        let mut last_user_ts = None;
        read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Busy);

        append_lines(&path, &[EFFORT_STDOUT]);
        read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
        assert_eq!(snap.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn prompt_expanding_command_stays_busy() {
        // Negative control: the invocation and its expansion with no
        // stdout answer after them are work waiting for a model turn.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[M_INVOCATION, M_EXPANSION]);

        let tail = ClaudeTranscriptTail::new(path);
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        assert_eq!(tail.poll_interval(), Duration::from_millis(250));
    }

    #[test]
    fn client_side_command_polls_at_idle_rate() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, answer) in [("model.jsonl", MODEL_STDOUT), ("clear.jsonl", CLEAR_STDOUT)] {
            let path = tmp.path().join(name);
            write_lines(&path, &[MODEL_INVOCATION, answer]);

            let tail = ClaudeTranscriptTail::new(path);
            assert_eq!(tail.poll_interval(), Duration::from_secs(2), "{name}");
        }
    }

    #[test]
    fn local_command_answer_does_not_reset_last_user_ts() {
        // Not a fresh prompt — same exemption as a tool-result entry.
        let tmp = tempfile::tempdir().unwrap();
        for (name, answer) in [("model.jsonl", MODEL_STDOUT), ("clear.jsonl", CLEAR_STDOUT)] {
            let path = tmp.path().join(name);
            write_lines(&path, &[answer]);

            let mut tail = FileTail::default();
            let mut snap: Option<TelemetrySnapshot> = None;
            let mut last_user_ts = Some(1.0);
            read_lines(&path, &mut tail, &mut snap, &mut last_user_ts);
            assert_eq!(last_user_ts, Some(1.0), "{name}");
        }
    }

    // --- quiet-window fallback (issue #351) ---
    //
    // The tests drive the clock through `poll_at`; nothing waits for it.
    // Each takes its start instant *after* `new()`, so the window that
    // construction opened is never longer than the one the test assumes.

    /// A timeout plus one second: the first instant that is unambiguously
    /// past the window.
    fn past_timeout() -> Duration {
        BUSY_QUIET_TIMEOUT + Duration::from_secs(1)
    }

    #[test]
    fn silent_busy_session_goes_idle_after_quiet_window() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = ClaudeTranscriptTail::new(path);
        let start = Instant::now();
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);

        assert!(!tail.poll_at(start + BUSY_QUIET_TIMEOUT / 2));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        assert_eq!(tail.poll_interval(), Duration::from_millis(250));

        assert!(tail.poll_at(start + past_timeout()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);
        assert!(tail.snapshot.as_ref().unwrap().quiet_timeout);
        assert_eq!(tail.poll_interval(), Duration::from_secs(2));

        // Already idle: a later poll has nothing to report.
        assert!(!tail.poll_at(start + past_timeout() * 2));
    }

    #[test]
    fn appended_bytes_restart_the_quiet_window() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = ClaudeTranscriptTail::new(path.clone());
        let start = Instant::now();

        // An entry that does not change the state still counts.
        let half = start + BUSY_QUIET_TIMEOUT / 2;
        append_lines(&path, &[r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{}}"#]);
        tail.poll_at(half);

        tail.poll_at(start + past_timeout());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);

        assert!(tail.poll_at(half + past_timeout()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn changed_mtime_alone_restarts_the_quiet_window() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);
        let original = std::fs::metadata(&path).unwrap().modified().unwrap();

        let mut tail = ClaudeTranscriptTail::new(path.clone());
        let start = Instant::now();

        // Same length, so no bytes are read; only the mtime moves.
        let rewritten = PROMPT.replace(r#""content":"go""#, r#""content":"GO""#);
        assert_eq!(rewritten.len(), PROMPT.len());
        write_lines(&path, &[&rewritten]);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(original + Duration::from_secs(3600))
            .unwrap();
        let half = start + BUSY_QUIET_TIMEOUT / 2;
        tail.poll_at(half);

        tail.poll_at(start + past_timeout());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);

        assert!(tail.poll_at(half + past_timeout()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn pending_tool_use_is_never_timed_out() {
        // A long tool call writes nothing until it finishes.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                PROMPT,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-08-09T08:00:05Z","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            ],
        );

        let mut tail = ClaudeTranscriptTail::new(path);
        let start = Instant::now();
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::PendingToolUse);

        assert!(!tail.poll_at(start + past_timeout()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::PendingToolUse);
    }

    #[test]
    fn pending_background_agents_are_never_timed_out() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = ClaudeTranscriptTail::new(path.clone());
        let start = Instant::now();
        // Launched live, after the catch-up read.
        append_lines(&path, &[LAUNCH, END_TURN]);
        tail.poll_at(start);
        assert!(!tail.pending_agents.is_empty());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);

        assert!(!tail.poll_at(start + past_timeout()));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
    }

    #[test]
    fn new_prompt_after_timeout_is_busy_again() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = ClaudeTranscriptTail::new(path.clone());
        let start = Instant::now();
        tail.poll_at(start + past_timeout());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);

        let resumed = start + past_timeout() * 2;
        append_lines(&path, &[PROMPT]);
        assert!(tail.poll_at(resumed));
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        // The transcript spoke again, so the timeout no longer explains
        // the state.
        assert!(!tail.snapshot.as_ref().unwrap().quiet_timeout);

        // The window restarted at the new prompt, not at the old one.
        tail.poll_at(resumed + BUSY_QUIET_TIMEOUT / 2);
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Busy);
        tail.poll_at(resumed + past_timeout());
        assert_eq!(tail.snapshot.as_ref().unwrap().state, SessionState::Idle);
    }

    #[test]
    fn resumed_output_after_timeout_is_busy_again() {
        // A long turn that outlived the window and then streams on: an
        // assistant entry without a terminal stop_reason.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = ClaudeTranscriptTail::new(path.clone());
        let start = Instant::now();
        tail.poll_at(start + past_timeout());
        assert!(tail.snapshot.as_ref().unwrap().quiet_timeout);

        append_lines(
            &path,
            &[r#"{"type":"assistant","sessionId":"s","timestamp":"2026-08-09T08:20:00Z","message":{"role":"assistant","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );
        tail.poll_at(start + past_timeout() * 2);
        let snap = tail.snapshot.as_ref().unwrap();
        assert_eq!(snap.state, SessionState::Busy);
        assert!(!snap.quiet_timeout);
    }

    #[test]
    fn end_turn_after_timeout_is_a_real_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(&path, &[PROMPT]);

        let mut tail = ClaudeTranscriptTail::new(path.clone());
        let start = Instant::now();
        tail.poll_at(start + past_timeout());

        append_lines(
            &path,
            &[r#"{"type":"assistant","sessionId":"s","timestamp":"2026-08-09T08:20:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#],
        );
        tail.poll_at(start + past_timeout() * 2);
        let snap = tail.snapshot.as_ref().unwrap();
        assert_eq!(snap.state, SessionState::Idle);
        // The transcript ended the turn itself this time.
        assert!(!snap.quiet_timeout);
    }

    #[test]
    fn a_turn_the_transcript_ends_is_not_a_quiet_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"hi"}}"#,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","model":"claude-opus-4-7[1m]","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":50}}}"#,
            ],
        );

        let tail = ClaudeTranscriptTail::new(path);
        let snap = tail.snapshot.as_ref().unwrap();
        assert_eq!(snap.state, SessionState::Idle);
        assert!(!snap.quiet_timeout);
    }

    #[test]
    fn timeout_changes_only_the_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"hi"}}"#,
                r#"{"type":"assistant","sessionId":"s","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","model":"claude-opus-4-7[1m]","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":50}}}"#,
                r#"{"type":"user","sessionId":"s","timestamp":"2026-04-22T08:02:00Z","message":{"role":"user","content":"again"}}"#,
            ],
        );

        let mut tail = ClaudeTranscriptTail::new(path);
        let start = Instant::now();
        let before = tail.snapshot.clone().unwrap();
        let last_user_ts = tail.last_user_ts;
        assert_eq!(before.state, SessionState::Busy);
        assert!(last_user_ts.is_some());

        assert!(tail.poll_at(start + past_timeout()));
        let after = tail.snapshot.clone().unwrap();
        assert_eq!(
            after,
            TelemetrySnapshot { state: SessionState::Idle, quiet_timeout: true, ..before }
        );
        assert_eq!(tail.last_user_ts, last_user_ts);
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
