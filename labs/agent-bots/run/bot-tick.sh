#!/usr/bin/env bash
#
# bot-tick.sh - decide what should run now, say why, and run it.
#
# One tick reads the world, prints a decision for every task it can
# see, and dispatches the ones that are ready. It is the first thing in
# this design that *reads* the board rather than only appending to it.
#
# Usage:
#   bot-tick.sh [options]
#
#   --tasks <dir>        Where task definitions live. Repeatable.
#                        Default: <labs>/examples and <state>/proposed.
#   --state <dir>        Control plane. Default: <labs>/agent-bots/.state
#   --repo <dir>         Repo root. Default: the repo this script is in.
#   --max <n>            Dispatch at most n tasks this tick. Default 1.
#   --parallel <n>       Run up to n dispatches at once. Default 1.
#   --backoff <minutes>  Do not retry a blocked task sooner than this.
#                        Default 30.
#   --give-up <n>        Consecutive blocked runs after which a task is
#                        left to a human. Default 3.
#   --chain              Pass --chain to every dispatch.
#   --dry-run            Decide and explain. Dispatch nothing.
#   --watch <seconds>    Keep ticking. Ctrl-C to stop.
#   -h, --help           This text.
#
# What it asks the board, and what it does not
#
# The board is an append-only event log; status lives on the task file,
# whose single writer is that task's runner (README 3.2). A scheduler
# needs both kinds of fact and they come from different places:
#
#   what is true now   the live task under .state/tasks, plus whether
#                      its worktree is on disk. Never the board - a run
#                      that died mid-flight appended no closing row, so
#                      the log's last word about it is a lie by
#                      omission.
#   what has happened  the board, and only the board. Attempts,
#                      refusals, the shape of a failure repeating.
#                      Handoff files cannot answer this: a dispatch that
#                      was refused never produced one.
#   how long ago       file mtimes. The board's timestamps are ISO
#                      strings and turning those into epoch seconds
#                      portably is a worse problem than it looks;
#                      `find -mmin` is on both GNU and BSD.
#
# See README F-23.
#
# Exit code is the number of dispatches that did not end `done`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

die() { printf 'bot-tick: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

TASK_DIRS=()
STATE=""
# Whether the caller named one, which is a different fact from which
# one is in use. bot-run reads the *presence* of --state as proof that
# somebody coordinated the control plane on purpose, and so waives its
# refusal to run from a linked worktree on the default plane. Forwarding
# the option unconditionally made that proof automatic: two linked
# checkouts would each resolve their own default state directory, each
# be told it was deliberate, and dispatch overlapping work at each
# other. Forward the flag only when it was actually given.
STATE_EXPLICIT=""
REPO=""
MAX=1
PARALLEL=1
BACKOFF=30
GIVE_UP=3
CHAIN=""
DRY_RUN=0
WATCH=0

while [ $# -gt 0 ]; do
    case "$1" in
        --tasks)     TASK_DIRS+=("${2:-}"); shift 2 ;;
        --state)     STATE="${2:-}"; STATE_EXPLICIT=1; shift 2 ;;
        --repo)      REPO="${2:-}"; shift 2 ;;
        --max)       MAX="${2:-}"; shift 2 ;;
        --parallel)  PARALLEL="${2:-}"; shift 2 ;;
        --backoff)   BACKOFF="${2:-}"; shift 2 ;;
        --give-up)   GIVE_UP="${2:-}"; shift 2 ;;
        --chain)     CHAIN="--chain"; shift ;;
        --dry-run)   DRY_RUN=1; shift ;;
        --watch)     WATCH="${2:-}"; shift 2 ;;
        -h|--help)   usage; exit 0 ;;
        *)           die "unknown argument: $1 (try --help)" ;;
    esac
done

REPO="${REPO:-$(git -C "$LAB_DIR" rev-parse --show-toplevel)}"
STATE="${STATE:-$LAB_DIR/.state}"
mkdir -p "$STATE"
STATE="$(cd "$STATE" && pwd)"
# An array rather than `${VAR:+--state $STATE}`: a state directory is a
# path, and a path is allowed a space in it.
STATE_ARGS=()
[ -z "$STATE_EXPLICIT" ] || STATE_ARGS=(--state "$STATE")
BOARD="$STATE/board.md"

if [ "${#TASK_DIRS[@]}" -eq 0 ]; then
    TASK_DIRS=("$LAB_DIR/examples" "$STATE/proposed")
fi

# shellcheck source=approval.sh
. "$SCRIPT_DIR/approval.sh"

RUNNER="$SCRIPT_DIR/bot-run.sh"
[ -x "$RUNNER" ] || [ -f "$RUNNER" ] || die "no runner at $RUNNER"

# --------------------------------------------------------------------
# Reading
# --------------------------------------------------------------------

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
field() { field_from "$1" "$(cat "$2")"; }

# minutes <duration> - 90, 90m, 6h and 2d all become minutes. A
# schedule written in seconds is a schedule nobody meant.
minutes() {
    local v="$1" n
    case "$v" in
        *m) n="${v%m}"; printf '%s\n' "$n" ;;
        *h) n="${v%h}"; printf '%s\n' "$((n * 60))" ;;
        *d) n="${v%d}"; printf '%s\n' "$((n * 60 * 24))" ;;
        *)  printf '%s\n' "$v" ;;
    esac
}

# Rows the board holds for one task. The task id sits in its own
# column, so an exact field match cannot collide with a substring of
# another id - T-0006 and T-0006-fix are different tasks.
board_rows() {
    [ -f "$BOARD" ] || return 0
    grep -F "| $1 | " "$BOARD" 2>/dev/null || true
}

# The outcomes this task has reached, oldest first. Only the board has
# these: a dispatch that was refused never wrote a handoff.
board_outcomes() {
    board_rows "$1" | sed -n 's/^.*| handoff | \(.*\) |$/\1/p'
}

board_attempts() {
    board_rows "$1" | grep -c '| dispatched |' || true
}

# How many blocked runs in a row, counting back from the most recent.
consecutive_blocked() {
    printf '%s\n' "$(board_outcomes "$1")" | awk '
        NF { lines[++n] = $0 }
        END {
            c = 0
            for (i = n; i >= 1; i--) {
                if (lines[i] == "blocked") c++; else break
            }
            print c
        }
    '
}

# Is there a handoff for this task newer than <minutes>? mtime rather
# than the board timestamp, because turning ISO 8601 into epoch seconds
# portably is a worse problem than this one deserves.
handoff_within() {   # handoff_within <id> <minutes>
    [ -d "$STATE/handoffs" ] || return 1
    local hit
    hit="$(find "$STATE/handoffs" -name "*__$1.md" -mmin "-$2" 2>/dev/null)"
    [ -n "$hit" ]
}

# If this build of find rejects the age test, every recurrence check
# silently reads "not run recently" and every routine fires on every
# tick. Ask once and say so, rather than scheduling on a false answer.
if ! find "$STATE" -prune -mmin +1 -print >/dev/null 2>&1; then
    die "this build of find rejects '-prune -mmin', which every
'every:' schedule here depends on. Without it a routine would run on
every tick instead of on its interval."
fi

# --------------------------------------------------------------------
# Deciding
# --------------------------------------------------------------------

DISPATCHED=0
FAILURES=0

decide_and_run() {
    local dispatched_this_tick=0
    local inbox_count=0
    local -a to_dispatch=()
    local -a to_dispatch_reset=()

    printf '\n%-14s %-9s %-12s %4s  %-12s %s\n' \
        "TASK" "OWNER" "DECISION" "RUNS" "LAST" "WHY"
    printf '%s\n' "---------------------------------------------------------------------------------"

    local dir file id owner every schedule live status worktree decision why reset
    local inbox_why approved
    local attempts outcomes last_outcome blocked_streak
    for dir in ${TASK_DIRS[@]+"${TASK_DIRS[@]}"}; do
        [ -d "$dir" ] || continue
        for file in "$dir"/*.md; do
            [ -f "$file" ] || continue

            id="$(field id "$file")"
            [ -n "$id" ] || continue
            owner="$(field owner "$file")"
            every="$(field every "$file")"
            schedule="$(field schedule "$file")"
            # A recurrence is by definition unattended, so `every:`
            # implies it; anything else has to say so.
            [ -z "$every" ] || schedule="auto"
            schedule="${schedule:-manual}"

            live="$STATE/tasks/$id.md"
            reset=""
            if [ -f "$live" ]; then
                status="$(field status "$live")"
                worktree="$(field worktree "$live")"
            else
                status="$(field status "$file")"
                worktree=""
            fi

            # Read first, decide second. These come from the board and
            # nowhere else: a dispatch that was refused wrote no handoff,
            # so the run directory cannot answer "how often, and how did
            # it end".
            attempts="$(board_attempts "$id")"
            outcomes="$(board_outcomes "$id")"
            last_outcome="$(printf '%s\n' "$outcomes" | sed '/^[[:space:]]*$/d' | tail -n1)"
            last_outcome="${last_outcome:-never}"
            blocked_streak="$(consecutive_blocked "$id")"

            # A proposal was written by a bot, and the scheduler reads
            # the proposal directory alongside the repository's own
            # tasks - so without this, one bot's conclusion started
            # another bot's work with nobody in between. Worse than it
            # sounds: a derived task used to inherit `schedule:` from
            # the review that produced it, so a recurring review made a
            # recurring fix task.
            inbox_why=""
            approved=""
            if approval_is_proposal "$file" "$STATE"; then
                case "$(approval_state_file "$file")" in
                    ok)    approved=1 ;;
                    stale) inbox_why="approved, then edited - approve it again" ;;
                    *)     inbox_why="waiting for approval" ;;
                esac
            fi

            decision="ready"
            why="never run"

            if [ -n "$inbox_why" ]; then
                decision="inbox"
                why="$inbox_why"
                inbox_count=$((inbox_count + 1))
            elif [ "$schedule" != "auto" ] && [ -z "$approved" ]; then
                # The gate, and it is opt-in on purpose. A scheduler
                # that runs everything it can find will run the first
                # fixture somebody drops in the examples folder - and
                # this repo has three. A directory of task files is not
                # a queue.
                decision="manual"
                why="no schedule: auto"
            elif [ "$status" = "dispatched" ]; then
                if [ -n "$worktree" ] && [ -d "$worktree" ]; then
                    decision="in-flight"; why="worktree at $worktree"
                else
                    decision="stale"
                    why="says dispatched, no worktree - needs --reset by hand"
                fi
            elif [ -n "$worktree" ] && [ -d "$worktree" ]; then
                # A blocked run keeps its worktree on purpose - it is
                # the evidence a human is meant to look at. Marking the
                # task due anyway would have the next tick pass --reset,
                # hit the runner's existing-path guard, and fail; the
                # routine would spend every interval re-failing instead
                # of waiting for the cleanup it is asking for.
                decision="held"
                why="$status, worktree kept at $worktree"
            elif [ -n "$every" ]; then
                # A recurrence is decided by the clock, not by the live
                # task. Asking the status first got this wrong: clearing
                # the live task made a routine that had run minutes ago
                # look like one that had never run, because "no live
                # task" reads as `todo`. The interval is the question;
                # the status only says whether it is running right now.
                if handoff_within "$id" "$(minutes "$every")"; then
                    decision="waiting"; why="every: $every, last run more recent than that"
                else
                    decision="due"; why="every: $every elapsed"
                fi
            elif [ "$status" != "todo" ] && [ -n "$status" ]; then
                decision="$status"; why="terminal, no every:"
            fi

            if [ "$decision" = "ready" ] || [ "$decision" = "due" ]; then
                if [ "$attempts" -gt 0 ]; then
                    why="attempt $((attempts + 1))"
                    if [ -f "$live" ]; then reset="--reset"; fi
                fi
                if [ "$blocked_streak" -ge "$GIVE_UP" ]; then
                    decision="looping"
                    why="$blocked_streak blocked runs in a row - a human has to change something"
                elif [ "$blocked_streak" -gt 0 ] && handoff_within "$id" "$BACKOFF"; then
                    decision="cooling"
                    why="blocked $blocked_streak time(s), last one under ${BACKOFF}m ago"
                fi
            fi

            if [ "$decision" = "ready" ] || [ "$decision" = "due" ]; then
                if [ "$dispatched_this_tick" -ge "$MAX" ]; then
                    decision="queued"; why="--max $MAX reached this tick"
                else
                    dispatched_this_tick=$((dispatched_this_tick + 1))
                    to_dispatch+=("$file")
                    to_dispatch_reset+=("$reset")
                fi
            fi

            printf '%-14s %-9s %-12s %4s  %-12s %s\n' \
                "$id" "${owner:-?}" "$decision" "$attempts" "$last_outcome" "$why"
        done
    done

    # Ahead of "nothing to dispatch", because those are different
    # sentences and only one of them asks the reader for something. A
    # tick that ends quiet while three proposals sit waiting has not
    # finished; it is holding a queue nobody has been shown.
    if [ "$inbox_count" -gt 0 ]; then
        say ""
        say "inbox: $inbox_count proposal(s) waiting for approval"
        say "    bash $SCRIPT_DIR/bot-approve.sh"
    fi

    [ "${#to_dispatch[@]}" -gt 0 ] || { say ""; say "nothing to dispatch"; return 0; }

    if [ "$DRY_RUN" -eq 1 ]; then
        say ""
        say "dry run - would dispatch ${#to_dispatch[@]}:"
        local f
        for f in "${to_dispatch[@]}"; do say "  $f"; done
        return 0
    fi

    # --------------------------------------------------------------
    # Dispatching
    #
    # Concurrency here is the first real test of the dispatch lock
    # (F-17) and the board's append discipline (F-14): two runners, one
    # control plane, one branch namespace. A pair whose `touches:`
    # collide is refused by the second runner rather than by this one -
    # the check belongs where the claim is made, and duplicating it
    # here would give two answers to one question.
    # --------------------------------------------------------------
    say ""
    local i
    BATCH_PIDS=()
    BATCH_FILES=()
    for i in "${!to_dispatch[@]}"; do
        say "dispatch  $(basename "${to_dispatch[$i]}")"
        bash "$RUNNER" \
            --task "${to_dispatch[$i]}" \
            --repo "$REPO" \
            ${STATE_ARGS[@]+"${STATE_ARGS[@]}"} \
            ${to_dispatch_reset[$i]:+${to_dispatch_reset[$i]}} \
            ${CHAIN:+$CHAIN} \
            > "$STATE/runs/tick-$(basename "${to_dispatch[$i]}" .md).out" 2>&1 &
        BATCH_PIDS+=("$!")
        BATCH_FILES+=("${to_dispatch[$i]}")

        if [ "${#BATCH_PIDS[@]}" -ge "$PARALLEL" ]; then
            wait_for_batch
        fi
    done
    [ "${#BATCH_PIDS[@]}" -eq 0 ] || wait_for_batch
}

# Two globals rather than namerefs: `local -n` is bash 4.3, and F-15
# put this script on macOS, whose /bin/bash is 3.2.
BATCH_PIDS=()
BATCH_FILES=()

wait_for_batch() {
    local i rc status_word
    for i in "${!BATCH_PIDS[@]}"; do
        rc=0
        wait "${BATCH_PIDS[$i]}" || rc=$?
        case "$rc" in
            0) status_word="done" ;;
            1) status_word="blocked" ;;
            2) status_word="needs-review" ;;
            3) status_word="refused - nothing ran, try again" ;;
            *) status_word="runner error ($rc)" ;;
        esac
        say "  $(basename "${BATCH_FILES[$i]}" .md)  ->  $status_word"
        DISPATCHED=$((DISPATCHED + 1))
        # A refusal is not a failure: the task is untouched and the next
        # tick will find it exactly as it was.
        if [ "$rc" -ne 0 ] && [ "$rc" -ne 3 ]; then
            FAILURES=$((FAILURES + 1))
        fi
    done
    BATCH_PIDS=()
    BATCH_FILES=()
}

mkdir -p "$STATE/runs"

if [ "$WATCH" -gt 0 ]; then
    say "watching every ${WATCH}s - Ctrl-C to stop"
    while true; do
        say ""
        say "=== tick $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        decide_and_run
        sleep "$WATCH"
    done
else
    decide_and_run
    say ""
    say "dispatched $DISPATCHED, not done $FAILURES"
    exit "$FAILURES"
fi
