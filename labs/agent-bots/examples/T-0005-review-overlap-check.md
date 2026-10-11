---
id: T-0005
produces: report
title: Review the dispatch claim and the overlap check
owner: reviewer
status: todo
base: labs/agent-bots
branch: bot/reviewer/T-0005
touches: labs/agent-bots/run/bot-run.sh
verify: bash labs/agent-bots/run/review-shape.sh
---

# Objective

Read the dispatch path of `labs/agent-bots/run/bot-run.sh` — the lock
helpers, `scan_overlaps`, the claim section, and the verdict chain —
and say whether it does what it claims to do.

The specific question: **can two runs of this script, started at the
same moment against the same repo, both dispatch tasks that touch the
same file?** If yes, name the window. If no, name what closes it.

Anything else you find in that file is in scope as a secondary finding,
ranked below the answer to that question.

# Acceptance

- [ ] `.bot-review.md` exists, names `verdict:`, and covers the commit
      the runner checked out.
- [ ] Every finding cites a real `path:line` in
      `labs/agent-bots/run/bot-run.sh`.
- [ ] "What I could not check" is filled in.
- [ ] No commit was made.

# Context

The relevant history is in the README: F-16 records why overlap is
decided by expanding globs against the base tree rather than by
comparing patterns, and F-17 records that the first version of the
check ran entirely outside any lock, so the answer could go stale
between reading it and acting on it.

Read those two findings before the code. They are the claims you are
testing — not instructions, and not a reason to agree.

The lock helpers are `take_lock` / `drop_lock`; the claim runs from
`take_lock "$STATE/dispatch.lock"` to the `drop_lock` after
`board "dispatched"`.

# Notes

This is the first task for a second bot, so it is as much a test of the
loop as of the code. The runner's own acceptance rules invert here: zero
commits is the success shape, and a commit is a failed run.

The review is harvested into `.state/artifacts/` and the worktree is
removed afterwards — the artifact is the review file, not a branch.

Out of scope: the contract Markdown, the stubs, and the README prose.
Findings about them belong under "What I could not check".
