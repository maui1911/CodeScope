#!/usr/bin/env bash
#
# bot-forget.sh - retire the record of a finished run.
#
# The live task under `.state/tasks/<id>.md` is what the runner reads
# and writes, and it outlives the run on purpose: it is how a later
# dispatch knows the task already finished, and it is what a human
# looks at after something went wrong. Nothing removed it, though, and
# nothing offered to - so the only way to clear one was `rm`, and that
# is a poor interface for a directory whose whole point is being the
# record.
#
# The first real task in this lab is what made that visible. T-0010
# finished, wrote `status: done`, and the regression suite refused to
# run while its record existed (now fixed - see sweep.sh) - and the
# obvious way out was to delete the evidence of the only real run the
# loop had ever completed. A lab whose recovery procedure is `rm -rf`
# under a state directory will eventually take somebody's evidence with
# it. See README F-46.
#
# What this removes: the live task, and with --surface the work surface
# if one is still on disk. What it never removes: the handoff, the
# artifacts, the memory notes and the board. Those are the record of
# what happened; the live task is only the record of *where the runner
# got to*, and once a verdict is written the board and the handoff say
# everything it does.
#
#   bash labs/agent-bots/run/bot-forget.sh T-0010
#   bash labs/agent-bots/run/bot-forget.sh T-0010 --surface
#
#   <task-id>            The id, as it appears in the frontmatter.
#   --surface            Also remove the work surface, if present.
#   --force              Forget a task still marked `dispatched`.
#   --state <dir>        Control plane. Default: <labs>/agent-bots/.state
#   -h, --help           This text.
#
# Exit: 0 forgotten, 1 refused or nothing to forget.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

die() { printf 'bot-forget: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

TASK_ID=""
STATE=""
SURFACE=0
FORCE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --surface) SURFACE=1; shift ;;
        --force)   FORCE=1; shift ;;
        --state)   STATE="${2:-}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        -*)        die "unknown option: $1 (try --help)" ;;
        *)
            [ -z "$TASK_ID" ] || die "one task id at a time, got '$TASK_ID' and '$1'"
            TASK_ID="$1"; shift ;;
    esac
done

[ -n "$TASK_ID" ] || die "which task? (try --help)"
# The id becomes a path below, so it is checked the way the runner
# checks it rather than trusted for being short.
case "$TASK_ID" in
    *[!a-zA-Z0-9_-]*) die "task id must be [a-zA-Z0-9_-]+, got '$TASK_ID'" ;;
esac

# Same door as bot-approve.sh, and for the same reason: a run that can
# retire its own record can make its own history. The containment that
# matters is that the control plane is not in the agent's worktree;
# this is the part that catches the runner calling itself.
if [ -n "${BOT_RUN_ACTIVE:-}" ] || [ -n "${BOT_CHAIN_DEPTH:-}" ]; then
    die "refusing to forget a run from inside a bot run"
fi

STATE="${STATE:-$LAB_DIR/.state}"
[ -d "$STATE" ] || die "no control plane at $STATE - nothing has run yet"
STATE="$(cd "$STATE" && pwd)"

LIVE="$STATE/tasks/$TASK_ID.md"
[ -f "$LIVE" ] || die "no live task at $LIVE - already forgotten, or never ran"

field() {   # field <key> <file>
    sed -n "s/^$1:[[:space:]]*//p" "$2" | head -n1
}

STATUS="$(field status "$LIVE")"
WORKTREE="$(field worktree "$LIVE")"

# `dispatched` is the one status that might still be moving. Refusing
# is not caution for its own sake: the runner is the single writer of
# this file (README 3.2), and taking it out from under a live run makes
# `set_field` fail after the handoff is written, which ends the run with
# no status at all. That is the bug the sweep's preflight was built
# around.
if [ "$STATUS" = "dispatched" ] && [ "$FORCE" -eq 0 ]; then
    printf 'bot-forget: %s still says "dispatched".\n\n' "$TASK_ID" >&2
    printf 'Either a run is in flight, or one died without writing a verdict.\n' >&2
    printf 'Those look identical from here and they want opposite things, so\n' >&2
    printf 'look before deciding:\n\n' >&2
    if [ -n "$WORKTREE" ]; then
        if [ -d "$WORKTREE" ]; then
            printf '    surface still on disk: %s\n' "$WORKTREE" >&2
        else
            printf '    surface is gone: %s\n' "$WORKTREE" >&2
            printf '    (a clean run removes it last, so this one probably finished)\n' >&2
        fi
    fi
    printf '    board:  grep -F "| %s | " %s/board.md\n\n' "$TASK_ID" "$STATE" >&2
    printf 'Then --force if it really is dead.\n' >&2
    exit 1
fi

REMOVED_SURFACE=""
if [ "$SURFACE" -eq 1 ] && [ -n "$WORKTREE" ] && [ -d "$WORKTREE" ]; then
    # The same check the runner makes before it removes a surface, and
    # it is stronger than "a runner made this": the marker names the
    # task, so it answers "made for *this* run" rather than only
    # "made by something like me". A hijacked surface whose `.git` was
    # replaced has no marker and is refused - which is the right answer
    # here, since that is a thing to look at rather than to delete.
    grep -q "surface for $TASK_ID\$" "$WORKTREE/.git/bot-surface" 2>/dev/null \
        || die "$WORKTREE has no .git/bot-surface marker for $TASK_ID - refusing to remove it"
    rm -rf "$WORKTREE"
    REMOVED_SURFACE="$WORKTREE"
fi

rm -f "$LIVE"

# The board is append-only and this is an event: somebody decided a
# record had served its purpose. Same principle as --chain recording
# its own bypass - the value of a door is not that it cannot be opened,
# it is that opening it leaves a mark.
printf '| %s | %s | forgotten | %s |\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$TASK_ID" "was ${STATUS:-no status}" \
    >> "$STATE/board.md"

printf 'forgotten: %s (was %s)\n' "$TASK_ID" "${STATUS:-no status}"
[ -z "$REMOVED_SURFACE" ] || printf 'removed surface: %s\n' "$REMOVED_SURFACE"
printf 'kept: the handoff, the board and anything under artifacts/ or bots/\n'
