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
# What this removes: the live task, and with --surface the work
# surface, the verify checkout beside it and the base pin in the
# project. What it never removes *from*: the handoff, the artifacts,
# the memory notes and the board - it appends a `forgotten` row to the
# board rather than editing it, because retiring a record is itself an
# event and the board is append-only either way.
#
# The live task is only the record of *where the runner got to*; once a
# verdict is written, the handoff and the board say everything it does.
#
#   bash labs/agent-bots/run/bot-forget.sh T-0010
#   bash labs/agent-bots/run/bot-forget.sh T-0010 --surface
#
#   <task-id>            The id, as it appears in the frontmatter.
#   --surface            Also remove the work surface, if present -
#                        the verify checkout beside it and the base pin
#                        in the project go with it, because that is
#                        what the runner's own removal does.
#   --force              Forget a record whose run has not reached a
#                        verdict, or one whose process is still alive.
#   --state <dir>        Control plane. Default: <labs>/agent-bots/.state
#   --repo <dir>         Project, for the base pin. Default: the
#                        repository this control plane is stamped to.
#   -h, --help           This text.
#
# Exit: 0 forgotten, 1 refused or nothing to forget.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

# shellcheck source=live-task.sh
. "$SCRIPT_DIR/live-task.sh"

die() { printf 'bot-forget: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

TASK_ID=""
STATE=""
REPO=""
SURFACE=0
FORCE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --surface) SURFACE=1; shift ;;
        --force)   FORCE=1; shift ;;
        --state)   STATE="${2:-}"; shift 2 ;;
        --repo)    REPO="${2:-}"; shift 2 ;;
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

# Hold the dispatch lock across every check and every removal below.
# The marker closes the gap while a run is in flight; the lock closes
# the one around it. Without it this script checks, and then acts, and
# a runner can take the claim in between - the checks said "no marker,
# a verdict", and by the time the removal runs a --reset has started
# building a new surface at the same path. Two readers of one resource
# who each check before acting is F-38 again, and the answer is the
# same one: one of them has to hold something.
#
# A client of the runner's lock protocol, not a copy of it. The owner
# file is named `owner.<token>` with the pid first, which is how
# take_lock tells a live holder from a dead one - so a runner that
# finds this script holding the lock waits, and one that finds it
# killed breaks it. What this script does *not* do is break anybody
# else's lock: it waits briefly and then refuses, and says whose it is.
FORGET_TOKEN="$$-$(date -u +%s)-${RANDOM}"
LOCK_DIR="$STATE/dispatch.lock"
# A claim holds this lock for seconds, so a short wait covers the normal
# case. BOT_FORGET_LOCK_WAIT exists for the sweep, which has to check
# the refusal without spending fifteen seconds on it.
LOCK_WAIT="${BOT_FORGET_LOCK_WAIT:-15}"
waited=0
until mkdir "$LOCK_DIR" 2>/dev/null; do
    if [ "$waited" -ge "$LOCK_WAIT" ]; then
        holder="$(ls "$LOCK_DIR" 2>/dev/null | head -n1)"
        die "a dispatch is being claimed ($LOCK_DIR is held${holder:+ by ${holder#owner.}}).
Try again in a moment. If that holder's pid is not running, the next
dispatch will break the lock; this script will not break somebody
else's."
    fi
    sleep 1
    waited=$((waited + 1))
done
: > "$LOCK_DIR/owner.$FORGET_TOKEN"
release_forget_lock() {
    rm -f "$LOCK_DIR/owner.$FORGET_TOKEN" 2>/dev/null || true
    rmdir "$LOCK_DIR" 2>/dev/null || true
}
trap release_forget_lock EXIT

LIVE="$STATE/tasks/$TASK_ID.md"
[ -f "$LIVE" ] || die "no live task at $LIVE - already forgotten, or never ran"

# One read, every question asked of those bytes. The alternative is
# reading a file three times in a directory another runner writes to,
# which is F-43 and was already a bug twice.
LIVE_TEXT="$(cat "$LIVE")"
STATUS="$(live_task_status "$LIVE_TEXT")"
WORKTREE="$(live_task_field worktree "$LIVE_TEXT")"

# Two separate questions, and conflating them was the bug.
#
# "Is a process in there?" is answered by the marker under
# `.state/running/`, never by the status. The runner writes a terminal
# status *before* its cleanup and rewrites it to `needs-review` if that
# cleanup fails, so `done` is on disk while there is still work to do -
# and deleting this file in that window makes the run's last
# `set_field` fail on a missing file, ending it with no status at all.
# That is precisely the bug sweep.sh's preflight was built around, and
# a status-only gate here walked straight back into it. F-47.
RUNNING="$(live_task_running "$STATE" "$TASK_ID")"
# Field two, not a line suffix: the path is last so that a state
# directory with a space in it still parses, which means `alive` is no
# longer at the end of the line. F-49.
ALIVE="$(printf '%s\n' "$RUNNING" | awk '$2 == "alive"' || true)"
if [ -n "$ALIVE" ] && [ "$FORCE" -eq 0 ]; then
    printf 'bot-forget: a run for %s is still going.\n\n' "$TASK_ID" >&2
    printf '%s\n\n' "$ALIVE" >&2
    printf 'The status says "%s", and that is not the question: the runner\n' \
        "${STATUS:-none}" >&2
    printf 'writes its verdict before cleaning up, so a finished-looking\n' >&2
    printf 'status is normal while a process is still in there. Wait for it.\n' >&2
    exit 1
fi

# "Did the run reach a verdict?" is the status, and the answer comes
# from live-task.sh so that this and the sweep cannot disagree about
# it - they did, one commit apart: the sweep called a missing status
# unsafe and this let it through.
if [ "$(live_task_verdict "$LIVE_TEXT")" != "record" ] && [ "$FORCE" -eq 0 ]; then
    printf 'bot-forget: %s has no verdict - status: %s\n\n' "$TASK_ID" "${STATUS:-none}" >&2
    printf 'A run in flight and a run that died leave the same word behind,\n' >&2
    printf 'and they want opposite things. Nothing here can tell them apart,\n' >&2
    printf 'so look first:\n\n' >&2
    if [ -n "$RUNNING" ]; then
        printf '%s\n' "$RUNNING" >&2
    else
        printf '    no run marker - nothing is working on it\n' >&2
    fi
    if [ -n "$WORKTREE" ]; then
        if [ -d "$WORKTREE" ]; then
            printf '    surface still on disk: %s\n' "$WORKTREE" >&2
        else
            printf '    surface is gone: %s\n' "$WORKTREE" >&2
        fi
    fi
    printf '    board:  grep -F "| %s | " %s/board.md\n\n' "$TASK_ID" "$STATE" >&2
    printf 'Then --force if it really is dead.\n' >&2
    exit 1
fi

# A surface is three things, not one, and removing only the middle one
# was a leak in both directions: an interrupted verification leaves a
# `-verify` checkout nothing else will ever remove, and every retired
# surface leaves its `refs/bot-base/<id>` pin in the project for good,
# holding the base objects alive. This mirrors `drop_surface` in
# bot-run.sh, including the order - the pin goes last, so a removal
# that refused still leaves the pin pointing at what the surface was
# cut from.
REMOVED_SURFACE=""
REMOVED_VERIFY=""
REMOVED_PIN=""
if [ "$SURFACE" -eq 1 ] && [ -n "$WORKTREE" ]; then
    # Which repository holds the pin, established before anything is
    # removed. The first version worked it out last, after the clone
    # was gone, and for a plain-folder project it worked it out wrong:
    # the stamp in `$STATE/REPO` is the folder, but the surface is cut
    # from `$STATE/snapshot.git` and the pin lives there (bot-run.sh,
    # ORIGIN_REPO). So --surface deleted the clone, rejected the folder
    # as not a repository, and stopped - leaving the record and the pin
    # both, minus the one part that proved what they were for. A check
    # that can refuse has to run before the first thing it would have
    # stopped. F-50.
    #
    # --repo wins; then a snapshot repository if this plane has one,
    # which is exactly the runner's own rule; then the stamp.
    PIN_REPO="$REPO"
    if [ -z "$PIN_REPO" ] && [ -d "$STATE/snapshot.git" ]; then
        PIN_REPO="$STATE/snapshot.git"
    fi
    [ -n "$PIN_REPO" ] || PIN_REPO="$(cat "$STATE/REPO" 2>/dev/null || true)"
    [ -n "$PIN_REPO" ] \
        || die "no project to remove refs/bot-base/$TASK_ID from - $STATE/REPO is missing, so pass --repo. Nothing was removed."
    git -C "$PIN_REPO" rev-parse --git-dir >/dev/null 2>&1 \
        || die "$PIN_REPO is not a git repository, so whether refs/bot-base/$TASK_ID exists cannot be established. Nothing was removed."

    # The verify checkout is a linked worktree, so its `.git` is a
    # *file* pointing back at its parent: that file both identifies it
    # and says whose it is. Same proof the runner uses.
    VERIFY_WT="$WORKTREE-verify"
    if [ -e "$VERIFY_WT" ]; then
        if [ -f "$VERIFY_WT/.git" ] \
           && grep -q "$(basename "$WORKTREE")" "$VERIFY_WT/.git" 2>/dev/null; then
            rm -rf "$VERIFY_WT"
            REMOVED_VERIFY="$VERIFY_WT"
        else
            die "$VERIFY_WT is not a verify checkout for $WORKTREE - refusing to remove it"
        fi
    fi

    if [ -d "$WORKTREE" ]; then
        # Stronger than "a runner made this": the marker names the
        # task, so it answers "made for *this* run" rather than "made
        # by something like me" - two branches whose leaf names collide
        # would each find a marked surface at that path. A hijacked
        # surface whose `.git` was replaced has no marker and is
        # refused, which is right: that is a thing to look at.
        grep -q "surface for $TASK_ID\$" "$WORKTREE/.git/bot-surface" 2>/dev/null \
            || die "$WORKTREE has no .git/bot-surface marker for $TASK_ID - refusing to remove it"
        rm -rf "$WORKTREE"
        [ ! -e "$WORKTREE" ] || die "could not remove $WORKTREE"
        REMOVED_SURFACE="$WORKTREE"
    fi

    # Nothing borrows the base objects any more. The stamp holds the
    # git directory this control plane belongs to, which is what
    # `git -C` wants; --repo overrides it for a plane whose project has
    # moved.
    #
    # Everything here is fatal rather than best-effort, and the reason
    # is the order of what follows: the live task is about to be
    # deleted, and it is the only record naming this pin. A silent
    # failure leaves a ref holding the base objects alive for ever and
    # removes the evidence needed to retry - which is exactly the leak
    # this path exists to close. The runner's own drop_surface can
    # afford `|| true` on the same call because its live task survives
    # and the next run can try again. This one cannot. F-49, and rule 5
    # of 3.6: a removal that could not be attempted is not a removal.
    if git -C "$PIN_REPO" rev-parse --verify --quiet "refs/bot-base/$TASK_ID" >/dev/null 2>&1
    then
        git -C "$PIN_REPO" update-ref -d "refs/bot-base/$TASK_ID" >/dev/null 2>&1 \
            || die "could not remove refs/bot-base/$TASK_ID from $PIN_REPO.
The live task is still here, so this can be retried once that ref is
free - which is the whole reason it is not being deleted first."
        REMOVED_PIN="refs/bot-base/$TASK_ID"
    fi
fi

# The mark goes down before the act, not after. The board is
# append-only and this is an event: somebody decided a record had
# served its purpose - same principle as --chain recording its own
# bypass, where the value of a door is not that it cannot be opened but
# that opening it leaves a mark. A mark written afterwards is a mark
# that can fail to be written, and then the record is gone with nothing
# saying who removed it. This way round, a board that cannot be
# appended to stops the removal instead.
printf '| %s | %s | forgotten | %s |\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$TASK_ID" "was ${STATUS:-no status}" \
    >> "$STATE/board.md" \
    || die "could not append to $STATE/board.md - nothing was removed"

rm -f "$LIVE"

# A marker whose process is gone is this id's litter, and the pid said
# so. Same judgement the lock protocol makes before it breaks a stale
# holder, and it happens after the gates rather than before, so nothing
# is cleared on a run that turned out to be alive.
printf '%s\n' "$RUNNING" | while read -r _pid state mark; do
    [ "$state" = "gone" ] || continue
    rm -f "$mark"
done

printf 'forgotten: %s (was %s)\n' "$TASK_ID" "${STATUS:-no status}"
[ -z "$REMOVED_VERIFY" ]  || printf 'removed verify checkout: %s\n' "$REMOVED_VERIFY"
[ -z "$REMOVED_SURFACE" ] || printf 'removed surface: %s\n' "$REMOVED_SURFACE"
[ -z "$REMOVED_PIN" ]     || printf 'removed base pin: %s\n' "$REMOVED_PIN"
printf 'kept: the handoff, the board and anything under artifacts/ or bots/\n'
