#!/usr/bin/env bash
# Stub agent: writes a valid `changes-requested` review whose findings
# cite real paths. Exists so the bot-to-bot handoff can be exercised
# without depending on a real reviewer happening to dislike something.
#
# Where fabulist.sh is the negative case - a well-formed review that
# invents a path - this is the positive one: everything checks out, so
# the runner should derive a task for the next bot and address the
# handoff to it.
set -e

SHA="$(git rev-parse HEAD)"

cat > .bot-review.md <<REVIEW
---
task: ${BOT_TASK_ID:-T-0006}
reviewed: $SHA
verdict: changes-requested
---

# What I checked

Read core/src/telemetry.rs end to end against the conventions. This is
a stub: the findings below are placeholders with real citations, not
real observations.

# Findings

- core/src/telemetry.rs:1 — placeholder finding, cited at a line that
  exists so the handoff can be exercised end to end.

# What I could not check

Everything. This is a stub agent; it read nothing and judged nothing.
Any run that treats this review as a real one has a problem upstream.
REVIEW

echo "Review written. One finding."
exit 0
