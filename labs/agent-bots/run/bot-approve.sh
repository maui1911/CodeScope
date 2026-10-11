#!/usr/bin/env bash
#
# bot-approve.sh - the inbox, and the one thing that opens the gate.
#
# A bot that finds something writes a task for another bot. Until now
# that task went straight into the scheduler's reach: `bot-tick.sh`
# reads `.state/proposed/` alongside the repository's own task files,
# and a derived task inherited `schedule:` from the review that
# produced it - so a recurring review produced a recurring fix task,
# and a machine's conclusion started a machine's work with nobody in
# between.
#
# This is the gate that was a flag. `--chain` on the runner is still
# there and still bypasses it, but it now says so on the board and in
# the handoff, which is the difference between a gate and a suggestion.
#
# The same gate covers the other thing a bot produces for its own
# future: a memory note. That is agent prose which the runner pastes
# into a later prompt, so it is the injection channel this loop would
# otherwise have built without a door on it - see run/memory.sh.
#
#   bash labs/agent-bots/run/bot-approve.sh                 # the inbox
#   bash labs/agent-bots/run/bot-approve.sh --id T-0006-fix
#   bash labs/agent-bots/run/bot-approve.sh --id T-0006-fix --revoke
#   bash labs/agent-bots/run/bot-approve.sh --memory reviewer/2026-...md
#   bash labs/agent-bots/run/bot-approve.sh --forget reviewer/2026-...md
#
#   --id <task-id>       a proposal in <state>/proposed
#   --task <path>        the same proposal, by path
#   --memory <bot>/<f>   a note in <state>/bots/<bot>/memory
#   --forget <bot>/<f>   delete a note - approved or not
#   --revoke             take an approval back off a task or a note
#   --state <dir>        Control plane. Default: <labs>/agent-bots/.state
#   --repo <dir>         Project. Default: the repo this script is in
#   -h, --help
#
# Exit: 0 approved (or listed), 1 nothing to approve or it was refused.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"

# shellcheck source=approval.sh
. "$SCRIPT_DIR/approval.sh"
# shellcheck source=memory.sh
. "$SCRIPT_DIR/memory.sh"

die()  { printf 'bot-approve: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

TASK=""
TASK_ID=""
MEMORY_REF=""
FORGET_REF=""
STATE=""
REPO=""
REVOKE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --id)      TASK_ID="${2:-}"; shift 2 ;;
        --task)    TASK="${2:-}"; shift 2 ;;
        --state)   STATE="${2:-}"; shift 2 ;;
        --repo)    REPO="${2:-}"; shift 2 ;;
        --memory)  MEMORY_REF="${2:-}"; shift 2 ;;
        --forget)  FORGET_REF="${2:-}"; shift 2 ;;
        --revoke)  REVOKE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *)         die "unknown argument: $1 (try --help)" ;;
    esac
done

# An approval written by a run is not an approval. The runner exports
# this for the agent's turn and for anything it chains into, so a bot
# that reaches for this script - directly, or through a verifier, or
# through a hook it left behind - finds the door shut. It cannot be the
# whole defence and it is not meant to be: the agent's actual
# containment is that the control plane is not in its worktree. This is
# the part that catches the runner calling itself.
if [ -n "${BOT_RUN_ACTIVE:-}" ] || [ -n "${BOT_CHAIN_DEPTH:-}" ]; then
    die "refusing to approve from inside a bot run.

The point of an approval is that something other than the loop decided.
A run that approves its own follow-up has not passed a gate, it has
renamed one. If this is deliberate, the runner's --chain does the same
thing and records itself as having done it."
fi

STATE="${STATE:-$LAB_DIR/.state}"
[ -d "$STATE" ] || die "no control plane at $STATE - nothing has run yet"
STATE="$(cd "$STATE" && pwd)"
PROPOSED="$STATE/proposed"

if [ -z "$REPO" ]; then
    REPO="$(git -C "$LAB_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
fi

# memory_ref_path <bot>/<file> - resolve, or refuse.
#
# Two components, neither allowed to contain a slash or a `..`, and the
# result is checked to be inside the directory it is supposed to be in.
# The argument comes from a person typing at a shell, which is not a
# threat - but this builds a path that other flags then delete, and a
# path built by string joining is worth one function of paranoia.
memory_ref_path() {
    local ref="$1" bot note path
    bot="${ref%%/*}"
    note="${ref#*/}"
    [ -n "$bot" ] && [ -n "$note" ] && [ "$bot" != "$ref" ] \
        || die "expected <bot>/<note-file>, got '$ref'"
    case "$bot$note" in
        */*|*..*) die "'$ref' is not a <bot>/<note-file> pair" ;;
    esac
    path="$(memory_dir "$STATE" "$bot")/$note"
    [ -f "$path" ] || die "no note at $path"
    printf '%s\n' "$path"
}

if [ -n "$FORGET_REF" ]; then
    FORGET_PATH="$(memory_ref_path "$FORGET_REF")"
    rm -f "$FORGET_PATH"
    printf 'forgotten: %s\n' "$FORGET_REF"
    exit 0
fi

if [ -n "$MEMORY_REF" ]; then
    NOTE="$(memory_ref_path "$MEMORY_REF")"
    NOTE_BOT="${MEMORY_REF%%/*}"

    NOTE_TEXT="$(cat "$NOTE")"

    if [ "$REVOKE" -eq 1 ]; then
        [ "$(memory_state "$NOTE_TEXT")" != "none" ] \
            || die "$MEMORY_REF is not approved - nothing to revoke"
        printf '%s\n' "$NOTE_TEXT" | awk '
            /^---[[:space:]]*$/ { fence++; print; next }
            fence == 1 && /^approved_(by|at|body):/ { next }
            { print }
        ' > "$NOTE.tmp" && mv "$NOTE.tmp" "$NOTE"
        printf 'revoked: %s is no longer read back into prompts\n' "$MEMORY_REF"
        exit 0
    fi

    if [ "$(memory_state "$NOTE_TEXT")" = "ok" ]; then
        printf '%s is already approved by %s\n' \
            "$MEMORY_REF" "$(memory_field approved_by "$NOTE_TEXT")"
        exit 0
    fi

    # The shape check again, here rather than only at harvest. A note on
    # disk is a file somebody may have edited since, and this is the
    # last point before it starts arriving in prompts.
    NOTE_REFUSED="$(memory_body_refused "$(memory_body "$NOTE_TEXT")")"
    [ -z "$NOTE_REFUSED" ] || die "refusing to approve $MEMORY_REF: $NOTE_REFUSED"

    # The cap is refused, not rotated. Dropping the oldest to make room
    # would mean a bot's memory quietly reshapes itself whenever it
    # learns something, and nobody would ever be told which fact went
    # away. docs/HANDOFF.md in this repository grew to 3600 lines
    # because nothing ever said no, and it was deleted rather than read.
    HAVE="$(memory_approved_count "$STATE" "$NOTE_BOT")"
    if [ "$HAVE" -ge "$MEMORY_MAX_NOTES" ]; then
        printf 'bot-approve: %s already has %s approved notes, which is the limit.\n\n' \
            "$NOTE_BOT" "$HAVE" >&2
        printf 'Memory is capped rather than rotated: a bot with two hundred notes\n' >&2
        printf 'has a diary, not a memory, and nothing would ever tell you which\n' >&2
        printf 'fact had been pushed out. Retire one first:\n\n' >&2
        for f in "$(memory_dir "$STATE" "$NOTE_BOT")"/*.md; do
            [ -f "$f" ] || continue
            [ "$(memory_state_file "$f")" = "ok" ] || continue
            printf '    --forget %s/%s\n' "$NOTE_BOT" "$(basename "$f")" >&2
        done
        exit 1
    fi

    WHO="$(git -C "${REPO:-$LAB_DIR}" config user.email 2>/dev/null || true)"
    [ -n "$WHO" ] || WHO="${USER:-${USERNAME:-unknown}}"
    WHEN="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    # The hash is of the bytes already in hand, and the rewrite below
    # reads from those same bytes rather than opening the note again -
    # otherwise an approval could be stamped onto text nobody hashed.
    BODY="$(memory_body_hash "$NOTE_TEXT")"

    printf '%s\n' "$NOTE_TEXT" | awk -v who="$WHO" -v when="$WHEN" -v body="$BODY" '
        /^---[[:space:]]*$/ {
            fence++
            if (fence == 2 && !done) {
                print "approved_by: " who
                print "approved_at: " when
                print "approved_body: " body
                done = 1
            }
            print; next
        }
        fence == 1 && /^approved_(by|at|body):/ { next }
        { print }
    ' > "$NOTE.tmp" && mv "$NOTE.tmp" "$NOTE"

    [ "$(memory_state_file "$NOTE")" = "ok" ] \
        || die "wrote an approval to $NOTE and it does not read back as valid"

    BOARD="$STATE/board.md"
    [ ! -f "$BOARD" ] \
        || printf '| %s | %s | memory-approved | %s |\n' \
            "$WHEN" "$NOTE_BOT" "$(basename "$NOTE")" >> "$BOARD"

    printf 'remembered: %s will be read back into %s prompts\n' "$MEMORY_REF" "$NOTE_BOT"
    exit 0
fi

# --------------------------------------------------------------------
# The inbox
#
# With no task named, this is a queue and not a dashboard: what is
# waiting, how old it is, and the one command that moves it. The sort is
# oldest first, because a proposal that has been sitting for three days
# is the one worth reading.
# --------------------------------------------------------------------

if [ -z "$TASK" ] && [ -z "$TASK_ID" ]; then
    # No early exit when the proposal directory is absent. A run that
    # only ever committed creates .state/bots/<bot>/memory and
    # never .state/proposed, so bailing out here reported an empty
    # shared inbox while notes were sitting in it. The glob below
    # yields nothing on its own, which is the same thing said without
    # skipping the rest of the queue.
    # A proposal is kept after it runs - it is the record of what was
    # dispatched - so listing every file in here would have the count
    # stop meaning "waiting" after the first one completed. What belongs
    # in an inbox is what somebody still has to do something about: not
    # yet approved, approved and not yet run, or approved and then
    # edited. A proposal whose live task has reached a terminal status is
    # history, and it is counted separately rather than hidden, because
    # "empty" and "nothing left to do here" are the same sentence only
    # if you can see the difference.
    found=0
    done_count=0
    running_count=0
    for f in "$PROPOSED"/*.md; do
        [ -f "$f" ] || continue
        ftext="$(cat "$f")"
        id="$(approval_field id "$ftext")"
        live_status=""
        [ -z "$id" ] || [ ! -f "$STATE/tasks/$id.md" ] \
            || live_status="$(approval_field_file status "$STATE/tasks/$id.md")"
        case "$live_status" in
            ""|todo) ;;
            # In flight: it is running right now, so there is nothing
            # for a reader to do about it and printing an approval
            # command next to it would be an instruction to nowhere.
            # Counted, because "not in the queue" and "not happening"
            # are different.
            dispatched) running_count=$((running_count + 1)); continue ;;
            *) done_count=$((done_count + 1)); continue ;;
        esac
        found=$((found + 1))
        title="$(approval_field title "$ftext")"
        owner="$(approval_field owner "$ftext")"
        state="$(approval_state "$ftext")"
        case "$state" in
            ok)    mark="approved by $(approval_field approved_by "$ftext")" ;;
            stale) mark="APPROVAL STALE - the task changed after it was approved" ;;
            *)     mark="waiting" ;;
        esac
        printf '%-16s %-9s %s\n' "${id:-?}" "$owner" "$mark"
        printf '                 %s\n' "${title:-(no title)}"
    done
    if [ "$found" -eq 0 ]; then
        printf 'inbox: empty - nothing waiting at %s\n' "$PROPOSED"
    else
        printf '\n%s proposal(s) waiting. To let one run:\n' "$found"
        printf '    bash %s --id <id>\n' "${BASH_SOURCE[0]}"
    fi
    [ "$running_count" -eq 0 ] \
        || printf '%s in flight right now.\n' "$running_count"
    [ "$done_count" -eq 0 ] \
        || printf '%s more already ran and are kept as the record of what was dispatched.\n' \
            "$done_count"

    # And the notes, in the same queue, because they are the same
    # question: a machine wants something remembered and nobody has
    # agreed to it yet. Waiting notes are shown in full - a note is one
    # fact and the whole point is that somebody reads it.
    mem_waiting=0
    for d in "$STATE"/bots/*/; do
        [ -d "$d" ] || continue
        b="$(basename "$d")"
        md="$(memory_dir "$STATE" "$b")"
        [ -d "$md" ] || continue
        for f in "$md"/*.md; do
            [ -f "$f" ] || continue
            ftext="$(cat "$f")"
            st="$(memory_state "$ftext")"
            [ "$st" != "ok" ] || continue
            [ "$mem_waiting" -ne 0 ] || printf '\nnotes waiting to be remembered:\n'
            mem_waiting=$((mem_waiting + 1))
            printf '  %s/%s  (from %s)%s\n' \
                "$b" "$(basename "$f")" "$(memory_field from "$ftext")" \
                "$([ "$st" = "stale" ] && printf ' - EDITED SINCE IT WAS APPROVED' || true)"
            printf '      %s\n' "$(memory_body "$ftext" | sed '/^[[:space:]]*$/d' | tr '\n' ' ')"
        done
    done
    if [ "$mem_waiting" -gt 0 ]; then
        printf '\nTo keep one:\n'
        printf '    bash %s --memory <bot>/<note-file>\n' "${BASH_SOURCE[0]}"
    fi
    exit 0
fi

# --------------------------------------------------------------------
# Approving one
# --------------------------------------------------------------------

[ -n "$TASK" ] || TASK="$PROPOSED/$TASK_ID.md"
[ -f "$TASK" ] || die "no proposal at $TASK"
TASK="$(cd "$(dirname "$TASK")" && pwd)/$(basename "$TASK")"

# Only proposals. A task file in the repository went through a human, a
# review and a merge on its way in, which is a stronger claim than
# anything this script could stamp on it - and stamping one anyway would
# put an approval field in a file that lives in git, where the next
# merge would carry somebody's one-off decision to everybody.
approval_is_proposal "$TASK" "$STATE" \
    || die "$TASK is not in $PROPOSED.

Only proposals are gated. A task committed to the repository is already
the thing an approval is trying to establish: a human wrote it, somebody
reviewed it, and it was merged."

# One read. The id, the state, the hash and the rewrite are all of
# these bytes. Three separate opens of a file in a directory anybody may
# edit is how an approval gets stamped onto text nobody hashed - which
# is the defect the review found in the memory half. This is the same
# defect in the task half, found by going looking for it rather than by
# being told.
TASK_TEXT="$(cat "$TASK")"

ID="$(approval_field id "$TASK_TEXT")"
[ -n "$ID" ] || die "$TASK has no 'id:' - it is not a task file"

if [ "$REVOKE" -eq 1 ]; then
    [ "$(approval_state "$TASK_TEXT")" != "none" ] \
        || die "$ID is not approved - nothing to revoke"
    printf '%s\n' "$TASK_TEXT" | awk '
        /^---[[:space:]]*$/ { fence++; print; next }
        fence == 1 && /^approved_(by|at|body):/ { next }
        { print }
    ' > "$TASK.tmp" && mv "$TASK.tmp" "$TASK"
    printf 'revoked: %s is back in the inbox\n' "$ID"
    exit 0
fi

STATE_NOW="$(approval_state "$TASK_TEXT")"
if [ "$STATE_NOW" = "ok" ]; then
    printf '%s is already approved by %s at %s\n' \
        "$ID" "$(approval_field approved_by "$TASK_TEXT")" \
        "$(approval_field approved_at "$TASK_TEXT")"
    exit 0
fi

# Who, from git, because that is the name the rest of this repository
# already answers to. Not an identity claim - nothing here authenticates
# anybody - a record of which account was sitting here.
WHO="$(git -C "${REPO:-$LAB_DIR}" config user.email 2>/dev/null || true)"
[ -n "$WHO" ] || WHO="${USER:-${USERNAME:-unknown}}"
WHEN="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# The hash is taken before the fields are written, over a body with no
# approval lines in it - which is exactly what approval_body_hash
# computes afterwards, so the two agree by construction rather than by
# both being careful.
BODY="$(approval_body_hash "$TASK_TEXT")"

printf '%s\n' "$TASK_TEXT" | awk -v who="$WHO" -v when="$WHEN" -v body="$BODY" '
    /^---[[:space:]]*$/ {
        fence++
        if (fence == 2 && !done) {
            print "approved_by: " who
            print "approved_at: " when
            print "approved_body: " body
            done = 1
        }
        print; next
    }
    fence == 1 && /^approved_(by|at|body):/ { next }
    { print }
' > "$TASK.tmp" && mv "$TASK.tmp" "$TASK"

[ "$(approval_state_file "$TASK")" = "ok" ] \
    || die "wrote an approval to $TASK and it does not read back as valid - nothing was approved"

BOARD="$STATE/board.md"
if [ -f "$BOARD" ]; then
    printf '| %s | %s | approved | %s |\n' "$WHEN" "$ID" "$WHO" >> "$BOARD"
fi

printf 'approved: %s by %s\n\n' "$ID" "$WHO"
printf 'It will be picked up by the next tick, or run it now:\n'
printf '    bash %s/bot-run.sh --task %s\n' "$SCRIPT_DIR" "$TASK"
