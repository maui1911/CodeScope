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
#   --worktree-root <d>  Where worktrees go. Default: <repo>.worktrees
#   --dry-run            Print the resolved plan and the prompt. Change
#                        nothing on disk.
#   --skip-agent         Full loop, but stub the agent call. Smoke-tests
#                        worktree + verifier + handoff on their own.
#   --keep               Keep the worktree even on a clean no-op run.
#   --reset              Discard the live task under state and start over.
#                        Needed to re-run a task that already finished.
#   --allow-overlap      Dispatch even though another in-flight task
#                        declares files this one also touches.
#   -h, --help           This text.
#
# Exit codes: 0 done, 1 blocked, 2 needs-review.
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
AGENT_EXIT=0

# Repo-relative, and deliberately a single variable: the prompt points
# the agent at these paths and the base check below proves they exist.
# Two copies of the same string would be free to drift.
CONTRACT_DIR="labs/agent-bots/contract"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

die() { printf 'bot-run: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

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

LOCKS_HELD=()

release_locks() {
    local d
    [ "${#LOCKS_HELD[@]}" -gt 0 ] || return 0
    for d in "${LOCKS_HELD[@]}"; do rmdir "$d" 2>/dev/null || true; done
    LOCKS_HELD=()
}
trap release_locks EXIT

take_lock() {   # take_lock <dir> <what>
    local dir="$1" what="$2" waited=0
    while ! mkdir "$dir" 2>/dev/null; do
        if [ -n "$(find "$dir" -maxdepth 0 -mmin +10 2>/dev/null)" ]; then
            say "  breaking a stale $what lock at $dir"
            rmdir "$dir" 2>/dev/null || true
            continue
        fi
        waited=$((waited + 1))
        [ "$waited" -le 300 ] || die "timed out waiting for the $what lock at $dir
Another run is holding it, or it was left behind. Remove it by hand if
no other run is in flight:
    rmdir $dir"
        sleep 0.2
    done
    LOCKS_HELD+=("$dir")
}

drop_lock() {   # drop_lock <dir>
    local dir="$1" d kept=()
    rmdir "$dir" 2>/dev/null || true
    if [ "${#LOCKS_HELD[@]}" -gt 0 ]; then
        for d in "${LOCKS_HELD[@]}"; do [ "$d" = "$dir" ] || kept+=("$d"); done
    fi
    LOCKS_HELD=("${kept[@]}")
}

while [ $# -gt 0 ]; do
    case "$1" in
        --task)          TASK="${2:-}"; shift 2 ;;
        --repo)          REPO="${2:-}"; shift 2 ;;
        --state)         STATE="${2:-}"; shift 2 ;;
        --worktree-root) WORKTREE_ROOT="${2:-}"; shift 2 ;;
        --dry-run)       DRY_RUN=1; shift ;;
        --skip-agent)    SKIP_AGENT=1; shift ;;
        --keep)          KEEP=1; shift ;;
        --reset)         RESET=1; shift ;;
        --allow-overlap) ALLOW_OVERLAP=1; shift ;;
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
[ -d "$REPO/.git" ] || git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 \
    || die "not a git repo: $REPO"

STATE="${STATE:-$LAB_DIR/.state}"
WORKTREE_ROOT="${WORKTREE_ROOT:-${REPO}.worktrees}"


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
    field_from "$1" "$(git -C "$REPO" show "$BASE_SHA:$CONTRACT_DIR/$2" 2>/dev/null || true)"
}

# base_has <contract-relative-path> - does the contract file exist at
# the base commit at all.
base_has() {
    git -C "$REPO" cat-file -e "$BASE_SHA:$CONTRACT_DIR/$1" 2>/dev/null
}

TASK_ID="$(field id)"
TASK_TITLE="$(field title)"
TASK_OWNER="$(field owner)"
TASK_STATUS="$(field status)"
TASK_BASE="$(field base)"
TASK_BRANCH="$(field branch)"
TASK_TOUCHES="$(field touches)"
TASK_VERIFY="$(field verify)"

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

BASE_SHA="$(git -C "$REPO" rev-parse --verify "$TASK_BASE^{commit}" 2>/dev/null)" \
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

if [ -n "$TASK_MODEL" ] && [ -z "$AGENT_MODEL_FLAG" ]; then
    die "task pins model '$TASK_MODEL' but profile '$AGENT_ID' has no model_flag"
fi

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
    for g in "${raw[@]}"; do
        g="${g#"${g%%[![:space:]]*}"}"
        g="${g%"${g##*[![:space:]]}"}"
        [ -n "$g" ] && GLOBS_OUT+=("$g")
    done
}

BASE_FILES="$(git -C "$REPO" ls-tree -r --name-only "$BASE_SHA")"

# expand_globs <comma-separated> - the paths at base those globs match.
expand_globs() {
    local file g
    local -a globs
    split_globs "$1"
    globs=("${GLOBS_OUT[@]}")
    [ "${#globs[@]}" -gt 0 ] || return 0
    while IFS= read -r file; do
        for g in "${globs[@]}"; do
            # shellcheck disable=SC2254  # a glob, on purpose
            case "$file" in $g) printf '%s\n' "$file"; break ;; esac
        done
    done <<< "$BASE_FILES"
}

MY_FILES="$(expand_globs "$TASK_TOUCHES" | sort -u)"
split_globs "$TASK_TOUCHES"
MY_GLOBS=("${GLOBS_OUT[@]}")

OVERLAP_REPORT=""
OVERLAP_IDS=""
STALE_DISPATCH=""

# A function because it has to run twice: once before the plan, so a
# human (and --dry-run) can see the collision, and once again inside the
# dispatch lock, where it is the answer that actually counts. See F-17.
scan_overlaps() {
OVERLAP_REPORT=""
OVERLAP_IDS=""
STALE_DISPATCH=""
if [ -d "$STATE/tasks" ]; then
    for live in "$STATE"/tasks/*.md; do
        [ -f "$live" ] || continue

        other_id="$(field id "$live")"
        [ -n "$other_id" ] || continue
        [ "$other_id" != "$TASK_ID" ] || continue
        [ "$(field status "$live")" = "dispatched" ] || continue

        other_branch="$(field branch "$live")"
        other_wt="$WORKTREE_ROOT/$(printf '%s' "$other_branch" | tr '/' '-')"
        if [ ! -d "$other_wt" ]; then
            # Status says in flight; the filesystem says the run is
            # over. A dispatch that died is not holding anything, and
            # blocking every later task on a ghost would be worse than
            # saying so out loud.
            STALE_DISPATCH="$STALE_DISPATCH $other_id"
            continue
        fi

        other_touches="$(field touches "$live")"
        other_files="$(expand_globs "$other_touches" | sort -u)"

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
        for g in "${GLOBS_OUT[@]}"; do
            for mine in "${MY_GLOBS[@]}"; do
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
    if [ "$ALLOW_OVERLAP" -eq 1 ]; then
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

Rules for this run:
  - You are already on branch '$TASK_BRANCH'. Do not switch branches.
  - Edit only files matching: $TASK_TOUCHES
  - The verifier is: $TASK_VERIFY
    It must exit 0. Run it before you edit anything, and again after.
  - Commit your work to this branch. Exactly one commit, message in English.
  - Do NOT push, do NOT open a PR, do NOT run cargo fmt.
  - Do NOT edit anything under $CONTRACT_DIR/.
  - Do NOT write a handoff or a report file. The runner does that.

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
# The display copy exists so the plan and the evidence can name the
# invocation without reprinting the whole prompt.
# --------------------------------------------------------------------

AGENT_ARGV=()
AGENT_ARGV_DISPLAY=()
for tok in $AGENT_HEADLESS; do
    if [ "$tok" = "{prompt}" ]; then
        AGENT_ARGV+=("$PROMPT"); AGENT_ARGV_DISPLAY+=("<prompt>")
    else
        AGENT_ARGV+=("$tok"); AGENT_ARGV_DISPLAY+=("$tok")
    fi
done
for tok in $AGENT_AUTONOMY; do
    AGENT_ARGV+=("$tok"); AGENT_ARGV_DISPLAY+=("$tok")
done
if [ -n "$TASK_MODEL" ]; then
    AGENT_ARGV+=("$AGENT_MODEL_FLAG" "$TASK_MODEL")
    AGENT_ARGV_DISPLAY+=("$AGENT_MODEL_FLAG" "$TASK_MODEL")
fi
AGENT_INVOCATION="$AGENT_CMD ${AGENT_ARGV_DISPLAY[*]}"

# --------------------------------------------------------------------
# Plan
# --------------------------------------------------------------------

step "Plan"
cat <<PLAN
  task      $TASK_ID  $TASK_TITLE
  owner     $TASK_OWNER
  repo      $REPO
  base      $TASK_BASE @ ${BASE_SHA:0:12}
  branch    $TASK_BRANCH
  worktree  $WT
  touches   $TASK_TOUCHES
  overlap   $OVERLAP_SUMMARY
  verify    $TASK_VERIFY
  state     $STATE
  agent     $AGENT_ID, profile verified $AGENT_VERIFIED
  profile   $PROFILE
  argv      $AGENT_INVOCATION
  reads     ${AGENT_INSTRUCTION_FILES:-(none declared)}
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
set_status() {
    awk -v s="$1" '
        !done && index($0, "status:") == 1 { print "status: " s; done = 1; next }
        { print }
    ' "$LIVE_TASK" > "$LIVE_TASK.tmp" && mv "$LIVE_TASK.tmp" "$LIVE_TASK"
}

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

if [ -n "$OVERLAP_IDS" ]; then
    if [ "$ALLOW_OVERLAP" -eq 1 ]; then
        board "overlap-allowed" "${OVERLAP_IDS# }"
    else
        board "dispatch-refused" "overlaps ${OVERLAP_IDS# }"
        die "another in-flight task already claims files this one touches.

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

    git -C $REPO worktree remove --force $VERIFY_WT"
fi

mkdir -p "$WORKTREE_ROOT"
git -C "$REPO" worktree add "$WT" -b "$TASK_BRANCH" "$BASE_SHA" >>"$RUN_LOG" 2>&1 \
    || { board "dispatch-failed" "$TASK_BRANCH"; die "git worktree add failed; see $RUN_LOG"; }
say "  created $WT"

# Only now, with a worktree that actually exists, does the live task
# come into being. A dispatch that fails must not leave one behind.
if [ "$RESET_PENDING" -eq 1 ]; then
    rm -f "$LIVE_TASK"
    say "  reset: discarded the previous live task"
fi
cp "$TASK" "$LIVE_TASK"
set_status dispatched
board "dispatched" "$TASK_BRANCH @ ${BASE_SHA:0:12}"

# The claim is now visible to every other runner, so the lock has done
# its job. Holding it through the agent run would serialise the bots
# themselves, which is the opposite of the point.
drop_lock "$STATE/dispatch.lock"

# --------------------------------------------------------------------
# Agent
# --------------------------------------------------------------------

step "Agent"
if [ "$SKIP_AGENT" -eq 1 ]; then
    say "  skipped (--skip-agent)"
    board "agent-skipped"
else
    say "  $AGENT_INVOCATION (output -> $RUN_LOG)"
    # stdin is closed on purpose: a headless agent that decides to
    # prompt would otherwise inherit the runner's terminal and hang.
    ( cd "$WT" && "$AGENT_CMD" "${AGENT_ARGV[@]}" ) \
        </dev/null >>"$RUN_LOG" 2>&1 || AGENT_EXIT=$?
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
    for f in "${instr_list[@]}"; do
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

# A worktree the agent wrecked must still produce a handoff, so none of
# these may take the script down under `set -e`.
HEAD_SHA="$(git -C "$WT" rev-parse HEAD 2>/dev/null)" || HEAD_SHA=""
if [ -z "$HEAD_SHA" ]; then
    HEAD_SHA="(unreadable)"
    COMMITS=0
    DIRTY=0
    TOUCHED=""
    NUMSTAT="worktree unreadable"
    TODO_VIOLATIONS=""
    TREE_BROKEN=1
else
    TREE_BROKEN=0
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

# Scope check: every committed file must match a declared glob.
SCOPE_VIOLATIONS=""
if [ -n "$TOUCHED" ]; then
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        ok=0
        IFS=',' read -ra globs <<< "$TASK_TOUCHES"
        for g in "${globs[@]}"; do
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

step "Verifier"
VERIFY_EXIT=0
VERIFY_WHERE="(not run)"
VERIFY_WT_LEFTOVER=""
TREE_MUTATED=""
if [ "$TREE_BROKEN" -eq 1 ]; then
    say "  skipped - worktree unreadable"
    VERIFY_EXIT=-1
    board "verify-skipped" "worktree unreadable"
elif ! git -C "$REPO" worktree add --detach "$VERIFY_WT" "$HEAD_SHA" >>"$RUN_LOG" 2>&1; then
    say "  skipped - could not create the verify checkout at $VERIFY_WT"
    VERIFY_EXIT=-1
    VERIFY_WHERE="(verify checkout failed)"
    board "verify-skipped" "verify checkout failed"
else
    VERIFY_WHERE="clean checkout of ${HEAD_SHA:0:12}"
    PRE_STATE="$(tree_state)"
    say "  $TASK_VERIFY"
    say "  in $VERIFY_WT"
    # The one tool-specific line in the runner, and it is a cost
    # decision, not a semantic one: a fresh checkout would otherwise
    # rebuild every dependency per run. The cache lives under state, so
    # it belongs to the runner - an agent cannot seed it from its own
    # worktree - and cargo's own lock serialises concurrent runs.
    ( cd "$VERIFY_WT" \
        && export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$STATE/cache/target}" \
        && eval "$TASK_VERIFY" ) </dev/null >>"$RUN_LOG" 2>&1 || VERIFY_EXIT=$?
    say "  exit $VERIFY_EXIT"
    board "verified" "exit $VERIFY_EXIT"

    # A checkout that will not go away is not cosmetic: the next run for
    # this task hits the leftover guard and refuses to start. It has to
    # reach the verdict, not just the console.
    if ! git -C "$REPO" worktree remove --force "$VERIFY_WT" >>"$RUN_LOG" 2>&1; then
        VERIFY_WT_LEFTOVER="$VERIFY_WT"
        say "  could not remove $VERIFY_WT"
    fi

    POST_STATE="$(tree_state)"
    if [ "$POST_STATE" != "$PRE_STATE" ]; then
        POST_HEAD="$(git -C "$WT" rev-parse HEAD 2>/dev/null)" || POST_HEAD="(unreadable)"
        POST_DIRTY="$(git -C "$WT" status --porcelain | wc -l | tr -d ' ')"
        TREE_MUTATED="verifier changed the tree it was measuring.
    head        $HEAD_SHA -> $POST_HEAD
    uncommitted $DIRTY -> $POST_DIRTY file(s)
    state       ${PRE_STATE:0:12} -> ${POST_STATE:0:12}
Head and the file count can both be unchanged and the contents still
differ; the state hash is what caught it."
    fi
fi

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
elif [ -n "$AGENT_BLOCKED" ]; then
    STATUS="blocked"
    BLOCKERS="agent reported blocked: $AGENT_BLOCKED"
elif [ "$AGENT_EXIT" -ne 0 ]; then
    STATUS="blocked"
    BLOCKERS="agent exited $AGENT_EXIT without reporting - see $RUN_LOG"
elif [ "$VERIFY_EXIT" -ne 0 ]; then
    STATUS="blocked"
    BLOCKERS="verifier exited $VERIFY_EXIT - see $RUN_LOG"
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
    git -C $REPO worktree remove --force $VERIFY_WT_LEFTOVER"
elif [ "$DIRTY" -ne 0 ]; then
    STATUS="needs-review"
    BLOCKERS="$DIRTY uncommitted file(s) left in the worktree"
elif [ "$COMMITS" -gt 1 ]; then
    # BOT.md acceptance criterion 3. The work may well be fine; a human
    # decides whether to squash.
    STATUS="needs-review"
    BLOCKERS="$COMMITS commits, charter asks for exactly one"
elif [ "$COMMITS" -eq 0 ]; then
    STATUS="done"
    NOOP=1
    BLOCKERS="none - no-op run, nothing needed changing"
else
    STATUS="done"
fi

# The no-op branch cleans up after itself below, so pointing a human at
# a worktree that is about to be deleted would be a lie.
if [ "$STATUS" = "blocked" ]; then
    NEXT="Human triages the blocker above. Task stays open; worktree kept at $WT."
elif [ "$STATUS" = "needs-review" ]; then
    NEXT="Human resolves the blocker above in $WT, then pushes and opens a PR."
elif [ "${NOOP:-0}" -eq 1 ]; then
    NEXT="Nothing to review - the verifier already passed at base. Give this bot a task with real work in it."
else
    NEXT="Human reviews $WT, then pushes and opens a PR."
fi

# --------------------------------------------------------------------
# Handoff - written by the runner
# --------------------------------------------------------------------

HANDOFF="$STATE/handoffs/${TS}_${TASK_OWNER}__to__human__${TASK_ID}.md"
cat > "$HANDOFF" <<HANDOFF_END
---
task: $TASK_ID
from: $TASK_OWNER
to: human
at: $TS_ISO
status: $STATUS
---

# Objective

$TASK_TITLE

# Artifact

    branch:   $TASK_BRANCH
    worktree: $WT

# Evidence

    base:     $BASE_SHA
    head:     $HEAD_SHA
    commits:  $COMMITS
    numstat:  $NUMSTAT
    uncommit: $DIRTY file(s)
    agent:    $AGENT_ID -> exit $AGENT_EXIT$([ "$SKIP_AGENT" -eq 1 ] && printf ' (skipped)')
    argv:     $AGENT_INVOCATION
    model:    ${TASK_MODEL:-(not pinned - whatever the CLI defaulted to)}
    reads:    $INSTR_STATUS
    verify:   $TASK_VERIFY -> exit $VERIFY_EXIT
              (ran in a $VERIFY_WHERE, so this is evidence about the
               branch and not about the agent's leftovers)
    touched:
$(if [ -n "$TOUCHED" ]; then printf '%s\n' "$TOUCHED" | sed 's/^/      /'; else echo "      (none)"; fi)
    log:      $RUN_LOG

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

if [ "$STATUS" = "done" ] && [ "$COMMITS" -eq 0 ] && [ "$DIRTY" -eq 0 ] && [ "$KEEP" -eq 0 ]; then
    step "Cleanup"
    say "  no-op run, removing worktree"
    git -C "$REPO" worktree remove "$WT" >>"$RUN_LOG" 2>&1 \
        || git -C "$REPO" worktree remove --force "$WT" >>"$RUN_LOG" 2>&1 \
        || say "  could not remove $WT - remove it by hand"
    git -C "$REPO" branch -D "$TASK_BRANCH" >>"$RUN_LOG" 2>&1 || true
    board "cleaned"
fi

step "Result: $STATUS"
say "  handoff  $HANDOFF"
say "  board    $BOARD"
say "  log      $RUN_LOG"

# Distinct codes so a caller can branch without parsing the handoff.
case "$STATUS" in
    blocked)      exit 1 ;;
    needs-review) exit 2 ;;
    *)            exit 0 ;;
esac
