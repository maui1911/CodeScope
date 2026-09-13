---
id: T-0011
title: Drop a silent Busy session to Idle after a quiet window
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0011
touches: core/src/agents/claude/telemetry.rs
verify: cargo test -p codescope-core --lib agents::claude::telemetry && ! grep -q 'thread::sleep' core/src/agents/claude/telemetry.rs
schedule: auto
---

# Objective

A Claude Code session that `ClaudeTranscriptTail` reports as
`SessionState::Busy` drops to `SessionState::Idle` once its transcript
has been quiet for a generous window: no new bytes read for
`BUSY_QUIET_TIMEOUT`. Today `Busy` is only ever left on an assistant
entry, so a transcript that stops without one keeps the session red
forever and keeps it polled at 250 ms. Issue #351.

#343 (T-0010, merged) removed the common cause by recognising the
entries the CLI writes when it answers a slash command itself. That is
an enumeration of captured shapes. This task is the fallback that does
not depend on the enumeration being complete: the CLI was killed, the
machine slept mid-turn, or a future CLI writes a shape nobody captured.

The window is measured by the tail's own clock from the last time it
read new bytes, not from any timestamp inside the transcript.

# Acceptance

- [ ] The `verify:` command exits 0. It includes a check that the file
      contains no `thread::sleep`: the tests drive time, they do not
      wait for it.
- [ ] The diff stays inside `touches:`.
- [ ] `BUSY_QUIET_TIMEOUT` is a named constant of **10 minutes**, with a
      doc comment saying why it is that large (see Notes).
- [ ] The clock is injectable. `poll()` keeps its signature and
      behaviour for callers (`src/app.rs` calls it) and delegates to a
      method that takes the current `std::time::Instant`, e.g.
      `poll_at(now)`. Tests call that method; nothing in the tests
      sleeps.
- [ ] A transcript whose last entry is a real user prompt, with no new
      bytes for `BUSY_QUIET_TIMEOUT` plus one second, is `Idle` after
      the next poll, and that poll returns `true` (the snapshot
      changed).
- [ ] The same transcript with new bytes appended at half the timeout
      stays `Busy` at the point where the unrefreshed version would
      have timed out. The window restarts on every read that advances.
- [ ] `poll_interval()` returns 2 s after the timeout fires, and 250 ms
      before it.
- [ ] **`PendingToolUse` is never timed out.** A transcript ending in an
      assistant `tool_use` with no result stays `PendingToolUse` past
      the timeout. Its own test.
- [ ] **A session with background agents pending is never timed out.**
      While `pending_agents` is non-empty the state stays `Busy` past
      the timeout. Its own test.
- [ ] After the fallback has fired, a new entry still moves the state
      the normal way: an appended user prompt makes it `Busy` again and
      restarts the window.
- [ ] Only the snapshot's `state` changes when the fallback fires.
      `model`, `tokens_used`, `turn_count` and `last_turn_duration` are
      untouched, and `last_user_ts` is not reset.

# Context

Point of entry: `core/src/agents/claude/telemetry.rs`.

- `ClaudeTranscriptTail` — the handle. Fields `tail: FileTail`,
  `last_user_ts`, `pending_agents`, `snapshot`. `new()` does an initial
  `poll()`, so a transcript that was already quiet when the tail was
  built must also start its window at construction, not at the epoch.
- `ClaudeTranscriptTail::poll` — calls `process_new_lines`. That
  function returns `false` straight away when the file length has not
  changed (`file_len == tail.last_pos`), which is the "no new bytes"
  case. Comparing `tail.last_pos` before and after the call is one way
  to tell whether bytes were read; a `true` return alone is not, since
  bytes can be read without the snapshot changing.
- `ClaudeTranscriptTail::poll_interval` — 250 ms for `Busy` and
  `PendingToolUse`, 2 s otherwise. It follows the snapshot, so it needs
  no change if the state is right.
- `process_new_lines` — the state machine. Leave the fallback out of
  it: the issue asks for it in the tail, where the clock lives, and the
  parser stays a pure function of the bytes.
- `mod tests` — `write_lines`, `append_lines`, and the tests around
  `prompt_expanding_command_stays_busy` and
  `client_side_command_polls_at_idle_rate` are the shape to copy. They
  build a transcript in a `tempfile::tempdir()`.

# Notes

**Why 10 minutes and not the issue's "single-digit minutes".** Being
wrong in one direction is cheap and in the other is expensive: a dead
session shown red for ten minutes costs a glance; a working session
shown idle invites the user to type into it or close it. The quiet
cases in a *working* session can be longer than the issue assumed: an
assistant entry is written per finished content block, so a long
thinking or generation step is silence until it ends.

**Why `PendingToolUse` is excluded, against the issue's proposal.** The
issue says a long-running tool call "keeps writing". It does not: the
`tool_result` entry is written once, when the tool finishes, so a
ten-minute `cargo test` is ten minutes of silence in exactly the state
that says a tool is running. `PendingToolUse` also covers a permission
prompt waiting for the user, which is correctly not idle however long
it waits. The fallback is for `Busy` only.

**Why pending background agents are excluded.** A background subagent
writes to its own transcript, not this one. While it runs, the main
transcript is legitimately silent and `process_new_lines` holds the
state at `Busy` on purpose (see the comment above the
`pending_agents` check). Timing that out would repaint the exact case
that comment exists for.

**The window restarts on bytes read, not on entries parsed.** A partial
line, or entries that do not change the snapshot, are still evidence
the CLI is alive.

**Do not add a sleep, a thread or a timer.** The tail is polled by the
app at `poll_interval()`; the fallback is evaluated inside that poll.
The `! grep -q 'thread::sleep'` in `verify:` enforces the test half of
this.

**Out of scope:** `src/app.rs`, the other agents' tails (Copilot,
OpenCode, Pi) even though they share the shape, and changing
`poll_interval()`'s values.

**Do not run `cargo fmt`.** Hand-format the hunks you touch to match
their surroundings. This is a repo-wide rule and the charter repeats
it.
