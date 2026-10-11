#!/usr/bin/env bash
#
# memory.sh - what a bot is allowed to remember, and on whose word.
#
# Sourced, never run. bot-run.sh harvests a note and reads notes back
# into a prompt, bot-approve.sh is the only thing that makes one live,
# and both have to mean the same thing by "remembered" - see the same
# argument in approval.sh.
#
# Per-bot memory is the first feature in this lab where the agent's own
# prose comes back as *input to the agent*, on a later run, through the
# runner. That is the threat model's rule 1 and rule 2 at once (README
# 3.6), and it is why this file is mostly limits:
#
#   - A note is a claim, never an instruction. It enters the prompt in
#     a labelled section that says so and says the charter wins.
#   - A note is not live until a human approves it. Same three fields,
#     same hash-of-the-body, same inbox as a proposed task. Until then
#     it sits there and nothing reads it.
#   - A note may not restructure the prompt it is pasted into. That is
#     checkable rather than a matter of taste: no `---` fence, no ATX
#     heading, nothing that looks like one of the runner's own section
#     markers.
#   - Memory is capped, and the cap is refused rather than rotated.
#     `docs/HANDOFF.md` in this very repository grew to 3600 lines
#     because nothing ever said no; it was deleted rather than read.
#     A bot with 200 notes has no memory, it has a diary.
#   - A note names the run that produced it and the commit it was
#     about, so a reader can go and look. F-23: prose is a claim about
#     what happened, and the thing that happened is somewhere else.
#
# Every function here takes *text*, not a path, and that is the point
# rather than a style choice. Validating a file and then rendering it is
# two opens of something a third party may replace in between - the note
# that went into the prompt need not be the note whose approval was
# checked. So a caller reads the bytes once and asks every question of
# those same bytes. The `_file` wrappers exist for the places that
# genuinely only have a name, and each of them reads exactly once.

# At most this many approved notes per bot, and this much of them. Both
# are checked where the note becomes live, not where it is written: a
# waiting note costs a reader's attention, an approved one costs every
# future prompt.
MEMORY_MAX_NOTES=20
MEMORY_MAX_NOTE_BYTES=400
MEMORY_MAX_TOTAL_BYTES=8000

# memory_dir <state> <bot>
memory_dir() { printf '%s/bots/%s/memory\n' "$1" "$2"; }

# memory_field <key> <text> - the frontmatter reader.
memory_field() {
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

# memory_body <text> - everything after the frontmatter.
memory_body() {
    printf '%s\n' "$1" | awk '
        /^---[[:space:]]*$/ { fence++; next }
        fence >= 2 { print }
    '
}

# memory_body_hash <text>
#
# Through the lab's repository, for the reason approval.sh spells out:
# `git hash-object` takes its object format from wherever it is run, and
# a note that hashes two ways is a note that goes stale by being read
# from a different directory.
memory_body_hash() {
    printf '%s\n' "$1" | awk '
        /^---[[:space:]]*$/ { fence++; print; next }
        fence == 1 && /^approved_(by|at|body):/ { next }
        { print }
    ' | git -C "$(approval_hash_repo)" hash-object --stdin
}

# memory_body_refused <text> - empty if the text may be remembered,
# otherwise the reason it may not.
#
# The checks are about the *shape* of the text, not its opinions. A note
# that says something wrong is a note a human should not approve; a note
# that closes the section it is quoted inside and opens a new one is a
# note that rewrites the prompt around it, and no amount of reading
# catches that reliably.
memory_body_refused() {
    local text="$1" bytes
    bytes="$(printf '%s' "$text" | wc -c | tr -d ' ')"
    if [ -z "$(printf '%s' "$text" | tr -d '[:space:]')" ]; then
        printf 'it is empty\n'
    elif [ "$bytes" -gt "$MEMORY_MAX_NOTE_BYTES" ]; then
        printf 'it is %s bytes and the limit is %s - a note is one fact, not a report\n' \
            "$bytes" "$MEMORY_MAX_NOTE_BYTES"
    elif printf '%s\n' "$text" | grep -q '^---[[:space:]]*$'; then
        printf 'it contains a "---" fence, which would end the frontmatter of the file it is stored in and the section of the prompt it is quoted in\n'
    elif printf '%s\n' "$text" | grep -q '^#'; then
        printf 'it contains a line starting with "#", which would open a new section in the prompt it is quoted in\n'
    elif printf '%s\n' "$text" | grep -q '^[[:space:]]*\(Rules for this run\|Your task, verbatim\|Read these first\)'; then
        printf 'it repeats one of the runner section headings, which is the prompt talking and not the bot\n'
    fi
}

# memory_state <text> -> none | stale | ok
#
# All three approval fields, because the contract says three and an
# approval missing its timestamp is an approval nobody can date. A
# truncated one used to pass on `approved_by` plus a matching hash and
# then render a blank `approved_at` wherever it was shown.
#
# `stale` is a state of its own and not a kind of `none`: one has never
# been looked at, the other was looked at and then changed underneath
# the person who looked, and those send a reader to do different things.
memory_state() {
    local who when body
    who="$(memory_field approved_by "$1")"
    when="$(memory_field approved_at "$1")"
    body="$(memory_field approved_body "$1")"
    if [ -z "$who" ] || [ -z "$when" ] || [ -z "$body" ]; then
        printf 'none\n'
    elif [ "$body" = "$(memory_body_hash "$1")" ]; then
        printf 'ok\n'
    else
        printf 'stale\n'
    fi
}

# memory_state_file <file> - one read, then every question of those
# bytes. Convenience for a caller that has a name and no text yet.
memory_state_file() { memory_state "$(cat "$1")"; }

# memory_approved_count <state> <bot>
#
# One read per note, and only the number the cap is about. A companion
# that also returned the waiting count was written first and had no
# caller: bot-approve lists the waiting notes rather than counting them,
# because a queue whose contents nobody can see is just a number.
memory_approved_count() {
    local dir f n=0
    dir="$(memory_dir "$1" "$2")"
    if [ -d "$dir" ]; then
        for f in "$dir"/*.md; do
            [ -f "$f" ] || continue
            [ "$(memory_state "$(cat "$f")")" = "ok" ] || continue
            n=$((n + 1))
        done
    fi
    printf '%s\n' "$n"
}


# memory_block <state> <bot>
#
# The approved notes, oldest first, as the prompt sees them - or nothing
# at all when there are none, because an empty "what you have learned"
# heading teaches a bot that the section is furniture.
#
# Each note is read once and both the approval check and the rendering
# are done against those bytes. Two opens would mean a note replaced in
# between goes into the prompt carrying somebody's approval of different
# text, which is the whole mechanism defeated by a race.
#
# Sorted by file name, which is the timestamp, so the order a reader
# sees is the order the bot learned them. Total size is enforced here as
# well as at approval: approvals accumulate over months and this is the
# one place that knows what the prompt actually gets.
memory_block() {
    local dir f text body from at n=0 total=0 bytes out=""
    dir="$(memory_dir "$1" "$2")"
    [ -d "$dir" ] || return 0
    for f in "$dir"/*.md; do
        [ -f "$f" ] || continue
        text="$(cat "$f")"
        [ "$(memory_state "$text")" = "ok" ] || continue
        body="$(memory_body "$text" | sed '/^[[:space:]]*$/d')"
        [ -n "$body" ] || continue
        bytes="$(printf '%s' "$body" | wc -c | tr -d ' ')"
        total=$((total + bytes))
        [ "$total" -le "$MEMORY_MAX_TOTAL_BYTES" ] || break
        n=$((n + 1))
        [ "$n" -le "$MEMORY_MAX_NOTES" ] || break
        from="$(memory_field from "$text")"
        at="$(memory_field at "$text")"
        out="$out  - (from ${from:-an earlier run}${at:+, ${at:0:12}}) $(printf '%s' "$body" | tr '\n' ' ')
"
    done
    [ -n "$out" ] || return 0
    printf '%s' "$out"
}
