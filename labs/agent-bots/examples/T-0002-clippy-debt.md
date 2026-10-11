---
id: T-0002
title: Clear the clippy debt in codescope-core
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0002
touches: core/src/agent.rs, core/src/agent_registry.rs, core/src/settings.rs, core/src/command_palette.rs, core/src/agents/opencode/telemetry.rs
verify: cargo clippy -p codescope-core --all-targets -- -D warnings && cargo test -p codescope-core --lib
---

# Objective

`cargo clippy -p codescope-core --all-targets -- -D warnings` exits 0,
with every one of the 542 library tests still passing and no behaviour
changed.

This is the first task with real work in it. As of `labs/agent-bots`
the crate carries 12 findings across four lints:

| Lint | Count | Where |
|---|---|---|
| `field_reassign_with_default` | 9 | `settings.rs`, `agent_registry.rs` |
| `unnecessary_sort_by` | 1 | `command_palette.rs` |
| `should_implement_trait` | 1 | `agent.rs` |
| `too_many_arguments` | 1 | `agents/opencode/telemetry.rs` |

Note that `touches:` covers every file the verifier judges. A
crate-wide gate with a narrower scope is the F-1 trap, and T-0001 fell
into it.

# Acceptance

- [ ] `cargo clippy -p codescope-core --all-targets -- -D warnings` exits 0.
- [ ] `cargo test -p codescope-core --lib` exits 0, all 542 passing.
- [ ] The diff stays inside `touches:`.
- [ ] No whitespace-only hunks — you did not reformat anything.
- [ ] Every `#[allow]` you add carries a one-line comment saying why the
      lint is wrong about that specific code.

# Context

Follow `skills/clippy-sweep.md`. Read it before you start: it says what
counts as a fix and what does not, and it tells you to run the verifier
*before* editing anything.

Two of these need judgement rather than mechanical compliance. The
skill covers how to decide; the decision is yours to make and to
justify.

`agent.rs` is worth extra care. Its `AgentId` values are written into
`projects.json` as plain strings and have to round-trip with records
written by older builds, so the parsing entry point is load-bearing
beyond this crate.

# Notes

Out of scope: any file not in `touches:`, and the ~1 finding in
`core/src/agents/` modules other than `opencode/telemetry.rs` if one
appears — report it in `.bot-blocked` rather than widening the diff.

Behaviour must not change. This is debt removal, not redesign: if a
lint can only be satisfied by changing what the code *does*, that is an
escalation, not a fix.
