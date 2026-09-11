---
id: T-0004
title: Overlap fixture - stands in for a run that is still in flight
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0004
touches: core/src/*.rs
verify: cargo test -p codescope-core --lib
---

# Objective

Never dispatch this one. It exists to be *another* task — the in-flight
neighbour that the overlap check is supposed to notice.

Its `touches:` is `core/src/*.rs`, which under `case` semantics crosses
`/` and so claims most of the crate, including the one file T-0001
declares. That makes it collide with almost anything.

# Acceptance

Not applicable. This task is a fixture, not work.

# Context

The overlap check reads every live task under `.state/tasks/` whose
status is `dispatched` *and* whose worktree still exists on disk, and
intersects its `touches:` with this run's — both expanded against the
base tree. See README §3.4 and F-16.

# Notes

To reproduce the check by hand, from the repo root:

    # 1. make it look like a run that is still in flight
    mkdir -p labs/agent-bots/.state/tasks
    cp labs/agent-bots/examples/T-0004-overlap-fixture.md \
       labs/agent-bots/.state/tasks/T-0004.md
    # then change `status: todo` to `status: dispatched` in that copy
    git worktree add ../codescope-public.worktrees/bot-fixer-T-0004 \
       -b bot/fixer/T-0004 HEAD

    # 2. any overlapping task is now refused before it costs a worktree
    labs/agent-bots/run/bot-run.sh \
       --task labs/agent-bots/examples/T-0001-smoke-test.md --reset

    # 3. delete the worktree but leave the live task at `dispatched`,
    #    and the same run reports a stale dispatch and proceeds
    git worktree remove --force ../codescope-public.worktrees/bot-fixer-T-0004

    # 4. clean up
    git branch -D bot/fixer/T-0004
    rm labs/agent-bots/.state/tasks/T-0004.md

Step 3 is the half worth keeping honest about: a crashed run leaves its
live task saying `dispatched` forever, and a check that believed that
field alone would wedge every later task behind a ghost.
