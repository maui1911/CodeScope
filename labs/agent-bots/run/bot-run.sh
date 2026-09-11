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
#   -h, --help           This text.
#
# Environment:
#   BOT_AGENT_CMD    Agent executable.        Default: claude
#   BOT_AGENT_ARGS   Extra args, word-split.  Default: --permission-mode acceptEdits
#
# The default args target Claude Code headless mode. Check them against
# your installed CLI version before the first real run - a wrong flag
# here fails loudly, but it fails after the worktree is created.

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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

die() { printf 'bot-run: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --task)          TASK="${2:-}"; shift 2 ;;
        --repo)          REPO="${2:-}"; shift 2 ;;
        --state)         STATE="${2:-}"; shift 2 ;;
        --worktree-root) WORKTREE_ROOT="${2:-}"; shift 2 ;;
        --dry-run)       DRY_RUN=1; shift ;;
        --skip-agent)    SKIP_AGENT=1; shift ;;
        --keep)          KEEP=1; shift ;;
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

BOT_AGENT_CMD="${BOT_AGENT_CMD:-claude}"
BOT_AGENT_ARGS="${BOT_AGENT_ARGS:---permission-mode acceptEdits}"

# --------------------------------------------------------------------
# Task frontmatter
#
# Deliberately a flat `key: value` block, not real YAML. A prototype
# that needs a YAML parser to read its own task files has already lost
# the plot; `touches` is comma-separated for the same reason.
# --------------------------------------------------------------------

field() {
    sed -n '/^---$/,/^---$/p' "$TASK" \
        | sed -n "s/^$1:[[:space:]]*//p" \
        | head -n1
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
[ "$TASK_STATUS" = "todo" ] \
    || die "task status is '$TASK_STATUS', expected 'todo' (reset it to re-run)"

BOT_DIR="$LAB_DIR/contract/bots/$TASK_OWNER"
[ -f "$BOT_DIR/BOT.md" ] || die "no charter for owner '$TASK_OWNER' at $BOT_DIR/BOT.md"

BASE_SHA="$(git -C "$REPO" rev-parse --verify "$TASK_BASE^{commit}" 2>/dev/null)" \
    || die "cannot resolve base ref '$TASK_BASE' (fetch first?)"

WT_LEAF="$(printf '%s' "$TASK_BRANCH" | tr '/' '-')"
WT="$WORKTREE_ROOT/$WT_LEAF"

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
  1. labs/agent-bots/contract/bots/$TASK_OWNER/BOT.md - your charter
  2. labs/agent-bots/contract/context/CONVENTIONS.md - hard rules
  3. labs/agent-bots/contract/context/ARCHITECTURE.md
  4. labs/agent-bots/contract/context/GLOSSARY.md

Your task, verbatim:
---
$(cat "$TASK")
---

Rules for this run:
  - You are already on branch '$TASK_BRANCH'. Do not switch branches.
  - Edit only files matching: $TASK_TOUCHES
  - The verifier is: $TASK_VERIFY
    It must exit 0. Run it before you edit anything, and again after.
  - Commit your work to this branch. One commit, message in English.
  - Do NOT push, do NOT open a PR, do NOT run cargo fmt.
  - Do NOT edit anything under labs/agent-bots/contract/.
  - Do NOT write a handoff or a report file. The runner does that.

If the task cannot be completed inside those limits, make no changes
and say why. Stopping is a valid outcome; guessing is not."

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
  verify    $TASK_VERIFY
  state     $STATE
  agent     $BOT_AGENT_CMD $BOT_AGENT_ARGS
PLAN

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

LIVE_TASK="$STATE/tasks/$TASK_ID.md"
RUN_LOG="$STATE/runs/$DAY/$TASK_ID-$TS.log"
BOARD="$STATE/board.md"

if [ ! -f "$BOARD" ]; then
    {
        echo "# Board"
        echo
        echo "Append-only event log. Never edit a line that is already here -"
        echo "task status lives on the task file, this is the audit trail."
        echo
        echo "| when | task | event | ref |"
        echo "|---|---|---|---|"
    } > "$BOARD"
fi

board() { printf '| %s | %s | %s | %s |\n' "$TS" "$TASK_ID" "$1" "${2:--}" >> "$BOARD"; }

cp "$TASK" "$LIVE_TASK"
set_status() {
    sed -i "0,/^status:.*$/s//status: $1/" "$LIVE_TASK"
}

# --------------------------------------------------------------------
# Worktree
# --------------------------------------------------------------------

step "Worktree"
[ -e "$WT" ] && die "worktree path already exists: $WT (remove it, or bump the task id)"

mkdir -p "$WORKTREE_ROOT"
git -C "$REPO" worktree add "$WT" -b "$TASK_BRANCH" "$BASE_SHA" >>"$RUN_LOG" 2>&1 \
    || { board "dispatch-failed" "$TASK_BRANCH"; die "git worktree add failed; see $RUN_LOG"; }
say "  created $WT"

set_status dispatched
board "dispatched" "$TASK_BRANCH @ ${BASE_SHA:0:12}"

# --------------------------------------------------------------------
# Agent
# --------------------------------------------------------------------

step "Agent"
if [ "$SKIP_AGENT" -eq 1 ]; then
    say "  skipped (--skip-agent)"
    board "agent-skipped"
else
    say "  $BOT_AGENT_CMD (output -> $RUN_LOG)"
    AGENT_EXIT=0
    # shellcheck disable=SC2086  # BOT_AGENT_ARGS is word-split on purpose
    ( cd "$WT" && "$BOT_AGENT_CMD" -p "$PROMPT" $BOT_AGENT_ARGS ) \
        >>"$RUN_LOG" 2>&1 || AGENT_EXIT=$?
    say "  exit $AGENT_EXIT"
    board "agent-ran" "exit $AGENT_EXIT"
fi

# --------------------------------------------------------------------
# Evidence - read off the tree, never off the agent's summary
# --------------------------------------------------------------------

step "Evidence"

HEAD_SHA="$(git -C "$WT" rev-parse HEAD)"
COMMITS="$(git -C "$WT" rev-list --count "$BASE_SHA..HEAD")"
DIRTY="$(git -C "$WT" status --porcelain | wc -l | tr -d ' ')"
TOUCHED="$(git -C "$WT" diff --name-only "$BASE_SHA..HEAD")"
NUMSTAT="$(git -C "$WT" diff --shortstat "$BASE_SHA..HEAD" | sed 's/^ *//')"
[ -n "$NUMSTAT" ] || NUMSTAT="no committed changes"

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

step "Verifier"
say "  $TASK_VERIFY"
VERIFY_EXIT=0
( cd "$WT" && eval "$TASK_VERIFY" ) >>"$RUN_LOG" 2>&1 || VERIFY_EXIT=$?
say "  exit $VERIFY_EXIT"
board "verified" "exit $VERIFY_EXIT"

# --------------------------------------------------------------------
# Verdict
# --------------------------------------------------------------------

BLOCKERS="none"
NOOP=0
if [ "$VERIFY_EXIT" -ne 0 ]; then
    STATUS="blocked"
    BLOCKERS="verifier exited $VERIFY_EXIT - see $RUN_LOG"
elif [ -n "$SCOPE_VIOLATIONS" ]; then
    STATUS="blocked"
    BLOCKERS="files changed outside touches:"$'\n'"$SCOPE_VIOLATIONS"
elif [ "$DIRTY" -ne 0 ]; then
    STATUS="needs-review"
    BLOCKERS="$DIRTY uncommitted file(s) left in the worktree"
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
    verify:   $TASK_VERIFY -> exit $VERIFY_EXIT
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

[ "$STATUS" = "blocked" ] && exit 1
exit 0
