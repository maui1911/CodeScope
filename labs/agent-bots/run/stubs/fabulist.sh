#!/usr/bin/env bash
# Stub agent: writes a confident, well-formed review whose findings
# cite a file that does not exist. Exists to prove that the review
# verifier checks the claims against the tree rather than checking that
# a review was produced. See F-19.
#
# Every other property is correct - frontmatter parses, the verdict is
# in the vocabulary, the reviewed SHA matches, the blind spots are
# filled in, nothing is committed. Only the citation is invented, which
# is the failure mode a reviewer actually has.
#
# The second finding cites and quotes something real, so the run fails
# on the invented path rather than on a stub that got the shape wrong.
set -e

SHA="$(git rev-parse HEAD)"
REAL_LINE="$(git show "$SHA:labs/agent-bots/run/bot-run.sh" | sed -n '1p')"

cat > .bot-review.md <<REVIEW
---
task: T-0005
reviewed: $SHA
verdict: changes-requested
---

# What I checked

Read the dispatch path end to end, including the lock helpers and the
claim section, looking for a window between the overlap scan and the
creation of the live task.

# Findings

- labs/agent-bots/run/dispatch.sh:88 — the lock is released before the
  live task is written, which reopens the window it was taken to close.
  > release_lock DISPATCH_LOCK; write_live_task
- labs/agent-bots/run/bot-run.sh:1 — secondary: the header comment does
  not mention the dispatch lock.
  > $REAL_LINE

# What I could not check

Behaviour under a real second runner; this was a reading, not a test.
REVIEW

echo "Review written. One blocking finding, one minor."
exit 0
