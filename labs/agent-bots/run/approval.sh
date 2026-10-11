#!/usr/bin/env bash
#
# approval.sh - what it means for a proposed task to have been approved.
#
# Sourced, never run. Three scripts have to agree on this and there is
# no fourth answer: bot-run.sh refuses to dispatch an unapproved
# proposal, bot-tick.sh refuses to queue one, and bot-approve.sh is the
# only thing that writes an approval. Three copies of the same six lines
# would be three chances to disagree about what "approved" means, and
# the one that drifts is the one that lets something through.
#
# What an approval is, and what it is not:
#
#   It is not proof that a human did it. Nothing on this side of the
#   filesystem can be. What it *is* is proof that something outside a
#   work surface did it - and that is a real boundary, because the agent
#   only ever has the surface. The control plane is not the agent's to
#   write, so an approval in the control plane is not the agent's.
#
#   It is also not a rubber stamp on a name. The approval records a hash
#   of the task as it read at the time, so approving `T-0006-fix` and
#   then editing `T-0006-fix` leaves an approval that no longer applies.
#   Approve the bytes, not the file name - F-17, one level up.
#
# Every function here takes *text*, not a path, for the reason memory.sh
# gives at length: validating a file and then reading it again is two
# opens of something that may be replaced in between, so the bytes that
# get used need not be the bytes whose approval was checked. A caller
# reads once and asks every question of those same bytes. The `_file`
# wrappers are for callers that only have a name, and each reads exactly
# once.

# Every digest in here goes through the lab's own repository, because
# `git hash-object` uses the *containing* repository's object format and
# all three callers have to agree. Approve from a SHA-256 repository and
# list from outside it and the same bytes hash two ways, which reads as
# `stale` - an approval refused for being in a different directory.
# $SCRIPT_DIR is set by every caller before it sources this, and is
# inside the lab.
approval_hash_repo() { printf '%s\n' "${SCRIPT_DIR:-.}"; }

# approval_field <key> <file>
#
# The frontmatter reader, kept separate from each script's own `field`
# so that sourcing this cannot quietly redefine one of them.
approval_field() {
    printf '%s\n' "$2" | awk -v key="$1" '
        /^---[[:space:]]*$/ { fence++; next }
        fence == 1 && !found && index($0, key ":") == 1 {
            value = substr($0, length(key) + 2)
            sub(/^[[:space:]]+/, "", value)
            sub(/[[:space:]]+$/, "", value)
            print value
            found = 1
        }
        fence > 1 { exit }
    '
}

# approval_body_hash <file>
#
# Everything except the approval lines themselves, so the hash is over
# what was approved rather than over the approval. Restricted to the
# frontmatter on purpose: a line in the prose that happens to begin
# `approved_by:` is prose, and stripping it would make two different
# tasks hash the same.
approval_body_hash() {
    printf '%s\n' "$1" | awk '
        /^---[[:space:]]*$/ { fence++; print; next }
        fence == 1 && /^approved_(by|at|body):/ { next }
        { print }
    ' | git -C "$(approval_hash_repo)" hash-object --stdin
}

# approval_state <file> -> none | stale | ok
#
# `stale` is a state of its own and not a kind of `none`, because the
# two need different words to a reader: one has never been looked at,
# the other was looked at and then changed underneath the person who
# looked. Reporting the second as the first would send them back to do
# the same reading again with no idea it had moved.
# All three fields, because the contract says three. A truncated
# approval carrying only `approved_by` and a matching hash used to pass,
# and then every place that shows an approval rendered a blank date - an
# approval nobody can put a time on is not the record it claims to be.
approval_state() {
    local who when body
    who="$(approval_field approved_by "$1")"
    when="$(approval_field approved_at "$1")"
    body="$(approval_field approved_body "$1")"
    if [ -z "$who" ] || [ -z "$when" ] || [ -z "$body" ]; then
        printf 'none\n'
    elif [ "$body" = "$(approval_body_hash "$1")" ]; then
        printf 'ok\n'
    else
        printf 'stale\n'
    fi
}

# approval_state_file <file> / approval_field_file <key> <file>
#
# One read each, for a caller that has a name and is only asking one
# question. A caller asking two must read the bytes itself - two
# wrappers in a row is the thing this shape exists to prevent.
approval_state_file() { approval_state "$(cat "$1")"; }
approval_field_file() { approval_field "$1" "$(cat "$2")"; }

# approval_is_proposal <file> <state-dir>
#
# Only files living in the control plane's proposal directory are
# gated. A task file in the repository is contract - a human wrote it,
# reviewed it and merged it, which is a stronger claim than any approval
# this script could record. The gate is for tasks that arrived without
# any of that.
approval_is_proposal() {
    case "$1" in
        "$2"/proposed/*) return 0 ;;
    esac
    return 1
}
