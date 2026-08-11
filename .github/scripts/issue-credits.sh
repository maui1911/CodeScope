#!/usr/bin/env bash
#
# Append an "issue reporters" credits section to a published GitHub release.
#
# Every fix that shipped because somebody outside the repo took the time to
# file an issue gets a thank-you line on the release page. Issues filed by the
# repo owner are skipped — self-credit is noise.
#
# How it finds the reporters:
#
#   1. previous release tag  -> the commit range this release covers
#   2. `compare` API         -> the commits in that range
#   3. trailing `(#123)` in  -> the PR each commit came from. Every commit on
#      the commit subject       `main` is a squash merge, so GitHub's own
#                               "(#123)" suffix is the PR number.
#   4. `closingIssuesReferences` (GraphQL) -> the issues each PR closes
#   5. issue author, minus the repo owner and bots -> the credit lines
#
# Re-running is safe: the section is delimited by HTML markers and a previous
# block is replaced rather than duplicated. Hand-edits outside the markers are
# preserved, which matters because release notes here get polished by hand
# after the workflow publishes them.
#
# Usage:
#   issue-credits.sh <tag> [--dry-run]
#
# Env:
#   GH_TOKEN     required — needs contents:write, pull-requests:read, issues:read
#   GITHUB_REPOSITORY  owner/repo (set by Actions; override for local runs)

set -euo pipefail

MARKER_START='<!-- issue-credits:start -->'
MARKER_END='<!-- issue-credits:end -->'

TAG="${1:-}"
DRY_RUN=false
[[ "${2:-}" == "--dry-run" ]] && DRY_RUN=true

if [[ -z "$TAG" ]]; then
    echo "usage: $0 <tag> [--dry-run]" >&2
    exit 2
fi

REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be set (owner/repo)}"
OWNER="${REPO%%/*}"
REPO_NAME="${REPO##*/}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- 1. the previous release, which bounds the commit range -------------------

# The releases API returns newest first, and this release already exists by the
# time we run, so the first non-draft entry that isn't us is the predecessor.
# No --paginate: with --jq, gh applies the filter per page, so a paginated
# `first` would emit one line per page. The newest page is all we need.
prev_tag="$(gh api "repos/$REPO/releases?per_page=100" \
    --jq "[.[] | select(.draft == false) | select(.tag_name != \"$TAG\") | .tag_name] | first // empty")"

if [[ -z "$prev_tag" ]]; then
    echo "no previous release found — nothing to diff against, skipping credits"
    exit 0
fi

echo "crediting reporters for $prev_tag..$TAG"

# --- 2/3. commits in range -> PR numbers -------------------------------------

# Match `(#N)` only at the end of the subject line: a squash merge always puts
# it there, while a subject that *mentions* other PRs mid-sentence (e.g. the
# HANDOFF commit "sessions 48-50 ... (#262-#266) (#267)") must resolve to #267
# alone.
gh api "repos/$REPO/compare/$prev_tag...$TAG" \
    --jq '.commits[].commit.message | split("\n")[0]' \
    | sed -nE 's/.*\(#([0-9]+)\)$/\1/p' \
    | sort -un > "$work/prs"

if [[ ! -s "$work/prs" ]]; then
    echo "no squash-merged PRs in range — skipping credits"
    exit 0
fi

echo "PRs in range: $(tr '\n' ' ' < "$work/prs")"

# --- 4/5. PR -> closed issues -> reporters -----------------------------------

: > "$work/credits"

while read -r pr; do
    # A PR that closes nothing is the common case, and a number that turns out
    # not to be a PR at all (a stray subject suffix) must not fail the release
    # — both just yield no lines.
    gh api graphql \
        -f query='
          query($owner: String!, $repo: String!, $pr: Int!) {
            repository(owner: $owner, name: $repo) {
              pullRequest(number: $pr) {
                closingIssuesReferences(first: 20) {
                  nodes { number author { login } }
                }
              }
            }
          }' \
        -F owner="$OWNER" -F repo="$REPO_NAME" -F pr="$pr" \
        --jq '.data.repository.pullRequest.closingIssuesReferences.nodes[]
              | select(.author != null)
              | "\(.number)\t\(.author.login)"' > "$work/issues" 2>/dev/null || true

    while IFS=$'\t' read -r issue author; do
        [[ -z "${author:-}" ]] && continue
        # Skip self-filed issues and bot accounts — neither wants thanking.
        [[ "$author" == "$OWNER" ]] && continue
        [[ "$author" == *"[bot]" ]] && continue
        printf '%s\t%s\t%s\n' "$pr" "$issue" "$author" >> "$work/credits"
    done < "$work/issues"
done < "$work/prs"

if [[ ! -s "$work/credits" ]]; then
    echo "no externally-filed issues closed in this release — nothing to credit"
    exit 0
fi

# --- 6. build the section ----------------------------------------------------

# Numeric on both keys so a PR closing #9 and #10 lists them in that order, and
# `-u` on (pr, issue) rather than the whole line: an issue has exactly one
# author, so that pair is already the identity of a credit line.
sort -u -t$'\t' -k1,1n -k2,2n "$work/credits" > "$work/credits-sorted"

{
    echo "$MARKER_START"
    echo
    echo "## Thanks"
    echo
    echo "This release fixes issues reported by the community — thank you:"
    echo
    while IFS=$'\t' read -r pr issue author; do
        echo "- #$pr — thanks @$author for reporting #$issue"
    done < "$work/credits-sorted"
    echo
    echo "$MARKER_END"
} > "$work/section"

echo "--- credits section ---"
cat "$work/section"
echo "-----------------------"

if [[ "$DRY_RUN" == true ]]; then
    echo "(dry run — release notes not modified)"
    exit 0
fi

# --- 7. splice it into the release notes -------------------------------------

gh release view "$TAG" --repo "$REPO" --json body --jq '.body' > "$work/body"

# Drop a previous block so re-runs replace instead of stacking — but only when
# both markers are there to bound it. With a start marker and no end marker
# (someone hand-edited the notes and clipped it), the strip would run to EOF
# and silently truncate the published release notes, so leave the body alone
# and let the operator sort out the stray marker.
if grep -qF "$MARKER_START" "$work/body" && grep -qF "$MARKER_END" "$work/body"; then
    awk -v s="$MARKER_START" -v e="$MARKER_END" '
        $0 == s { skip = 1 }
        !skip   { print }
        $0 == e { skip = 0 }
    ' "$work/body" > "$work/body-clean"
elif grep -qF "$MARKER_START" "$work/body"; then
    echo "warning: '$MARKER_START' present without its end marker — appending" >&2
    echo "         a fresh block instead of stripping, to avoid truncating the" >&2
    echo "         notes. Remove the stray marker by hand." >&2
    cp "$work/body" "$work/body-clean"
else
    cp "$work/body" "$work/body-clean"
fi

{
    # Command substitution strips trailing newlines, so stripping a previous
    # block never leaves a growing pile of blank lines behind.
    printf '%s\n\n' "$(cat "$work/body-clean")"
    cat "$work/section"
} > "$work/body-new"

gh release edit "$TAG" --repo "$REPO" --notes-file "$work/body-new"
echo "release notes for $TAG updated with $(wc -l < "$work/credits-sorted") credit line(s)"
