#!/usr/bin/env bash
#
# bot-approve.sh - the inbox, and the one thing that opens the gate.
#
# A bot that finds something writes a task for another bot. Until now
# that task went straight into the scheduler's reach: `bot-tick.sh`
# reads `.state/proposed/` alongside the repository's own examples, and
# a derived task inherited `schedule:` from the review that produced it
# - so a recurring review produced a recurring fix task, and a machine's
# conclusion started a machine's work with nobody in between.
#
# This is the gate that was a flag. `--chain` on the runner is still
# there and still bypasses it, but it now says so on the board and in
# the handoff, which is the difference between a gate and a suggestion.
#
#   bash labs/agent-bots/run/bot-approve.sh                 # the inbox
#   bash labs/agent-bots/run/bot-approve.sh --id T-0006-fix
#   bash labs/agent-bots/run/bot-approve.sh --id T-0006-fix --revoke
#
#   --id <task-id>       a proposal in <state>/proposed
#   --task <path>        the same proposal, by path
#   --revoke             take an approval back off a task
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

die()  { printf 'bot-approve: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; }

TASK=""
TASK_ID=""
STATE=""
REPO=""
REVOKE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --id)      TASK_ID="${2:-}"; shift 2 ;;
        --task)    TASK="${2:-}"; shift 2 ;;
        --state)   STATE="${2:-}"; shift 2 ;;
        --repo)    REPO="${2:-}"; shift 2 ;;
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

# --------------------------------------------------------------------
# The inbox
#
# With no task named, this is a queue and not a dashboard: what is
# waiting, how old it is, and the one command that moves it. The sort is
# oldest first, because a proposal that has been sitting for three days
# is the one worth reading.
# --------------------------------------------------------------------

if [ -z "$TASK" ] && [ -z "$TASK_ID" ]; then
    [ -d "$PROPOSED" ] || { printf 'inbox: empty - no proposals at %s\n' "$PROPOSED"; exit 0; }
    found=0
    for f in "$PROPOSED"/*.md; do
        [ -f "$f" ] || continue
        found=$((found + 1))
        id="$(approval_field id "$f")"
        title="$(approval_field title "$f")"
        owner="$(approval_field owner "$f")"
        state="$(approval_state "$f")"
        case "$state" in
            ok)    mark="approved by $(approval_field approved_by "$f")" ;;
            stale) mark="APPROVAL STALE - the task changed after it was approved" ;;
            *)     mark="waiting" ;;
        esac
        printf '%-16s %-9s %s\n' "${id:-?}" "$owner" "$mark"
        printf '                 %s\n' "${title:-(no title)}"
    done
    if [ "$found" -eq 0 ]; then
        printf 'inbox: empty - no proposals at %s\n' "$PROPOSED"
    else
        printf '\n%s proposal(s). To let one run:\n' "$found"
        printf '    bash %s --id <id>\n' "${BASH_SOURCE[0]}"
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

ID="$(approval_field id "$TASK")"
[ -n "$ID" ] || die "$TASK has no 'id:' - it is not a task file"

if [ "$REVOKE" -eq 1 ]; then
    [ "$(approval_state "$TASK")" != "none" ] || die "$ID is not approved - nothing to revoke"
    awk '
        /^---[[:space:]]*$/ { fence++; print; next }
        fence == 1 && /^approved_(by|at|body):/ { next }
        { print }
    ' "$TASK" > "$TASK.tmp" && mv "$TASK.tmp" "$TASK"
    printf 'revoked: %s is back in the inbox\n' "$ID"
    exit 0
fi

STATE_NOW="$(approval_state "$TASK")"
if [ "$STATE_NOW" = "ok" ]; then
    printf '%s is already approved by %s at %s\n' \
        "$ID" "$(approval_field approved_by "$TASK")" "$(approval_field approved_at "$TASK")"
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
BODY="$(approval_body_hash "$TASK")"

awk -v who="$WHO" -v when="$WHEN" -v body="$BODY" '
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
' "$TASK" > "$TASK.tmp" && mv "$TASK.tmp" "$TASK"

[ "$(approval_state "$TASK")" = "ok" ] \
    || die "wrote an approval to $TASK and it does not read back as valid - nothing was approved"

BOARD="$STATE/board.md"
if [ -f "$BOARD" ]; then
    printf '| %s | %s | approved | %s |\n' "$WHEN" "$ID" "$WHO" >> "$BOARD"
fi

printf 'approved: %s by %s\n\n' "$ID" "$WHO"
printf 'It will be picked up by the next tick, or run it now:\n'
printf '    bash %s/bot-run.sh --task %s\n' "$SCRIPT_DIR" "$TASK"
