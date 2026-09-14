#!/usr/bin/env bash
# Stub agent: writes one file, commits it, and - if told to - reaches
# outside the surface to edit the project while it is at it.
#
# The counterpart to mover.sh for a project that is not a repository.
# There is no ref to move there; "the base moved" means somebody edited
# the folder, and the only way the runner can tell is by snapshotting it
# again. This is what makes that happen.
#
#   BOT_SCRIBE_FILE           what to write in the surface (default notes.md)
#   BOT_SCRIBE_BODY           what to put in it
#   BOT_SCRIBE_DISTURB        absolute path in the *project* to write
#   BOT_SCRIBE_DISTURB_BODY   what to put there
#
# Point DISTURB at the same relative path the bot is writing and the
# replay conflicts; point it somewhere else and the replay is clean.
set -eu

FILE="${BOT_SCRIBE_FILE:-notes.md}"
mkdir -p "$(dirname "$FILE")"
printf '%s\n' "${BOT_SCRIBE_BODY:-Written by a bot.}" > "$FILE"
# -f on purpose. Point BOT_SCRIBE_FILE at something the surface's
# exclude file holds back and this stub commits it anyway, which is the
# only way to exercise the last guard: the runner strips protected paths
# from its own `add`, so one can only reach a commit if an agent put it
# there deliberately.
git add -f -- "$FILE"
git -c user.name=scribe -c user.email=scribe@example.invalid \
    commit -q -m "Add ${FILE}"

if [ -n "${BOT_SCRIBE_DISTURB:-}" ]; then
    mkdir -p "$(dirname "$BOT_SCRIBE_DISTURB")"
    printf '%s\n' "${BOT_SCRIBE_DISTURB_BODY:-Edited by a human.}" \
        > "$BOT_SCRIBE_DISTURB"
    echo "committed, and the project moved underneath"
else
    echo "committed"
fi
exit 0
