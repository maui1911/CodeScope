#!/usr/bin/env bash
# Stub agent: does the work, and writes down one thing it learned.
#
# `.bot-memory` is the channel a bot uses to ask for something to be
# remembered. Asking is all it does: the note is stored unapproved and
# nothing reads it back into a prompt until a human agrees, because a
# note is agent prose that the runner would otherwise be feeding to the
# agent on every later run - the loop's own injection channel, built by
# the loop, for free.
#
#   BOT_REMEMBER_FILE   the honest part of the work
#   BOT_REMEMBER_NOTE   what to ask to have remembered
set -eu

FILE="${BOT_REMEMBER_FILE:-notes.md}"
mkdir -p "$(dirname "$FILE")"
printf 'Written by an agent that also kept a note.\n' > "$FILE"
printf 'Add a file\n' > .bot-commit-msg

printf '%s\n' "${BOT_REMEMBER_NOTE:-The verifier wants a clean checkout; running it in place leaves target/ behind and the next run reads that as meddling.}" > .bot-memory

echo "wrote $FILE and one note"
exit 0
