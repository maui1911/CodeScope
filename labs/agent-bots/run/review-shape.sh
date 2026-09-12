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
#   BOT_ARTIFACT      path to the harvested report
#   BOT_SUBJECT_SHA   the commit the worktree was checked out at
#   BOT_TOUCHES       the task's touches: globs, comma-separated
#   BOT_TASK_ID       the task this review is supposed to answer
#
# All four are named for the *job* and not for the role that had it
# first. A report is an artifact, and the commit it is about is its
# subject, whether the reader is a reviewer, a doc writer or a
# benchmark. The two that were BOT_REVIEW and BOT_REVIEWED_SHA could
# only be renamed across two merges, because this script is read from
# the checkout under test while the runner exporting them is at base -
# see the block comment at the export site and #348.
#
# It runs inside the clean verify checkout, so `git` here is that tree.

set -euo pipefail

fail() { printf 'review-shape: %s\n' "$*" >&2; exit 1; }

[ -n "${BOT_ARTIFACT:-}" ] \
    || fail "BOT_ARTIFACT is not set - this verifier is for produces: report tasks"
[ -f "$BOT_ARTIFACT" ] || fail "no report at $BOT_ARTIFACT"

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
    ' "$BOT_ARTIFACT"
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
if [ -n "${BOT_SUBJECT_SHA:-}" ] && [ "$REVIEWED" != "$BOT_SUBJECT_SHA" ]; then
    fail "review says it covers $REVIEWED, but the worktree was $BOT_SUBJECT_SHA"
fi

if [ -n "${BOT_TASK_ID:-}" ] && [ "$TASK" != "$BOT_TASK_ID" ]; then
    fail "review answers task '$TASK', but this run is '$BOT_TASK_ID'"
fi

grep -q '^# What I could not check' "$BOT_ARTIFACT" \
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
' "$BOT_ARTIFACT")"
[ -n "$BLIND" ] || fail "'What I could not check' is empty"

grep -q '^# Findings' "$BOT_ARTIFACT" || fail "no 'Findings' section"

FINDINGS_BODY="$(awk '
    /^# Findings/ { inside = 1; next }
    inside && /^# / { exit }
    inside { print }
' "$BOT_ARTIFACT")"

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

# --------------------------------------------------------------------
# The claims, against the tree they are about
#
# Everything above checks that a citation *resolves*: the file is there
# at that commit, the line is inside it, the path is in scope. That
# catches an invented path and an invented coordinate and nothing else.
# A reviewer can still point at a line that really exists and describe
# something that is not on it - which is the failure mode that reads
# most like competence, because every mechanical property checks out.
#
# So a finding also quotes what it is about, and the quote has to be
# there. Substring, not equality: quoting a fragment of a long line is
# the honest way to cite one, and a fragment cannot be guessed either.
# Whitespace is normalised on both sides, because re-indenting a quote
# is not the same as inventing one.
#
# This does not move F-19's ceiling. Nothing here can tell you whether
# the finding is *right* - only that the reviewer was looking at the
# line it says it was looking at. That is the difference between having
# read the file and having read the file listing.
# --------------------------------------------------------------------

# Below this, a match proves nothing: `{` occurs on half the lines in
# any Rust file, and a quote that identifies nothing is not evidence.
# Counted without whitespace, so indentation cannot pad it.
QUOTE_MIN=8

norm() {   # norm <text> - collapse whitespace runs, trim both ends
    printf '%s' "$1" | tr '\t' ' ' | sed -e 's/  */ /g' -e 's/^ //' -e 's/ $//'
}

COUNT=0
CUR_CITE=""
CUR_QUOTE=""
CUR_QN=0

check_finding() {
    local path line lines ok g i src want have dense last
    [ -n "$CUR_CITE" ] || return 0
    COUNT=$((COUNT + 1))
    path="${CUR_CITE%:*}"
    line="${CUR_CITE##*:}"

    git cat-file -e "$BOT_SUBJECT_SHA:$path" 2>/dev/null \
        || fail "finding cites '$path', which does not exist at $BOT_SUBJECT_SHA"

    [ "$(git cat-file -t "$BOT_SUBJECT_SHA:$path" 2>/dev/null)" = "blob" ] \
        || fail "finding cites '$path', which is not a file at $BOT_SUBJECT_SHA"

    # awk, not `wc -l`: wc counts newlines, so a file whose last line
    # has none - generated config, a hand-edited fixture - comes back one
    # short, and a finding citing that last line is refused as past the
    # end of a file it is inside. awk counts records, and an unterminated
    # final line is a record.
    lines="$(git cat-file blob "$BOT_SUBJECT_SHA:$path" | awk 'END { print NR }')"
    if [ "$line" -lt 1 ] || [ "$line" -gt "$lines" ]; then
        fail "finding cites '$path:$line', but that file has $lines lines at $BOT_SUBJECT_SHA"
    fi

    ok=0
    for g in ${GLOBS_OUT[@]+"${GLOBS_OUT[@]}"}; do
        # shellcheck disable=SC2254  # a glob, on purpose
        case "$path" in $g) ok=1; break ;; esac
    done
    [ "$ok" -eq 1 ] \
        || fail "finding cites '$path', which is outside touches: ${BOT_TOUCHES:-}"

    [ "$CUR_QN" -gt 0 ] || fail "the finding at '$CUR_CITE' quotes nothing.

Every finding quotes the code it is about, on '> ' lines under its
bullet. A citation that resolves proves a file was opened; a quote that
matches proves this line was read."

    dense="$(norm "$(printf '%s' "$CUR_QUOTE" | tr '\n' ' ')" | tr -d ' ')"
    [ "${#dense}" -ge "$QUOTE_MIN" ] \
        || fail "the quote under '$CUR_CITE' carries ${#dense} characters of content.
Anything shorter than $QUOTE_MIN matches too much to be evidence. Quote more
of the line, or more lines."

    # A multi-line quote is consecutive lines starting at the cited one.
    # Reading the window in one go rather than a line at a time: the
    # blob is fetched once per finding, not once per quoted line.
    last=$((line + CUR_QN - 1))
    [ "$last" -le "$lines" ] \
        || fail "the quote under '$CUR_CITE' is $CUR_QN lines long, which runs past the end
of '$path' ($lines lines at $BOT_SUBJECT_SHA)"

    src="$(git cat-file blob "$BOT_SUBJECT_SHA:$path" | sed -n "${line},${last}p")"

    i=0
    while IFS= read -r want; do
        i=$((i + 1))
        want="$(norm "$want")"
        # A blank quote line is not a claim about anything. It still
        # costs an index, so a quote can straddle a blank source line
        # without the ones after it sliding.
        [ -n "$want" ] || continue
        have="$(norm "$(printf '%s\n' "$src" | sed -n "${i}p")")"
        case "$have" in
            *"$want"*) ;;
            *) fail "the quote under '$CUR_CITE' is not what is at that line.

  $path:$((line + i - 1)) is
      $have
  the review quotes
      $want" ;;
        esac
    done <<< "$CUR_QUOTE"
}

# One pass over the findings, pairing each bullet with the quote lines
# that follow it. A bullet may wrap over several lines - the template
# does it - so anything that is not a new bullet and not a quote line
# belongs to the prose and is ignored.
while IFS= read -r ln; do
    case "$ln" in
        -[[:space:]]*)
            check_finding
            CUR_CITE="$(printf '%s\n' "$ln" \
                | sed -n 's/^-[[:space:]]\{1,\}\([^[:space:]]\{1,\}\):\([0-9]\{1,\}\).*/\1:\2/p')"
            CUR_QUOTE=""
            CUR_QN=0
            ;;
        *)
            stripped="${ln#"${ln%%[![:space:]]*}"}"
            case "$stripped" in
                '>'*)
                    q="${stripped#>}"
                    q="${q# }"
                    if [ "$CUR_QN" -eq 0 ]; then
                        CUR_QUOTE="$q"
                    else
                        CUR_QUOTE="$CUR_QUOTE
$q"
                    fi
                    CUR_QN=$((CUR_QN + 1))
                    ;;
            esac
            ;;
    esac
done <<< "$FINDINGS_BODY"
check_finding

echo "review-shape: ok - $COUNT cited and quoted finding(s), $VERDICT"
exit 0
