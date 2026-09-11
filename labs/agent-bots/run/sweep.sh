#!/usr/bin/env bash
#
# sweep.sh - run every stub through the runner and check the verdict.
#
# The stubs in run/stubs/ are regression tests for failure modes the
# runner used to get wrong (F-4, F-8, F-18, F-19, F-21, F-22). This
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
    git -C "$REPO" worktree remove --force "$WT_ROOT/$leaf" >/dev/null 2>&1
    git -C "$REPO" worktree remove --force "$WT_ROOT/$leaf-verify" >/dev/null 2>&1
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

# Anything left behind is a finding in its own right: the runner is
# supposed to clean up after every one of these.
LEFT_WT="$(git -C "$REPO" worktree list | grep -c "$(basename "$WT_ROOT")/bot-" || true)"
LEFT_LOCKS="$(ls -d "$STATE"/*.lock 2>/dev/null | wc -l | tr -d ' ')"
printf '\nbot worktrees left: %s\nlocks left:         %s\n' "$LEFT_WT" "$LEFT_LOCKS"
[ "$LEFT_WT" -eq 0 ] || FAILURES=$((FAILURES + 1))
[ "$LEFT_LOCKS" -eq 0 ] || FAILURES=$((FAILURES + 1))

printf 'failures: %s\n' "$FAILURES"
exit "$FAILURES"
