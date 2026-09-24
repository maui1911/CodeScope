---
task: T-0000
from: fixer
to: human
at: 1970-01-01T00:00:00Z
status: done
---

# Objective

Restate what this run was for, in one sentence. If the objective moved
during the run, say so here — that is a finding, not a detail.

# Artifact

Where the work is. A **ref**, never a copy of the content:

    branch: bot/fixer/T-0000
    worktree: C:/dev/codescope-public.worktrees/bot-fixer-T-0000

# Evidence

Verifiable facts only. The reader checks these against the tree; they
do not take the summary on trust.

    base:    <sha>
    head:    <sha>
    numstat: <files changed>, +<added> -<removed>
    verify:  <the command>  -> exit <code>
    touched: <files, from git diff --name-only base..head>

# Status

`done` | `blocked` | `needs-review`. One word, then one sentence of why.

# Blockers

What stopped this, if anything. Empty is a valid answer — write
`none`, do not delete the section.

# Next action

Exactly one, with a named owner. "Human reviews and pushes" is a valid
next action. "Someone should probably look at X" is not.
