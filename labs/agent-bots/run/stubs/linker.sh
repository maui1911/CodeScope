#!/usr/bin/env bash
# Stub agent: answers with a symlink instead of a file.
#
# `.bot-review.md`, `.bot-blocked` and `.bot-commit-msg` are the three
# channels out of the work surface, and the runner `mv`s them into the
# control plane. `[ -f ]` is true through a symlink and `mv` moves the
# link rather than what it points at - so a link is a way to nominate
# any file on this machine as this run's evidence, have the verifier
# read it, and have the runner store it.
#
# Nothing outside the surface is a run's to carry out of it. The runner
# removes the link unread and blocks the run.
#
#   BOT_LINKER_CHANNEL   which channel to fake (default .bot-review.md)
#   BOT_LINKER_TARGET    what to point it at (default the host's HOME)
set -eu

CHANNEL="${BOT_LINKER_CHANNEL:-.bot-review.md}"
TARGET="${BOT_LINKER_TARGET:-${HOME:-/etc}/.bashrc}"

ln -s "$TARGET" "$CHANNEL"
echo "left $CHANNEL pointing at $TARGET"
exit 0
