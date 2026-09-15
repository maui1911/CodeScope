#!/usr/bin/env bash
# Stub agent: does the work, commits it, and then puts the branch back
# where it found it.
#
# The runner asks `merge-base --is-ancestor "$BASE_SHA" HEAD`, which is
# true when HEAD *is* the base - so this run reads as zero commits, in
# scope, clean worktree: the no-op shape. The no-op path removes the
# surface and the branch, which is the only place the commit still
# existed. "Nothing happened" and "it was undone" are not the same
# report, and only one of them may be cleaned up.
#
#   BOT_RESET_FILE   the work that gets committed and then discarded
set -eu

FILE="${BOT_RESET_FILE:-notes.md}"
mkdir -p "$(dirname "$FILE")"
printf 'Work that is about to be thrown away.\n' > "$FILE"

git add -- "$FILE"
git -c user.name=stub -c user.email=stub@invalid commit --quiet -m "work, briefly"
git reset --hard HEAD~1 >/dev/null 2>&1

echo "committed $FILE and reset the branch back"
exit 0
