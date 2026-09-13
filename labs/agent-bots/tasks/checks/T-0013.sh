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

# Everything above the test module, which is where the API has to be.
CODE="$(awk '/^#\[cfg\(test\)\]/ { exit } { print }' "$MOD")"

# Declarations, not mentions. A plain substring search also matched a
# comment, so a module that only *named* these items in a doc comment
# passed. Block comments are dropped first, then `//` lines; each item
# has to start a remaining line as `pub <kind> <name>` followed by
# something that is not part of an identifier.
#
# Here-strings rather than `printf | grep -q`: grep -q stops at the first
# match, and under pipefail the writer it abandons can fail the pipeline
# on a module larger than a pipe buffer - a found match read as missing.
UNBLOCKED="$(perl -0777 -pe 's{/\*.*?\*/}{}gs' <<< "$CODE")"
DECLS="$(grep -Ev '^[[:space:]]*//' <<< "$UNBLOCKED" || true)"
for decl in \
    'fn lab_control_plane' \
    'fn frontmatter_field' \
    'enum TaskStatus' \
    'fn parse' \
    'fn needs_attention' \
    'struct BoardEvent' \
    'fn parse_board' \
    'struct Handoff' \
    'fn parse_handoff' \
    'struct InboxItem' \
    'fn load_inbox'; do
    grep -Eq "^[[:space:]]*pub ${decl}([^[:alnum:]_]|$)" <<< "$DECLS" \
        || fail "$MOD declares no 'pub $decl' outside comments and tests"
done

# The app reads these structs from outside the module, so every field
# the task names has to be `pub` too. The module's own tests would pass
# with private fields.
struct_fields() {   # struct_fields <struct> <field>...
    local name="$1" body
    shift
    body="$(awk -v name="$name" '
        $0 ~ "^[[:space:]]*pub struct " name "[[:space:]]*\\{" { inside = 1; next }
        inside && /^[[:space:]]*\}/ { exit }
        inside { print }' <<< "$DECLS")"
    for field in "$@"; do
        grep -Eq "^[[:space:]]*pub ${field}[[:space:]]*:" <<< "$body" \
            || fail "$MOD: struct $name has no 'pub $field:' field"
    done
}
struct_fields BoardEvent at task event detail
struct_fields Handoff path task from at status blockers next_action
struct_fields InboxItem id title owner status branch worktree base_sha last_event handoff

# The module only reads the control plane. Writes in its tests go
# through tempfile fixtures, so this looks only above the test module.
# Besides the qualified names, a mutating std::fs function imported by
# name (`use std::fs::{read_to_string, write};`) is called bare, so
# those calls count too; a method call (`.write(`) or a macro
# (`write!`) does not.
for call in 'fs::write' 'create_dir' 'remove_file' 'remove_dir' 'OpenOptions' 'File::create' 'File::options' 'write_all'; do
    ! grep -q "$call" <<< "$CODE" \
        || fail "$MOD calls $call outside its tests - this module only reads"
done
BARE='(^|[^[:alnum:]_.])(write|create_dir|create_dir_all|remove_file|remove_dir|remove_dir_all|rename|copy|hard_link|set_permissions)[[:space:]]*\('
! grep -Eq "$BARE" <<< "$DECLS" \
    || fail "$MOD calls a mutating std::fs function outside its tests - this module only reads"

MIN_TESTS=16
LISTING="$(cargo test -p codescope-core --lib bots -- --list 2>/dev/null)" \
    || fail "cargo could not build or list the bots tests"
COUNT="$(printf '%s\n' "$LISTING" | grep -c '^bots::.*: test$' || true)"
[ "$COUNT" -ge "$MIN_TESTS" ] || fail "found $COUNT bots tests, the task needs at least $MIN_TESTS"

cargo test -p codescope-core --lib bots
