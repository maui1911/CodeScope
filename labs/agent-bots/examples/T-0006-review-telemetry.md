---
id: T-0006
kind: review
title: Review core/src/telemetry.rs against the conventions
owner: reviewer
status: todo
base: labs/agent-bots
branch: bot/reviewer/T-0006
touches: core/src/telemetry.rs
verify: bash labs/agent-bots/run/review-shape.sh
schedule: auto
every: 1d
on_changes_requested: fixer
derived_verify: cargo test -p codescope-core --lib
---

# Objective

Read `core/src/telemetry.rs` and judge it against
`context/CONVENTIONS.md` and the project's `CLAUDE.md`.

The question: **is there anything in this file that would break, or
that a reader would have to work out twice?** Correctness first — a
wrong state transition, a swallowed error, a panic path that is not
proven unreachable. Clarity a distant second. Taste, not at all.

`approve` is the expected answer, and it is a complete review. This
file is load-bearing and has been read before.

# Acceptance

- [ ] `.bot-review.md` exists, names `verdict:`, covers the commit the
      runner checked out.
- [ ] Every finding cites a real `path:line` in `core/src/telemetry.rs`.
- [ ] "What I could not check" is filled in.
- [ ] No commit was made.

# Context

`telemetry.rs` holds the vendor-neutral primitives — `SessionState`,
`TelemetrySnapshot` — shared by every per-agent parser under
`core/src/agents/`. A public shape here ripples into all of them, so
"this signature should change" is a finding about the crate, not about
this file, and belongs under "What I could not check".

This is also the task that exercises the bot-to-bot handoff. A verdict
of `changes-requested` makes the runner write a task for `fixer`,
scoped to the paths your findings cite, based on the commit you
reviewed, verified by `cargo test -p codescope-core --lib`.

So the verdict has a consequence: it decides whether another bot is
given work. That is a reason to be accurate, not a reason to be timid —
a finding you believe belongs in the review. What it rules out is
padding, because here padding becomes somebody else's task.

# Notes

This one is a **routine**: `schedule: auto` with `every: 1d`, so
`bot-tick.sh` re-runs it once a day and hands any findings to the
fixer. It is the only task in `examples/` that the scheduler will touch
on its own — the rest are fixtures and one-offs, and a scheduler that
ran everything it could find would run those too.

Out of scope: the per-agent parsers, `context_window_for_model`, and
anything under `core/src/agents/`. Findings about them go under "What I
could not check" — the derived task can only be scoped to paths you
were allowed to look at, so a finding outside `touches:` cannot be
acted on and would only block the handoff.
