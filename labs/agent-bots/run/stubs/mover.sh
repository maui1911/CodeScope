#!/usr/bin/env bash
# Stub agent: does its work, and moves the base ref out from under
# itself while it is at it.
#
# This is the one failure mode no amount of care at dispatch can catch.
# The overlap check asks "is another task holding these files right
# now"; it has never asked "will this still land on the base by the time
# a human merges it". Between those two questions sits every task that
# takes longer than the branch it was cut from stays still - which, once
# more than one bot is running, is the normal case.
#
# Driven entirely by the environment, because the four interesting
# outcomes differ only in what lands on the base:
#
#   BOT_SWEEP_REPO          repo root (worktrees cannot be assumed to
#                           resolve the common git dir the same way)
#   BOT_SWEEP_BASE_BRANCH   the branch to advance, short name
#   BOT_SWEEP_MINE          path this bot commits
#   BOT_SWEEP_MINE_BODY     what it puts there
#   BOT_SWEEP_THEIRS        path the base commit adds
#   BOT_SWEEP_THEIRS_BODY   what that one contains
#
# Same path, different bodies -> conflict. Same path, same body ->
# the rebase drops the commit as already upstream. Different paths ->
# a clean rebase, which is then only as good as the verifier says.
set -eu

: "${BOT_SWEEP_REPO:?mover.sh needs BOT_SWEEP_REPO}"
: "${BOT_SWEEP_BASE_BRANCH:?mover.sh needs BOT_SWEEP_BASE_BRANCH}"
: "${BOT_SWEEP_MINE:?mover.sh needs BOT_SWEEP_MINE}"
: "${BOT_SWEEP_THEIRS:?mover.sh needs BOT_SWEEP_THEIRS}"

# The bot's own work, in its own worktree. Ordinary, and it verifies.
mkdir -p "$(dirname "$BOT_SWEEP_MINE")"
printf '%s\n' "${BOT_SWEEP_MINE_BODY:-bot}" > "$BOT_SWEEP_MINE"
git add -- "$BOT_SWEEP_MINE"
git -c user.name=mover -c user.email=mover@example.invalid \
    commit -q -m "sweep: the work this bot was asked for"

# And now the world moves. Built with plumbing rather than a checkout:
# a second worktree here would be a second thing to clean up, and this
# has to work while the runner is holding the only one it knows about.
REF="refs/heads/$BOT_SWEEP_BASE_BRANCH"
BASE_COMMIT="$(git -C "$BOT_SWEEP_REPO" rev-parse --verify "$REF")"

TMP_INDEX="$(mktemp -u 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/mover.$$")"
export GIT_INDEX_FILE="$TMP_INDEX"
git -C "$BOT_SWEEP_REPO" read-tree "$BASE_COMMIT"
BLOB="$(printf '%s\n' "${BOT_SWEEP_THEIRS_BODY:-base}" \
    | git -C "$BOT_SWEEP_REPO" hash-object -w --stdin)"
git -C "$BOT_SWEEP_REPO" update-index --add \
    --cacheinfo "100644,$BLOB,$BOT_SWEEP_THEIRS"
TREE="$(git -C "$BOT_SWEEP_REPO" write-tree)"
unset GIT_INDEX_FILE
rm -f "$TMP_INDEX"

# An explicit identity: commit-tree reads the ambient one otherwise, and
# a stub that silently signs as whoever ran the sweep is the same
# mistake F-17 caught in the meddling verifier.
COMMIT="$(GIT_AUTHOR_NAME=mover GIT_AUTHOR_EMAIL=mover@example.invalid \
    GIT_COMMITTER_NAME=mover GIT_COMMITTER_EMAIL=mover@example.invalid \
    git -C "$BOT_SWEEP_REPO" commit-tree "$TREE" -p "$BASE_COMMIT" \
        -m "sweep: something else landed on the base")"
git -C "$BOT_SWEEP_REPO" update-ref "$REF" "$COMMIT" "$BASE_COMMIT"

echo "committed my change; $BOT_SWEEP_BASE_BRANCH moved to $COMMIT"
exit 0
