#!/usr/bin/env bash
#
# A *verifier* stub, and the other half of meddler.sh.
#
# meddler.sh reaches sideways into the agent's worktree, which the
# runner watched. This one stays home: it rewrites tracked source in the
# clean checkout it was handed and then exits 0. Nothing it did is
# visible in the agent's tree at all, and "exit 0" is then a statement
# about a tree that is no longer the commit it was cut from.
#
# The distinction the check has to keep: *untracked* files here are
# ordinary - a test writes a fixture, a tool leaves a cache - so only
# tracked content counts. This stub changes tracked content.
set -euo pipefail

TARGET="${BOT_SELFMEDDLE_FILE:-labs/agent-bots/README.md}"

[ -f "$TARGET" ] || { echo "selfmeddler: no $TARGET here" >&2; exit 1; }
printf '\n<!-- the verifier was here -->\n' >> "$TARGET"

echo "All checks passed."
exit 0
