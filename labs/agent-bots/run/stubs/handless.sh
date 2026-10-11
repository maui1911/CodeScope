#!/usr/bin/env bash
# Stub agent: does the work and cannot commit it.
#
# The shape every sandboxed agent has. Codex denies the model writes to
# `.git` wherever `.git` is, so it can edit every file it was given and
# not record a single one of them - see F-28. Before the runner learned
# to commit what an agent leaves behind, that arrived as "uncommitted
# files in the worktree", which is the diagnosis of an agent that was
# sloppy rather than one that was not allowed.
#
#   BOT_HANDLESS_FILE   what to write (default notes.md)
#   BOT_HANDLESS_ALSO   a second path to write, for the case where an
#                       agent touches something on its way past that the
#                       runner must not carry into the commit
#   BOT_HANDLESS_MSG    the commit message to leave in .bot-commit-msg.
#                       Unset means the agent wrote none, which is the
#                       other half of the test: the work still has to be
#                       committed, and the run still has to say that
#                       nobody explained it.
set -eu

FILE="${BOT_HANDLESS_FILE:-notes.md}"
mkdir -p "$(dirname "$FILE")"
printf 'Written by an agent that cannot commit.\n' > "$FILE"

# Not an accident, and not necessarily malice either: an agent that runs
# a build, a formatter or a dependency install can rewrite a file nobody
# asked it to. The runner commits what is left behind, so this is the
# case that decides what "left behind" may include.
[ -z "${BOT_HANDLESS_ALSO:-}" ] \
    || printf 'SECRET=rewritten-by-the-agent\n' > "$BOT_HANDLESS_ALSO"

if [ -n "${BOT_HANDLESS_MSG:-}" ]; then
    printf '%s\n' "$BOT_HANDLESS_MSG" > .bot-commit-msg
    echo "wrote $FILE and a commit message; committing is not mine to do"
else
    echo "wrote $FILE and no commit message"
fi
exit 0
