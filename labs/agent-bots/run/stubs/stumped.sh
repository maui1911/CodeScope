#!/usr/bin/env bash
# Stub agent: a reviewer that completes the paperwork and says it could
# not do the job. `verdict: blocked` is the reviewer's own escalation -
# REVIEW.md defines it as a review that could not be completed - and it
# has to survive the trip to the handoff.
#
# It used to not. A blocked review made no commit, so it fell through
# to the generic zero-commit branch and was reported as `done`: the task
# went terminal, the scheduler never came back, and the only record of
# the refusal was inside a file nobody had been told to open. Same shape
# as F-4, one layer up.
set -e

SHA="$(git rev-parse HEAD)"

cat > .bot-review.md <<REVIEW
---
task: ${BOT_TASK_ID:-T-0006}
reviewed: $SHA
verdict: blocked
---

# What I checked

Nothing. This is a stub standing in for a reviewer that could not do
the job it was given.

# Findings

No findings.

# What I could not check

All of it. The point of this stub is that a review which says so must
not be reported as a successful run.
REVIEW

echo "Could not complete the review."
exit 0
