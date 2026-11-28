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

# live_task_field <key> <text> - the frontmatter reader.
#
# Frontmatter only, counting fences, which is the runner's own parser
# and not a shorthand for it. A plain `sed -n 's/^status:...'` over the
# whole document reads a *body* line too: a task with no frontmatter
# status and the words `status: done` somewhere in its prose came back
# `done`, and this file's whole job is deciding whether something may
# be deleted. Missing has to read as missing. F-49.
#
# awk reads to the end rather than stopping at the closing fence, for
# the reason the runner's field_from gives: under `set -o pipefail` an
# early exit SIGPIPEs the printf, and a task whose body is longer than a
# pipe buffer took its caller down with a 141 despite a status that
# parsed fine. Past the second fence nothing matches, so reading on
# changes the cost and not the answer.
live_task_field() {
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

# live_task_status <text> - the declared status, or empty.
live_task_status() { live_task_field status "$1"; }

# live_task_verdict <text> -> record | blocks
#
# `record` means the run reached a verdict - and *only* that. It does
# not mean nothing will write this file again: the runner writes its
# verdict before cleaning up and rewrites it if the cleanup fails, so a
# `record` can have a live process behind it. Whether anybody is still
# in there is live_task_running's question, and every caller that acts
# on a record has to ask it too; this function used to say otherwise,
# and two callers believed it. F-50. `blocks` covers three cases that
# want the same answer:
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
# One line per marker: `<pid> alive|gone|unknown <path>`. Empty when no
# run for this id has a marker, which is the only evidence that nothing
# is working on it.
#
# `unknown` is a marker whose pid could not be read - unreadable, empty
# or not a number. It used to come back `gone`, and `gone` is permission
# to act: a lookup that failed was taken as proof the process had
# ended. Only a pid that `kill -0` rejects says that. Every caller
# treats anything but `gone` as somebody possibly in there.
#
# The path goes *last* because it is the field that can contain a
# space, and the last field of a `read` absorbs the remainder. With the
# path first, one space in the state directory - `C:/Users/Some
# Name/...`, which is most of Windows - shifted every field and no
# caller removed a dead marker again. A record format is an interface,
# and this one is read by two scripts. F-49.
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
        case "$pid" in
            ''|*[!0-9]*)
                printf '? unknown %s\n' "$m" ;;
            *)
                if kill -0 "$pid" 2>/dev/null; then
                    printf '%s alive %s\n' "$pid" "$m"
                else
                    printf '%s gone %s\n' "$pid" "$m"
                fi ;;
        esac
    done
}

# live_task_settled <state> <task-id> <text> -> active | settled | unfinished
#
# Both questions, in the order that matters: a live process first,
# because it outranks whatever the status says, and only then the
# verdict. `settled` is the one answer that lets something act around
# a live task; the sweep's preflight acted on the verdict alone and so
# started beside a run still in its cleanup. A function for the reason
# F-46 gave: the preflight runs before anything can assert about it, so
# the decision has to be reachable from somewhere that can. F-50.
live_task_settled() {
    if live_task_running "$1" "$2" | awk '$2 != "gone" { found = 1 } END { exit !found }'
    then
        printf 'active\n'
    elif [ "$(live_task_verdict "$3")" = "record" ]; then
        printf 'settled\n'
    else
        printf 'unfinished\n'
    fi
}

# remove_proof_last <dir> <proof> - remove a tree, its ownership proof last
#
# <proof> is the path, relative to <dir>, of what says whose the tree is:
# `.git` for a verify checkout, `.git/bot-surface` for a surface. Plain
# `rm -rf` walks in name order, so `.git` goes early - and when a file
# deeper down will not go (a Windows lock on a built binary, an editor
# handle), the removal failed with the proof already gone. Every retry
# then refused the tree as "not a bot surface" and the only way out was
# by hand. So: everything else first, the proof's own directory next,
# the proof last, and only once nothing before it failed - a tree that
# could not be removed keeps what identifies it. Fails if anything is
# left.
#
# "Nothing before it failed" is not enough on its own: the directory
# itself can be what is locked. On Windows a process whose working
# directory is the tree - a terminal opened on a blocked surface to read
# the evidence, a dev server the agent left behind - lets every entry go
# and not the folder, so the final `rm -rf` took the proof and then
# failed. Whether the folder can go is asked first, by renaming it away
# and back; a rename is refused in exactly that case.
remove_proof_last() {
    local dir="$1" proof="$2" head rest e ok=1
    head="${proof%%/*}"
    rest=""
    [ "$head" = "$proof" ] || rest="${proof#*/}"
    # Never through a link. The globs below expand the *target's*
    # entries, so a surface - or its `.git` - that is a symlink had files
    # outside it removed one by one before the proof was ever reached.
    # A tree whose root or proof directory is a link is not a tree this
    # removes.
    [ ! -L "$dir" ] || return 1
    [ ! -L "$dir/$head" ] || return 1
    for e in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
        [ -e "$e" ] || [ -L "$e" ] || continue
        [ "${e##*/}" != "$head" ] || continue
        rm -rf "$e" 2>/dev/null || true
        [ ! -e "$e" ] || ok=0
    done
    if [ -n "$rest" ] && [ -d "$dir/$head" ]; then
        for e in "$dir/$head"/* "$dir/$head"/.[!.]* "$dir/$head"/..?*; do
            [ -e "$e" ] || [ -L "$e" ] || continue
            [ "${e##*/}" != "$rest" ] || continue
            rm -rf "$e" 2>/dev/null || true
            [ ! -e "$e" ] || ok=0
        done
    fi
    [ "$ok" -eq 1 ] || return 1
    if [ -e "$dir" ]; then
        # A name that already exists would make `mv` move the tree
        # *into* it rather than rename it. And the rename back is
        # retried, then named if it still fails: a tree left under the
        # probe name is one no caller looks for (bot-forget refuses a
        # record whose surface has one beside it).
        local probe="$dir.removing.$$" tries=0
        [ ! -e "$probe" ] || return 1
        mv "$dir" "$probe" 2>/dev/null || return 1
        # The destination has to stay absent for every attempt: if
        # something recreated `$dir` meanwhile, `mv` would move the
        # tree *into* it and the `rm -rf` below would take both.
        until { [ ! -e "$dir" ] && [ ! -L "$dir" ] && mv "$probe" "$dir" 2>/dev/null; }; do
            tries=$((tries + 1))
            if [ "$tries" -ge 5 ] || [ -e "$dir" ] || [ -L "$dir" ]; then
                printf 'remove_proof_last: %s is stranded at %s\n' "$dir" "$probe" >&2
                return 1
            fi
            sleep 1
        done
    fi
    rm -rf "$dir" 2>/dev/null || true
    [ ! -e "$dir" ]
}
