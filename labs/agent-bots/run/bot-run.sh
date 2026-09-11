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
#   -h, --help           This text.
#
# Exit codes: 0 done, 1 blocked, 2 needs-review.
#
# Environment:
#   BOT_AGENT_CMD    Agent executable.        Default: claude
#   BOT_AGENT_ARGS   Extra args, word-split.  Default: --permission-mode auto
#
# The default args target Claude Code headless mode; check them against
# your installed CLI version. `auto` and not `acceptEdits`: the prompt
# asks the agent to run the verifier and to commit, which are Bash
# calls, and acceptEdits covers edits only. An unattended run under
# acceptEdits cannot do its job - it produces no commit, which is
# exactly the shape F-4 describes.
#
# Be honest about what that buys: the worktree bounds what the agent is
# *meant* to touch, not what it *can*. It is a work surface, not a
# security boundary - the same thing this design criticises Grok Bot
# for. What actually contains the blast radius is that the branch is
# throwaway and nothing here ever pushes.

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
BOT_AGENT_ARGS="${BOT_AGENT_ARGS:---permission-mode auto}"

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

# Owner sanity. This name is spliced into filesystem paths below, so it
# has to be a plain identifier - not a traversal.
case "$TASK_OWNER" in
    *[!a-zA-Z0-9_-]*|"") die "owner must be [a-zA-Z0-9_-]+, got '$TASK_OWNER'" ;;
esac
case "$TASK_ID" in
    *[!a-zA-Z0-9_-]*|"") die "task id must be [a-zA-Z0-9_-]+, got '$TASK_ID'" ;;
esac

# The runner's own checkout, used only for a friendly "unknown owner"
# error. The charter that actually governs the run is the one at the
# base commit, checked below - these are different trees.
BOT_DIR="$LAB_DIR/contract/bots/$TASK_OWNER"
[ -f "$BOT_DIR/BOT.md" ] || die "no charter for owner '$TASK_OWNER' at $BOT_DIR/BOT.md"

BASE_SHA="$(git -C "$REPO" rev-parse --verify "$TASK_BASE^{commit}" 2>/dev/null)" \
    || die "cannot resolve base ref '$TASK_BASE' (fetch first?)"

# F-3: the agent reads the contract from *its* checkout, which is the
# base commit - never from the runner's working tree. A contract that
# is not on the base means the prompt points at files that do not
# exist. Fail here, before a worktree and an agent run are spent on it.
# Same shape as F-1: prove the precondition at base, not halfway.
for rel in "bots/$TASK_OWNER/BOT.md" \
           "context/CONVENTIONS.md" \
           "context/ARCHITECTURE.md" \
           "context/GLOSSARY.md"; do
    git -C "$REPO" cat-file -e "$BASE_SHA:$CONTRACT_DIR/$rel" 2>/dev/null || die \
"contract file missing at base '$TASK_BASE': $CONTRACT_DIR/$rel

The agent reads its contract from the base commit, not from this
checkout. Commit the contract, then point the task's 'base:' at a ref
that contains it."
done

WT_LEAF="$(printf '%s' "$TASK_BRANCH" | tr '/' '-')"
WT="$WORKTREE_ROOT/$WT_LEAF"

# --------------------------------------------------------------------
# Live task
#
# The repo copy is a *definition*; this is the live task. Status is read
# from here when it exists, so a re-run cannot be waved through by a
# definition that still says todo (which is what the first version did -
# it read the definition and then overwrote the live copy on top).
# --------------------------------------------------------------------

LIVE_TASK="$STATE/tasks/$TASK_ID.md"

if [ "$RESET" -eq 1 ] && [ -f "$LIVE_TASK" ]; then
    rm -f "$LIVE_TASK"
    say "reset: discarded live task $TASK_ID"
fi

EFFECTIVE_STATUS="$TASK_STATUS"
if [ -f "$LIVE_TASK" ]; then
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

# Each row carries its own clock. Stamping every row of a run with the
# run's start time made the log unsortable the moment two runs overlap.
board() {
    printf '| %s | %s | %s | %s |\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$TASK_ID" "$1" "${2:--}" >> "$BOARD"
}

set_status() {
    sed -i "0,/^status:.*$/s//status: $1/" "$LIVE_TASK"
}

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

mkdir -p "$WORKTREE_ROOT"
git -C "$REPO" worktree add "$WT" -b "$TASK_BRANCH" "$BASE_SHA" >>"$RUN_LOG" 2>&1 \
    || { board "dispatch-failed" "$TASK_BRANCH"; die "git worktree add failed; see $RUN_LOG"; }
say "  created $WT"

# Only now, with a worktree that actually exists, does the live task
# come into being. A dispatch that fails must not leave one behind.
cp "$TASK" "$LIVE_TASK"
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
    # stdin is closed on purpose: a headless agent that decides to
    # prompt would otherwise inherit the runner's terminal and hang.
    # shellcheck disable=SC2086  # BOT_AGENT_ARGS is word-split on purpose
    ( cd "$WT" && "$BOT_AGENT_CMD" -p "$PROMPT" $BOT_AGENT_ARGS ) \
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
AGENT_BLOCKED=""
if [ -f "$WT/.bot-blocked" ]; then
    AGENT_BLOCKED="$(tr -d '\r' < "$WT/.bot-blocked" | head -c 2000)"
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

step "Verifier"
VERIFY_EXIT=0
if [ "$TREE_BROKEN" -eq 1 ]; then
    say "  skipped - worktree unreadable"
    VERIFY_EXIT=-1
    board "verify-skipped" "worktree unreadable"
else
    say "  $TASK_VERIFY"
    # NOTE: this runs on the working tree, not on HEAD, so an
    # uncommitted edit can be what makes it pass. The handoff says so.
    ( cd "$WT" && eval "$TASK_VERIFY" ) </dev/null >>"$RUN_LOG" 2>&1 || VERIFY_EXIT=$?
    say "  exit $VERIFY_EXIT"
    board "verified" "exit $VERIFY_EXIT"
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
    agent:    exit $AGENT_EXIT$([ "$SKIP_AGENT" -eq 1 ] && printf ' (skipped)')
    verify:   $TASK_VERIFY -> exit $VERIFY_EXIT
              (run on the working tree, not on head - an uncommitted
               edit can be what makes it pass)
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
