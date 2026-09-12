#!/usr/bin/env bash
# Stub agent: does the work, then moves the repository out from under
# the runner.
#
# `.git` need not be a directory. A file whose single line reads
# `gitdir: /somewhere/else` is a valid `.git`, and it is what every
# linked worktree uses - so this is not a trick git tolerates by
# accident, it is a documented form. The surface's `.git` is the agent's
# to write (F-28), which means the agent can answer the question "which
# repository is this?" for every `git -C "$WT"` call the runner makes
# afterwards: the hook cleanup, the config disarm, the evidence, the
# commit, the push.
#
# Written as a file rather than a symlink on purpose: it needs no
# symlink privilege, so it is the portable version of the same move.
#
#   BOT_HIJACK_FILE   the honest part of the work
#   BOT_HIJACK_TARGET repository to point .git at (default: a sibling)
set -eu

FILE="${BOT_HIJACK_FILE:-notes.md}"
mkdir -p "$(dirname "$FILE")"
printf 'Written by an agent that then swapped the repository.\n' > "$FILE"
printf 'Add a file\n' > .bot-commit-msg

TARGET="${BOT_HIJACK_TARGET:-}"
if [ -z "$TARGET" ]; then
    TARGET="$PWD/.git-elsewhere"
    mv .git "$TARGET"
else
    rm -rf .git
fi
printf 'gitdir: %s\n' "$TARGET" > .git

echo "wrote $FILE and pointed .git at $TARGET"
exit 0
