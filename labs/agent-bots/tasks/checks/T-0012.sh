#!/usr/bin/env bash
#
# T-0012.sh - the verifier for tasks/T-0012-relaunch-building-blocks.md.
#
# Outside the task's `touches:`, and read at base like every verifier,
# so the bot cannot edit what judges it. It exists because the plain
# `cargo test <filter>` a task would otherwise declare passes on an
# empty module: a filter that matches no tests exits 0. So this checks
# that the API the task names is there, that tests for it exist in
# number, and only then that they pass.
#
# Runs from the root of the verify checkout.

set -euo pipefail

fail() { printf 'T-0012 verify: %s\n' "$*" >&2; exit 1; }

MOD=core/src/relaunch.rs

grep -q '^pub mod relaunch;' core/src/lib.rs || fail "core/src/lib.rs does not declare 'pub mod relaunch;'"
[ -f "$MOD" ] || fail "$MOD does not exist"

for symbol in \
    'pub const WAIT_FOR_PID_ARG' \
    'pub fn relaunch_command' \
    'pub fn parse_wait_for_pid' \
    'pub fn wait_for_exit' \
    'pub fn pid_is_alive'; do
    grep -q "$symbol" "$MOD" || fail "$MOD has no '$symbol'"
done

! grep -q 'thread::sleep' "$MOD" || fail "$MOD calls thread::sleep - the wait takes its sleep as a parameter"

# The acceptance list names at least this many distinct cases. Counted
# from the harness's own listing, not from the source, so a test that is
# commented out or behind a cfg that does not build here does not count.
MIN_TESTS=12
LISTING="$(cargo test -p codescope-core --lib relaunch -- --list 2>/dev/null)" \
    || fail "cargo could not build or list the relaunch tests"
COUNT="$(printf '%s\n' "$LISTING" | grep -c '^relaunch::.*: test$' || true)"
[ "$COUNT" -ge "$MIN_TESTS" ] || fail "found $COUNT relaunch tests, the task needs at least $MIN_TESTS"

cargo test -p codescope-core --lib relaunch
