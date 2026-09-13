#!/usr/bin/env bash
#
# T-0013.sh - the verifier for tasks/T-0013-bots-inbox-reader.md.
#
# Outside the task's `touches:` and read at base, so the bot cannot edit
# what judges it. Same shape as T-0012.sh: a `cargo test <filter>` alone
# passes on an empty module, so this checks the API is there, that the
# module only reads, that enough tests exist, and only then runs them.
#
# Runs from the root of the verify checkout.

set -euo pipefail

fail() { printf 'T-0013 verify: %s\n' "$*" >&2; exit 1; }

MOD=core/src/bots.rs

grep -q '^pub mod bots;' core/src/lib.rs || fail "core/src/lib.rs does not declare 'pub mod bots;'"
[ -f "$MOD" ] || fail "$MOD does not exist"

for symbol in \
    'pub fn lab_control_plane' \
    'pub fn frontmatter_field' \
    'pub enum TaskStatus' \
    'pub fn needs_attention' \
    'pub struct BoardEvent' \
    'pub fn parse_board' \
    'pub struct Handoff' \
    'pub fn parse_handoff' \
    'pub struct InboxItem' \
    'pub fn load_inbox'; do
    grep -q "$symbol" "$MOD" || fail "$MOD has no '$symbol'"
done

# The module only reads the control plane. Writes in its tests go
# through tempfile fixtures, so this looks only above the test module.
CODE="$(awk '/^#\[cfg\(test\)\]/ { exit } { print }' "$MOD")"
for call in 'fs::write' 'create_dir' 'remove_file' 'remove_dir' 'OpenOptions' 'File::create'; do
    ! printf '%s\n' "$CODE" | grep -q "$call" \
        || fail "$MOD calls $call outside its tests - this module only reads"
done

MIN_TESTS=16
LISTING="$(cargo test -p codescope-core --lib bots -- --list 2>/dev/null)" \
    || fail "cargo could not build or list the bots tests"
COUNT="$(printf '%s\n' "$LISTING" | grep -c '^bots::.*: test$' || true)"
[ "$COUNT" -ge "$MIN_TESTS" ] || fail "found $COUNT bots tests, the task needs at least $MIN_TESTS"

cargo test -p codescope-core --lib bots
