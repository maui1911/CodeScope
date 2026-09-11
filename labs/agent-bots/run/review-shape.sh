#!/usr/bin/env bash
#
# review-shape.sh - the verifier for a `kind: review` task.
#
# "No verifier, no dispatch" is the rule, and a review has no cargo
# command that can prove it. What it does have is a shape, and a set of
# claims that can be checked against the tree they are about:
#
#   - the review exists, parses, and names a verdict in the vocabulary
#   - it is about the commit the runner actually handed over
#   - every finding cites a path that exists at that commit
#   - every cited path is inside the task's `touches:`
#   - it says what it could not check
#
# What this cannot do is tell you whether the review is any good. That
# ceiling is the point of F-19: a patch has an executable predicate, a
# judgement does not. This checks that a reviewer looked at the right
# thing and did not invent file names - the strongest honest claim
# available, and nowhere near "the review is correct".
#
# The runner exports what it needs; there are no arguments.
#
#   BOT_REVIEW        path to the harvested review
#   BOT_REVIEWED_SHA  the commit the worktree was checked out at
#   BOT_TOUCHES       the task's touches: globs, comma-separated
#
# It runs inside the clean verify checkout, so `git` here is that tree.

set -euo pipefail

fail() { printf 'review-shape: %s\n' "$*" >&2; exit 1; }

[ -n "${BOT_REVIEW:-}" ] || fail "BOT_REVIEW is not set - this verifier is for kind: review tasks"
[ -f "$BOT_REVIEW" ] || fail "no review at $BOT_REVIEW"

field() {
    awk -v key="$1" '
        /^---[[:space:]]*$/ { fence++; next }
        fence == 1 && !found && index($0, key ":") == 1 {
            value = substr($0, length(key) + 2)
            sub(/^[[:space:]]+/, "", value)
            sub(/[[:space:]]+$/, "", value)
            print value
            found = 1
        }
    ' "$BOT_REVIEW"
}

TASK="$(field task)"
REVIEWED="$(field reviewed)"
VERDICT="$(field verdict)"

[ -n "$TASK" ]     || fail "frontmatter is missing 'task:'"
[ -n "$REVIEWED" ] || fail "frontmatter is missing 'reviewed:'"
[ -n "$VERDICT" ]  || fail "frontmatter is missing 'verdict:'"

case "$VERDICT" in
    approve|changes-requested|blocked) ;;
    *) fail "verdict '$VERDICT' is not one of approve, changes-requested, blocked" ;;
esac

# A review of the wrong commit is worse than no review: it reads as
# current and describes something else.
if [ -n "${BOT_REVIEWED_SHA:-}" ] && [ "$REVIEWED" != "$BOT_REVIEWED_SHA" ]; then
    fail "review says it covers $REVIEWED, but the worktree was $BOT_REVIEWED_SHA"
fi

grep -q '^# What I could not check' "$BOT_REVIEW" \
    || fail "no 'What I could not check' section - the blind spots are part of the answer"

BLIND="$(sed -n '/^# What I could not check/,$p' "$BOT_REVIEW" \
    | sed '1d' | sed '/^[[:space:]]*$/d' | head -n 5)"
[ -n "$BLIND" ] || fail "'What I could not check' is empty"

grep -q '^# Findings' "$BOT_REVIEW" || fail "no 'Findings' section"

FINDINGS_BODY="$(sed -n '/^# Findings/,/^# /p' "$BOT_REVIEW" | sed '1d;$d')"

if printf '%s\n' "$FINDINGS_BODY" | grep -q '^No findings\.'; then
    echo "review-shape: ok - no findings, $VERDICT"
    exit 0
fi

CITED="$(printf '%s\n' "$FINDINGS_BODY" \
    | sed -n 's/^-[[:space:]]\{1,\}\([^[:space:]]\{1,\}\):[0-9]\{1,\}.*/\1/p')"

[ -n "$CITED" ] || fail "no findings and no 'No findings.' line - a review has to say which it is"

split_globs() {
    GLOBS_OUT=()
    local raw=() g
    IFS=',' read -ra raw <<< "$1"
    for g in "${raw[@]}"; do
        g="${g#"${g%%[![:space:]]*}"}"
        g="${g%"${g##*[![:space:]]}"}"
        [ -n "$g" ] && GLOBS_OUT+=("$g")
    done
}
split_globs "${BOT_TOUCHES:-}"

COUNT=0
while IFS= read -r path; do
    [ -n "$path" ] || continue
    COUNT=$((COUNT + 1))

    git cat-file -e "$BOT_REVIEWED_SHA:$path" 2>/dev/null \
        || fail "finding cites '$path', which does not exist at $BOT_REVIEWED_SHA"

    if [ "${#GLOBS_OUT[@]}" -gt 0 ]; then
        ok=0
        for g in "${GLOBS_OUT[@]}"; do
            # shellcheck disable=SC2254  # a glob, on purpose
            case "$path" in $g) ok=1; break ;; esac
        done
        [ "$ok" -eq 1 ] \
            || fail "finding cites '$path', which is outside touches: ${BOT_TOUCHES:-}"
    fi
done <<< "$CITED"

echo "review-shape: ok - $COUNT cited path(s), $VERDICT"
exit 0
