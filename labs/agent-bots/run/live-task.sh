#!/usr/bin/env bash
#
# live-task.sh - what a live task means to something that is not its
# runner.
#
# Sourced, never run. `.state/tasks/<id>.md` has exactly one writer:
# that task's runner (README 3.2). It has more than one *reader* that
# has to act on what it says, and they were written a commit apart and
# promptly disagreed - sweep.sh treated a missing `status:` as unsafe
# and refused, bot-forget.sh let it through and deleted the file. One
# rule stated twice is two rules, and the reader picks. Same argument
# as approval.sh and memory.sh, and text-first for the same reason: a
# caller with two questions reads the bytes once and asks both of
# those bytes (F-43).
#
# What this file cannot tell you is whether a run is *working* right
# now. `status:` says where the run got to, and the runner writes a
# terminal status before its cleanup and rewrites it if that cleanup
# fails - so `done` is on disk while there is still work to do. The
# answer to "is anybody in there" is the marker under
# `.state/running/`, which exists for exactly as long as a process
# does. See live_task_running below and F-47.

# live_task_status <text> - the declared status, or empty.
live_task_status() {
    printf '%s\n' "$1" | sed -n 's/^status:[[:space:]]*//p' | head -n1
}

# live_task_field <key> <text>
live_task_field() {
    printf '%s\n' "$2" | sed -n "s/^$1:[[:space:]]*//p" | head -n1
}

# live_task_verdict <text> -> record | blocks
#
# `record` means the run reached a verdict: a reader may act around it,
# and nothing is going to rewrite it out from under them. `blocks`
# means the opposite, and covers three cases that want the same answer:
#
#   dispatched   a run is in flight, or one died leaving this behind.
#                Those look identical from outside and want opposite
#                things, which is why the answer is "ask a human".
#   unknown      a status this vocabulary does not have. Guessing which
#                side of the line it falls on is how a tool ends up
#                deleting somebody's work.
#   missing      the frontmatter has no status at all. Same.
#
# The vocabulary is the handoff's, minus `todo` and `dispatched` which
# only a task has - see contract/templates/TASK.md.
live_task_verdict() {
    case "$(live_task_status "$1")" in
        done|blocked|needs-review) printf 'record\n' ;;
        *)                         printf 'blocks\n' ;;
    esac
}

# live_task_running <state> <task-id>
#
# One line per marker: `<file> <pid> alive|gone`. Empty when no run for
# this id has a marker, which is the only evidence that nothing is
# working on it.
#
# `kill -0` rather than the marker's existence, because a killed run
# leaves its marker behind - the same liveness test the lock protocol
# uses on a holder it is thinking about breaking, and for the same
# reason: a file is a claim, a process is a fact.
live_task_running() {
    local m pid
    for m in "$1/running/$2".*; do
        [ -f "$m" ] || continue
        pid="$(cat "$m" 2>/dev/null || true)"
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            printf '%s %s alive\n' "$m" "$pid"
        else
            printf '%s %s gone\n' "$m" "${pid:-unknown}"
        fi
    done
}
