#!/usr/bin/env bash
#
# review-shape.sh - the verifier for the reviewer's report.
#
# One shape among several: `produces: report` says a task leaves a file
# beside the tree, and `shape:` says which template that file has to
# match. This is the verifier for templates/REVIEW.md. Another report
# role brings its own template and its own checker; the runner hands
# every one of them the same three facts.
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
#   BOT_REVIEW        path to the harvested report
#   BOT_REVIEWED_SHA  the commit the worktree was checked out at
#
# Named for the reviewer rather than for the role, because the
# runner exporting them lives at base while this script is read from
# the checkout under test - so the pair can only be renamed once
# both halves are already merged. See #348.
#   BOT_TOUCHES       the task's touches: globs, comma-separated
#   BOT_TASK_ID       the task this review is supposed to answer
#
# It runs inside the clean verify checkout, so `git` here is that tree.

set -euo pipefail

fail() { printf 'review-shape: %s\n' "$*" >&2; exit 1; }

[ -n "${BOT_REVIEW:-}" ] || fail "BOT_REVIEW is not set - this verifier is for produces: report tasks"
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

if [ -n "${BOT_TASK_ID:-}" ] && [ "$TASK" != "$BOT_TASK_ID" ]; then
    fail "review answers task '$TASK', but this run is '$BOT_TASK_ID'"
fi

grep -q '^# What I could not check' "$BOT_REVIEW" \
    || fail "no 'What I could not check' section - the blind spots are part of the answer"

# One reader that consumes the whole file. The `sed | sed | head`
# version SIGPIPEd under pipefail on a review with more than five
# blind-spot lines - the wrong way round, since that is the thorough
# answer - and it counted the template's closing `---` as content, so
# an empty section passed.
BLIND="$(awk '
    /^# What I could not check/ { inside = 1; next }
    inside && /^# / { exit }
    inside && /^---[[:space:]]*$/ { exit }
    inside && NF { print }
' "$BOT_REVIEW")"
[ -n "$BLIND" ] || fail "'What I could not check' is empty"

grep -q '^# Findings' "$BOT_REVIEW" || fail "no 'Findings' section"

FINDINGS_BODY="$(awk '
    /^# Findings/ { inside = 1; next }
    inside && /^# / { exit }
    inside { print }
' "$BOT_REVIEW")"

# Exactly that line and nothing else. `grep -q` matched it *anywhere*
# in the section and returned straight away, so a review could claim no
# findings, list five, and skip every citation and scope check below.
FINDINGS_TRIMMED="$(printf '%s\n' "$FINDINGS_BODY" | sed '/^[[:space:]]*$/d')"
if [ "$FINDINGS_TRIMMED" = "No findings." ]; then
    # A verdict is a claim about the findings, so the two have to agree.
    # `changes-requested` with nothing to change is not something a
    # reader can work around: it asks for work and names none of it, and
    # downstream it would derive a task scoped to no paths at all.
    [ "$VERDICT" != "changes-requested" ] \
        || fail "verdict is changes-requested but the review lists no findings - which is it?"
    echo "review-shape: ok - no findings, $VERDICT"
    exit 0
fi

# Path and line, kept together. Dropping the line number before
# validating meant `bot-run.sh:999999` passed as long as the file
# existed - and what the contract promises is that the *citation* is
# real, not that the file is.
CITED="$(printf '%s\n' "$FINDINGS_BODY" \
    | sed -n 's/^-[[:space:]]\{1,\}\([^[:space:]]\{1,\}\):\([0-9]\{1,\}\).*/\1:\2/p')"

[ -n "$CITED" ] || fail "no findings and no 'No findings.' line - a review has to say which it is"

# Every bullet, not only the ones that happened to parse. Checking the
# extracted subset let a review pass with one good citation and a
# second, malformed finding beside it - and the contract is that every
# finding cites a real path:line, not that at least one does.
BULLETS="$(printf '%s\n' "$FINDINGS_BODY" | grep -c '^-[[:space:]]' || true)"
CITED_COUNT="$(printf '%s\n' "$CITED" | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')"
if [ "$BULLETS" != "$CITED_COUNT" ]; then
    fail "$BULLETS finding(s), $CITED_COUNT with a usable path:line citation.
Every finding has to name where it is. The offenders:
$(printf '%s\n' "$FINDINGS_BODY" \
    | grep '^-[[:space:]]' \
    | grep -v '^-[[:space:]]\{1,\}[^[:space:]]\{1,\}:[0-9]\{1,\}' \
    | sed 's/^/    /')"
fi

split_globs() {
    GLOBS_OUT=()
    local raw=() g
    IFS=',' read -ra raw <<< "$1"
    for g in ${raw[@]+"${raw[@]}"}; do
        g="${g#"${g%%[![:space:]]*}"}"
        g="${g%"${g##*[![:space:]]}"}"
        [ -n "$g" ] && GLOBS_OUT+=("$g")
    done
}
split_globs "${BOT_TOUCHES:-}"

# An empty scope is not an unlimited one. Skipping the loop when no glob
# parsed would let a review of `touches: ,` cite anything in the repo.
[ "${#GLOBS_OUT[@]}" -gt 0 ] \
    || fail "BOT_TOUCHES ('${BOT_TOUCHES:-}') normalises to no globs, so nothing can be in scope"

COUNT=0
while IFS= read -r citation; do
    [ -n "$citation" ] || continue
    COUNT=$((COUNT + 1))
    path="${citation%:*}"
    line="${citation##*:}"

    git cat-file -e "$BOT_REVIEWED_SHA:$path" 2>/dev/null \
        || fail "finding cites '$path', which does not exist at $BOT_REVIEWED_SHA"

    [ "$(git cat-file -t "$BOT_REVIEWED_SHA:$path" 2>/dev/null)" = "blob" ] \
        || fail "finding cites '$path', which is not a file at $BOT_REVIEWED_SHA"

    lines="$(git cat-file blob "$BOT_REVIEWED_SHA:$path" | wc -l | tr -d ' ')"
    if [ "$line" -lt 1 ] || [ "$line" -gt "$lines" ]; then
        fail "finding cites '$path:$line', but that file has $lines lines at $BOT_REVIEWED_SHA"
    fi

    ok=0
    for g in ${GLOBS_OUT[@]+"${GLOBS_OUT[@]}"}; do
        # shellcheck disable=SC2254  # a glob, on purpose
        case "$path" in $g) ok=1; break ;; esac
    done
    [ "$ok" -eq 1 ] \
        || fail "finding cites '$path', which is outside touches: ${BOT_TOUCHES:-}"
done <<< "$CITED"

echo "review-shape: ok - $COUNT cited path:line(s), $VERDICT"
exit 0
