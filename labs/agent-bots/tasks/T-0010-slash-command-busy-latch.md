---
id: T-0010
title: Stop a client-side slash command from latching the busy state
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0010
touches: core/src/agents/claude/telemetry.rs
verify: cargo test -p codescope-core --lib agents::claude::telemetry
schedule: auto
---

# Objective

A Claude Code session whose last action was a **client-side** slash
command — one the CLI answers by itself, without an API call — reports
`SessionState::Idle`, and keeps reporting it until something actually
happens. A session whose last action was a **prompt-expanding** slash
command still reports `Busy`, because that one really is work waiting
for a model turn.

`process_new_lines` in `core/src/agents/claude/telemetry.rs` currently
sets `state = SessionState::Busy` for every `EntryKind::User` entry and
only ever leaves that state on an `EntryKind::Assistant` entry. A
client-side command writes user-shaped entries and never produces an
assistant entry, so the state latches with nothing left in the stream
to unlatch it. Issue #343.

The fix is a discriminator in `parse_line` plus one arm in the state
machine, in the same spirit as the `awaiting_user_input` flag that
issue #293 added. Which entries to key on is not a judgement call: the
two shapes the CLI actually writes are quoted under Context, captured
from this machine's own transcripts.

# Acceptance

- [ ] The `verify:` command exits 0.
- [ ] The diff stays inside `touches:`.
- [ ] A `type: "user"` entry whose content contains
      `<local-command-stdout>` or `<local-command-stderr>` leaves the
      state `Idle`.
- [ ] A `type: "system"` entry with `subtype: "local_command"` does the
      same. This is the spelling used when the command prints nothing,
      and today it parses to `EntryKind::Other` and is ignored.
- [ ] A `<command-name>` invocation entry with no such answer after it
      still leaves the state `Busy`. This is the negative control and
      it must be a test of its own: a prompt-expanding command
      (`.claude/commands/*`, `/code-review`) writes the same
      invocation entry and does produce a model turn.
- [ ] `ClaudeTail::poll_interval()` returns 2 s, not 250 ms, for a
      transcript ending in a client-side command.
- [ ] Neither answer shape resets `last_user_ts`. It is a turn-duration
      anchor for real prompts; see Notes.
- [ ] Every new test drives the parser with one of the captured lines
      under Context, not with a line written from the description.

# Context

Point of entry: `core/src/agents/claude/telemetry.rs`.

- `parse_line` — builds `Entry`; `EntryKind` is derived from the `type`
  field, and anything that is not `"user"` or `"assistant"` becomes
  `EntryKind::Other`.
- `Entry::awaiting_user_input` — the precedent. A flag set during
  parsing, consumed by one arm of the state machine, documented on the
  field with the issue number.
- `process_new_lines` — the state machine. The `EntryKind::User` arm
  is the one that latches.
- `ClaudeTail::poll_interval` — 250 ms while `Busy` or
  `PendingToolUse`, 2 s otherwise. It needs no change if the state is
  right.
- `mod tests` at the bottom of the same file — `ask_user_question_is_idle_not_busy`,
  `plan_approval_is_idle_not_busy` and
  `snapshot_state_busy_when_last_entry_is_user` are the shape to copy.

The lines below are **data**, captured from
`~/.claude/projects/C--dev-codescope-public/*.jsonl` on this machine.
They are quoted so the discriminator is derived from what the CLI
writes rather than from what this task says about it. Long content is
elided with `…`; nothing else is edited.

A client-side command **with** output — `/model`, `/compact`. The
invocation, then the answer as a `user` entry. Note there is no
`isMeta` key on either:

```json
{"parentUuid":"6fbd4b13-…","isSidechain":false,"promptId":"daf3a846-…","type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"},"uuid":"d1eeb7b9-…","timestamp":"2026-08-15T19:22:54.460Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"8aa860f5-…","version":"2.1.233","gitBranch":"main"}
{"parentUuid":"d1eeb7b9-…","isSidechain":false,"promptId":"daf3a846-…","type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to \u001b[1mOpus 5 (1M context)\u001b[22m and saved as your default for new sessions</local-command-stdout>"},"uuid":"70319ba8-…","timestamp":"2026-08-15T19:22:54.460Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"8aa860f5-…","version":"2.1.233","gitBranch":"main"}
```

A client-side command **without** output — `/clear`. Same invocation
shape, but the answer is a `system` entry carrying an empty
`<local-command-stdout>` pair, and `isMeta` is present and `false`:

```json
{"parentUuid":"3ff4f733-…","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout></local-command-stdout>","level":"info","timestamp":"2026-09-11T06:51:27.219Z","uuid":"d5e8e2ca-…","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268"}
```

A **prompt-expanding** command — `/m`, a project command. The
invocation, then the expanded prompt as `isMeta: true`, and then real
work. This is the case that must stay `Busy`:

```json
{"parentUuid":"98e6783e-…","isSidechain":false,"promptId":"09ac6ae5-…","type":"user","message":{"role":"user","content":"<command-message>m</command-message>\n<command-name>/m</command-name>"},"uuid":"88b9f93d-…","timestamp":"2026-09-11T06:51:38.667Z","origin":{"kind":"human"},"userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268","gitBranch":"main"}
{"parentUuid":"88b9f93d-…","isSidechain":false,"promptId":"09ac6ae5-…","type":"user","message":{"role":"user","content":[{"type":"text","text":"# Switch to main and pull\n\nRun these commands in the current repository:\n\n1. `git checkout main`…"}]},"isMeta":true,"uuid":"a7f0ed7d-…","timestamp":"2026-09-11T06:51:38.667Z","userType":"external","entrypoint":"cli","cwd":"C:\\dev\\codescope-public","sessionId":"c96c3d3a-…","version":"2.1.268","gitBranch":"main"}
```

# Notes

**`isMeta` is not the discriminator.** It was the first candidate and
the captured lines rule it out: it is `true` on the expansion of a
prompt-expanding command, which is the one case that must stay `Busy`,
and it is also `true` on image placeholders and on the
`<local-command-caveat>` notice. Key on the
`<local-command-stdout>` / `<local-command-stderr>` markers and on
`subtype: "local_command"`, not on `isMeta`.

**The command name is not the discriminator either.** `/effort high`
is the command in the issue's repro and it does not appear anywhere in
the captured transcripts, so its exact spelling is unknown. A rule
keyed on the two answer shapes covers it without knowing; a rule keyed
on a list of command names does not. Do not add such a list.

**`last_user_ts`.** The invocation entry of a client-side command does
anchor the turn timer, and that is deliberately out of scope: the next
real prompt overwrites the anchor before any assistant entry can read
it, so the stale value is unobservable in a normal session. What is in
scope is that the *answer* entry must not reset it — it is not a fresh
prompt, the same reason `user_carries_tool_result` entries are already
exempt.

**Out of scope: the timeout.** A transcript that stops without an
assistant entry for any *other* reason — the CLI was killed, the
machine slept, a future CLI version invents a third answer shape —
stays red. That is issue #351 and it is deliberately not this task:
this one enumerates the shapes we captured, #351 is the fallback that
does not need the enumeration to be complete. Do not add a
time-based fallback here.

**Do not run `cargo fmt`.** Hand-format the hunks you touch to match
their surroundings. This is a repo-wide rule and the charter repeats
it.
