#!/usr/bin/env bash
#
# bot-run.sh - run one task, for one bot, in its own git worktree.
#
# This is the labs prototype of the agent-bots loop. It uses no
# CodeScope code: the whole point is to prove the file contract before
# any of it is wired into the GPUI shell. See ../README.md.
#
#   dispatch -> worktree -> agent -> verifier -> evidence -> handoff
#
# Two rules this script exists to enforce:
#
#   1. The runner writes the handoff, never the agent. Evidence that
#      the agent reports about itself is not evidence.
#   2. A task without a `verify:` command is never dispatched. If
#      nothing can prove the work, there is nothing to run.
#
# It never pushes and never opens a PR. The loop stops at "branch is
# ready, here is the SHA".
#
# Usage:
#   bot-run.sh --task <file> [options]
#
#   --task <file>        Task definition to run. Required.
#   --repo <dir>         Repo root. Default: the task file's repo.
#   --state <dir>        Control plane. Default: <labs>/agent-bots/.state
#   --worktree-root <d>  Where work surfaces go. Default: <repo>.worktrees
#   --dry-run            Print the resolved plan and the prompt. Change
#                        nothing on disk.
#   --skip-agent         Full loop, but stub the agent call. Smoke-tests
#                        worktree + verifier + handoff on their own.
#   --keep               Keep the work surface even on a clean no-op run.
#   --reset              Discard the live task under state and start over.
#                        Needed to re-run a task that already finished.
#   --allow-overlap      Dispatch even though another in-flight task
#                        declares files this one also touches.
#   --chain              After a review that asks for changes, dispatch
#                        the task derived from it instead of stopping
#                        at "here is the command". Off by default.
#   --no-rebase          Do not replay the work onto the base ref if it
#                        moved while the run was in flight. Hands off a
#                        branch verified against a commit that is no
#                        longer the tip.
#   -h, --help           This text.
#
# Exit codes: 0 done, 1 blocked, 2 needs-review, 3 never started.
#
# 3 is its own code because it is not a result. An overlapping claim, or
# a lock broken mid-dispatch, means nothing ran and nothing was decided:
# the right answer is to come back later, not to count a failure against
# the task. A scheduler that cannot tell those apart backs off from work
# it never attempted.
#
# Which agent runs is data, not code. The task's `agent:` wins, else
# the charter's; the profile lives in contract/agents/<id>.agent.md and
# carries the invocation, the flag that lets it work unattended, and
# the instruction files it reads. Nothing here assumes Claude Code -
# Codex alone rules that out, since its headless mode is a subcommand
# rather than a flag. See F-10.
#
# Environment overrides, for the stubs in run/stubs/:
#   BOT_AGENT_CMD    Replaces the profile's command.
#   BOT_AGENT_ARGS   Replaces the whole argv, prompt included. Counts
#                    as set even when empty, which is how a stub gets
#                    invoked bare.
#
# On autonomy flags generally - be honest about what they buy: the
# worktree bounds what the agent is *meant* to touch, not what it
# *can*. It is a work surface, not a security boundary, which is the
# same thing this design criticises Grok Bot for. What contains the
# blast radius is that the branch is throwaway and nothing here ever
# pushes. Codex is the exception: it sandboxes model-generated shell
# commands itself.

set -euo pipefail

# --------------------------------------------------------------------
# Arguments
# --------------------------------------------------------------------

TASK=""
REPO=""
STATE=""
WORKTREE_ROOT=""
DRY_RUN=0
SKIP_AGENT=0
KEEP=0
RESET=0
ALLOW_OVERLAP=0
CHAIN=0
NO_REBASE=0
AGENT_EXIT=0

# Repo-relative, and deliberately a single variable: the prompt points
# the agent at these paths and the base check below proves they exist.
# Two copies of the same string would be free to drift.
CONTRACT_DIR="labs/agent-bots/contract"

# Paths no run commits, whatever the project's .gitignore says about
# them. They are excluded three times over, because each layer has a
# hole the next one covers: `info/exclude` is overridden by a
# .gitignore negation, a pathspec only binds the `git add` this script
# runs, and neither of them constrains an agent that commits by itself.
# The last line of defence is the verdict - a committed diff carrying
# one of these is a blocked run.
PROTECTED_EXCLUDE_FILE=".env
.env.*
*.pem
*.key
*.p12
*.pfx
*.keystore
id_rsa*
id_ed25519*
.npmrc
.netrc"

is_protected() {   # is_protected <path> - by basename, which is how these are named
    case "${1##*/}" in
        .env|.env.*|*.pem|*.key|*.p12|*.pfx|*.keystore|id_rsa*|id_ed25519*|.npmrc|.netrc)
            return 0 ;;
    esac
    return 1
}

# strip_protected <base-commit-or-empty> [git args...] - drop protected
# paths from the index that has just been built. The trailing arguments
# are the git invocation prefix, because the snapshot builds its index
# with --git-dir and a temporary GIT_INDEX_FILE while the surface does
# neither.
#
# After the fact rather than as a pathspec on the `add`, for two
# reasons. git refuses an `add` whose pathspec set names a file its
# ignore rules already exclude, so the two mechanisms fight; and this
# one does not care how a path got into the index, which is the point -
# a .gitignore negation re-including a secret is exactly the case
# info/exclude cannot hold.
#
# The base commit is what makes "hold it back" mean the same thing for
# a path that is already in the history. `rm --cached` on one of those
# does not unstage the agent's edit, it stages a *deletion* - so a
# project that committed its .env years ago would have this runner
# quietly remove it, and the protected-diff check downstream would only
# notice after the commit existed. Anything the base already carries is
# put back exactly as the base has it; only genuinely new paths are
# dropped.
# What the last call held back, for the verdict to read. Collected here
# rather than inferred later, because by then the index is clean and
# there is nothing left to notice: the whole point of this function is
# that it makes the evidence stop mentioning these paths.
PROTECTED_HELD=""

strip_protected() {
    local base="$1" f staged entry mode sha
    shift
    staged="$(git "$@" ls-files 2>/dev/null || true)"
    [ -n "$staged" ] || return 0
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        is_protected "$f" || continue
        if [ -n "$base" ] && git "$@" cat-file -e "$base:$f" 2>/dev/null; then
            entry="$(git "$@" ls-tree "$base" -- "$f" 2>/dev/null || true)"
            mode="${entry%% *}"
            sha="$(printf '%s' "$entry" | awk '{print $3}')"
            if [ -n "$mode" ] && [ -n "$sha" ] \
                && git "$@" update-index --cacheinfo "$mode,$sha,$f" >/dev/null 2>&1; then
                say "  held back $f - a protected path, left as $base has it"
                PROTECTED_HELD="$PROTECTED_HELD    $f"$'\n'
                continue
            fi
        fi
        git "$@" rm --cached -q --ignore-unmatch -- "$f" >/dev/null 2>&1 || true
        say "  held back $f - a protected path"
        PROTECTED_HELD="$PROTECTED_HELD    $f"$'\n'
    done <<< "$staged"
    return 0
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

die() { printf 'bot-run: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

# Nothing ran and nothing was decided - come back later. Separate from
# die so a scheduler can tell "refused" from "failed": one is a reason
# to retry, the other is a reason to stop.
refuse() { printf 'bot-run: %s\n' "$*" >&2; exit 3; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

# --------------------------------------------------------------------
# Locks
#
# mkdir is the portable atomic primitive: exactly one process creates
# the directory. The two things that make a mkdir lock dangerous are
# both handled here, because a lock nobody releases is worse than no
# lock at all - it wedges every later run rather than one.
#
#   - the holder dies: the EXIT trap releases whatever is still held,
#     and anything older than ten minutes is treated as a crash and
#     broken by the next run.
#   - the waiter gives up: it dies with the path in the message rather
#     than proceeding unlocked.
# --------------------------------------------------------------------

# Identifies this run inside a lock. A bare PID is not enough - they
# are reused - so the clock and $RANDOM go in as well.
RUN_TOKEN="$$-$(date -u +%s)-${RANDOM}"

LOCKS_HELD=()

# Only ever remove a lock this run still owns. Without the token check
# a run whose lock was broken out from under it deletes the *next*
# holder's lock on the way out, leaving that one inside the critical
# section with the door open behind it.
_release_one() {
    local dir="$1"
    if [ "$(cat "$dir/owner" 2>/dev/null || true)" = "$RUN_TOKEN" ]; then
        rm -f "$dir/owner"
        rmdir "$dir" 2>/dev/null || true
    fi
}

release_locks() {
    local d
    for d in ${LOCKS_HELD[@]+"${LOCKS_HELD[@]}"}; do _release_one "$d"; done
    LOCKS_HELD=()
}
trap release_locks EXIT

take_lock() {   # take_lock <dir> <what>
    local dir="$1" what="$2" waited=0 owner_before owner_now
    while ! mkdir "$dir" 2>/dev/null; do
        # Breaking a stale lock is itself a race, and the first version
        # lost it: two runs both judged the same directory stale, both
        # removed it, and both walked in. Two things make it safe. The
        # break is serialised by a second mkdir, so only one process
        # breaks; and the owner token is re-read *after* winning that,
        # so a lock that was recreated in the meantime is not the one
        # being judged. A fresh holder cannot appear without the
        # directory first going away, which is the property that closes
        # it: any change of identity shows up in the token.
        # `-prune` rather than `-maxdepth 0`: same "this path only, do
        # not descend", and it is POSIX where -maxdepth is an
        # extension. Whether a given BSD find implements -maxdepth is a
        # question this no longer has to have an opinion about.
        owner_before="$(cat "$dir/owner" 2>/dev/null || true)"
        if [ "${FIND_AGE_OK:-1}" -eq 1 ] \
           && [ -n "$(find "$dir" -prune -mmin +10 -print 2>/dev/null)" ] \
           && mkdir "$dir.break" 2>/dev/null; then
            owner_now="$(cat "$dir/owner" 2>/dev/null || true)"
            # An old lock with no owner file at all is the run that was
            # killed between `mkdir` and writing its token. Refusing to
            # break those - which the first version did, by requiring a
            # non-empty owner - meant the one crash the ten-minute
            # recovery exists for was the one it could not recover.
            if [ "$owner_now" = "$owner_before" ]; then
                say "  breaking a stale $what lock at $dir (owner ${owner_before:-none recorded})"
                rm -rf "$dir"
            fi
            rmdir "$dir.break" 2>/dev/null || true
        fi
        # Unconditionally, including after a break attempt: the first
        # version skipped both on the stale path, so a lock it could not
        # remove spun forever at full speed.
        waited=$((waited + 1))
        [ "$waited" -le 300 ] || die "timed out waiting for the $what lock at $dir
Another run is holding it, or it was left behind. Remove it by hand if
no other run is in flight:
    rm -rf $dir$([ "${FIND_AGE_OK:-1}" -eq 1 ] || printf '%s' "

This build of find rejects '-prune -mmin', so the stale-lock
takeover is disabled here and a lock left by a killed run will never be
reclaimed on its own.")"
        sleep 0.2
    done
    printf '%s\n' "$RUN_TOKEN" > "$dir/owner"
    LOCKS_HELD+=("$dir")
}

drop_lock() {   # drop_lock <dir>
    local dir="$1" d kept=()
    _release_one "$dir"
    for d in ${LOCKS_HELD[@]+"${LOCKS_HELD[@]}"}; do
        [ "$d" = "$dir" ] || kept+=("$d")
    done
    LOCKS_HELD=(${kept[@]+"${kept[@]}"})
}

while [ $# -gt 0 ]; do
    case "$1" in
        --task)          TASK="${2:-}"; shift 2 ;;
        --repo)          REPO="${2:-}"; shift 2 ;;
        --state)         STATE="${2:-}"; BOT_STATE_WAS_EXPLICIT=1; shift 2 ;;
        --worktree-root) WORKTREE_ROOT="${2:-}"; shift 2 ;;
        --dry-run)       DRY_RUN=1; shift ;;
        --skip-agent)    SKIP_AGENT=1; shift ;;
        --keep)          KEEP=1; shift ;;
        --reset)         RESET=1; shift ;;
        --allow-overlap) ALLOW_OVERLAP=1; shift ;;
        --chain)         CHAIN=1; shift ;;
        --no-rebase)     NO_REBASE=1; shift ;;
        -h|--help)       usage; exit 0 ;;
        *)               die "unknown argument: $1 (try --help)" ;;
    esac
done

[ -n "$TASK" ] || die "--task is required (try --help)"
[ -f "$TASK" ] || die "task file not found: $TASK"
TASK="$(cd "$(dirname "$TASK")" && pwd)/$(basename "$TASK")"

if [ -z "$REPO" ]; then
    REPO="$(git -C "$(dirname "$TASK")" rev-parse --show-toplevel)" \
        || die "could not resolve a repo from the task file; pass --repo"
fi
# --------------------------------------------------------------------
# The work surface
#
# Always a standalone repository with its own .git inside it, never a
# linked worktree. Two things forced that, and they turned out to be
# one thing:
#
#   - A linked worktree's git directory lives under the *main* repo, so
#     an agent that sandboxes itself by directory can edit every file it
#     was given and cannot write a commit. F-26.
#   - A project need not be a git repo at all. CodeScope opens plain
#     folders, and there is no worktree to add to a folder.
#
# Give the work surface its own .git and both answers are the same
# answer: the git directory sits inside the one directory the agent is
# allowed to write, and how that directory got filled - cloned from a
# repo, or imported from a folder - is a detail below this line.
#
# `is_work_tree` is the same question the product asks in
# core/src/git.rs before it shows any git UI.
# --------------------------------------------------------------------

SURFACE="clone"
if git -C "$REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    # Canonical, so that "the same project" is one string rather than
    # three spellings of one path. The state directory is keyed on it.
    REPO="$(git -C "$REPO" rev-parse --show-toplevel)"
else
    SURFACE="import"
    [ -d "$REPO" ] || die "not a directory, and not a git repo: $REPO"
    REPO="$(cd "$REPO" && pwd)"
fi

STATE="${STATE:-$LAB_DIR/.state}"
WORKTREE_ROOT="${WORKTREE_ROOT:-${REPO}.worktrees}"

# Absolute, both of them, and not as tidiness: the verifier runs after a
# `cd` into the verify checkout, so a relative --state would hand it a
# review path that resolves inside a throwaway worktree - every review
# task failing with "no review", and the build cache quietly created in
# there too.
abspath() {   # abspath <path> - works on a path that does not exist yet
    case "$1" in
        /*|[A-Za-z]:[\\/]*) printf '%s\n' "$1" ;;
        # Relative: anchor on $PWD and keep every component. Resolving
        # through `cd "$(dirname ...)"` dropped them - for `foo/bar`
        # whose parent does not exist yet the cd fails, the fallback
        # appends only `bar`, and the worktree root silently moves.
        *) printf '%s\n' "$PWD/$1" ;;
    esac
}
mkdir -p "$STATE"
STATE="$(cd "$STATE" && pwd)"
WORKTREE_ROOT="$(abspath "$WORKTREE_ROOT")"

# Nothing the runner writes may live inside a folder project. A git
# project is safe by accident - the control plane is either outside it
# or ignored by it - but an import walks the whole folder, so state
# under $REPO would put this run's logs, prompts, handoffs and the
# snapshot repository itself into the work surface, hand them to the
# agent, and then snapshot them again next time. It compounds, and the
# first sign of it is a surface that grows every run.
if [ "$SURFACE" = "import" ]; then
    for d in "$STATE" "$WORKTREE_ROOT"; do
        case "$d" in
            "$REPO"|"$REPO"/*) die \
"$d is inside $REPO, which is a plain folder.

A folder project is imported whole, so anything the runner keeps in
there becomes part of the snapshot and part of what the agent can read
- including the snapshot itself, on the next run. Put it somewhere
else:

    --state <a directory outside $REPO>
    --worktree-root <a directory outside $REPO>" ;;
        esac
    done
fi

# A control plane belongs to one repository. Live tasks and locks are
# keyed by task id alone, so pointing the default state at a second repo
# with --repo would have one run read, overwrite or block on the other's
# T-0001. Stamp it once and refuse to answer for anyone else.
#
# The identity is the *common git directory*, not the worktree root.
# Every linked worktree of one repository shares it, which is what makes
# the advice below - point every checkout at one --state - something
# this check permits rather than something it blocks. Using the
# top-level path would have rejected exactly the arrangement it tells
# you to use.
REPO_IDENTITY="$(git -C "$REPO" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || printf '%s' "$REPO")"
if [ -f "$STATE/REPO" ]; then
    STATE_REPO="$(cat "$STATE/REPO")"
    [ "$STATE_REPO" = "$REPO_IDENTITY" ] || die \
"this control plane belongs to another repository.

    state:     $STATE
    it is for: $STATE_REPO
    you asked: $REPO_IDENTITY

Task ids and locks are not namespaced by repo, so sharing one state
directory between two would have them claim each other's tasks. Give
this repo its own:

    --state <a directory for $REPO>

Linked worktrees of one repository are not two repositories, and do
not trip this: they share a common git directory, which is what is
compared here."
else
    printf '%s\n' "$REPO_IDENTITY" > "$STATE/REPO"
fi

# `find -prune -mmin` is the whole basis of every age check here -
# the stale-lock break, and the scheduler's recurrence. Both are BSD
# primitives as well as GNU ones, but "documented" and "present on the
# machine in front of you" are different claims, and the failure mode if
# they are absent is silence: the expression errors, the test reads
# false, and stale locks are simply never reclaimed. Ask once, out loud.
FIND_AGE_OK=1
find "$STATE" -prune -mmin +1 -print >/dev/null 2>&1 || FIND_AGE_OK=0

# The control plane is keyed to this checkout, and the conflicts it
# exists to prevent are keyed to the repository. For a single checkout
# those are the same thing. For a linked worktree they are not: two
# checkouts of one repo would take different dispatch locks and scan
# different task directories while creating branches in one shared
# object store, and both would pass the overlap check. Refuse the
# default there rather than coordinate something it cannot see.
if [ -z "${BOT_STATE_WAS_EXPLICIT:-}" ]; then
    GIT_DIR_HERE="$(git -C "$REPO" rev-parse --absolute-git-dir 2>/dev/null || true)"
    GIT_COMMON="$(git -C "$REPO" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    if [ -n "$GIT_DIR_HERE" ] && [ -n "$GIT_COMMON" ] && [ "$GIT_DIR_HERE" != "$GIT_COMMON" ]; then
        die "this is a linked worktree, and --state was not given.

The default control plane sits beside the script, so runs from two
checkouts of one repository would not see each other while sharing a
branch namespace - the overlap check would pass and both would
dispatch. Point every checkout at one state directory:

    --state <path shared by all checkouts of $REPO>"
    fi
fi


# --------------------------------------------------------------------
# Task frontmatter
#
# Deliberately a flat `key: value` block, not real YAML. A prototype
# that needs a YAML parser to read its own task files has already lost
# the plot; `touches` is comma-separated for the same reason.
# --------------------------------------------------------------------

# field_from <key> <text> - the actual parser. awk reads its input to
# the end rather than quitting on the first hit: under `set -o pipefail`
# an early exit SIGPIPEs the writer, and that 141 would take the runner
# down on a file whose only crime was being long.
field_from() {
    printf '%s\n' "$2" | awk -v key="$1" '
        /^---[[:space:]]*$/ { fence++; next }
        fence == 1 && !found && index($0, key ":") == 1 {
            value = substr($0, length(key) + 2)
            sub(/^[[:space:]]+/, "", value)
            sub(/[[:space:]]+$/, "", value)
            print value
            found = 1
        }
    '
}

# field <key> [file] - reads a frontmatter value off disk. Defaults to
# the task, but the same parser reads a charter and an agent profile,
# which is the point of keeping the format this dull.
field() {
    field_from "$1" "$(cat "${2:-$TASK}")"
}

# field_at_base <key> <contract-relative-path> - the same read, but out
# of the base commit. The agent sees the contract as it is *there*, so
# anything the runner decides from the contract has to be read from
# there too, or the two halves of one dispatch disagree. See F-12.
field_at_base() {
    field_from "$1" "$(git -C "$ORIGIN_REPO" show "$BASE_SHA:$CONTRACT_DIR/$2" 2>/dev/null || true)"
}

# base_has <contract-relative-path> - does the contract file exist at
# the base commit at all.
base_has() {
    git -C "$ORIGIN_REPO" cat-file -e "$BASE_SHA:$CONTRACT_DIR/$1" 2>/dev/null
}

TASK_ID="$(field id)"
TASK_TITLE="$(field title)"
TASK_OWNER="$(field owner)"
TASK_STATUS="$(field status)"
TASK_BASE="$(field base)"
TASK_BRANCH="$(field branch)"
TASK_TOUCHES="$(field touches)"
TASK_VERIFY="$(field verify)"

# What this task puts into the world, which is the axis the runner
# actually branches on. `commit` lands in the tree and gets the whole
# branch/verify/rebase machinery; `report` lands beside it - one file
# the runner harvests into the control plane, and a commit is a failed
# run. Everything else - surface, evidence, verifier, handoff - is
# identical, which is the claim the second value exists to test.
#
# It was `kind: change | review` until the roster stopped being all
# code writers. Same axis, named after the deliverable rather than
# after the one role that first had it: `review` was spelled into
# every branch in this file, so every future bot that reads instead of
# writing would have arrived as another exception to the writing one.
# See F-19 and F-32.
TASK_PRODUCES="$(field produces)"
TASK_PRODUCES="${TASK_PRODUCES:-commit}"
case "$TASK_PRODUCES" in
    commit|report) ;;
    *) die "produces '$TASK_PRODUCES' is not one of: commit, report" ;;
esac

# A report task names the file it leaves behind and the template that
# file has to match. Both default to the reviewer's, because that is
# the role that got here first - but neither is hardcoded any more,
# which is the point of the rename rather than a side effect of it.
TASK_ARTIFACT="$(field artifact)"
TASK_ARTIFACT="${TASK_ARTIFACT:-.bot-review.md}"
TASK_SHAPE="$(field shape)"
TASK_SHAPE="${TASK_SHAPE:-templates/REVIEW.md}"
if [ "$TASK_PRODUCES" = "report" ]; then
    # A plain filename in the surface root, inside the runner's own
    # `.bot-` namespace. That namespace is reserved for the channels
    # between agent and runner, so an artifact can never collide with a
    # file the project owns, and it is the one name a report bot is
    # allowed to create - anything else it writes shows up as a dirty
    # tree or a commit, both of which already fail the run.
    case "$TASK_ARTIFACT" in
        */*|*..*) die "artifact '$TASK_ARTIFACT' must be a plain filename in the surface root" ;;
    esac
    case "$TASK_ARTIFACT" in
        .bot-commit-msg) die "artifact '.bot-commit-msg' is the commit-message channel - pick another name" ;;
        .bot-blocked)    die "artifact '.bot-blocked' is the refusal channel - pick another name" ;;
        .bot-?*)         ;;
        *) die "artifact '$TASK_ARTIFACT' must start with '.bot-'" ;;
    esac
    case "$TASK_SHAPE" in
        templates/?*.md) ;;
        *) die "shape '$TASK_SHAPE' must name a template under the contract, e.g. templates/REVIEW.md" ;;
    esac
fi

# What a `changes-requested` verdict turns into. Both come from the
# task, which is contract - never from the review, which is a claim.
# "No verifier, no dispatch" applies one level up: a review task that
# cannot say how its follow-up would be proven does not get to produce
# one. See F-21.
TASK_ON_CHANGES="$(field on_changes_requested)"
TASK_DERIVED_VERIFY="$(field derived_verify)"

# Inherited by the derived task, one hop and no further. A task a human
# allowed to run unattended may produce a follow-up that runs
# unattended; a task a human starts by hand produces one that waits for
# a hand. The permission travels with the work rather than being
# re-decided by whichever bot happened to write the file.
TASK_SCHEDULE="$(field schedule)"
[ -z "$(field every)" ] || TASK_SCHEDULE="auto"
TASK_SCHEDULE="${TASK_SCHEDULE:-manual}"
case "$TASK_SCHEDULE" in
    auto|manual) ;;
    *) die "schedule '$TASK_SCHEDULE' is not one of: auto, manual" ;;
esac

for f in id owner base branch touches verify; do
    var="TASK_$(printf '%s' "$f" | tr '[:lower:]' '[:upper:]')"
    [ -n "${!var}" ] || die "task is missing required field: $f"
done

# Guard rails that are code, not prompt text.
case "$TASK_BRANCH" in
    main|master|HEAD) die "refusing to run on branch '$TASK_BRANCH'" ;;
esac

# Owner sanity. This name is spliced into filesystem paths below, so it
# has to be a plain identifier - not a traversal.
case "$TASK_OWNER" in
    *[!a-zA-Z0-9_-]*|"") die "owner must be [a-zA-Z0-9_-]+, got '$TASK_OWNER'" ;;
esac
case "$TASK_ID" in
    *[!a-zA-Z0-9_-]*|"") die "task id must be [a-zA-Z0-9_-]+, got '$TASK_ID'" ;;
esac

# Display only. The charter that governs the run is the one at the base
# commit, and that is the only one worth gating on - a second existence
# test against the runner's checkout would reject a perfectly good run
# launched from an older tree. See F-12.
BOT_DIR="$LAB_DIR/contract/bots/$TASK_OWNER"

# --------------------------------------------------------------------
# The origin repository
#
# What the surface is cloned from and pushed back to. For a git project
# that is the project. For a plain folder there is nothing to clone, so
# the folder is snapshotted into a bare repository under state first -
# and from that line down, every other line in this script is the same
# for both kinds.
#
# The snapshot is a commit, which makes it three things at once: the
# base the work is cut from, the tree the scope and overlap checks
# expand their globs against, and a restore point the folder did not
# have. That last one is not a side effect worth apologising for. A
# folder with no version control has no undo, and pointing an
# autonomous agent at one without giving it an undo would be the
# reckless part of this whole design.
# --------------------------------------------------------------------

IMPORT_LOG="$STATE/import.log"
IMPORT_REPORT=""

snapshot_folder() {   # snapshot the folder into $ORIGIN_REPO as refs/heads/folder
    local idx cidx tree ctree parent commit count
    local -a parentarg=()

    # Serialised, because refs/heads/folder is shared between every run
    # against this project and this function moves it. Two runners
    # reading the same parent and racing their update-ref would have one
    # snapshot silently replace the other, and the drift check downstream
    # would then be comparing against a tree nobody is standing on. Held
    # only across the snapshot itself - it is not the dispatch claim.
    take_lock "$STATE/snapshot.lock" "snapshot"

    [ -d "$ORIGIN_REPO" ] || git init --bare --quiet "$ORIGIN_REPO" \
        || die "could not create the snapshot repository at $ORIGIN_REPO"

    # Read before the index is built rather than after: it is both the
    # commit this snapshot descends from and the answer to "was this
    # protected path already in here", which strip_protected needs.
    parent="$(git --git-dir="$ORIGIN_REPO" rev-parse -q --verify refs/heads/folder 2>/dev/null || true)"

    # What never gets imported. A folder has no .gitignore discipline by
    # definition - that is most of what makes it a folder - so without
    # this the first snapshot of a node project is node_modules and the
    # first of a rust one is target/. The second half of the list is not
    # about size: an agent that cannot read a file cannot leak it, and a
    # directory that was never a repository has never had a reason to
    # keep a .env out of itself.
    #
    # It goes in the snapshot repo's info/exclude rather than in the
    # user's folder, because it is this runner's opinion and the folder
    # is not this runner's to write to.
    { cat <<'EXCLUDE'
.git/
node_modules/
target/
dist/
build/
out/
.venv/
venv/
__pycache__/
.next/
.nuxt/
.gradle/
vendor/
EXCLUDE
      printf '%s\n' "$PROTECTED_EXCLUDE_FILE"
    } > "$ORIGIN_REPO/info/exclude"

    mkdir -p "$STATE/tmp"
    idx="$STATE/tmp/import-$$.idx"
    cidx="$STATE/tmp/contract-$$.idx"
    rm -f "$idx" "$cidx"

    # The folder as it is now. A .gitignore inside it is honoured on top
    # of the list above: the folder's own opinion about what is not
    # source outranks a default written by a stranger.
    ( cd "$REPO" && GIT_INDEX_FILE="$idx" \
        git -c core.bare=false --git-dir="$ORIGIN_REPO" --work-tree="$REPO" \
            add -A -- . ) >>"$IMPORT_LOG" 2>&1 \
        || { rm -f "$idx"; die "could not import $REPO; see $IMPORT_LOG"; }

    # The contract travels with the snapshot. The rule that an agent
    # reads its charter from its own checkout does not get an exception
    # for folders - it gets a path.
    ( cd "$LAB_DIR/contract" && GIT_INDEX_FILE="$cidx" \
        git -c core.bare=false --git-dir="$ORIGIN_REPO" --work-tree="$LAB_DIR/contract" \
            add -A -- . ) >>"$IMPORT_LOG" 2>&1 \
        || { rm -f "$idx" "$cidx"; die "could not import the contract; see $IMPORT_LOG"; }
    GIT_INDEX_FILE="$idx" strip_protected "$parent" -c core.bare=false \
        --git-dir="$ORIGIN_REPO" --work-tree="$REPO"

    ctree="$(GIT_INDEX_FILE="$cidx" git --git-dir="$ORIGIN_REPO" write-tree)"
    rm -f "$cidx"
    GIT_INDEX_FILE="$idx" git -c core.bare=false --git-dir="$ORIGIN_REPO" \
        --work-tree="$REPO" read-tree --prefix="$CONTRACT_DIR/" "$ctree" >>"$IMPORT_LOG" 2>&1 \
        || { rm -f "$idx"; die "could not graft the contract in; see $IMPORT_LOG"; }

    tree="$(GIT_INDEX_FILE="$idx" git --git-dir="$ORIGIN_REPO" write-tree)"
    rm -f "$idx"

    if [ -n "$parent" ] \
       && [ "$(git --git-dir="$ORIGIN_REPO" rev-parse "$parent^{tree}")" = "$tree" ]; then
        # Nothing has changed. Reuse the commit rather than making an
        # identical one, so two runs against an untouched folder share
        # a base - which is what lets the overlap check tell that they
        # are talking about the same tree.
        IMPORT_REPORT="unchanged since ${parent:0:12}"
        drop_lock "$STATE/snapshot.lock"
        return 0
    fi

    [ -z "$parent" ] || parentarg=(-p "$parent")
    # An explicit identity. A snapshot signed as whoever happened to run
    # the bot would be a commit nobody made.
    commit="$(GIT_AUTHOR_NAME=bot-run GIT_AUTHOR_EMAIL=bot-run@invalid \
        GIT_COMMITTER_NAME=bot-run GIT_COMMITTER_EMAIL=bot-run@invalid \
        git --git-dir="$ORIGIN_REPO" commit-tree "$tree" \
            ${parentarg[@]+"${parentarg[@]}"} -m "snapshot of $REPO")" \
        || die "could not commit the snapshot; see $IMPORT_LOG"
    git --git-dir="$ORIGIN_REPO" update-ref refs/heads/folder "$commit" \
        || die "could not move refs/heads/folder; see $IMPORT_LOG"
    count="$(git --git-dir="$ORIGIN_REPO" ls-tree -r --name-only "$commit" | wc -l | tr -d ' ')"
    IMPORT_REPORT="$count file(s) at ${commit:0:12}${parent:+, was ${parent:0:12}}"
    drop_lock "$STATE/snapshot.lock"
    return 0
}

ORIGIN_REPO="$REPO"
if [ "$SURFACE" = "import" ]; then
    [ "$TASK_BASE" = "folder" ] || die \
"$REPO is a plain folder, so this task's base: must be the word 'folder'
and not '$TASK_BASE'. There is no ref to cut from; the runner snapshots
the folder and the work is cut from that."
    ORIGIN_REPO="$STATE/snapshot.git"
    # Not labs/agent-bots/contract: that path means something in this
    # repository and nothing in somebody's folder.
    CONTRACT_DIR=".bot-contract"
    snapshot_folder
fi

BASE_SHA="$(git -C "$ORIGIN_REPO" rev-parse --verify "$TASK_BASE^{commit}" 2>/dev/null)" \
    || die "cannot resolve base ref '$TASK_BASE' (fetch first?)"

# --------------------------------------------------------------------
# Agent profile
#
# Nothing here may assume Claude Code. CodeScope is CLI-agnostic and so
# is this: the invocation is data, held in contract/agents/<id>.agent.md.
# Codex alone proves why it has to be - its headless mode is a
# subcommand (`codex exec <prompt>`), not a `-p` flag, so there is no
# single argv shape to hard-code. See F-10.
#
# Precedence: the task may override, otherwise the charter decides.
# There is no built-in default; a bot says what it runs on.
# --------------------------------------------------------------------

missing_at_base() {
    die "contract file missing at base '$TASK_BASE': $CONTRACT_DIR/$1

The agent reads its contract from the base commit, not from this
checkout. Commit the contract, then point the task's 'base:' at a ref
that contains it."
}

# F-3: the agent reads the contract from *its* checkout, which is the
# base commit - never from the runner's working tree. A contract that
# is not on the base means the prompt points at files that do not
# exist. Fail here, before a worktree and an agent run are spent on it.
# Same shape as F-1: prove the precondition at base, not halfway.
for rel in "bots/$TASK_OWNER/BOT.md" \
           "context/CONVENTIONS.md" \
           "context/ARCHITECTURE.md" \
           "context/GLOSSARY.md"; do
    base_has "$rel" || missing_at_base "$rel"
done

# The charter is what a bot *is*; a task only says what this run of it
# does. A fixer handed a report task, or a reviewer handed a commit
# task, is a dispatch error - and not something to find out from the
# evidence afterwards, which is how it was found the first time: a
# chain run where the child inherited the parent's stub override and
# the fixer behaved like a reviewer.
#
# Silence is not a claim. A charter that does not declare `produces:`
# constrains nothing, exactly as an undeclared `agent:` falls through
# to the task's - and for the same reason: the contract is read at
# base, so reading an absent field as `commit` would refuse every
# report task in the world until a charter it does not own had been
# through a PR. Declaring it is what binds it.
CHARTER_PRODUCES="$(field_at_base produces "bots/$TASK_OWNER/BOT.md")"
if [ -n "$CHARTER_PRODUCES" ] && [ "$CHARTER_PRODUCES" != "$TASK_PRODUCES" ]; then
    die "this task produces '$TASK_PRODUCES' and bot '$TASK_OWNER' produces '$CHARTER_PRODUCES'.

The charter is the job description; a task does not get to change it.
Give this task to a bot that produces '$TASK_PRODUCES', or write one."
fi

# A report task is pointed at its shape template as well, so that is
# part of the task's contract and gets the same treatment.
if [ "$TASK_PRODUCES" = "report" ]; then
    base_has "$TASK_SHAPE" || missing_at_base "$TASK_SHAPE"

    # Checked now, not after the review is written: the moment to find
    # out that a handoff cannot be delivered is before the run that
    # produces it. Same shape as F-1 and F-3.
    if [ -n "$TASK_ON_CHANGES" ]; then
        case "$TASK_ON_CHANGES" in
            *[!a-zA-Z0-9_-]*) die "on_changes_requested must be [a-zA-Z0-9_-]+, got '$TASK_ON_CHANGES'" ;;
        esac
        base_has "bots/$TASK_ON_CHANGES/BOT.md" || missing_at_base "bots/$TASK_ON_CHANGES/BOT.md"
        [ -n "$TASK_DERIVED_VERIFY" ] || die \
"task hands 'changes-requested' to '$TASK_ON_CHANGES' but declares no derived_verify:

The derived task needs a verifier for the same reason this one does -
and it has to come from here, because the only other source is the
review, and a bot does not get to choose how its own follow-up is
judged. See F-21."
    fi
fi

TASK_AGENT="$(field agent)"
TASK_MODEL="$(field model)"
CHARTER_AGENT="$(field_at_base agent "bots/$TASK_OWNER/BOT.md")"
AGENT_ID="${TASK_AGENT:-$CHARTER_AGENT}"

[ -n "$AGENT_ID" ] || die "no agent declared - set 'agent:' on the task or in $BOT_DIR/BOT.md"
case "$AGENT_ID" in
    *[!a-zA-Z0-9_-]*) die "agent id must be [a-zA-Z0-9_-]+, got '$AGENT_ID'" ;;
esac

# F-12: read from the base commit, not from this checkout. Proving the
# profile *exists* at base and then reading argv out of the working
# tree pins half a contract: an uncommitted edit here, or a branch the
# runner happens to be sitting on, would send one CLI to a worktree
# built from another. The display name says which tree answered.
PROFILE_REL="agents/$AGENT_ID.agent.md"
base_has "$PROFILE_REL" || missing_at_base "$PROFILE_REL"
PROFILE="$TASK_BASE:$CONTRACT_DIR/$PROFILE_REL"

AGENT_CMD="$(field_at_base command "$PROFILE_REL")"
AGENT_HEADLESS="$(field_at_base headless "$PROFILE_REL")"
AGENT_AUTONOMY="$(field_at_base autonomy "$PROFILE_REL")"
AGENT_MODEL_FLAG="$(field_at_base model_flag "$PROFILE_REL")"
AGENT_INSTRUCTION_FILES="$(field_at_base instruction_files "$PROFILE_REL")"
AGENT_VERIFIED="$(field_at_base verified "$PROFILE_REL")"
AGENT_SHELL="$(field_at_base shell "$PROFILE_REL")"
AGENT_PROMPT_VIA="$(field_at_base prompt "$PROFILE_REL")"
AGENT_PROMPT_VIA="${AGENT_PROMPT_VIA:-argv}"
AGENT_SHELL="${AGENT_SHELL:-posix}"

[ -n "$AGENT_CMD" ] || die "profile '$AGENT_ID' declares no command"

# The stubs in run/stubs/ are injected here. BOT_AGENT_ARGS counts as
# set even when empty, which is how a stub gets invoked bare.
AGENT_OVERRIDDEN=0
if [ -n "${BOT_AGENT_CMD:-}" ]; then
    AGENT_CMD="$BOT_AGENT_CMD"; AGENT_OVERRIDDEN=1
fi
if [ "${BOT_AGENT_ARGS+set}" = set ]; then
    AGENT_HEADLESS="$BOT_AGENT_ARGS"; AGENT_AUTONOMY=""; AGENT_OVERRIDDEN=1
fi

if [ "$AGENT_OVERRIDDEN" -eq 0 ] && [ -z "$AGENT_HEADLESS" ]; then
    die "profile '$AGENT_ID' has an empty 'headless' template.

That marks an unverified stub, not a default. Fill in the invocation
against the real CLI and set 'verified' before dispatching with it -
a guessed flag fails inside the agent run, after a worktree has
already been spent on it."
fi

# `verified:` was printed in the plan and checked by nobody, while the
# error above tells profile authors to set it before dispatching. A
# field that documents a safety boundary and gates nothing is the
# boundary not existing. Same shape as F-18.
if [ "$AGENT_OVERRIDDEN" -eq 0 ] \
   && { [ -z "$AGENT_VERIFIED" ] || [ "$AGENT_VERIFIED" = "never" ]; }; then
    die "profile '$AGENT_ID' is marked verified: ${AGENT_VERIFIED:-(absent)}.

The invocation in a profile is a claim about a CLI's real flags, and an
unverified one fails inside the agent run - after a worktree has been
spent on it. Run it by hand once, then record the date you did."
fi

if [ -n "$TASK_MODEL" ] && [ -z "$AGENT_MODEL_FLAG" ]; then
    die "task pins model '$TASK_MODEL' but profile '$AGENT_ID' has no model_flag"
fi

# Which shell family the agent runs its own commands in. `posix` leaves
# the environment alone; `native` means this agent must not be handed a
# POSIX shell, and the runner takes them off its PATH. See F-27 - it is
# a per-OS fact about an agent, and the only reason it is a contract
# field rather than a detail is that the runner is the only thing that
# can act on it.
case "$AGENT_SHELL" in
    posix|native) ;;
    *) die "profile '$AGENT_ID' declares shell '$AGENT_SHELL'; expected posix or native" ;;
esac

# How the prompt reaches the agent. `argv` substitutes {prompt} as one
# argument; `stdin` feeds it on standard input and leaves argv short.
# Short matters: cmd.exe truncates a command line at 8191 characters,
# and a prompt carrying a whole task file goes past that - silently,
# taking the trailing flags with it. See F-27.
case "$AGENT_PROMPT_VIA" in
    argv|stdin) ;;
    *) die "profile '$AGENT_ID' declares prompt '$AGENT_PROMPT_VIA'; expected argv or stdin" ;;
esac
[ "$AGENT_OVERRIDDEN" -eq 0 ] || AGENT_PROMPT_VIA="argv"

# A stub is a bash script. Taking bash away from one would test the
# PATH surgery and nothing else.
[ "$AGENT_OVERRIDDEN" -eq 0 ] || AGENT_SHELL="posix"

IS_WINDOWS=0
case "$(uname -s 2>/dev/null || printf 'unknown')" in
    MINGW*|MSYS*|CYGWIN*) IS_WINDOWS=1 ;;
esac

WT_LEAF="$(printf '%s' "$TASK_BRANCH" | tr '/' '-')"
WT="$WORKTREE_ROOT/$WT_LEAF"
# Throwaway second checkout, detached at the branch tip, used only to
# run the verifier. See the Verifier section for why it exists.
VERIFY_WT="$WT-verify"

# --------------------------------------------------------------------
# Live task
#
# The repo copy is a *definition*; this is the live task. Status is read
# from here when it exists, so a re-run cannot be waved through by a
# definition that still says todo (which is what the first version did -
# it read the definition and then overwrote the live copy on top).
# --------------------------------------------------------------------

LIVE_TASK="$STATE/tasks/$TASK_ID.md"

# The discard itself happens inside the dispatch lock, once the
# worktree exists. Deleting it here instead would hand the preflight a
# way to abandon a live worktree: the runner dies on "worktree path
# already exists", and the control plane has already forgotten the run
# that owns it. --dry-run then needs no special case, because nothing
# has been written yet either way.
RESET_PENDING=0
if [ "$RESET" -eq 1 ] && [ -f "$LIVE_TASK" ]; then
    RESET_PENDING=1
    if [ "$DRY_RUN" -eq 1 ]; then
        say "reset: would discard live task $TASK_ID (dry run - left alone)"
    else
        say "reset: live task $TASK_ID will be discarded once the worktree exists"
    fi
fi

EFFECTIVE_STATUS="$TASK_STATUS"
if [ -f "$LIVE_TASK" ] && [ "$RESET_PENDING" -eq 0 ]; then
    EFFECTIVE_STATUS="$(sed -n 's/^status:[[:space:]]*//p' "$LIVE_TASK" | head -n1)"
fi

case "$EFFECTIVE_STATUS" in
    todo) ;;
    dispatched)
        die "task $TASK_ID is already in flight (live status: dispatched).
A previous run died before writing a handoff. Inspect $WT, then re-run
with --reset once the worktree and branch are gone." ;;
    *)
        die "task $TASK_ID is '$EFFECTIVE_STATUS', expected 'todo'.
Re-run it with --reset to start over." ;;
esac

# --------------------------------------------------------------------
# Overlap
#
# Two bots editing one file on separate branches do not conflict while
# they work. They conflict at merge - after both runs have reported
# success, and after both handoffs have gone out saying so. The only
# cheap place to catch that is before the second dispatch.
#
# Overlap is decided by expanding both tasks' globs against the base
# tree and intersecting the results, not by comparing the patterns:
# whether two arbitrary shell globs *can* match a common string has no
# cheap honest answer, and the tree is right there. F-16 covers what
# that misses.
# --------------------------------------------------------------------

# split_globs <comma-separated> - fills GLOBS_OUT, trimmed, no blanks.
GLOBS_OUT=()
split_globs() {
    local raw=() g
    GLOBS_OUT=()
    IFS=',' read -ra raw <<< "$1"
    for g in ${raw[@]+"${raw[@]}"}; do
        g="${g#"${g%%[![:space:]]*}"}"
        g="${g%"${g##*[![:space:]]}"}"
        [ -n "$g" ] && GLOBS_OUT+=("$g")
    done
}

# expand_globs <comma-separated> [sha] - the paths those globs match in
# a tree. The sha matters: expanding another task's globs against *this*
# task's base answers a question nobody asked. A file that exists only
# in the other task's base does not expand, and a wildcard that would
# have collided with it passes.
expand_globs() {
    local file g files
    local -a globs
    split_globs "$1"
    globs=(${GLOBS_OUT[@]+"${GLOBS_OUT[@]}"})
    [ "${#globs[@]}" -gt 0 ] || return 0
    files="$(git -C "$ORIGIN_REPO" ls-tree -r --name-only "${2:-$BASE_SHA}" 2>/dev/null || true)"
    [ -n "$files" ] || return 0
    while IFS= read -r file; do
        for g in ${globs[@]+"${globs[@]}"}; do
            # shellcheck disable=SC2254  # a glob, on purpose
            case "$file" in $g) printf '%s\n' "$file"; break ;; esac
        done
    done <<< "$files"
}

split_globs "$TASK_TOUCHES"
MY_GLOBS=(${GLOBS_OUT[@]+"${GLOBS_OUT[@]}"})

# `touches: ,` is non-empty as a string and empty as a scope. Left
# alone it means "no glob matches anything", which the scope check
# reads as "every file is a violation" and review-shape.sh reads as
# "skip the scope loop entirely" - opposite answers to the same typo.
[ "${#MY_GLOBS[@]}" -gt 0 ] \
    || die "touches: '$TASK_TOUCHES' normalises to no globs at all"

MY_FILES="$(expand_globs "$TASK_TOUCHES" | sort -u)"

OVERLAP_REPORT=""
OVERLAP_IDS=""
STALE_DISPATCH=""
BRANCH_TAKEN=""

# A function because it has to run twice: once before the plan, so a
# human (and --dry-run) can see the collision, and once again inside the
# dispatch lock, where it is the answer that actually counts. See F-17.
scan_overlaps() {
OVERLAP_REPORT=""
OVERLAP_IDS=""
STALE_DISPATCH=""
BRANCH_TAKEN=""
if [ -d "$STATE/tasks" ]; then
    for live in "$STATE"/tasks/*.md; do
        [ -f "$live" ] || continue

        other_id="$(field id "$live")"
        [ -n "$other_id" ] || continue
        [ "$other_id" != "$TASK_ID" ] || continue
        [ "$(field status "$live")" = "dispatched" ] || continue

        # Before the `produces:` skip, because a report gets a branch
        # too. The origin-ref check at dispatch cannot see this one: a
        # standalone clone creates refs/heads/<branch> in the *clone*,
        # and the project only learns the name exists at the push right
        # at the end. So two task ids naming one branch both pass, both
        # do all their work, and the loser finds out as a push failure.
        # `touches:` does not catch it either - the collision is in the
        # name, not in the files.
        [ "$(field branch "$live")" != "$TASK_BRANCH" ] \
            || BRANCH_TAKEN="$other_id"

        # A report claims nothing: it cannot conflict at merge, which
        # is the only reason this check exists.
        [ "$(field produces "$live")" != "report" ] || continue

        other_branch="$(field branch "$live")"
        # The path that run recorded for itself, not one recomputed from
        # this run's --worktree-root. The fallback covers a live task
        # written before that field existed.
        other_wt="$(field worktree "$live")"
        other_wt="${other_wt:-$WORKTREE_ROOT/$(printf '%s' "$other_branch" | tr '/' '-')}"
        if [ ! -d "$other_wt" ]; then
            # Status says in flight; the filesystem says the run is
            # over. A dispatch that died is not holding anything, and
            # blocking every later task on a ghost would be worse than
            # saying so out loud.
            STALE_DISPATCH="$STALE_DISPATCH $other_id"
            continue
        fi

        other_touches="$(field touches "$live")"
        # Against that task's own base, recorded at its dispatch. Two
        # tasks that branched from different commits see different
        # trees, and the file that collides may exist in only one.
        other_base="$(field base_sha "$live")"
        other_files="$(expand_globs "$other_touches" "${other_base:-$BASE_SHA}" | sort -u)"

        shared=""
        if [ -n "$MY_FILES" ] && [ -n "$other_files" ]; then
            shared="$(comm -12 <(printf '%s\n' "$MY_FILES") \
                              <(printf '%s\n' "$other_files") || true)"
        fi

        # A glob naming a path that does not exist at base expands to
        # nothing, so the patterns get compared too: two tasks that both
        # declare `core/src/new_thing.rs` collide just as hard over a
        # file neither has created yet.
        split_globs "$other_touches"
        for g in ${GLOBS_OUT[@]+"${GLOBS_OUT[@]}"}; do
            for mine in ${MY_GLOBS[@]+"${MY_GLOBS[@]}"}; do
                if [ "$g" = "$mine" ]; then
                    shared="$shared"$'\n'"$g  (declared by both, absent at base)"
                fi
            done
        done

        shared="$(printf '%s\n' "$shared" | sed '/^[[:space:]]*$/d' | sort -u)"
        [ -n "$shared" ] || continue

        OVERLAP_IDS="$OVERLAP_IDS $other_id"
        OVERLAP_REPORT="$OVERLAP_REPORT  $other_id on $other_branch:"$'\n'
        OVERLAP_REPORT="$OVERLAP_REPORT$(printf '%s\n' "$shared" | sed 's/^/      /')"$'\n'
    done
fi
}

scan_overlaps

if [ -n "$OVERLAP_IDS" ]; then
    if [ "$TASK_PRODUCES" = "report" ]; then
        OVERLAP_SUMMARY="${OVERLAP_IDS# } - a report writes nothing into the tree, so it proceeds"
    elif [ "$ALLOW_OVERLAP" -eq 1 ]; then
        OVERLAP_SUMMARY="${OVERLAP_IDS# } - allowed by --allow-overlap"
    else
        OVERLAP_SUMMARY="${OVERLAP_IDS# } - dispatch will be refused"
    fi
else
    OVERLAP_SUMMARY="none"
fi

# --------------------------------------------------------------------
# The prompt
#
# Deterministic and short. Everything durable lives in the contract
# files; repeating it here would just give the two copies a chance to
# disagree. The agent is told to commit and stop - it does not write
# the handoff, because self-reported evidence is not evidence.
# --------------------------------------------------------------------

PROMPT="You are the bot '$TASK_OWNER' working in a git worktree.

Read these first, in order:
  1. $CONTRACT_DIR/bots/$TASK_OWNER/BOT.md - your charter
  2. $CONTRACT_DIR/context/CONVENTIONS.md - hard rules
  3. $CONTRACT_DIR/context/ARCHITECTURE.md
  4. $CONTRACT_DIR/context/GLOSSARY.md

Your task, verbatim:
---
$(cat "$TASK")
---

$(if [ "$TASK_PRODUCES" = "report" ]; then cat <<REPORT_RULES
Rules for this run:
  - You are reporting, not changing. Commit NOTHING.
  - Read only. The files in scope are: $TASK_TOUCHES
  - Write exactly one file: '$TASK_ARTIFACT' in the worktree root,
    in the shape of $CONTRACT_DIR/$TASK_SHAPE. That template is the
    contract for what you produce - follow it, frontmatter included.
  - The frontmatter names this task ($TASK_ID) and the commit the
    report is about, which is $BASE_SHA - the one you are on.
  - Every claim must cite a real path:line inside $TASK_TOUCHES
    that exists at that commit, and quote what is on that line. A
    made-up path fails the run, and so does a quote that is not
    there - a citation proves a file was opened, a quote proves the
    line was read.
  - Having nothing to report is a complete report. Do not pad.
  - Leave nothing else behind: no scratch files, no notes, no commit.
  - Do NOT push, do NOT open a PR, do NOT edit anything under
    $CONTRACT_DIR/.
  - Do NOT write a handoff. The runner does that.
REPORT_RULES
else cat <<CHANGE_RULES
Rules for this run:
  - You are already on branch '$TASK_BRANCH'. Do not switch branches.
  - Edit only files matching: $TASK_TOUCHES
  - The verifier is: $TASK_VERIFY
    It must exit 0.$(if [ "$AGENT_SHELL" = "native" ]; then printf ' It is written for a POSIX shell and you
    may not have one; the runner runs it after you stop, in a clean
    checkout of your commit, and that result is the one that counts.
    Run it yourself only if your shell can.'; else printf ' Run it before you edit anything, and again
    after.'; fi)
  - Commit your work to this branch. Exactly one commit, message in English.
  - If nothing needs changing, commit nothing and say so. A no-op is a
    result, not a failure - do not manufacture a commit to have one.
  - If you cannot commit - some sandboxes will not let you write to
    .git at all - then leave your changes in the tree and write the
    commit message you would have used to '.bot-commit-msg' in the
    worktree root. The runner commits what you leave behind, and that
    file is the only place your reasoning survives.
  - No new TODO or FIXME without a linked issue number.
  - Do NOT push, do NOT open a PR, do NOT run cargo fmt.
  - Do NOT edit anything under $CONTRACT_DIR/.
  - Do NOT write a handoff or a report file. The runner does that.
CHANGE_RULES
fi)

If you cannot complete the task inside those limits, write one line
saying why to the file '.bot-blocked' in the worktree root, make no
other changes, and stop. That is your only channel back: the runner
reads it, and nothing else you say reaches the handoff. Stopping is a
valid outcome; guessing is not."

# --------------------------------------------------------------------
# Resolved argv
#
# `{prompt}` is substituted as a single argument wherever the profile
# puts it - leading for Claude Code and Gemini, trailing after a
# subcommand for Codex. Everything else is word-split, which is what
# lets a profile carry an opaque fragment like `-s workspace-write`
# without the runner knowing what it means.
#
# `{git_dir}` is the second substitution, and it exists because of what
# a linked worktree is: the checkout is at $WT, and its git directory is
# not - it lives under the main repo. An agent that sandboxes itself by
# directory can therefore edit every file it was given and cannot write
# a single commit. See F-26. A profile that needs to hand its sandbox
# that path says so here, and only the runner knows what the path is.
#
# The display copy exists so the plan and the evidence can name the
# invocation without reprinting the whole prompt.
# --------------------------------------------------------------------

# The surface is a standalone repository, so its git directory is
# inside the one directory the agent is allowed to write. The
# substitution is kept because a profile may still have to name it - a
# sandbox that grants the workspace does not necessarily grant the .git
# inside it, which is exactly what Codex does. See F-28.
GIT_COMMON_DIR="$WT/.git"

case "$AGENT_HEADLESS" in
    *"{prompt}"*)
        [ "$AGENT_PROMPT_VIA" = "argv" ] || die \
"profile '$AGENT_ID' says prompt: stdin and still has {prompt} in its
headless template. Those are two different places to put the same
thing; pick one."
        ;;
    *)
        [ "$AGENT_PROMPT_VIA" = "stdin" ] || [ "$AGENT_OVERRIDDEN" -eq 1 ] || die \
"profile '$AGENT_ID' has no {prompt} in its headless template and does
not say prompt: stdin, so the agent would be started with no task."
        ;;
esac

AGENT_ARGV=()
AGENT_ARGV_DISPLAY=()
# Globbing off for both loops. These are argv fragments, not patterns:
# a profile carrying `-c roots=["..."]` would otherwise be read as a
# character class and matched against the runner's working directory,
# and the failure would be an agent invoked with a mangled flag.
set -f
for tok in $AGENT_HEADLESS; do
    if [ "$tok" = "{prompt}" ]; then
        AGENT_ARGV+=("$PROMPT"); AGENT_ARGV_DISPLAY+=("<prompt>")
    else
        tok="${tok//\{git_dir\}/$GIT_COMMON_DIR}"
        AGENT_ARGV+=("$tok"); AGENT_ARGV_DISPLAY+=("$tok")
    fi
done
for tok in $AGENT_AUTONOMY; do
    tok="${tok//\{git_dir\}/$GIT_COMMON_DIR}"
    AGENT_ARGV+=("$tok"); AGENT_ARGV_DISPLAY+=("$tok")
done
set +f
if [ -n "$TASK_MODEL" ]; then
    AGENT_ARGV+=("$AGENT_MODEL_FLAG" "$TASK_MODEL")
    AGENT_ARGV_DISPLAY+=("$AGENT_MODEL_FLAG" "$TASK_MODEL")
fi
AGENT_INVOCATION="$AGENT_CMD ${AGENT_ARGV_DISPLAY[*]}"

# --------------------------------------------------------------------
# The agent's PATH
#
# Only ever different from the runner's when a profile says `shell:
# native`, and then only on Windows. Codex is the case: it picks the
# shell it runs commands in by looking at PATH, finds Git Bash, and Git
# Bash dies inside Codex's own Windows sandbox - MSYS fork emulation
# needs shared memory a restricted token denies. Off the PATH, it picks
# PowerShell and works. See F-27.
#
# The rule is the intent rather than a list of directory names: drop
# every entry that carries a POSIX shell. A hardcoded `/usr/bin` would
# be a guess about somebody else's install.
# --------------------------------------------------------------------

AGENT_PATH="$PATH"
AGENT_SHELL_NOTE="posix (runner's own PATH)"
if [ "$AGENT_SHELL" = "native" ] && [ "$IS_WINDOWS" -eq 1 ]; then
    AGENT_PATH=""
    IFS=':' read -ra PATH_PARTS <<< "$PATH"
    for p in ${PATH_PARTS[@]+"${PATH_PARTS[@]}"}; do
        [ -n "$p" ] || continue
        if [ -e "$p/bash.exe" ] || [ -e "$p/sh.exe" ]; then continue; fi
        AGENT_PATH="${AGENT_PATH:+$AGENT_PATH:}$p"
    done

    # An npm launcher like `codex` is itself an sh script: it dies
    # calling `sed` before the agent has started, which looks exactly
    # like the agent failing. So the command is resolved against the
    # runner's PATH - the one that still has a shell on it - and a
    # Windows executable sibling wins over a POSIX script.
    AGENT_RESOLVED="$(command -v "$AGENT_CMD" 2>/dev/null || true)"
    if [ -n "$AGENT_RESOLVED" ]; then
        case "$AGENT_RESOLVED" in
            *.exe|*.cmd|*.bat|*.com) AGENT_CMD="$AGENT_RESOLVED" ;;
            *)
                for ext in .cmd .exe .bat; do
                    if [ -f "$AGENT_RESOLVED$ext" ]; then
                        AGENT_CMD="$AGENT_RESOLVED$ext"
                        break
                    fi
                done
                ;;
        esac
    fi
    AGENT_INVOCATION="$AGENT_CMD ${AGENT_ARGV_DISPLAY[*]}"
    AGENT_SHELL_NOTE="native (no POSIX shell on the agent's PATH)"
fi

# --------------------------------------------------------------------
# Plan
# --------------------------------------------------------------------

step "Plan"
# The base is read twice - once here to cut the branch from, once at
# the end to land it on - so the plan says which of the two this is.
BASE_PLAN="$TASK_BASE @ ${BASE_SHA:0:12}, re-read after the run"
[ "$NO_REBASE" -eq 0 ] || BASE_PLAN="$TASK_BASE @ ${BASE_SHA:0:12} (fixed: --no-rebase)"
PRODUCES_PLAN="$TASK_PRODUCES"
[ "$TASK_PRODUCES" != "report" ] \
    || PRODUCES_PLAN="$TASK_PRODUCES  $TASK_ARTIFACT, shaped by $TASK_SHAPE"
cat <<PLAN
  task      $TASK_ID  $TASK_TITLE
  produces  $PRODUCES_PLAN
  owner     $TASK_OWNER
  repo      $REPO
  base      $BASE_PLAN
  branch    $TASK_BRANCH
  surface   $WT ($SURFACE${IMPORT_REPORT:+ - $IMPORT_REPORT})
  origin    $ORIGIN_REPO
  touches   $TASK_TOUCHES${BRANCH_TAKEN:+
  conflict  $BRANCH_TAKEN already claims branch $TASK_BRANCH}
  overlap   $OVERLAP_SUMMARY
  verify    $TASK_VERIFY
  state     $STATE
  agent     $AGENT_ID, profile verified $AGENT_VERIFIED
  profile   $PROFILE
  argv      $AGENT_INVOCATION
  reads     ${AGENT_INSTRUCTION_FILES:-(none declared)}
  shell     $AGENT_SHELL_NOTE
PLAN

if [ -n "$STALE_DISPATCH" ]; then
    say "  stale     dispatched with no worktree, ignored:$STALE_DISPATCH"
fi

if [ -n "$OVERLAP_REPORT" ]; then
    say ""
    say "  overlapping paths:"
    printf '%s' "$OVERLAP_REPORT"
fi

if [ "$DRY_RUN" -eq 1 ]; then
    step "Prompt (dry run - nothing was changed)"
    printf '%s\n' "$PROMPT"
    exit 0
fi

# --------------------------------------------------------------------
# Control plane
#
# The task file in the repo is a *definition*; the copy under state is
# the *live task*, and its status field has exactly one writer - this
# run. That split is what keeps mutable state out of git.
# --------------------------------------------------------------------

# Two clocks on purpose: TS is filesystem-safe and goes in file names,
# TS_ISO is real ISO 8601 and goes in file contents.
TS="$(date -u +%Y-%m-%dT%H-%M-%SZ)"
TS_ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
DAY="${TS%%T*}"

mkdir -p "$STATE/tasks" "$STATE/handoffs" "$STATE/runs/$DAY" "$STATE/bots/$TASK_OWNER"

RUN_LOG="$STATE/runs/$DAY/$TASK_ID-$TS.log"
BOARD="$STATE/board.md"

# F-14: "append-only" is a rule about intent, not a guarantee about
# concurrency. Two runs starting together both saw no board and both
# truncated it with `>`, so the later header wiped the earlier run's
# rows - an audit trail that loses evidence exactly when two bots are
# running, which is the only time it matters.
#
# Two things are needed, not one. The lock serialises creation; the
# write-then-rename makes the file's *appearance* atomic, because `>`
# creates an empty file before the header lands in it and a waiter that
# only checks `-f` would start appending rows into a half-written
# header.
if [ ! -f "$BOARD" ]; then
    take_lock "$BOARD.lock" "board"
    if [ ! -f "$BOARD" ]; then
        {
            echo "# Board"
            echo
            echo "Append-only event log. Never edit a line that is already here -"
            echo "task status lives on the task file, this is the audit trail."
            echo
            echo "| when | task | event | ref |"
            echo "|---|---|---|---|"
        } > "$BOARD.tmp.$$"
        mv "$BOARD.tmp.$$" "$BOARD"
    fi
    drop_lock "$BOARD.lock"
fi

# Each row carries its own clock. Stamping every row of a run with the
# run's start time made the log unsortable the moment two runs overlap.
#
# The append itself needs no lock: one short line through `>>` is an
# O_APPEND write well under PIPE_BUF, so concurrent rows interleave but
# never tear. Only creating the file was ever the race.
board() {
    printf '| %s | %s | %s | %s |\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$TASK_ID" "$1" "${2:--}" >> "$BOARD"
}

# Rewrite-and-rename, not `sed -i`. GNU sed takes `-i` with no argument
# and understands the `0,/re/` address; BSD sed - macOS, which this
# project ships - does neither, and would fail here *after* the handoff
# was written, leaving the live task stuck on `dispatched` forever.
set_field() {   # set_field <key> <value> - replace, or insert before the closing fence
    awk -v k="$1" -v v="$2" '
        /^---[[:space:]]*$/ {
            fence++
            if (fence == 2 && !done) { print k ": " v; done = 1 }
            print; next
        }
        fence == 1 && !done && index($0, k ":") == 1 { print k ": " v; done = 1; next }
        { print }
    ' "$LIVE_TASK" > "$LIVE_TASK.tmp" && mv "$LIVE_TASK.tmp" "$LIVE_TASK"
}

set_status() { set_field status "$1"; }

# --------------------------------------------------------------------
# Claim
#
# F-17: everything above this line is an observation. Two runners could
# both read "no in-flight task claims these files", both find the path
# free, and both dispatch - the overlap check would be advice that
# happened to be true when it was read.
#
# So the claim is serialised: one lock, held from the last look at the
# state to the moment this run is visible in it. Inside it the status
# and the overlap scan are re-read, because the first pass ran outside
# the lock and is only good enough to print.
#
# The refusal lands on the board before the die - a dispatch that was
# refused is the record of two tasks written to collide, and that is
# worth keeping.
# --------------------------------------------------------------------

take_lock "$STATE/dispatch.lock" "dispatch"

if [ -f "$LIVE_TASK" ] && [ "$RESET_PENDING" -eq 0 ]; then
    LOCKED_STATUS="$(field status "$LIVE_TASK")"
    [ "$LOCKED_STATUS" = "todo" ] || die \
        "task $TASK_ID became '$LOCKED_STATUS' while this run was starting.
Another runner claimed it first."
fi

scan_overlaps

if [ -n "$BRANCH_TAKEN" ]; then
    board "dispatch-refused" "branch $TASK_BRANCH claimed by $BRANCH_TAKEN"
    refuse "task $BRANCH_TAKEN is in flight on branch '$TASK_BRANCH', which this
task also declares.

One branch cannot hold two tasks' work. The project's ref is free right
now - a surface is a standalone clone, so the branch only appears in
$ORIGIN_REPO at the push - which is why this is checked here and not
against git: both runs would pass that check, both would do all their
work, and the second would find out as a failed push.

Give one of them a branch of its own, or wait for $BRANCH_TAKEN."
fi

if [ -n "$OVERLAP_IDS" ]; then
    if [ "$TASK_PRODUCES" = "report" ]; then
        # Not a refusal, and not nothing either: the report is about the
        # base commit, and someone is editing those files right now, so
        # it will be describing a tree that has already moved.
        board "report-on-moving-target" "${OVERLAP_IDS# }"
        say "  note: ${OVERLAP_IDS# } is editing files in scope - this report describes ${BASE_SHA:0:12}, not their branches"
    elif [ "$ALLOW_OVERLAP" -eq 1 ]; then
        board "overlap-allowed" "${OVERLAP_IDS# }"
    else
        board "dispatch-refused" "overlaps ${OVERLAP_IDS# }"
        refuse "another in-flight task already claims files this one touches.

$OVERLAP_REPORT
Two branches editing one file do not conflict now; they conflict at
merge, once both runs have reported success and both handoffs have gone
out saying so. Wait for the other task to land, narrow one of the two
'touches:' lists, or pass --allow-overlap if this is a collision you
mean to resolve by hand."
    fi
fi

# --------------------------------------------------------------------
# Worktree
# --------------------------------------------------------------------

step "Worktree"
if [ -e "$WT" ]; then
    die "worktree path already exists: $WT

Clear both halves before re-running - removing only the directory
leaves the branch behind, and 'worktree add -b' then fails too:
    git -C $REPO worktree remove --force $WT
    git -C $REPO branch -D $TASK_BRANCH"
fi
if [ -e "$VERIFY_WT" ]; then
    die "verify checkout left over from an earlier run: $VERIFY_WT

    rm -rf $VERIFY_WT"
fi
if git -C "$ORIGIN_REPO" rev-parse --verify --quiet "refs/heads/$TASK_BRANCH" >/dev/null 2>&1; then
    # The work happens on a clone and is pushed back here at the end.
    # A branch of this name already in the project is either an earlier
    # run nobody cleaned up or somebody's work, and the push would be
    # the thing that told you.
    die "branch '$TASK_BRANCH' already exists in $ORIGIN_REPO

That is where this run's result gets pushed, so it has to be free:
    git -C $ORIGIN_REPO branch -D $TASK_BRANCH"
fi

mkdir -p "$WORKTREE_ROOT"
# --shared keeps the objects in the project's store and reads them
# through alternates, so a clone per task costs a checkout and not a
# copy of the history - the same price a worktree charged. --no-checkout
# because the branch to check out does not exist yet, and checking out
# the default branch first would be a second full materialisation.
#
# Alternates are live, not a snapshot: a commit that lands in the
# project after this clone is still readable here, which is what lets
# the rebase step replay onto a base that moved.
SURFACE_OK=0
if git clone --shared --no-checkout --quiet "$ORIGIN_REPO" "$WT" >>"$RUN_LOG" 2>&1; then
    git -C "$WT" checkout --quiet -b "$TASK_BRANCH" "$BASE_SHA" >>"$RUN_LOG" 2>&1 \
        && SURFACE_OK=1
fi
if [ "$SURFACE_OK" -eq 0 ]; then
    # Only if the clone actually got as far as making one. The
    # existing-path guard ran earlier, so between then and here a
    # directory could have appeared that is not ours - and an `rm -rf`
    # on a computed path is not the place to assume otherwise.
    if [ -e "$WT/.git" ]; then
        rm -rf "$WT"
    elif [ -d "$WT" ]; then
        rmdir "$WT" 2>/dev/null || say "  left $WT alone - it is not a clone this run made"
    fi
    board "dispatch-failed" "$TASK_BRANCH"
    die "could not create the work surface at $WT; see $RUN_LOG"
fi

# Proof that this directory is ours before anything ever removes it.
# Cleanup is `rm -rf` now rather than `git worktree remove`, and an
# `rm -rf` that trusts a computed path is one bad variable away from
# eating something else. See F-24 - a harness that can destroy your
# work is worse than no harness.
printf 'bot-run surface for %s\n' "$TASK_ID" > "$WT/.git/bot-surface"

# --shared means the base objects are read out of $ORIGIN_REPO through
# alternates rather than copied here. A surface kept as evidence - a
# conflict, a red re-verify, a push that failed - therefore depends on
# the origin still being able to reach them, and the base ref is free to
# move, be deleted or be force-pushed while it waits. Then the evidence
# is a directory git cannot read. Pin the commit for as long as the
# surface exists; drop_surface drops it.
git -C "$ORIGIN_REPO" update-ref "refs/bot-base/$TASK_ID" "$BASE_SHA" >>"$RUN_LOG" 2>&1 \
    || say "  note: could not pin $BASE_SHA in $ORIGIN_REPO - see $RUN_LOG"
printf '%s\n' "$PROTECTED_EXCLUDE_FILE" >> "$WT/.git/info/exclude"

# Whose commit this is, set on the surface rather than on the commit, so
# that it holds whoever ends up making it. Codex committed its own work
# here and signed it as the human whose git config it inherited - a bot
# commit attributed to a person, which is F-17's ambient identity in the
# one place it actually matters. The runner adds itself as committer
# when the runner commits; git's own author/committer split says the
# rest.
git -C "$WT" config user.name "$TASK_OWNER (via $AGENT_ID)" >>"$RUN_LOG" 2>&1 || true
git -C "$WT" config user.email "$TASK_OWNER@bots.invalid" >>"$RUN_LOG" 2>&1 || true

say "  created $WT (clone of $ORIGIN_REPO)"

# Only now, with a worktree that actually exists, does the live task
# come into being. A dispatch that fails must not leave one behind.
# From here to `board "dispatched"` the worktree exists and the control
# plane does not know about it yet. A failure in between - a full disk,
# a permission - would leave a branch and a worktree that no live task
# claims, and the next attempt would stop at the existing-path guard
# needing hands. Undo the half-dispatch instead.
# drop_surface - remove a work surface, and refuse to remove anything
# else. The marker written at creation is the whole check: a path this
# script computed is not by itself a reason to delete a directory tree.
drop_surface() {
    # Two separate paths, two separate proofs. $VERIFY_WT is a sibling
    # directory name this script computed, and a linked worktree always
    # has a `.git` *file* pointing back at its parent - so that file
    # both identifies it and says whose it is.
    if [ -e "$VERIFY_WT" ]; then
        if [ -f "$VERIFY_WT/.git" ] \
           && grep -q "$(printf '%s' "$WT_LEAF")" "$VERIFY_WT/.git" 2>/dev/null; then
            rm -rf "$VERIFY_WT"
        else
            say "  refusing to remove $VERIFY_WT - not a verify checkout this run made"
            return 1
        fi
    fi
    [ -f "$WT/.git/bot-surface" ] || { say "  refusing to remove $WT - not a surface this run made"; return 1; }
    rm -rf "$WT"
    [ ! -e "$WT" ] || return 1
    # Nothing borrows the base objects any more. Last, so that a removal
    # that refused above still leaves the pin in place.
    git -C "$ORIGIN_REPO" update-ref -d "refs/bot-base/$TASK_ID" >/dev/null 2>&1 || true
    return 0
}

rollback_dispatch() {
    local rc=$?
    [ "$rc" -eq 0 ] && return 0
    say ""
    say "dispatch failed after the work surface was created - rolling it back"
    rm -f "$LIVE_TASK" "$LIVE_TASK.tmp"
    drop_surface >/dev/null 2>&1 || true
    board "dispatch-failed" "rolled back"
    release_locks
    exit "$rc"
}
trap rollback_dispatch EXIT

if [ "$RESET_PENDING" -eq 1 ]; then
    rm -f "$LIVE_TASK"
    say "  reset: discarded the previous live task"
fi
cp "$TASK" "$LIVE_TASK"
# Where this run's worktree actually is, rather than where a later run
# would guess it is. The overlap scan tests that path to decide whether
# a dispatch is still alive, and it used to recompute it from its *own*
# --worktree-root - so a run started with a different root read a live
# claim as a ghost and dispatched straight over it.
set_field worktree "$WT"
set_field base_sha "$BASE_SHA"

# Last thing before this run becomes visible to everyone else: are we
# still the lock holder? If the lock was broken while we were inside
# it, the exclusion we are about to rely on stopped being true, and
# claiming anyway would make the board say two runs agreed when they
# never met.
[ "$(cat "$STATE/dispatch.lock/owner" 2>/dev/null || true)" = "$RUN_TOKEN" ] || refuse \
"the dispatch lock was broken while this run was inside it - refusing to claim.
Nothing was dispatched. Re-run once no other run is in flight."

set_status dispatched
board "dispatched" "$TASK_BRANCH @ ${BASE_SHA:0:12}"

# The claim is now visible to every other runner, so the lock has done
# its job. Holding it through the agent run would serialise the bots
# themselves, which is the opposite of the point.
drop_lock "$STATE/dispatch.lock"

# Past the half-dispatch window: from here a failure leaves a worktree
# a human is meant to look at, which is the whole point of `blocked`.
trap release_locks EXIT

# --------------------------------------------------------------------
# Agent
# --------------------------------------------------------------------

# The prompt as sent, byte for byte. It is the one input to the run
# that was never written down anywhere: the log holds whatever the
# agent chose to echo back, which is not the same thing.
PROMPT_FILE="${RUN_LOG%.log}-prompt.txt"
printf '%s\n' "$PROMPT" > "$PROMPT_FILE"

AGENT_STDIN="/dev/null"
[ "$AGENT_PROMPT_VIA" = "argv" ] || AGENT_STDIN="$PROMPT_FILE"

step "Agent"
if [ "$SKIP_AGENT" -eq 1 ]; then
    say "  skipped (--skip-agent)"
    board "agent-skipped"
else
    say "  $AGENT_INVOCATION (output -> $RUN_LOG)"
    # stdin is the prompt file or nothing at all - never the runner's
    # terminal. A headless agent that decides to ask a question would
    # otherwise inherit it and hang, and either of these ends in EOF.
    ( cd "$WT" && PATH="$AGENT_PATH" "$AGENT_CMD" "${AGENT_ARGV[@]}" ) \
        <"$AGENT_STDIN" >>"$RUN_LOG" 2>&1 || AGENT_EXIT=$?
    say "  exit $AGENT_EXIT"
    board "agent-ran" "exit $AGENT_EXIT"
fi

# --------------------------------------------------------------------
# Evidence - read off the tree, never off the agent's summary
# --------------------------------------------------------------------

step "Evidence"

# The agent's one channel back. Read it, then delete it before anything
# else looks at the tree - it must never reach a commit or inflate the
# uncommitted-file count. It is a *claim*: it explains a verdict, it
# never substitutes for one.
# Which of the agent's declared instruction files were actually there.
# A MISSING here means the bot never saw the project conventions and
# every judgement it made was worse-informed than it looked. Partial
# answer to F-7: the runner cannot control what the host loads, but it
# can record what was in reach.
INSTR_STATUS=""
if [ -n "$AGENT_INSTRUCTION_FILES" ]; then
    IFS=',' read -ra instr_list <<< "$AGENT_INSTRUCTION_FILES"
    for f in ${instr_list[@]+"${instr_list[@]}"}; do
        f="$(printf '%s' "$f" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        [ -n "$f" ] || continue
        if [ -f "$WT/$f" ]; then
            INSTR_STATUS="$INSTR_STATUS $f=present"
        else
            INSTR_STATUS="$INSTR_STATUS $f=MISSING"
            say "  warning: $f not in the worktree - agent ran without it"
        fi
    done
    INSTR_STATUS="${INSTR_STATUS# }"
else
    INSTR_STATUS="(none declared)"
fi

# `-f` is true through a symlink and `mv` moves the link rather than
# what it points at, so an artifact could be a link to any file on the
# host: stored as this run's evidence, read by the verifier through the
# link, and published into the control plane. Nothing outside the
# surface is this run's to carry out of it. The link is removed rather
# than followed, and the run is blocked below - an agent that did this
# was not answering the task.
CHANNEL_SYMLINK=""
for chan in ".bot-blocked" ".bot-commit-msg" "$TASK_ARTIFACT"; do
    if [ -L "$WT/$chan" ]; then
        CHANNEL_SYMLINK="${CHANNEL_SYMLINK:+$CHANNEL_SYMLINK, }$chan"
        rm -f "$WT/$chan"
        say "  removed $chan - a symlink, not a channel"
    fi
done

AGENT_BLOCKED=""
if [ -f "$WT/.bot-blocked" ]; then
    # head first, tr second: the other order hands `tr` a file it is
    # still reading when `head` closes the pipe, and under pipefail
    # that SIGPIPE aborts the runner - so a refusal longer than 2000
    # bytes would be the one case that never produces the blocked
    # handoff it promises.
    AGENT_BLOCKED="$(head -c 2000 "$WT/.bot-blocked" | tr -d '\r')"
    rm -f "$WT/.bot-blocked"
    [ -n "$AGENT_BLOCKED" ] || AGENT_BLOCKED="(agent wrote .bot-blocked but left it empty)"
    say "  agent reported blocked"
fi

# The report channel, harvested the same way and for the same reason:
# it is the artifact, so it belongs in the control plane, and it must
# be out of the worktree before the evidence is read or it would count
# as an uncommitted file the bot left behind.
#
# Only for a report task. Harvesting it unconditionally meant a commit
# task that wrote one had the file quietly moved out of its worktree -
# so it never counted as an uncommitted file, and a run that produced a
# report instead of a commit came back `done`. Found by a chain run
# where the child inherited the parent's stub override and the fixer
# behaved like a reviewer. `produces:` on the charter now refuses that
# pairing at dispatch; this stays anyway, because a guard that was once
# needed does not get dropped just because a second one showed up.
ARTIFACT_FILE=""
ARTIFACT_VERDICT=""
if [ "$TASK_PRODUCES" = "report" ] && [ -f "$WT/$TASK_ARTIFACT" ]; then
    mkdir -p "$STATE/artifacts"
    ARTIFACT_FILE="$STATE/artifacts/${TS}_${TASK_OWNER}_${TASK_ID}_${TASK_ARTIFACT#.bot-}"
    mv "$WT/$TASK_ARTIFACT" "$ARTIFACT_FILE"
    ARTIFACT_VERDICT="$(field verdict "$ARTIFACT_FILE")"
    say "  report    $ARTIFACT_FILE (${ARTIFACT_VERDICT:-no verdict})"
    board "report-written" "${ARTIFACT_VERDICT:-no verdict}"
elif [ "$TASK_PRODUCES" = "report" ]; then
    say "  report    none written"
fi

# The third channel out of the worktree, harvested like the other two.
# An agent that cannot commit still has to be able to say *why* it did
# what it did, and a message is the only part of a commit that is not
# mechanical.
#
# Only for a commit task. A report bot writing a commit message is out
# of contract, and harvesting it would quietly tidy away the evidence
# of that - which is the bug F-21 caught in the report channel, one
# file over.
COMMIT_MSG_FILE=""
if [ "$TASK_PRODUCES" = "commit" ] && [ -s "$WT/.bot-commit-msg" ]; then
    COMMIT_MSG_FILE="${RUN_LOG%.log}-commit-msg.txt"
    mv "$WT/.bot-commit-msg" "$COMMIT_MSG_FILE"
    say "  message   $COMMIT_MSG_FILE"
elif [ "$TASK_PRODUCES" = "commit" ] && [ -f "$WT/.bot-commit-msg" ]; then
    rm -f "$WT/.bot-commit-msg"
fi

# --------------------------------------------------------------------
# The commit the agent could not make
#
# Additive, not a replacement: an agent that commits its own work still
# does, and this only ever touches what is *left over*. A sandboxed
# agent can write files and not history - Codex denies the model writes
# to `.git` wherever `.git` is (F-28) - so without this the loop is
# closed to every agent whose sandbox works.
#
# It changes nothing about the evidence. A commit is a mechanical act
# that asserts nothing; every check below still reads the tree, and
# `git add -A` means out-of-scope files land in the diff where the scope
# check already catches them rather than being quietly dropped. The one
# thing the agent contributes is the message, and a message was never
# evidence.
#
# Not for a report, which must leave the tree alone, and not after a
# blocked or crashed run - committing a half-finished tree would turn a
# refusal into a result.
# --------------------------------------------------------------------

COMMITTED_BY="agent"
RUNNER_COMMIT_NOTE=""
if [ "$TASK_PRODUCES" = "commit" ] && [ -z "$AGENT_BLOCKED" ] && [ "$AGENT_EXIT" -eq 0 ]; then
    LEFTOVER="$(git -C "$WT" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
    if [ "${LEFTOVER:-0}" -gt 0 ]; then
        step "Commit"
        say "  $LEFTOVER path(s) left uncommitted - committing them"
        COMMIT_FROM="$COMMIT_MSG_FILE"
        if [ -z "$COMMIT_FROM" ]; then
            # Still committed, because work that cannot be measured
            # cannot be judged. Flagged, because the reasoning is the
            # half a runner cannot supply.
            COMMIT_FROM="${RUN_LOG%.log}-commit-msg.txt"
            { printf '%s: %s\n\n' "$TASK_ID" "$TASK_TITLE"
              printf 'Subject written by the runner: the agent left these changes\n'
              printf 'uncommitted and wrote nothing to .bot-commit-msg.\n'
            } > "$COMMIT_FROM"
            RUNNER_COMMIT_NOTE="the agent left its work uncommitted and wrote no commit message"
        fi
        # Author is the bot, committer is the runner. That is what the
        # split is for, and it is the honest reading: the bot did the
        # work, this script recorded it.
        PROTECTED_HELD=""
        if git -C "$WT" add -A >>"$RUN_LOG" 2>&1 \
            && strip_protected HEAD -C "$WT" \
            && GIT_COMMITTER_NAME=bot-run GIT_COMMITTER_EMAIL=bot-run@invalid \
               git -C "$WT" \
                 -c "user.name=$TASK_OWNER (via $AGENT_ID)" \
                 -c "user.email=$TASK_OWNER@bots.invalid" \
                 commit --quiet --file="$COMMIT_FROM" >>"$RUN_LOG" 2>&1; then
            COMMITTED_BY="runner"
            board "runner-committed" "$LEFTOVER path(s)"
            say "  committed by the runner"
        else
            # Left exactly as it was. The uncommitted-files branch of
            # the verdict below then describes it, which is what would
            # have happened before this step existed.
            say "  could not commit - see $RUN_LOG"
            board "runner-commit-failed" "$LEFTOVER path(s)"
        fi
    fi
fi

# A worktree the agent wrecked must still produce a handoff, so none of
# these may take the script down under `set -e`.
HEAD_SHA="$(git -C "$WT" rev-parse HEAD 2>/dev/null)" || HEAD_SHA=""
if [ -z "$HEAD_SHA" ]; then
    HEAD_SHA="(unreadable)"
    HEAD_BRANCH="(unreadable)"
    COMMITS=0
    DIRTY=0
    TOUCHED=""
    NUMSTAT="worktree unreadable"
    TODO_VIOLATIONS=""
    BASE_IS_ANCESTOR=1
    TREE_BROKEN=1
else
    TREE_BROKEN=0
    # Which branch the evidence is about. The charter says not to switch
    # branches; nothing checked, and the push below names $TASK_BRANCH
    # while every number here comes from wherever HEAD happens to be. An
    # agent that switched would produce a green handoff for a branch
    # still sitting at the base.
    HEAD_BRANCH="$(git -C "$WT" symbolic-ref --quiet --short HEAD 2>/dev/null || printf '(detached)')"
    # `BASE_SHA..HEAD` counts commits reachable from HEAD and not from
    # base, which is zero both when nothing happened and when the branch
    # was reset onto the base or behind it. Those are opposite outcomes,
    # and the second one reaches the no-op path that deletes the branch.
    BASE_IS_ANCESTOR=1
    git -C "$WT" merge-base --is-ancestor "$BASE_SHA" HEAD 2>/dev/null || BASE_IS_ANCESTOR=0
    COMMITS="$(git -C "$WT" rev-list --count "$BASE_SHA..HEAD")"
    DIRTY="$(git -C "$WT" status --porcelain | wc -l | tr -d ' ')"
    # quotepath=off keeps non-ASCII paths unquoted, so the scope check
    # compares the real name instead of "core/src/\303\244.rs".
    # --no-renames makes a rename show up as both the deleted source and
    # the added destination, so moving an out-of-scope file into scope
    # cannot delete it invisibly.
    TOUCHED="$(git -C "$WT" -c core.quotepath=off diff --name-only --no-renames "$BASE_SHA..HEAD")"
    NUMSTAT="$(git -C "$WT" diff --shortstat "$BASE_SHA..HEAD" | sed 's/^ *//')"
    [ -n "$NUMSTAT" ] || NUMSTAT="no committed changes"

    # CLAUDE.md, and the charter's acceptance criterion 4. It was in the
    # charter and nowhere in the code, which made it a suggestion - see
    # F-18. Added lines only: pre-existing debt is not this bot's.
    TODO_VIOLATIONS="$(git -C "$WT" diff -U0 "$BASE_SHA..HEAD" \
        | grep -E '^\+' | grep -Ev '^\+\+\+' \
        | grep -E 'TODO|FIXME' | grep -Ev '#[0-9]+' || true)"
fi

say "  head      ${HEAD_SHA:0:12}"
say "  commits   $COMMITS"
say "  uncommit  $DIRTY file(s)"
say "  diff      $NUMSTAT"

# Whatever is in the diff, whoever put it there. The pathspecs keep
# these out of the runner's own commits; this is the check that also
# covers a commit the agent made for itself.
PROTECTED_IN_DIFF=""
if [ -n "$TOUCHED" ]; then
    while IFS= read -r pfile; do
        [ -n "$pfile" ] || continue
        if is_protected "$pfile"; then
            PROTECTED_IN_DIFF="$PROTECTED_IN_DIFF    $pfile"$'\n'
        fi
    done <<< "$TOUCHED"
fi

# Scope check: every committed file must match a declared glob.
SCOPE_VIOLATIONS=""
if [ -n "$TOUCHED" ]; then
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        ok=0
        IFS=',' read -ra globs <<< "$TASK_TOUCHES"
        for g in ${globs[@]+"${globs[@]}"}; do
            g="$(printf '%s' "$g" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
            [ -n "$g" ] || continue
            # shellcheck disable=SC2254  # glob on purpose
            case "$file" in $g) ok=1; break ;; esac
        done
        [ "$ok" -eq 1 ] || SCOPE_VIOLATIONS="$SCOPE_VIOLATIONS$file"$'\n'
    done <<< "$TOUCHED"
fi

# --------------------------------------------------------------------
# Verifier
# --------------------------------------------------------------------

# F-13: verify the tree you are handing off, not the one next to it.
# This used to run in the agent's worktree, which is the wrong subject
# twice over. An uncommitted edit could be the thing that made it pass,
# so a green verifier said nothing about the branch a human would pull.
# And `verify:` is arbitrary code (F-6): anything it committed or wrote
# landed *after* the evidence above was read, so the handoff could
# describe a tree that no longer existed.
#
# A detached checkout of HEAD answers both. The verifier now sees
# exactly the commits the branch carries, and it has no path to the
# agent's worktree - which the post-run comparison then proves rather
# than assumes.
# A content hash, not a count. Comparing head plus the number of
# porcelain lines missed the case that matters most: a verifier that
# rewrites an already-modified file, or swaps one dirty path for
# another, leaves both numbers identical. `git diff HEAD` puts the
# tracked content in the hash; porcelain covers what is untracked.
# Untracked *content* is still outside it, which is the residual.
tree_state() {
    {
        git -C "$WT" rev-parse HEAD
        git -C "$WT" -c core.quotepath=off status --porcelain
        git -C "$WT" diff HEAD
    } 2>/dev/null | git hash-object --stdin
}

# run_verify <sha> <label> <event>
#
# Verify a clean detached checkout of <sha>. Reports through globals:
# VERIFY_EXIT, VERIFY_WHERE, VERIFY_TAIL, TREE_MUTATED, and
# VERIFY_WT_LEFTOVER, which it only ever sets.
#
# A function because it runs twice. The second time is after a rebase
# onto a base that moved under the run, and "verified" has to mean the
# same thing both times: two copies of this block would be two chances
# to disagree about what the word covers.
run_verify() {
    local sha="$1" label="${2:-}" event="${3:-verified}"
    local pre_state post_state post_head post_dirty log_mark
    VERIFY_EXIT=0
    VERIFY_WHERE="(not run)"
    VERIFY_TAIL=""
    TREE_MUTATED=""

    if [ "$TREE_BROKEN" -eq 1 ]; then
        say "  skipped - worktree unreadable"
        VERIFY_EXIT=-1
        board "verify-skipped" "worktree unreadable"
        return 0
    fi
    if [ -n "$VERIFY_WT_LEFTOVER" ]; then
        # An earlier pass could not remove its checkout. A second one at
        # the same path would fail anyway, and forcing the first out of
        # the way here would destroy the evidence of why it is stuck.
        say "  skipped - $VERIFY_WT_LEFTOVER is still there"
        VERIFY_EXIT=-1
        VERIFY_WHERE="(earlier verify checkout left over)"
        board "verify-skipped" "checkout left over"
        return 0
    fi
    if ! git -C "$WT" worktree add --detach "$VERIFY_WT" "$sha" >>"$RUN_LOG" 2>&1; then
        say "  skipped - could not create the verify checkout at $VERIFY_WT"
        VERIFY_EXIT=-1
        VERIFY_WHERE="(verify checkout failed)"
        board "verify-skipped" "verify checkout failed"
        return 0
    fi

    VERIFY_WHERE="clean checkout of ${sha:0:12}$label"
    pre_state="$(tree_state)"
    # Where the verifier's output starts, so its last words can go in
    # the handoff. "See the log" is the least useful sentence a handoff
    # can contain: the reader is reading it *because* they were not
    # watching the run.
    log_mark="$(wc -l < "$RUN_LOG" | tr -d ' ')"
    say "  $TASK_VERIFY"
    say "  in $VERIFY_WT"
    # The one tool-specific line in the runner, and it is a cost
    # decision, not a semantic one: a fresh checkout would otherwise
    # rebuild every dependency per run. The cache lives under state, so
    # it belongs to the runner - an agent cannot seed it from its own
    # worktree - and cargo's own lock serialises concurrent runs.
    # A report task's verifier is given the report instead of a build:
    # the subject changes with what the task produces, the contract
    # ("an executable that must exit 0") does not. See F-19.
    #
    # These two names are the last place "review" survives in the
    # runner, and they are the one part that could not be renamed
    # with the rest: `verify:` runs inside the verify checkout, so
    # the script that reads them is the copy at base, not the one in
    # the working tree. Renaming either side alone turns the sweep
    # red, and renaming both can only be proven after the merge.
    # BOT_ARTIFACT / BOT_SUBJECT_SHA once this is at base - #348.
    ( cd "$VERIFY_WT" \
        && export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$STATE/cache/target}" \
        && export BOT_REVIEW="$ARTIFACT_FILE" \
        && export BOT_REVIEWED_SHA="$sha" \
        && export BOT_TOUCHES="$TASK_TOUCHES" \
        && export BOT_TASK_ID="$TASK_ID" \
        && eval "$TASK_VERIFY" ) </dev/null >>"$RUN_LOG" 2>&1 || VERIFY_EXIT=$?
    say "  exit $VERIFY_EXIT"
    board "$event" "exit $VERIFY_EXIT"
    if [ "$VERIFY_EXIT" -ne 0 ]; then
        VERIFY_TAIL="$(tail -n "+$((log_mark + 1))" "$RUN_LOG" \
            | sed '/^[[:space:]]*$/d' | tail -n 5)"
    fi

    # A checkout that will not go away is not cosmetic: the next run for
    # this task hits the leftover guard and refuses to start. It has to
    # reach the verdict, not just the console.
    if ! git -C "$WT" worktree remove --force "$VERIFY_WT" >>"$RUN_LOG" 2>&1; then
        VERIFY_WT_LEFTOVER="$VERIFY_WT"
        say "  could not remove $VERIFY_WT"
    fi

    post_state="$(tree_state)"
    if [ "$post_state" != "$pre_state" ]; then
        post_head="$(git -C "$WT" rev-parse HEAD 2>/dev/null)" || post_head="(unreadable)"
        post_dirty="$(git -C "$WT" status --porcelain | wc -l | tr -d ' ')"
        TREE_MUTATED="verifier changed the tree it was measuring.
    head        $sha -> $post_head
    uncommitted $DIRTY -> $post_dirty file(s)
    state       ${pre_state:0:12} -> ${post_state:0:12}
Head and the file count can both be unchanged and the contents still
differ; the state hash is what caught it."
    fi
    return 0
}

step "Verifier"
VERIFY_WT_LEFTOVER=""
run_verify "$HEAD_SHA"

# --------------------------------------------------------------------
# Verdict
# --------------------------------------------------------------------

# Ordered by root cause, not by severity: a crashed agent and a failing
# verifier both end the run, but only one of them explains the other.
# The first version of this block read neither the agent's exit code nor
# its report, so a crash arrived as a green no-op and the cleanup path
# then deleted the branch. See F-4.
BLOCKERS="none"
NOOP=0
if [ "$TREE_BROKEN" -eq 1 ]; then
    STATUS="blocked"
    BLOCKERS="worktree is unreadable - git could not resolve HEAD in $WT"
elif [ -n "$TREE_MUTATED" ]; then
    # Ahead of everything else, including a passing verifier: if the
    # evidence no longer describes the tree, no verdict built on it is
    # worth reporting.
    STATUS="blocked"
    BLOCKERS="$TREE_MUTATED"
elif [ "$BASE_IS_ANCESTOR" -eq 0 ]; then
    STATUS="blocked"
    BLOCKERS="the branch no longer descends from the commit it was dispatched at.
base $BASE_SHA is not an ancestor of head $HEAD_SHA, so the commit count
below describes nothing and any work that was done has been discarded."
elif [ -n "$CHANNEL_SYMLINK" ]; then
    # Above the agent's own channels, because one of the things this
    # could be is a symlinked '.bot-blocked' - a refusal the runner
    # would otherwise quote out of a file the agent never wrote.
    STATUS="blocked"
    BLOCKERS="a channel between agent and runner was a symlink: $CHANNEL_SYMLINK
The runner does not follow those - a link can point anywhere on this
machine, and following one would put a host file into the control plane
as this run's evidence. They were removed unread."
elif [ "$HEAD_BRANCH" != "$TASK_BRANCH" ]; then
    STATUS="blocked"
    BLOCKERS="the worktree is not on '$TASK_BRANCH' any more - HEAD is $HEAD_BRANCH.
Every number below was read from HEAD, and the result would have been
pushed from '$TASK_BRANCH', so they would describe two different trees.
The charter says not to switch branches; this is what that rule is for."
elif [ -n "$AGENT_BLOCKED" ]; then
    STATUS="blocked"
    BLOCKERS="agent reported blocked: $AGENT_BLOCKED"
elif [ "$AGENT_EXIT" -ne 0 ]; then
    STATUS="blocked"
    BLOCKERS="agent exited $AGENT_EXIT without reporting - see $RUN_LOG"
elif [ "$TASK_PRODUCES" = "report" ] && [ "$COMMITS" -ne 0 ]; then
    # The one hard boundary for a bot that does not write code. A commit
    # from it is a failed run even when the change is an improvement:
    # the whole value of the role is that it has no stake in the diff.
    STATUS="blocked"
    BLOCKERS="a bot that produces a report must not commit, and this run made $COMMITS commit(s)"
elif [ "$TASK_PRODUCES" = "report" ] && [ -z "$ARTIFACT_FILE" ]; then
    STATUS="blocked"
    BLOCKERS="no $TASK_ARTIFACT was written - the run produced nothing to read"
elif [ "$VERIFY_EXIT" -ne 0 ]; then
    STATUS="blocked"
    BLOCKERS="verifier exited $VERIFY_EXIT${VERIFY_TAIL:+:
$VERIFY_TAIL}
Full output: $RUN_LOG"
elif [ -n "$PROTECTED_IN_DIFF" ]; then
    STATUS="blocked"
    BLOCKERS="this commit carries files that never travel:"$'\n'"$PROTECTED_IN_DIFF
They are excluded from every `git add` this runner performs and from
the surface's exclude file, so one reaching a commit means an agent put
it there deliberately. Nothing is pushed."
elif [ -n "$SCOPE_VIOLATIONS" ]; then
    STATUS="blocked"
    BLOCKERS="files changed outside touches:"$'\n'"$SCOPE_VIOLATIONS"
elif [ -n "$TODO_VIOLATIONS" ]; then
    STATUS="blocked"
    BLOCKERS="new TODO/FIXME without a linked issue number:"$'\n'"$TODO_VIOLATIONS"
elif [ -n "$VERIFY_WT_LEFTOVER" ]; then
    # Not the work's fault, and still not something to hand over
    # quietly: the next run for this task cannot start until it is gone.
    STATUS="needs-review"
    BLOCKERS="the verify checkout could not be removed: $VERIFY_WT_LEFTOVER
Remove it before re-running:
    rm -rf $VERIFY_WT_LEFTOVER"
elif [ "$DIRTY" -ne 0 ]; then
    STATUS="needs-review"
    BLOCKERS="$DIRTY uncommitted file(s) left in the worktree"
elif [ "$COMMITS" -gt 0 ] && [ -z "$TOUCHED" ]; then
    # F-22: a commit with no diff. Found by the first bot-to-bot chain,
    # where it was the *right* answer - the fixer read the review,
    # checked the finding against the code, rejected it, and put the
    # reasoning in the commit message because the task named that as
    # the channel for disagreement.
    #
    # The runner still must not call it `done`. Every other `done` means
    # "a verifier proved something about a diff"; here there is no diff,
    # and what has to be judged is an argument. The same rule as F-18:
    # an outcome the runner cannot check does not get reported as
    # proven. So it goes to a human with the message in front of them.
    STATUS="needs-review"
    BLOCKERS="$COMMITS commit(s), none of them changing a file.

That is a legitimate answer - a reasoned refusal is a result, and the
message is the artifact - but it is an argument, and no verifier reads
arguments. Read it:
$(git -C "$WT" log --format='    %h %s' "$BASE_SHA..HEAD" 2>/dev/null)"
elif [ -n "$RUNNER_COMMIT_NOTE" ]; then
    # The work is here and it verified. What is missing is the half a
    # runner cannot write: why. Same rule as F-18 - an outcome the
    # runner cannot check does not get reported as proven.
    STATUS="needs-review"
    BLOCKERS="$RUNNER_COMMIT_NOTE.
The changes are committed and the verifier passed on them, so the work
is sound as far as anything here can tell. What is missing is the
reason for it, and no verifier reads reasons."
elif [ "$COMMITS" -gt 1 ]; then
    # BOT.md acceptance criterion 3. The work may well be fine; a human
    # decides whether to squash.
    STATUS="needs-review"
    BLOCKERS="$COMMITS commits, charter asks for exactly one"
elif [ -n "$PROTECTED_HELD" ]; then
    # Not `blocked`: the work is committed and verified, and nothing
    # that never travels went anywhere. But an agent rewrote one of
    # them, and a run that says `done` about that has decided something
    # only a human gets to decide.
    STATUS="needs-review"
    BLOCKERS="the agent changed files that never travel, and this run did not carry them:
$PROTECTED_HELD
The commit has them exactly as $TASK_BASE does. The agent's version is
still on disk in $WT and was pushed nowhere. Whether the edit was meant
- a rotated key, a formatter, an install that rewrote .npmrc - is not
something a verifier can answer."
elif [ "$TASK_PRODUCES" = "report" ] && [ "$ARTIFACT_VERDICT" = "blocked" ]; then
    # The bot's own escalation. A shape template defines `blocked` as
    # work that could not be completed, and a run that hands one back as
    # `done` buries it: the task goes terminal, the scheduler never
    # comes back to it, and the only record of the refusal is inside a
    # file nobody was told to open.
    STATUS="blocked"
    BLOCKERS="$TASK_OWNER could not complete this report. Its reasons are
in $ARTIFACT_FILE"
elif [ "$COMMITS" -eq 0 ]; then
    STATUS="done"
    NOOP=1
    if [ "$TASK_PRODUCES" = "report" ]; then
        # Zero commits is the *success* shape here, not an empty run.
        BLOCKERS="none - report delivered, verdict ${ARTIFACT_VERDICT:-(none)}"
    else
        BLOCKERS="none - no-op run, nothing needed changing"
    fi
else
    STATUS="done"
fi

# --------------------------------------------------------------------
# Rebase onto a base that moved
#
# The overlap check answers "are these two running at the same time".
# It has never answered "do these two merge", and the second question is
# the one a handoff is really making a claim about. Two branches cut
# from one commit collide at merge - long after both runs reported
# success and both handoffs went out saying so. That is the other half
# of F-16.
#
# So once the work is otherwise done, the base ref is read again. If it
# moved while this run was in flight - usually because the task this one
# was told to wait for landed - the commits are replayed onto the new
# tip and verified *there*. Only "clean rebase, still green" keeps the
# `done`. Anything else restores the tree that did verify and hands the
# branch to a human, because no runner can tell a semantic collision
# from a mistake.
#
# Three outcomes worth naming:
#
#   conflict   the work no longer applies. The paths are the message.
#   red        it applies and no longer works. This is the collision the
#              overlap check cannot see by construction: two changes
#              with no file in common that still disagree - a rename on
#              one side, a caller on the other.
#   emptied    every commit became a no-op against the new base. Someone
#              else already did this. Worth one human glance before the
#              branch is thrown away, because afterwards "already fixed"
#              and "silently dropped" look identical.
#
# The branch is only ever moved on success. On any other outcome it is
# put back exactly where it was, so the invariant a reader depends on
# holds without qualification: the branch named in a handoff is a tree
# that verified. What did not work is in the handoff as prose, not as a
# tip somebody has to bisect for.
#
# No fetch. This re-reads the *local* ref, so `origin/main` moves only
# because something else fetched it. A runner that reached the network
# in order to measure would be changing the world it is describing, and
# the answer would depend on when it ran.
# --------------------------------------------------------------------

REBASE_STATE="not attempted"
REBASED=0

# Puts the worktree back on its branch after the replay. `move` points
# the branch at the verified tip first; `keep` leaves it exactly where
# it has been the whole time, which is the point of detaching.
REATTACH_FAILED=""
reattach() {   # reattach <move|keep>
    local rc=0
    if [ "$1" = "move" ]; then
        git -C "$WT" checkout -B "$TASK_BRANCH" >>"$RUN_LOG" 2>&1 || rc=$?
    else
        git -C "$WT" checkout "$TASK_BRANCH" >>"$RUN_LOG" 2>&1 || rc=$?
    fi
    [ "$rc" -eq 0 ] || REATTACH_FAILED="$1"
    return 0
}

if [ "$NO_REBASE" -eq 1 ]; then
    REBASE_STATE="skipped (--no-rebase)"
elif [ "$STATUS" != "done" ] || [ "$NOOP" -eq 1 ]; then
    # Gated on the verdict rather than on a copy of its conditions. A
    # run that is not being handed off as done already has a reason, and
    # a rebase on top of it would bury that reason under a newer one.
    REBASE_STATE="not attempted - the run is '$STATUS'"
elif [ "$TASK_PRODUCES" = "report" ]; then
    REBASE_STATE="not applicable - a report has nothing to replay"
else
    step "Rebase"
    # For a folder, "has the base moved" is "has the folder changed",
    # and the only way to ask is to snapshot it again. An unchanged
    # folder reuses its commit, so this is a stat walk and not a copy.
    [ "$SURFACE" != "import" ] || snapshot_folder
    NEW_BASE_SHA="$(git -C "$ORIGIN_REPO" rev-parse --verify "$TASK_BASE^{commit}" 2>/dev/null)" \
        || NEW_BASE_SHA=""

    if [ -z "$NEW_BASE_SHA" ]; then
        # The ref was deleted while this run was in flight - a merged
        # branch, usually. The work is still good against the commit it
        # was cut from, and where it now belongs is not a runner's call.
        REBASE_STATE="'$TASK_BASE' no longer resolves"
        say "  $REBASE_STATE"
        board "rebase-skipped" "$TASK_BASE unresolvable"
        STATUS="needs-review"
        BLOCKERS="the base ref '$TASK_BASE' no longer resolves.
This branch verified against $BASE_SHA, which is now unreachable by
name - the ref was deleted or renamed while the run was in flight.
Somebody has to say what it should land on."
    elif [ "$NEW_BASE_SHA" = "$BASE_SHA" ]; then
        REBASE_STATE="not needed - $TASK_BASE is still ${BASE_SHA:0:12}"
        say "  $REBASE_STATE"
    elif ! git -C "$WT" checkout --detach >>"$RUN_LOG" 2>&1; then
        # Detaching is the first thing, not an implementation detail:
        # `git rebase` moves the checked-out branch as its opening act,
        # so replaying on the branch itself would leave $TASK_BRANCH at
        # an unverified tip for as long as the second verifier takes -
        # and a run killed inside that window leaves it there for good.
        # If the worktree will not detach, nothing has moved yet and
        # nothing should.
        REBASE_STATE="could not detach $WT - not attempted"
        say "  $REBASE_STATE"
        board "rebase-skipped" "detach failed"
        STATUS="needs-review"
        BLOCKERS="$TASK_BASE has moved to ${NEW_BASE_SHA:0:12} and the worktree could
not be detached to replay the work there, so nothing was attempted and
$TASK_BRANCH is untouched. It verified against ${BASE_SHA:0:12}. See
$RUN_LOG for what git said."
    else
        say "  $TASK_BASE moved ${BASE_SHA:0:12} -> ${NEW_BASE_SHA:0:12}"
        PRE_REBASE_HEAD="$HEAD_SHA"
        # --onto, not a plain rebase: the new tip need not contain the
        # old one. A base that was force-pushed, or repointed at a
        # different branch entirely, still has an answer - replay
        # BASE_SHA..HEAD there - whereas `git rebase <upstream>` works
        # out which commits to replay from a merge base that is no
        # longer the commit this run was dispatched at.
        #
        # autoStash off on purpose. The tree is clean here, because a
        # dirty one is `needs-review` above and never reaches this; and
        # if that ever stops being true, a rebase that quietly pockets
        # the uncommitted files and puts them back afterwards is the
        # last thing this script should be doing with evidence.
        # updateRefs off explicitly. Detaching stops `rebase` from moving
        # the branch it is standing on; it does not stop
        # `rebase.updateRefs=true`, which force-updates any ref pointing
        # into the range being replayed - and $TASK_BRANCH points at
        # exactly that. A user's config would otherwise undo the whole
        # point of detaching, silently, on somebody else's machine.
        REBASE_EXIT=0
        git -C "$WT" -c rebase.autoStash=false -c rebase.updateRefs=false \
            -c core.quotepath=off \
            rebase --onto "$NEW_BASE_SHA" "$BASE_SHA" >>"$RUN_LOG" 2>&1 || REBASE_EXIT=$?

        # And then check rather than trust: the branch is supposed to be
        # exactly where it was.
        BRANCH_MOVED=""
        BRANCH_NOW="$(git -C "$WT" rev-parse --verify -q "refs/heads/$TASK_BRANCH" 2>/dev/null || true)"
        [ "$BRANCH_NOW" = "$PRE_REBASE_HEAD" ] || BRANCH_MOVED="${BRANCH_NOW:-(gone)}"

        if [ -n "$BRANCH_MOVED" ]; then
            git -C "$WT" rebase --abort >>"$RUN_LOG" 2>&1 || true
            git -C "$WT" update-ref "refs/heads/$TASK_BRANCH" "$PRE_REBASE_HEAD" \
                >>"$RUN_LOG" 2>&1 || true
            reattach keep
            REBASE_STATE="the branch moved during a detached replay - put back"
            say "  $REBASE_STATE"
            board "rebase-stuck" "branch moved under a detached replay"
            STATUS="blocked"
            BLOCKERS="$TASK_BRANCH was replayed on a detached head precisely so that it
could not move, and it moved anyway: ${PRE_REBASE_HEAD:0:12} ->
$BRANCH_MOVED. It has been put back, and this run is not reporting a
verdict built on a tree something else was editing. See $RUN_LOG."
        elif [ "$REBASE_EXIT" -ne 0 ]; then
            REBASE_CONFLICTS="$(git -C "$WT" -c core.quotepath=off \
                diff --name-only --diff-filter=U 2>/dev/null | sort -u || true)"
            [ -n "$REBASE_CONFLICTS" ] \
                || REBASE_CONFLICTS="(git named none - see $RUN_LOG)"
            if git -C "$WT" rebase --abort >>"$RUN_LOG" 2>&1; then
                reattach keep
                REBASE_STATE="conflict - branch left on ${PRE_REBASE_HEAD:0:12}"
                say "  conflict; aborted, branch never moved"
                board "rebase-conflict" "${NEW_BASE_SHA:0:12}"
                STATUS="needs-review"
                BLOCKERS="verified against ${BASE_SHA:0:12}, and it no longer applies.
$TASK_BASE has moved to ${NEW_BASE_SHA:0:12}, and replaying this work
onto it conflicts in:
$(printf '%s\n' "$REBASE_CONFLICTS" | sed 's/^/    /')
The replay ran on a detached head, so $TASK_BRANCH never moved: it is
still the tree that passed the verifier. Resolving this is a judgement
about two changes, which is the one thing a runner must not make up."
            else
                # Abort failed: the worktree is mid-rebase, a state no
                # later run can start from. Say it loudly - the next
                # dispatch would otherwise stop at the existing-path
                # guard with no idea why.
                REBASE_STATE="conflict, and the abort failed"
                say "  conflict, and 'rebase --abort' failed"
                board "rebase-stuck" "${NEW_BASE_SHA:0:12}"
                STATUS="blocked"
                BLOCKERS="the rebase onto ${NEW_BASE_SHA:0:12} conflicted and could not be
aborted. $WT is mid-rebase, and no run can use it until that is undone.
$TASK_BRANCH itself is untouched and still points at the verified tree:
    git -C $WT rebase --abort
    git -C $WT checkout $TASK_BRANCH
Conflicting paths:
$(printf '%s\n' "$REBASE_CONFLICTS" | sed 's/^/    /')"
            fi
        else
            REBASED_HEAD="$(git -C "$WT" rev-parse HEAD)"
            REBASED_COMMITS="$(git -C "$WT" rev-list --count "$NEW_BASE_SHA..$REBASED_HEAD")"

            if [ "$REBASED_COMMITS" -eq 0 ]; then
                # git drops a commit whose patch is already upstream. The
                # rebase succeeded and nothing is left: somebody else did
                # this work. Reported rather than cleaned up, because
                # from here "already fixed" and "silently lost" are the
                # same picture.
                reattach keep
                REBASE_STATE="emptied - the new base already carries this work"
                say "  $REBASE_STATE"
                board "rebase-emptied" "${NEW_BASE_SHA:0:12}"
                STATUS="needs-review"
                BLOCKERS="replaying this onto ${NEW_BASE_SHA:0:12} left no commits at all:
every patch in it is already present on $TASK_BASE. The work was done
twice, or it was done elsewhere first. $TASK_BRANCH never moved, so the
diff is still readable. It probably wants throwing away, and that is a
decision, not a cleanup rule."
            else
                # Verify the rebased tree, keeping the first run's
                # result to fall back on: that is what describes the
                # branch if this one does not hold up.
                FIRST_VERIFY_EXIT="$VERIFY_EXIT"
                FIRST_VERIFY_WHERE="$VERIFY_WHERE"
                FIRST_VERIFY_TAIL="$VERIFY_TAIL"
                say "  clean; re-verifying on ${NEW_BASE_SHA:0:12}"
                run_verify "$REBASED_HEAD" \
                    " (rebased onto ${NEW_BASE_SHA:0:12})" "re-verified"
                REVERIFY_EXIT="$VERIFY_EXIT"
                REVERIFY_TAIL="$VERIFY_TAIL"

                # Scope, against the tree that is about to become the
                # answer. A replay is not a copy: a rename-aware merge
                # can land a patch on a path the original diff never
                # touched, so the check that ran on the old diff has not
                # been run on this one.
                REBASED_TOUCHED="$(git -C "$WT" -c core.quotepath=off \
                    diff --name-only --no-renames "$NEW_BASE_SHA..$REBASED_HEAD" 2>/dev/null || true)"
                REBASED_SCOPE=""
                REBASED_PROTECTED=""
                if [ -n "$REBASED_TOUCHED" ]; then
                    while IFS= read -r rfile; do
                        [ -n "$rfile" ] || continue
                        rok=0
                        for g in ${MY_GLOBS[@]+"${MY_GLOBS[@]}"}; do
                            # shellcheck disable=SC2254  # a glob, on purpose
                            case "$rfile" in $g) rok=1; break ;; esac
                        done
                        [ "$rok" -eq 1 ] || REBASED_SCOPE="$REBASED_SCOPE$rfile"$'\n'
                        # Same argument as the scope check one line up,
                        # and a worse outcome if it is skipped: a
                        # rename on the base side can land this patch
                        # at a path the original diff never had, and
                        # PROTECTED_IN_DIFF describes the diff before
                        # the replay.
                        ! is_protected "$rfile" \
                            || REBASED_PROTECTED="$REBASED_PROTECTED    $rfile"$'\n'
                    done <<< "$REBASED_TOUCHED"
                fi

                # Everything below restores the first verifier's result
                # unless the branch actually moves, because that result
                # is what describes the branch as it then stands.
                REBASE_KEPT_REASON=""
                if [ "$REVERIFY_EXIT" -lt 0 ]; then
                    # The verifier never ran; the checkout could not be
                    # made. "does not verify" would be a claim about a
                    # tree nothing measured, which is the one sentence
                    # this runner exists not to write.
                    REBASE_STATE="could not verify the rebased tree - branch not moved"
                    board "rebase-unverified" "${NEW_BASE_SHA:0:12}"
                    STATUS="needs-review"
                    REBASE_KEPT_REASON="the work replayed cleanly onto ${NEW_BASE_SHA:0:12} and then could
not be verified there at all: $VERIFY_WHERE. Nothing is known about the
rebased tree, so $TASK_BRANCH was left on ${PRE_REBASE_HEAD:0:12}, which
did verify against ${BASE_SHA:0:12}. See $RUN_LOG."
                elif [ -n "$VERIFY_WT_LEFTOVER" ]; then
                    # Its own outcome, and not `rebase-red`: the rebased
                    # tree may be perfectly good. What is wrong is that
                    # the next run for this task cannot start. Folding it
                    # in with a failing verifier would put "does not
                    # verify" on the board next to an exit code of 0.
                    REBASE_STATE="re-verify checkout could not be removed - branch not moved"
                    board "rebase-stuck" "verify checkout left over"
                    STATUS="needs-review"
                    REBASE_KEPT_REASON="the re-verify checkout could not be removed: $VERIFY_WT_LEFTOVER
It exited $REVERIFY_EXIT, so the rebased tree may well be fine - but the
branch was left on ${PRE_REBASE_HEAD:0:12} rather than moved onto a
result this run could not finish measuring. Remove it before re-running:
    rm -rf $VERIFY_WT_LEFTOVER"
                elif [ -n "$TREE_MUTATED" ] || [ "$REVERIFY_EXIT" -ne 0 ]; then
                    REBASE_STATE="clean, but red on ${NEW_BASE_SHA:0:12} - branch not moved"
                    board "rebase-red" "verify exit $REVERIFY_EXIT"
                    STATUS="needs-review"
                    REBASE_KEPT_REASON="this work verifies on ${BASE_SHA:0:12} and does not verify on
${NEW_BASE_SHA:0:12}, which is where $TASK_BASE now points. It rebased
without a single conflict, so no file here is contested: something on
the new base disagrees with this change in a way no merge could have
shown.
    $TASK_VERIFY -> exit $REVERIFY_EXIT${REVERIFY_TAIL:+
$(printf '%s\n' "$REVERIFY_TAIL" | sed 's/^/    /')}
The replay ran on a detached head, so $TASK_BRANCH never left
${PRE_REBASE_HEAD:0:12} - the tree that passed - and the evidence
recorded below is still true of it.${TREE_MUTATED:+
The re-verify also changed the worktree: $TREE_MUTATED}"
                elif [ -n "$REBASED_PROTECTED" ]; then
                    # Ahead of scope: one of these is a file in the
                    # wrong place, the other is a secret.
                    REBASE_STATE="clean and green on ${NEW_BASE_SHA:0:12}, carrying files that never travel"
                    board "rebase-protected" "${NEW_BASE_SHA:0:12}"
                    STATUS="blocked"
                    REBASE_KEPT_REASON="replaying this onto ${NEW_BASE_SHA:0:12} put paths that never
travel into the diff:
$(printf '%s' "$REBASED_PROTECTED")
It verified there, and a verifier has no opinion about what a path is
called. The branch was left on ${PRE_REBASE_HEAD:0:12}, where the diff
carried none of them, and nothing was pushed."
                elif [ -n "$REBASED_SCOPE" ]; then
                    # Green, and out of bounds. A verifier has no opinion
                    # about scope, so a passing re-verify is not on its
                    # own permission to move the branch.
                    REBASE_STATE="clean and green on ${NEW_BASE_SHA:0:12}, and out of scope there"
                    board "rebase-out-of-scope" "${NEW_BASE_SHA:0:12}"
                    STATUS="blocked"
                    REBASE_KEPT_REASON="replaying this onto ${NEW_BASE_SHA:0:12} put changes outside
touches:
$(printf '%s' "$REBASED_SCOPE" | sed 's/^/    /')
It verified there, and a verifier has no opinion about scope. The branch
was left on ${PRE_REBASE_HEAD:0:12}, where the diff was in bounds."
                else
                    # Move the branch, and only then believe it. These
                    # lines used to run before the reattach was checked,
                    # so a failed `checkout -B` produced a handoff
                    # describing a tree the branch did not point at.
                    reattach move
                    if [ -n "$REATTACH_FAILED" ]; then
                        REBASE_STATE="re-verified, but the branch could not be moved onto it"
                        REBASE_KEPT_REASON=""
                    else
                        BASE_SHA="$NEW_BASE_SHA"
                        HEAD_SHA="$REBASED_HEAD"
                        COMMITS="$REBASED_COMMITS"
                        TOUCHED="$REBASED_TOUCHED"
                        NUMSTAT="$(git -C "$WT" diff --shortstat "$BASE_SHA..HEAD" \
                            | sed 's/^ *//')"
                        [ -n "$NUMSTAT" ] || NUMSTAT="no committed changes"
                        # The live task records the base its worktree
                        # stands on, and the overlap scan expands other
                        # tasks' globs against it. Leaving the
                        # dispatch-time value there would have the next
                        # runner compare this branch's files against a
                        # tree it no longer sits on.
                        set_field base_sha "$BASE_SHA"
                        REBASED=1
                        REBASE_STATE="clean, re-verified on ${NEW_BASE_SHA:0:12}"
                        board "rebased" "${PRE_REBASE_HEAD:0:12} -> ${HEAD_SHA:0:12}"

                        # One more look at the ref. This step narrows the
                        # window between "verified" and "merged"; it
                        # cannot close it, and a runner that re-read
                        # until the base held still would never finish in
                        # a busy repo. So the honest thing is to notice
                        # and say so: the handoff's claim is about a
                        # commit, not about a tip.
                        LATE_BASE_SHA="$(git -C "$ORIGIN_REPO" rev-parse --verify \
                            "$TASK_BASE^{commit}" 2>/dev/null || true)"
                        if [ -n "$LATE_BASE_SHA" ] && [ "$LATE_BASE_SHA" != "$NEW_BASE_SHA" ]; then
                            REBASE_STATE="$REBASE_STATE - and $TASK_BASE moved on again to ${LATE_BASE_SHA:0:12} while that ran"
                            board "base-moved-again" "${LATE_BASE_SHA:0:12}"
                        fi
                    fi
                fi

                say "  $REBASE_STATE"
                if [ -n "$REBASE_KEPT_REASON" ]; then
                    # One place, one set of restores. Four branches each
                    # putting the first verifier's result back by hand is
                    # four chances to forget one.
                    reattach keep
                    BLOCKERS="$REBASE_KEPT_REASON"
                    VERIFY_EXIT="$FIRST_VERIFY_EXIT"
                    VERIFY_WHERE="$FIRST_VERIFY_WHERE"
                    VERIFY_TAIL="$FIRST_VERIFY_TAIL"
                    TREE_MUTATED=""
                fi
            fi
        fi

        # Whatever happened above, the worktree was supposed to end up
        # back on its branch. A failure here is not cosmetic in one
        # direction: if the *move* did not take, the handoff is about to
        # name a branch that does not point at the tree it describes,
        # which is the one thing this whole step exists to prevent.
        if [ "$REATTACH_FAILED" = "move" ]; then
            REBASE_STATE="re-verified, but the branch could not be moved onto it"
            board "rebase-stuck" "checkout -B failed"
            STATUS="blocked"
            BLOCKERS="the rebased tree verified and $TASK_BRANCH could not be moved onto
it. The worktree is detached at ${REBASED_HEAD:-(unknown)} and the
branch still points at ${PRE_REBASE_HEAD:0:12}, so nothing is lost and
nothing is ready:
    git -C $WT checkout -B $TASK_BRANCH ${REBASED_HEAD:-HEAD}
See $RUN_LOG for what git said."
        elif [ -n "$REATTACH_FAILED" ]; then
            # The branch is right; only the worktree is off it. Worth a
            # line, not a verdict of its own.
            BLOCKERS="$BLOCKERS

Note: $WT is left detached - 'git -C $WT checkout $TASK_BRANCH' failed.
$TASK_BRANCH itself points where this handoff says it does."
        fi
    fi
fi

# --------------------------------------------------------------------
# Getting the result out
#
# The work is on a clone, and a clone under .worktrees is not where
# anybody looks for a branch. So the branch is pushed back into the
# project it came from.
#
# That is not the push this loop refuses to make. The refused one goes
# to a remote, where the work becomes visible to other people and hard
# to take back; this one moves a ref inside the repository the task
# already named. It is also the only way a result outlives the surface
# it was made on, because cleanup removes the clone.
# --------------------------------------------------------------------

PUSHED=""
PUSH_FAILED=""
PATCH_FILE=""
# PROTECTED_IN_DIFF gates this as hard as a broken tree does. The
# blocker it writes says "Nothing is pushed", and that has to be true of
# the patch as well - a folder run would otherwise write the secret into
# .state/patches/ instead, which is the same publication by a different
# door.
if [ -n "$PROTECTED_IN_DIFF" ]; then
    say ""
    say "Result branch: not pushed - the diff carries paths that never travel."
fi
if [ "$TREE_BROKEN" -eq 0 ] && [ "$COMMITS" -gt 0 ] && [ -z "$PROTECTED_IN_DIFF" ]; then
    step "Result branch"
    # The path, not the remote name. `origin` lives in the surface's
    # own .git/config, which the agent can write - Codex is explicitly
    # granted that directory - so `git remote set-url` would redirect
    # this push wherever it liked. $ORIGIN_REPO is the runner's own
    # variable and has never been inside the worktree.
    if git -C "$WT" push --quiet "$ORIGIN_REPO" \
        "refs/heads/$TASK_BRANCH:refs/heads/$TASK_BRANCH" >>"$RUN_LOG" 2>&1; then
        PUSHED="$TASK_BRANCH"
        say "  $TASK_BRANCH -> $ORIGIN_REPO"
        board "pushed" "$TASK_BRANCH @ ${HEAD_SHA:0:12}"
        if [ "$SURFACE" = "import" ]; then
            # A folder has nothing to pull a branch into. The snapshot
            # keeps the history so nothing is lost, and the patch is the
            # part a human can actually act on: `git apply --check` will
            # say whether it still fits the folder as it stands now,
            # which a branch in a repository the folder has never heard
            # of cannot.
            mkdir -p "$STATE/patches"
            PATCH_FILE="$STATE/patches/${TS}_${TASK_ID}.patch"
            if git -C "$WT" format-patch --stdout "$BASE_SHA..$TASK_BRANCH" \
                > "$PATCH_FILE" 2>>"$RUN_LOG"; then
                say "  patch    $PATCH_FILE"
                board "patch-written" "$PATCH_FILE"
            else
                rm -f "$PATCH_FILE"
                PATCH_FILE=""
                say "  could not write the patch"
                board "patch-failed" "$TASK_ID"
                # The branch is in the snapshot and the folder cannot
                # pull from it, so without the patch there is nothing a
                # human can act on - which is not a `done`.
                [ "$STATUS" != "done" ] || STATUS="needs-review"
                [ "$BLOCKERS" != "none" ] || BLOCKERS=""
                BLOCKERS="${BLOCKERS:+$BLOCKERS

}the work is committed and verified, and the patch could not be
written. $REPO is a plain folder, so a branch in $ORIGIN_REPO is not
something it can pull from; the patch was the artifact. Read it out by
hand before anything removes the surface:
    git -C $WT format-patch --stdout $BASE_SHA..$TASK_BRANCH"
            fi
        fi
    else
        PUSH_FAILED="$TASK_BRANCH"
        say "  could not push $TASK_BRANCH into $REPO"
        board "push-failed" "$TASK_BRANCH"
        # A result nobody can reach from the project is not a result
        # yet, however green the verifier was.
        [ "$STATUS" != "done" ] || STATUS="needs-review"
        [ "$BLOCKERS" != "none" ] || BLOCKERS=""
        BLOCKERS="${BLOCKERS:+$BLOCKERS

}the branch could not be pushed back into $ORIGIN_REPO, so it exists
only on the work surface:
    git -C $WT log --oneline $BASE_SHA..$TASK_BRANCH
Move it by hand before anything removes $WT. See $RUN_LOG."
    fi
fi

# --------------------------------------------------------------------
# Handoff to another bot
#
# F-21: a handoff between bots is a task the runner writes, not a
# message a bot sends. Letting the reviewer emit the next task fails
# for exactly the reason the agent does not write its own handoff - it
# would be self-reported scope, and the bot that decides what is wrong
# would also decide what is allowed to be touched to fix it.
#
# So every field comes from somewhere that is not the review's prose:
#
#   owner, verify  from the review *task*, which is contract
#   touches        from the cited paths, re-checked against the tree
#   base           from the commit the worktree was actually at
#   objective      a *path* to the review, never a copy of it
#
# That last one is the Grok Bot lesson taken literally: the message
# carries a path. Inline the findings and there are two copies free to
# disagree, and the fixer reads the copy.
# --------------------------------------------------------------------

DERIVED_TASK=""
DERIVED_ID=""
DERIVED_WHY=""

if [ "$TASK_PRODUCES" = "report" ] && [ "$STATUS" = "done" ] \
   && [ "$ARTIFACT_VERDICT" = "changes-requested" ]; then
    if [ -z "$TASK_ON_CHANGES" ]; then
        DERIVED_WHY="the task declares no on_changes_requested:, so this review stops with a human"
    else
        CITED="$(sed -n 's/^-[[:space:]]\{1,\}\([^[:space:]]\{1,\}\):[0-9]\{1,\}.*/\1/p' \
            "$ARTIFACT_FILE" | sort -u)"

        # Re-checked here even though review-shape.sh already checked
        # them: `verify:` is data, a task is free to name a different
        # one, and a path that scopes a bot is not something to take on
        # trust from the file it came out of.
        CITED_OK=""
        CITED_BAD=""
        while IFS= read -r cited_path; do
            [ -n "$cited_path" ] || continue
            if ! git -C "$WT" cat-file -e "$HEAD_SHA:$cited_path" 2>/dev/null; then
                CITED_BAD="$CITED_BAD $cited_path(missing)"
                continue
            fi
            in_scope=0
            for g in ${MY_GLOBS[@]+"${MY_GLOBS[@]}"}; do
                # shellcheck disable=SC2254  # a glob, on purpose
                case "$cited_path" in $g) in_scope=1; break ;; esac
            done
            if [ "$in_scope" -eq 1 ]; then
                CITED_OK="$CITED_OK$cited_path"$'\n'
            else
                CITED_BAD="$CITED_BAD $cited_path(out of scope)"
            fi
        done <<< "$CITED"

        if [ -z "$CITED_OK" ]; then
            DERIVED_WHY="the review asks for changes but cites no usable path, so there is nothing to scope a task to"
        elif [ -n "$CITED_BAD" ]; then
            DERIVED_WHY="the review cites paths the tree does not support:$CITED_BAD - a human decides what that means"
        elif [ -f "$STATE/tasks/$TASK_ID-fix.md" ]; then
            # The derived id is stable, so a second review of the same
            # task would write over a follow-up that is still open. Two
            # fix tasks for one review is not a queue, it is a fork; the
            # same reasoning as the overlap check, one level up.
            DERIVED_WHY="a follow-up for this review already exists and is '$(field status "$STATE/tasks/$TASK_ID-fix.md")' at $STATE/tasks/$TASK_ID-fix.md - resolve that one first"
        else
            DERIVED_ID="$TASK_ID-fix"
            DERIVED_TASK="$STATE/proposed/$DERIVED_ID.md"
            DERIVED_TOUCHES="$(printf '%s' "$CITED_OK" \
                | awk 'NF { if (n++) printf ", "; printf "%s", $0 } END { print "" }')"
            mkdir -p "$STATE/proposed"

            cat > "$DERIVED_TASK" <<DERIVED_END
---
id: $DERIVED_ID
produces: commit
title: Address the findings from $TASK_ID
owner: $TASK_ON_CHANGES
status: todo
base: $HEAD_SHA
branch: bot/$TASK_ON_CHANGES/$DERIVED_ID
touches: $DERIVED_TOUCHES
verify: $TASK_DERIVED_VERIFY
schedule: $TASK_SCHEDULE
---

# Objective

Answer the findings in the review at:

    $ARTIFACT_FILE

Read that file. It is the task. Each finding names a path and a line
and states one claim; answer every one of them, either by changing the
code or by establishing that the finding is wrong.

A finding you disagree with is not a finding you skip. Say so in the
commit message, with the reason. Silence reads as agreement, and the
next reader cannot tell the difference between "fixed" and "missed".

# Acceptance

- [ ] The \`verify:\` command exits 0.
- [ ] The diff stays inside \`touches:\`.
- [ ] Every finding in the review is either fixed or answered.

# Context

Written by the runner from the review above: verdict
\`$ARTIFACT_VERDICT\`, about commit $HEAD_SHA, by \`$TASK_OWNER\`.

Nothing in this file came from the reviewer's prose. \`owner:\` and
\`verify:\` are from $TASK_ID's own frontmatter, which is contract.
\`touches:\` is the set of paths the findings cite, each one re-checked
to exist at that commit and to fall inside what the reviewer was
allowed to look at. \`base:\` is the commit the reviewer actually had
in its worktree, not a branch tip that may since have moved.

The review is a set of *claims*. It has been checked for shape and for
whether its citations are real; it has not been checked for whether it
is right. That is your job, and disagreeing with it is a valid outcome.

# Notes

This task was generated. It lives under \`.state/proposed/\` because
that is the gate: a proposal becomes work when a human runs it. See
README F-21.
DERIVED_END

            board "handed-off" "$DERIVED_ID -> $TASK_ON_CHANGES"
            say "  handoff   $DERIVED_ID to $TASK_ON_CHANGES ($DERIVED_TASK)"
        fi
    fi
fi

# The no-op branch cleans up after itself below, so pointing a human at
# a worktree that is about to be deleted would be a lie.
if [ "$STATUS" = "blocked" ]; then
    NEXT="Human triages the blocker above. Task stays open; worktree kept at $WT."
elif [ "$STATUS" = "needs-review" ]; then
    NEXT="Human resolves the blocker above in $WT, then pushes and opens a PR."
elif [ "$TASK_PRODUCES" = "report" ] && [ -n "$DERIVED_TASK" ]; then
    NEXT="Read $ARTIFACT_FILE, then hand it on:

    labs/agent-bots/run/bot-run.sh --task $DERIVED_TASK

That task is scoped to the paths the findings cite and is based on the
commit that was reviewed. Running it is the acceptance - nothing is in
flight until a human (or --chain) starts it."
elif [ "$TASK_PRODUCES" = "report" ]; then
    NEXT="Read $ARTIFACT_FILE - verdict ${ARTIFACT_VERDICT:-(none)}. Nothing was changed and nothing can be merged from this run; acting on a finding is a new task for a bot that commits.${DERIVED_WHY:+
No task was derived: $DERIVED_WHY.}"
elif [ "${NOOP:-0}" -eq 1 ]; then
    NEXT="Nothing to review - the verifier already passed at base. Give this bot a task with real work in it."
else
    if [ -n "$PATCH_FILE" ]; then
        NEXT="Human reads $PATCH_FILE, checks it still fits with 'git apply --check --directory=. $PATCH_FILE' from $REPO, and applies it. $REPO is a plain folder: the snapshot in $ORIGIN_REPO is the only history there is."
    elif [ -n "$PUSHED" ]; then
        NEXT="Human reviews 'git -C $ORIGIN_REPO log --patch $TASK_BASE..$TASK_BRANCH', then opens a PR. $WT is a throwaway clone and can go."
    else
        NEXT="Human reviews $WT. Nothing was pushed back into $ORIGIN_REPO, so the surface is the only copy."
    fi
fi

# --------------------------------------------------------------------
# Handoff - written by the runner
# --------------------------------------------------------------------

# Appended to the last evidence line rather than given a line of its
# own, so a commit task does not carry a blank gap where a report would
# have been.
REPORT_EVIDENCE=""
if [ "$TASK_PRODUCES" = "report" ]; then
    REPORT_EVIDENCE="
    report:   ${ARTIFACT_FILE:-(none written)}
    verdict:  ${ARTIFACT_VERDICT:-(none)}
    subject:  $HEAD_SHA${DERIVED_TASK:+
    derived:  $DERIVED_ID -> $TASK_ON_CHANGES
              $DERIVED_TASK}"
fi

# Who this handoff is addressed to. Every run until now said "human",
# because there was nobody else to say. A derived task changes that: the
# handoff names the bot that gets it, and the artifact is the path to
# the task rather than a branch. The message carries a path.
# What this run actually produced. A report's branch is thrown away at
# cleanup, so naming it here would point the reader at something that
# is about to stop existing.
if [ "$TASK_PRODUCES" = "report" ]; then
    ARTIFACT="    report:   ${ARTIFACT_FILE:-(none written)}${DERIVED_TASK:+
    task:     $DERIVED_TASK}"
else
    ARTIFACT="    branch:   $TASK_BRANCH${PUSHED:+ (pushed into $ORIGIN_REPO)}${PUSH_FAILED:+ (ON THE SURFACE ONLY - the push failed)}
    surface:  $WT${PATCH_FILE:+
    patch:    $PATCH_FILE}"
fi

HANDOFF_TO="human"
[ -z "$DERIVED_TASK" ] || HANDOFF_TO="$TASK_ON_CHANGES"

HANDOFF="$STATE/handoffs/${TS}_${TASK_OWNER}__to__${HANDOFF_TO}__${TASK_ID}.md"
cat > "$HANDOFF" <<HANDOFF_END
---
task: $TASK_ID
produces: $TASK_PRODUCES
from: $TASK_OWNER
to: $HANDOFF_TO
at: $TS_ISO
status: $STATUS
---

# Objective

$TASK_TITLE

# Artifact

$ARTIFACT

# Evidence

    base:     $BASE_SHA
    head:     $HEAD_SHA
    commits:  $COMMITS
    numstat:  $NUMSTAT
    subjects:
$(if [ "$TREE_BROKEN" -eq 0 ] && [ "$COMMITS" -gt 0 ]; then git -C "$WT" log --format='      %h %s' "$BASE_SHA..HEAD"; else echo "      (no commits)"; fi)
    uncommit: $DIRTY file(s)
    committed: by the $COMMITTED_BY${COMMIT_MSG_FILE:+, message from $COMMIT_MSG_FILE}
    agent:    $AGENT_ID -> exit $AGENT_EXIT$([ "$SKIP_AGENT" -eq 1 ] && printf ' (skipped)')
    argv:     $AGENT_INVOCATION
    model:    ${TASK_MODEL:-(not pinned - whatever the CLI defaulted to)}
    reads:    $INSTR_STATUS
    shell:    $AGENT_SHELL_NOTE
    verify:   $TASK_VERIFY -> exit $VERIFY_EXIT
              (ran in a $VERIFY_WHERE, so this is evidence about the
               branch and not about the agent's leftovers)
    rebase:   $REBASE_STATE
    touched:
$(if [ -n "$TOUCHED" ]; then printf '%s\n' "$TOUCHED" | sed 's/^/      /'; else echo "      (none)"; fi)
    log:      $RUN_LOG
    prompt:   $PROMPT_FILE (via $AGENT_PROMPT_VIA)$REPORT_EVIDENCE

# Status

$STATUS

# Blockers

$BLOCKERS

# Next action

$NEXT
HANDOFF_END

set_status "$STATUS"
board "handoff" "$STATUS"

# --------------------------------------------------------------------
# Cleanup
# --------------------------------------------------------------------

# Two ways a surface stops being needed: a no-op left nothing on it,
# or the result is out and safe somewhere else. Anything else - blocked,
# needs-review, a push that failed - keeps it, because then the surface
# is the evidence and in one case the only copy.
CLEAN_WHY=""
if [ "$KEEP" -eq 1 ]; then
    CLEAN_WHY=""
elif [ "$STATUS" = "done" ] && [ "$TASK_PRODUCES" = "report" ] && [ "$DIRTY" -eq 0 ]; then
    # Zero commits is this task's success shape, so "no-op run" would be
    # the wrong words for the one line a reader is given about it.
    CLEAN_WHY="report harvested"
elif [ "$STATUS" = "done" ] && [ "$COMMITS" -eq 0 ] && [ "$DIRTY" -eq 0 ]; then
    CLEAN_WHY="no-op run"
elif [ "$STATUS" = "done" ] && [ -n "$PUSHED" ]; then
    # Otherwise every successful task leaves a full clone under
    # .worktrees for ever, while the handoff calls it throwaway.
    CLEAN_WHY="result pushed to $ORIGIN_REPO"
fi

if [ -n "$CLEAN_WHY" ]; then
    step "Cleanup"
    say "  $CLEAN_WHY, removing the work surface"
    CLEAN_FAILED=""
    drop_surface >>"$RUN_LOG" 2>&1 || CLEAN_FAILED="work surface $WT"

    if [ -n "$CLEAN_FAILED" ]; then
        # The handoff has already been written saying `done`, and it was
        # true of the work. It is not true of the tree: the next run for
        # this task will stop at the existing-path guard. Recording
        # `cleaned` and walking away would leave a live task claiming a
        # clean no-op while its worktree is still sitting there.
        say "  could not remove $CLEAN_FAILED"
        board "clean-failed" "$CLEAN_FAILED"
        STATUS="needs-review"
        set_status "$STATUS"
        {
            printf '\n# Cleanup failed\n\n'
            printf 'The run finished (%s), but %s could not be\n' "$CLEAN_WHY" "$CLEAN_FAILED"
            printf 'removed. Remove it before re-running this task:\n\n'
            printf '    rm -rf %s\n' "$WT"
        } >> "$HANDOFF"
    else
        board "cleaned"
    fi
fi

step "Result: $STATUS"
say "  handoff  $HANDOFF"
say "  board    $BOARD"
say "  log      $RUN_LOG"
[ -z "$DERIVED_TASK" ] || say "  next     $DERIVED_TASK"

# --------------------------------------------------------------------
# Chain
#
# Off by default, and that default is the point: a task written by a
# machine and started by a machine with nothing in between is a
# different risk class from one a human read first. In the product this
# is where the approval inbox goes. Here it is a flag, so the chain can
# be demonstrated without pretending the gate does not matter.
#
# A derived task always `produces: commit`, and a commit task never
# derives anything, so the chain is one link long by construction. The
# depth counter is belt and braces against that stopping being true.
# --------------------------------------------------------------------

if [ "$CHAIN" -eq 1 ] && [ -n "$DERIVED_TASK" ]; then
    depth="${BOT_CHAIN_DEPTH:-0}"
    if [ "$depth" -ge 3 ]; then
        say "  chain stopped at depth $depth"
    else
        step "Chain"
        say "  dispatching $DERIVED_ID to $TASK_ON_CHANGES"
        board "chained" "$DERIVED_ID"
        CHAIN_EXIT=0
        BOT_CHAIN_DEPTH=$((depth + 1))             bash "${BASH_SOURCE[0]}"                 --task "$DERIVED_TASK"                 --repo "$REPO"                 --state "$STATE"                 --worktree-root "$WORKTREE_ROOT" || CHAIN_EXIT=$?
        # The chain's outcome is the one a caller cares about, and this
        # run can only be `done` or it would not have got here.
        exit "$CHAIN_EXIT"
    fi
fi

# Distinct codes so a caller can branch without parsing the handoff.
case "$STATUS" in
    blocked)      exit 1 ;;
    needs-review) exit 2 ;;
    *)            exit 0 ;;
esac
