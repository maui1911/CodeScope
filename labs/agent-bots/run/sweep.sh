#!/usr/bin/env bash
#
# sweep.sh - run every stub through the runner and check the verdict.
#
# The stubs in run/stubs/ are regression tests for failure modes the
# runner used to get wrong (F-4, F-8, F-18, F-19, F-21, F-22, F-25). This
# drives all of them in one go, because a set of regression tests
# nobody runs together is a set of regression tests.
#
# It uses the real verifiers, so it compiles the crate a few times; the
# shared cache under .state/cache makes that a minute rather than ten.
# It cleans up after itself and reports anything it left behind.
#
#   bash labs/agent-bots/run/sweep.sh
#
# Exit code is the number of failures.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(dirname "$SCRIPT_DIR")"
REPO="$(git -C "$LAB_DIR" rev-parse --show-toplevel)"

RUN="$SCRIPT_DIR/bot-run.sh"
STUBS="$SCRIPT_DIR/stubs"
EX="$LAB_DIR/examples"
STATE="$LAB_DIR/.state"
WT_ROOT="${REPO}.worktrees"

FAILURES=0

# Every branch this sweep is allowed to destroy. It force-deletes, so
# the list has to be exhaustive *and* checked before anything runs: a
# developer with real work on bot/fixer/T-0001 would otherwise lose it
# to a regression suite.
SWEEP_BRANCHES="
bot/fixer/T-0001
bot/fixer/T-0003
bot/fixer/T-0006-fix
bot/fixer/T-990A
bot/fixer/T-990B
bot/reviewer/T-0005
bot/reviewer/T-0006
bot/fixer/T-991A
bot/fixer/T-991B
bot/fixer/T-991C
bot/fixer/T-991D
bot-sweep/base
"

PRE_EXISTING=""
for b in $SWEEP_BRANCHES; do
    if git -C "$REPO" rev-parse --verify --quiet "refs/heads/$b" >/dev/null 2>&1; then
        PRE_EXISTING="$PRE_EXISTING  $b"$'\n'
    fi
done
if [ -n "$PRE_EXISTING" ]; then
    printf 'sweep: refusing to run - these branches already exist and this\n'
    printf 'script force-deletes every one of them:\n\n%s\n' "$PRE_EXISTING"
    printf 'They are left over from an earlier run, or they are yours. Either\n'
    printf 'way the sweep is not the thing that should decide.\n'
    exit 1
fi

cleanup() {   # cleanup <branch> - only ever a branch from SWEEP_BRANCHES
    local leaf
    case " $(printf '%s' "$SWEEP_BRANCHES" | tr '\n' ' ') " in
        *" $1 "*) ;;
        *) printf 'sweep: refusing to clean up unowned branch %s\n' "$1"; return 0 ;;
    esac
    leaf="$(printf '%s' "$1" | tr '/' '-')"
    # A work surface is a standalone clone now, not a linked worktree,
    # so there is no registration to remove - only a directory. Which
    # means the allowlist above is not enough on its own: it protects
    # branch *names*, and this is about to `rm -rf` a path. The runner
    # writes a marker into every surface it makes; nothing without one
    # gets deleted here, exactly as in bot-run.sh.
    if [ -e "$WT_ROOT/$leaf" ]; then
        if [ -f "$WT_ROOT/$leaf/.git/bot-surface" ]; then
            rm -rf "$WT_ROOT/$leaf"
        else
            printf 'sweep: refusing to remove %s - no bot-surface marker\n' "$WT_ROOT/$leaf"
        fi
    fi
    # A linked worktree has a `.git` file rather than a directory, which
    # is both what identifies the verify checkout and what says it
    # belongs to this surface.
    if [ -e "$WT_ROOT/$leaf-verify" ]; then
        if [ -f "$WT_ROOT/$leaf-verify/.git" ]; then
            rm -rf "$WT_ROOT/$leaf-verify"
        else
            printf 'sweep: refusing to remove %s - not a verify checkout\n' "$WT_ROOT/$leaf-verify"
        fi
    fi
    git -C "$REPO" branch -D "$1" >/dev/null 2>&1
    return 0
}

check() {   # check <label> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf 'ok    %-28s exit %s\n' "$1" "$3"
    else
        printf 'FAIL  %-28s expected %s, got %s\n' "$1" "$2" "$3"
        FAILURES=$((FAILURES + 1))
    fi
}

run() {   # run <label> <expected> <task> <stub-or-empty> [extra args...]
    local label="$1" expect="$2" task="$3" stub="$4" rc=0
    shift 4
    if [ -n "$stub" ]; then
        BOT_AGENT_CMD="$STUBS/$stub" BOT_AGENT_ARGS="" \
            bash "$RUN" --task "$task" --reset "$@" >/dev/null 2>&1 || rc=$?
    else
        bash "$RUN" --task "$task" --reset "$@" >/dev/null 2>&1 || rc=$?
    fi
    check "$label" "$expect" "$rc"
}

rm -f "$STATE"/tasks/*.md "$STATE"/proposed/*.md 2>/dev/null

cleanup bot/fixer/T-0001
run "skip-agent no-op"       0 "$EX/T-0001-smoke-test.md" "" --skip-agent
cleanup bot/fixer/T-0001

run "blocked.sh"             1 "$EX/T-0001-smoke-test.md" blocked.sh
cleanup bot/fixer/T-0001

run "crash.sh"               1 "$EX/T-0001-smoke-test.md" crash.sh
cleanup bot/fixer/T-0001

run "liar.sh"                1 "$EX/T-0001-smoke-test.md" liar.sh
cleanup bot/fixer/T-0001

run "sloppy.sh"              1 "$EX/T-0001-smoke-test.md" sloppy.sh
cleanup bot/fixer/T-0001

run "meddling verifier"      1 "$EX/T-0003-meddling-verifier.md" "" --skip-agent
cleanup bot/fixer/T-0003

run "fabulist.sh"            1 "$EX/T-0005-review-overlap-check.md" fabulist.sh
cleanup bot/reviewer/T-0005

run "stumped.sh"             1 "$EX/T-0006-review-telemetry.md" stumped.sh
cleanup bot/reviewer/T-0006

rm -f "$STATE/tasks/T-0006-fix.md" "$STATE/proposed/T-0006-fix.md" 2>/dev/null
run "critic.sh"              0 "$EX/T-0006-review-telemetry.md" critic.sh
cleanup bot/reviewer/T-0006

if [ -f "$STATE/proposed/T-0006-fix.md" ]; then
    printf 'ok    %-28s derived for fixer\n' "handoff"
else
    printf 'FAIL  %-28s no task was derived\n' "handoff"
    FAILURES=$((FAILURES + 1))
fi

run "refusenik.sh"           2 "$STATE/proposed/T-0006-fix.md" refusenik.sh
cleanup bot/fixer/T-0006-fix

rm -f "$STATE"/tasks/*.md "$STATE"/proposed/*.md 2>/dev/null

# --------------------------------------------------------------------
# Two runners, one control plane
#
# The only test that exercises the dispatch lock, the board's append
# discipline and the overlap refusal at the same time - by doing the
# thing all three exist for. Two tasks declaring the same file, started
# together: exactly one may claim it, and the loser must come back as
# `refused` rather than as a failure.
#
# The fixtures are generated rather than kept in examples/, because a
# task file that lives in a task directory is a task the scheduler can
# see, and these two should never run on their own.
# --------------------------------------------------------------------

TMP_TASKS="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep.$$")"
mkdir -p "$TMP_TASKS"

for n in A B; do
    cat > "$TMP_TASKS/T-990$n.md" <<EOF
---
id: T-990$n
title: Concurrency fixture $n
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-990$n
touches: core/src/telemetry.rs
verify: true
schedule: auto
---

# Objective

Generated by sweep.sh. Two of these declare the same file on purpose.
EOF
done

TICK_OUT="$TMP_TASKS/tick.out"
BOT_AGENT_CMD=true BOT_AGENT_ARGS="" \
    bash "$SCRIPT_DIR/bot-tick.sh" --tasks "$TMP_TASKS" --state "$STATE" \
        --repo "$REPO" --max 2 --parallel 2 > "$TICK_OUT" 2>&1 || true

CLAIMED="$(grep -c ' ->  done' "$TICK_OUT" || true)"
REFUSED="$(grep -c ' ->  refused' "$TICK_OUT" || true)"
if [ "$CLAIMED" = "1" ] && [ "$REFUSED" = "1" ]; then
    printf 'ok    %-28s one claimed, one refused\n' "two runners at once"
else
    printf 'FAIL  %-28s claimed=%s refused=%s (see %s)\n' \
        "two runners at once" "$CLAIMED" "$REFUSED" "$TICK_OUT"
    FAILURES=$((FAILURES + 1))
fi

cleanup bot/fixer/T-990A
cleanup bot/fixer/T-990B
rm -f "$STATE"/tasks/T-990*.md 2>/dev/null
rm -rf "$TMP_TASKS"

# --------------------------------------------------------------------
# A base that moves under the run
#
# Four tasks that differ in one thing only: what lands on the base
# between the dispatch and the handoff. The branch is cut from a
# throwaway ref so the sweep can move it without touching anything a
# human cares about.
#
# The exit code alone is not enough here - three of the four are
# `needs-review` for three different reasons, and a runner that reported
# the wrong one would still score 2. So the board event is checked too:
# it is the only place the distinction is written down.
#
# And then the branch ref, which is the only one of the three checks
# that is not the run's own account of itself. The invariant is that
# the branch moves on success and on nothing else; every other check
# here would stay green if it stopped holding.
# --------------------------------------------------------------------

SWEEP_BASE="bot-sweep/base"
SWEEP_BASE_COMMIT="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" branch -f "$SWEEP_BASE" "$SWEEP_BASE_COMMIT" >/dev/null 2>&1

MOVE_TASKS="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep-move.$$")"
mkdir -p "$MOVE_TASKS"

move_task() {   # move_task <id> <verify>
    cat > "$MOVE_TASKS/$1.md" <<EOF
---
id: $1
title: Base-moves fixture $1
owner: fixer
status: todo
base: $SWEEP_BASE
branch: bot/fixer/$1
touches: labs/agent-bots/.sweep/*
verify: $2
---

# Objective

Generated by sweep.sh. The base ref moves while this runs.
EOF
}

run_mover() {   # run_mover <label> <exit> <event> <id> <mine> <body> <theirs> <body> <moved|kept>
    local label="$1" expect="$2" event="$3" id="$4" rc=0 mark=0
    local want="${9}" newbase="" tip="" ok=0 desc=""
    git -C "$REPO" update-ref "refs/heads/$SWEEP_BASE" "$SWEEP_BASE_COMMIT"
    # Only the rows this run appends count. The board is append-only and
    # keeps every earlier attempt, so grepping the whole of it would pass
    # on a row written minutes ago by a run that has since been fixed or
    # broken - the log asked what is true now, which is F-23.
    mark="$(wc -l < "$STATE/board.md" 2>/dev/null || printf 0)"
    BOT_AGENT_CMD="$STUBS/mover.sh" BOT_AGENT_ARGS="" \
        BOT_SWEEP_REPO="$REPO" BOT_SWEEP_BASE_BRANCH="$SWEEP_BASE" \
        BOT_SWEEP_MINE="$5" BOT_SWEEP_MINE_BODY="$6" \
        BOT_SWEEP_THEIRS="$7" BOT_SWEEP_THEIRS_BODY="$8" \
        bash "$RUN" --task "$MOVE_TASKS/$id.md" --reset --repo "$REPO" --state "$STATE" >/dev/null 2>&1 || rc=$?
    check "$label" "$expect" "$rc"
    if tail -n "+$((mark + 1))" "$STATE/board.md" 2>/dev/null \
        | grep -q "| $id | $event |"; then
        printf 'ok    %-28s board says %s\n' "$label event" "$event"
    else
        printf 'FAIL  %-28s no "%s" row for %s\n' "$label event" "$event" "$id"
        FAILURES=$((FAILURES + 1))
    fi

    # Where the branch ref actually ended up. The exit code and the
    # board row both come from the same run saying what it did; this is
    # the only check here that reads the thing itself. Without it the
    # invariant the whole step rests on - the branch moves on success
    # and on nothing else - could regress while the sweep stayed green.
    newbase="$(git -C "$REPO" rev-parse --verify "refs/heads/$SWEEP_BASE" 2>/dev/null || printf '')"
    tip="$(git -C "$REPO" rev-parse --verify "refs/heads/bot/fixer/$id" 2>/dev/null || printf '')"
    # Ancestry *and* content. Ancestry alone would pass for a branch
    # moved to any descendant of the right commit - including one with
    # the bot's patch dropped, which is precisely the regression the
    # "moves on success and on nothing else" invariant is about.
    if [ "$want" = moved ]; then
        desc="branch sits on the moved base, carrying the work"
        [ -n "$tip" ] && [ -n "$newbase" ] \
            && git -C "$REPO" merge-base --is-ancestor "$newbase" "$tip" 2>/dev/null \
            && git -C "$REPO" cat-file -e "$tip:$5" 2>/dev/null \
            && ok=1
    else
        desc="branch never left the old base, still carrying the work"
        [ -n "$tip" ] && [ -n "$newbase" ] \
            && git -C "$REPO" merge-base --is-ancestor "$SWEEP_BASE_COMMIT" "$tip" 2>/dev/null \
            && ! git -C "$REPO" merge-base --is-ancestor "$newbase" "$tip" 2>/dev/null \
            && git -C "$REPO" cat-file -e "$tip:$5" 2>/dev/null \
            && ok=1
    fi
    if [ "$ok" = 1 ]; then
        printf 'ok    %-28s %s\n' "$label ref" "$desc"
    else
        printf 'FAIL  %-28s expected %s; tip %s, base %s\n' \
            "$label ref" "$want" "${tip:-(none)}" "${newbase:-(none)}"
        FAILURES=$((FAILURES + 1))
    fi

    cleanup "bot/fixer/$id"
}

move_task T-991A true
move_task T-991B true
move_task T-991C '! test -e labs/agent-bots/.sweep/poison.txt'
move_task T-991D true

# Different files: the rebase is clean and the verifier still passes, so
# the branch moves and the run keeps its `done`. The only case where a
# handoff may go on claiming the work lands.
run_mover "base moved, clean"   0 rebased          T-991A \
    labs/agent-bots/.sweep/mine.txt bot \
    labs/agent-bots/.sweep/theirs.txt base moved

# Same file, different content: it no longer applies.
run_mover "base moved, conflict" 2 rebase-conflict T-991B \
    labs/agent-bots/.sweep/contested.txt bot \
    labs/agent-bots/.sweep/contested.txt base kept

# Different files, and the base brings something the verifier refuses.
# No merge could have shown this: the two changes share no path at all.
run_mover "base moved, now red"  2 rebase-red      T-991C \
    labs/agent-bots/.sweep/mine.txt bot \
    labs/agent-bots/.sweep/poison.txt poison kept

# Same file, same content: somebody already did this.
run_mover "base moved, emptied"  2 rebase-emptied  T-991D \
    labs/agent-bots/.sweep/same.txt identical \
    labs/agent-bots/.sweep/same.txt identical kept

git -C "$REPO" branch -D "$SWEEP_BASE" >/dev/null 2>&1
rm -f "$STATE"/tasks/T-991*.md 2>/dev/null
rm -rf "$MOVE_TASKS"

# --------------------------------------------------------------------
# A project that is not a repository
#
# CodeScope opens plain folders, so the runner has to work on one. There
# is no ref to cut from and nothing to clone, so the folder is
# snapshotted into a bare repo under state and the surface is a clone of
# that - which means every check below this line is the same check the
# git cases run, against a base that was manufactured rather than found.
#
# Its own fixture and its own state directory, because a control plane
# belongs to one project and this one is not the repo the sweep lives
# in.
# --------------------------------------------------------------------

FOLDER_ROOT="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep-folder.$$")"
FPROJ="$FOLDER_ROOT/proj"
FSTATE="$FOLDER_ROOT/state"

folder_fixture() {
    rm -rf "$FPROJ" "$FSTATE" "$FPROJ.worktrees"
    mkdir -p "$FPROJ/src" "$FPROJ/node_modules" "$FSTATE"
    printf 'A plain folder, not a repository.\n' > "$FPROJ/README.md"
    printf 'console.log("hi");\n' > "$FPROJ/src/app.js"
    printf 'ignored.txt\n' > "$FPROJ/.gitignore"
    printf 'the folder itself says not this one\n' > "$FPROJ/ignored.txt"
    printf 'module.exports = {};\n' > "$FPROJ/node_modules/dep.js"
    printf 'SECRET=hunter2\n' > "$FPROJ/.env"
    cat > "$FOLDER_ROOT/T-992.md" <<'EOF'
---
id: T-992
title: Folder surface fixture
owner: fixer
status: todo
base: folder
branch: bot/fixer/T-992
touches: notes.md
verify: test -f notes.md
---

# Objective

Generated by sweep.sh. The project here is a plain folder.
EOF
}

run_folder() {   # run_folder <label> <exit> <event-or-empty> [disturb] [body] [stub] [msg]
    local label="$1" expect="$2" event="$3" rc=0
    folder_fixture
    BOT_AGENT_CMD="$STUBS/${6:-scribe.sh}" BOT_AGENT_ARGS="" \
        BOT_SCRIBE_DISTURB="${4:-}" BOT_SCRIBE_DISTURB_BODY="${5:-}" \
        BOT_HANDLESS_MSG="${7:-}" BOT_HANDLESS_FILE="${8:-}" \
        BOT_SCRIBE_FILE="${8:-}" \
        bash "$RUN" --task "$FOLDER_ROOT/T-992.md" --reset --repo "$FPROJ" --state "$FSTATE" >/dev/null 2>&1 || rc=$?
    check "$label" "$expect" "$rc"
    if [ -n "$event" ]; then
        if grep -q "| T-992 | $event |" "$FSTATE/board.md" 2>/dev/null; then
            printf 'ok    %-28s board says %s\n' "$label event" "$event"
        else
            printf 'FAIL  %-28s no "%s" row\n' "$label event" "$event"
            FAILURES=$((FAILURES + 1))
        fi
    fi
}

# Nothing disturbs it: the snapshot becomes the base, the work lands on
# it, and a folder that had no history gets a patch it can apply.
run_folder "folder surface"        0 patch-written

# What the snapshot must not contain. Two different rules - the folder's
# own .gitignore, and the runner's list of things a folder has never had
# a reason to keep out of itself - and a secret leaking into a snapshot
# an agent then reads is worse than the problem this all solves.
IMPORTED="$(git --git-dir="$FSTATE/snapshot.git" ls-tree -r --name-only refs/heads/folder 2>/dev/null | grep -v '^.bot-contract/' || true)"
LEAKED=""
for unwanted in node_modules/dep.js .env ignored.txt; do
    case "$IMPORTED" in
        *"$unwanted"*) LEAKED="$LEAKED $unwanted" ;;
    esac
done
if [ -z "$LEAKED" ] && [ -n "$IMPORTED" ]; then
    printf 'ok    %-28s node_modules, .env and .gitignore honoured\n' "folder import excludes"
else
    printf 'FAIL  %-28s imported:%s\n' "folder import excludes" "${LEAKED:- nothing at all}"
    FAILURES=$((FAILURES + 1))
fi

# The contract travels with the snapshot, or the agent is reading a
# charter that is not there.
if [ "$(git --git-dir="$FSTATE/snapshot.git" ls-tree -r --name-only refs/heads/folder 2>/dev/null | grep -c '^.bot-contract/')" -gt 0 ]; then
    printf 'ok    %-28s contract grafted in\n' "folder contract"
else
    printf 'FAIL  %-28s no .bot-contract in the snapshot\n' "folder contract"
    FAILURES=$((FAILURES + 1))
fi

# Somebody edits a different file in the folder while the bot works: the
# snapshot moves, the replay is clean, and it verifies there.
run_folder "folder moved, clean"   0 rebased "$FPROJ/README.md" "Edited by a human."

# Somebody writes the same file the bot is writing.
run_folder "folder moved, clash"   2 rebase-conflict "$FPROJ/notes.md" "Written by a human."

# An agent that can do the work and cannot record it - which is every
# sandboxed agent, and was indistinguishable from a sloppy one until
# the runner learned to commit what gets left behind.
run_folder "handless agent"        0 runner-committed "" "" handless.sh \
    "Add notes.md

Written by the agent, committed by the runner."

# The same, with nobody explaining it. The work is committed either way,
# because work that cannot be measured cannot be judged - but a change
# whose reason is recorded nowhere does not get called done.
run_folder "handless, no message"  2 runner-committed "" "" handless.sh

# A secret an agent commits on purpose. The runner strips protected
# paths from its own `add` and the surface carries an exclude file, so
# this can only happen deliberately - and it is the guard that has to
# hold when the other two are gone.
run_folder "secret in the commit"  1 "" "" "" scribe.sh "" .env

rm -rf "$FOLDER_ROOT" "$FPROJ.worktrees"

# Anything left behind is a finding in its own right: the runner is
# supposed to clean up after every one of these. Counted off the
# filesystem rather than `git worktree list`, because a surface is a
# clone and the project has never heard of it.
LEFT_WT="$(ls -d "$WT_ROOT"/bot-* 2>/dev/null | wc -l | tr -d ' ')"
LEFT_LOCKS="$(ls -d "$STATE"/*.lock 2>/dev/null | wc -l | tr -d ' ')"
printf '\nbot worktrees left: %s\nlocks left:         %s\n' "$LEFT_WT" "$LEFT_LOCKS"
[ "$LEFT_WT" -eq 0 ] || FAILURES=$((FAILURES + 1))
[ "$LEFT_LOCKS" -eq 0 ] || FAILURES=$((FAILURES + 1))

printf 'failures: %s\n' "$FAILURES"
exit "$FAILURES"
