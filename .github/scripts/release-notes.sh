#!/usr/bin/env bash
#
# Fill in the generated half of a published GitHub release's notes.
#
# cargo-dist publishes only a download table, and the prose above it has always
# been typed by hand — so a release where that step is skipped ships with no
# notes at all (v0.4.0, v0.5.0, v0.5.1, v0.5.2 and v0.5.4 all went out that
# way). This writes two blocks so that never happens again:
#
#   "What's changed" — one line per merged PR in the range, grouped by
#                      conventional-commit type. A starting point to polish,
#                      not a replacement for real prose.
#   "Thanks"         — the people outside this repo who filed the issues the
#                      release fixes. Issues filed by the repo owner are
#                      skipped; self-credit is noise.
#
# Where the data comes from:
#
#   1. previous release tag  -> the commit range this release covers
#   2. `compare` API         -> the commits in that range. Every commit on
#                               `main` is a squash merge, so each subject is
#                               exactly "<pr title> (#<pr number>)" — both
#                               blocks come out of that one call.
#   3. `closingIssuesReferences` (GraphQL) -> the issues each PR closes
#   4. issue author, minus the repo owner and bots -> the credit lines
#
# Each block is delimited by HTML markers and replaced in place on a re-run,
# so hand-polish *outside* the markers survives. Text you write inside them is
# overwritten — move anything you want to keep above the summary block.
#
# Usage:
#   release-notes.sh <tag> [--dry-run]
#
# Env:
#   GH_TOKEN     required — needs contents:write, pull-requests:read, issues:read
#   GITHUB_REPOSITORY  owner/repo (set by Actions; override for local runs)
#
# Note: the GraphQL step cannot be exercised from a Claude cloud/web session —
# `api.github.com/graphql` there answers "This GraphQL query is not enabled for
# this session". Dry-run from a normal local terminal.

set -euo pipefail

SUMMARY_START='<!-- release-summary:start -->'
SUMMARY_END='<!-- release-summary:end -->'
DOWNLOAD_START='<!-- download-table:start -->'
DOWNLOAD_END='<!-- download-table:end -->'
CREDITS_START='<!-- issue-credits:start -->'
CREDITS_END='<!-- issue-credits:end -->'

# Assets that are built and attached but deliberately kept out of the download
# table. The file stays on the release under Assets for anyone who needs it.
TABLE_EXCLUDE=(
    "codescope-x86_64-apple-darwin.tar.gz"   # Intel macOS: shipped, not advertised
)

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

# Drop a previously generated block so re-runs replace instead of stacking —
# but only when both markers are there to bound it. With a start marker and no
# end marker (someone hand-edited the notes and clipped it), the strip would
# run to EOF and silently truncate the published notes.
strip_block() {
    local file="$1" start="$2" end="$3"

    if grep -qF "$start" "$file" && grep -qF "$end" "$file"; then
        awk -v s="$start" -v e="$end" '
            $0 == s { skip = 1 }
            !skip   { print }
            $0 == e { skip = 0 }
        ' "$file"
    else
        if grep -qF "$start" "$file"; then
            echo "warning: '$start' present without its end marker — appending a" >&2
            echo "         fresh block instead of stripping, to avoid truncating" >&2
            echo "         the notes. Remove the stray marker by hand." >&2
        fi
        cat "$file"
    fi
}

# --- 1. the previous release, which bounds the commit range -------------------

# The releases API returns newest first, and this release already exists by the
# time we run, so the first non-draft entry that isn't us is the predecessor.
# No --paginate: with --jq, gh applies the filter per page, so a paginated
# `first` would emit one line per page. The newest page is all we need.
prev_tag="$(gh api "repos/$REPO/releases?per_page=100" \
    --jq "[.[] | select(.draft == false) | select(.tag_name != \"$TAG\") | .tag_name] | first // empty")"

if [[ -z "$prev_tag" ]]; then
    echo "no previous release found — nothing to diff against, skipping"
    exit 0
fi

echo "generating notes for $prev_tag..$TAG"

# --- 2. commits in range -> "<title> (#<pr>)" subjects ------------------------

gh api "repos/$REPO/compare/$prev_tag...$TAG" \
    --jq '.commits[].commit.message | split("\n")[0]' > "$work/subjects"

# Match `(#N)` only at the end of the subject: a squash merge always puts it
# there, while a subject that *mentions* other PRs mid-sentence (e.g. the
# HANDOFF commit "sessions 48-50 ... (#262-#266) (#267)") must resolve to #267
# alone.
sed -nE 's/.*\(#([0-9]+)\)$/\1/p' "$work/subjects" | sort -un > "$work/prs"

if [[ ! -s "$work/prs" ]]; then
    echo "no squash-merged PRs in range — skipping"
    exit 0
fi

echo "PRs in range: $(tr '\n' ' ' < "$work/prs")"

# --- 3. the "What's changed" block -------------------------------------------

: > "$work/added"; : > "$work/fixed"; : > "$work/other"

# Held in variables: bash's `[[ =~ ]]` cannot parse `(`, `)` and `|` inline.
RE_PR_SUFFIX='\(#([0-9]+)\)$'
RE_VERSION_BUMP='^(release|chore)(\([^)]*\))?!?:[[:space:]]*bump[[:space:]]+version'
RE_RELEASE_TYPE='^release(\([^)]*\))?!?:[[:space:]]'
RE_CONVENTIONAL='^([a-z]+)(\(([^)]*)\))?(!)?:[[:space:]]*(.*)$'

while IFS= read -r subject; do
    [[ "$subject" =~ $RE_PR_SUFFIX ]] || continue
    pr="${BASH_REMATCH[1]}"
    title="${subject% (#$pr)}"

    # The version-bump PR is bookkeeping, not a change anyone reads about.
    if [[ "$title" =~ $RE_VERSION_BUMP ]] || [[ "$title" =~ $RE_RELEASE_TYPE ]]; then
        echo "  excluding #$pr ($title) — version bump" >&2
        continue
    fi

    # Split a conventional-commit subject into type / scope / description so the
    # scope can carry the bullet. Anything that doesn't match is kept verbatim.
    if [[ "$title" =~ $RE_CONVENTIONAL ]]; then
        type="${BASH_REMATCH[1]}"
        scope="${BASH_REMATCH[3]}"
        desc="${BASH_REMATCH[5]}"
        if [[ -n "$scope" ]]; then
            line="- **$scope**: $desc (#$pr)"
        else
            line="- $desc (#$pr)"
        fi
    else
        type="other"
        line="- $title (#$pr)"
    fi

    case "$type" in
        feat)      echo "$line" >> "$work/added" ;;
        fix|perf)  echo "$line" >> "$work/fixed" ;;
        *)         echo "$line" >> "$work/other" ;;
    esac
done < "$work/subjects"

{
    echo "$SUMMARY_START"
    echo
    echo "## What's changed"
    if [[ -s "$work/fixed" ]]; then
        echo; echo "### Fixed"; echo; cat "$work/fixed"
    fi
    if [[ -s "$work/added" ]]; then
        echo; echo "### Added"; echo; cat "$work/added"
    fi
    if [[ -s "$work/other" ]]; then
        echo; echo "### Other"; echo; cat "$work/other"
    fi
    echo
    echo "$SUMMARY_END"
} > "$work/summary"

# --- 3b. the download table ---------------------------------------------------

# cargo-dist builds its own table from `targets` in dist-workspace.toml, which
# lists only the three unix targets — the Windows installer and zip come from
# the separate `windows-package` job (Inno Setup) and are invisible to it. So
# the generated table left the single most important file for a Windows-first
# app unlisted. Build the table from what is actually attached to the release
# instead, Windows first.
gh api "repos/$REPO/releases/tags/$TAG" --jq '.assets[].name' > "$work/assets"

# A row per asset we advertise, in the order a reader most likely wants them.
# Anything not named here is skipped — logged below so it is never silent.
label_for() {
    case "$1" in
        *-setup.exe)                        echo "Windows installer" ;;
        *-windows.zip)                      echo "Windows portable (zip)" ;;
        *aarch64-apple-darwin.tar.gz)       echo "Apple Silicon macOS" ;;
        *x86_64-unknown-linux-gnu.tar.gz)   echo "x64 Linux" ;;
        *)                                  echo "" ;;
    esac
}

rank_for() {
    case "$1" in
        *-setup.exe)                        echo 1 ;;
        *-windows.zip)                      echo 2 ;;
        *aarch64-apple-darwin.tar.gz)       echo 3 ;;
        *x86_64-unknown-linux-gnu.tar.gz)   echo 4 ;;
        *)                                  echo 9 ;;
    esac
}

asset_url() { echo "https://github.com/$REPO/releases/download/$TAG/$1"; }

: > "$work/rows"
while IFS= read -r asset; do
    [[ -z "$asset" ]] && continue
    # Checksums are rendered as a column on their owner's row, not as rows.
    [[ "$asset" == *.sha256 ]] && continue

    skip=false
    for excluded in "${TABLE_EXCLUDE[@]}"; do
        if [[ "$asset" == "$excluded" ]]; then
            echo "  excluding $asset from the table (still attached to the release)" >&2
            skip=true
            break
        fi
    done
    [[ "$skip" == true ]] && continue

    label="$(label_for "$asset")"
    if [[ -z "$label" ]]; then
        echo "  not advertised in the table: $asset" >&2
        continue
    fi

    # Only the cargo-dist archives carry a per-file .sha256; the Inno Setup
    # output does not, so that column is an em dash rather than a dead link.
    if grep -qxF "$asset.sha256" "$work/assets"; then
        checksum="[checksum]($(asset_url "$asset.sha256"))"
    else
        checksum="—"
    fi

    printf '%s\t| [%s](%s) | %s | %s |\n' \
        "$(rank_for "$asset")" "$asset" "$(asset_url "$asset")" "$label" "$checksum" \
        >> "$work/rows"
done < "$work/assets"

: > "$work/download"
if [[ -s "$work/rows" ]]; then
    {
        echo "$DOWNLOAD_START"
        echo
        echo "## Download codescope ${TAG#v}"
        echo
        echo "|  File  | Platform | Checksum |"
        echo "|--------|----------|----------|"
        sort -t$'\t' -k1,1n "$work/rows" | cut -f2-
        echo
        echo "$DOWNLOAD_END"
    } > "$work/download"
else
    echo "warning: no advertisable assets found — leaving the existing table alone" >&2
fi

# --- 4. PR -> closed issues -> reporters --------------------------------------

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

# Numeric on both keys so a PR closing #9 and #10 lists them in that order, and
# `-u` on (pr, issue) rather than the whole line: an issue has exactly one
# author, so that pair is already the identity of a credit line.
sort -u -t$'\t' -k1,1n -k2,2n "$work/credits" > "$work/credits-sorted"

: > "$work/thanks"
if [[ -s "$work/credits-sorted" ]]; then
    {
        echo "$CREDITS_START"
        echo
        echo "## Thanks"
        echo
        echo "This release fixes issues reported by the community — thank you:"
        echo
        while IFS=$'\t' read -r pr issue author; do
            echo "- #$pr — thanks @$author for reporting #$issue"
        done < "$work/credits-sorted"
        echo
        echo "$CREDITS_END"
    } > "$work/thanks"
else
    echo "no externally-filed issues closed in this release — nothing to credit"
fi

# --- 5. splice both blocks into the release notes -----------------------------

echo "--- generated blocks ---"
cat "$work/summary"
[[ -s "$work/thanks" ]] && cat "$work/thanks"
echo "------------------------"

if [[ "$DRY_RUN" == true ]]; then
    echo "(dry run — release notes not modified)"
    exit 0
fi

gh release view "$TAG" --repo "$REPO" --json body --jq '.body' > "$work/body"

strip_block "$work/body" "$SUMMARY_START" "$SUMMARY_END" > "$work/body-1"
strip_block "$work/body-1" "$DOWNLOAD_START" "$DOWNLOAD_END" > "$work/body-2"
strip_block "$work/body-2" "$CREDITS_START" "$CREDITS_END" > "$work/body-3"

# On the first run the body still holds cargo-dist's own unmarked table. Drop
# it so the generated one replaces it instead of appearing alongside — but only
# when there is a replacement to put back.
if [[ -s "$work/download" ]]; then
    awk '
        /^## Download codescope / { skip = 1; next }
        skip && (/^## / || /^<!--/) { skip = 0 }
        !skip { print }
    ' "$work/body-3" > "$work/body-clean"
else
    cp "$work/body-3" "$work/body-clean"
fi

# Summary, then whatever prose was written by hand, then the download table,
# then the credits — the shape the hand-written v0.5.3 notes settled on.
{
    printf '%s\n\n' "$(cat "$work/summary")"
    # `sed '/./,$!d'` drops the blank lines the strips leave at the top;
    # command substitution drops the trailing ones. Without both, every re-run
    # adds another blank line.
    body="$(sed -e '/./,$!d' "$work/body-clean")"
    [[ -n "$body" ]] && printf '%s\n\n' "$body"
    [[ -s "$work/download" ]] && printf '%s\n\n' "$(cat "$work/download")"
    [[ -s "$work/thanks" ]] && cat "$work/thanks"
} > "$work/body-new"

gh release edit "$TAG" --repo "$REPO" --notes-file "$work/body-new"
echo "release notes for $TAG updated ($(wc -l < "$work/credits-sorted") credit line(s))"
