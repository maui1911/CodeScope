---
id: T-0001
title: Loop smoke test - core/src/telemetry.rs
owner: fixer
status: todo
base: origin/main
branch: bot/fixer/T-0001
touches: core/src/telemetry.rs
verify: cargo test -p codescope-core --lib
---

# Objective

`core/src/telemetry.rs` is unchanged or improved, and the
`codescope-core` library tests still pass.

This is the loop smoke test, not real work. If nothing needs changing
the run is a no-op — **that is the point**. It exercises worktree
creation, the agent invocation, the verifier, evidence capture, the
handoff write and the cleanup path without depending on a real defect
being present. Replace it with a task that has actual work in it once
the mechanics are proven.

# Acceptance

- [ ] `cargo test -p codescope-core --lib` exits 0.
- [ ] The diff is empty, or touches only `core/src/telemetry.rs`.
- [ ] No whitespace-only hunks.

# Context

`telemetry.rs` holds the vendor-neutral primitives — `SessionState`,
`TelemetrySnapshot` — shared by every per-agent parser under
`core/src/agents/`. Changing a public shape here ripples into all of
them, so prefer the narrowest possible fix and treat any signature
change as out of scope.

# Notes

The verifier here is deliberately **tests, not clippy**. The first
version of this task used
`cargo clippy -p codescope-core --all-targets -- -D warnings`, which
fails on ~12 pre-existing findings in files this task may not touch
(`settings.rs` and others). That made every run `blocked` on debt the
bot was forbidden from fixing. See README section 7.

The rule that came out of it: **a verifier must be a predicate the
owner can actually satisfy inside its own `touches:`.** A verifier
wider than the task is a trap, not a safety net.

Out of scope: the per-agent parsers, `context_window_for_model`, and
anything under `core/src/agents/`.
