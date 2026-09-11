---
id: T-0003
title: Regression - a verifier that moves the tree it is judging
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0003
touches: core/src/telemetry.rs
verify: bash labs/agent-bots/run/stubs/meddler.sh
---

# Objective

Not real work, and not a task any agent should ever be given: this is a
harness for one runner behaviour. Run it with `--skip-agent`.

The verifier is `run/stubs/meddler.sh`, which exits 0 while committing
into the agent's worktree behind the runner's back. The run must come
back **blocked**, with the head it was handed and the head it found
both named in the blocker.

# Acceptance

- [ ] The run exits 1 (`blocked`).
- [ ] The blocker reads "verifier changed the tree it was measuring".
- [ ] The handoff does **not** report `done` on a passing verifier.

# Context

The runner reads its evidence — head, commit count, dirty count,
touched paths, numstat — before the verifier runs, because a verifier
is supposed to be a predicate. It is also arbitrary code executed on
the host (F-6), so "supposed to" carries no weight. Verification now
happens in a detached checkout of the branch tip, which removes the
ordinary path to the agent's worktree; the post-run comparison is what
turns that from an assumption into a check. See F-13.

# Notes

Running this with a real agent would be pointless — it never gets as
far as caring what the agent did. `--skip-agent` keeps the test about
the one thing it is testing.

A blocked run keeps its worktree by design, and this one also leaves
the meddler's empty commit on the branch. Remove both before re-running:

    git -C <repo> worktree remove --force <repo>.worktrees/bot-fixer-T-0003
    git -C <repo> branch -D bot/fixer/T-0003
