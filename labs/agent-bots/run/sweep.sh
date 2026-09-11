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
bot/fixer/T-995A
bot/fixer/shared
bot-sweep/base
bot-sweep/committed-secret
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
# What a task may declare it produces
#
# `produces:` decides whether a run gets a branch or a file beside one,
# so a task naming something else has to be refused before a surface
# exists rather than reported on afterwards. Same for `artifact:`: it
# is a filename the runner will `mv` out of the worktree, so a path
# that leaves the root, or one of the two names already spoken for by
# the other agent-to-runner channels, is a dispatch error.
#
# Not covered here: a task whose `produces:` disagrees with its owner's
# charter. That check reads the charter at base, and no charter at base
# declares the field yet - the declarations land with this change. The
# same shape as #348, and the reason the check treats an undeclared
# charter as making no claim at all. See README F-32.
# --------------------------------------------------------------------

BAD_TASKS="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep-bad.$$")"
mkdir -p "$BAD_TASKS"

bad_task() {   # bad_task <label> <id> <extra frontmatter lines>
    cat > "$BAD_TASKS/$2.md" <<EOF
---
id: $2
title: Generated by sweep.sh
owner: reviewer
status: todo
base: labs/agent-bots
branch: bot/reviewer/$2
touches: labs/agent-bots/run/bot-run.sh
verify: true
$3
---

# Objective

Generated by sweep.sh. This task is malformed on purpose.
EOF
    run "$1" 1 "$BAD_TASKS/$2.md" ""
}

bad_task "produces: nonsense"     T-993A "produces: opinion"
bad_task "artifact leaves root"   T-993B "produces: report
artifact: ../escape.md"
bad_task "artifact is a channel"  T-993C "produces: report
artifact: .bot-commit-msg"
bad_task "artifact is not a bot's" T-993D "produces: report
artifact: review.md"
bad_task "shape is not a template" T-993E "produces: report
shape: ../../../etc/passwd"

rm -rf "$BAD_TASKS"

# --------------------------------------------------------------------
# The claim verifier, on its own
#
# review-shape.sh is a pure function of (review, commit, touches), and
# until now it was only ever exercised through three stubs and a whole
# run. That has a second cost besides coverage: `verify:` runs inside
# the verify checkout, so a full run can only ever exercise the copy of
# this script sitting at the task's base - never the one being edited.
# These call the working-tree copy directly, which is the only way a
# change to it can be proven before it merges. See #348.
#
# The fixtures quote the tree rather than hardcoding it: a review that
# says what is really on line 5 has to keep saying it when line 5
# changes, and a test that pins the text would just be a second thing
# to update.
# --------------------------------------------------------------------

SHAPE="$SCRIPT_DIR/review-shape.sh"
SHAPE_DIR="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep-shape.$$")"
mkdir -p "$SHAPE_DIR"
SHAPE_SHA="$(git -C "$REPO" rev-parse HEAD)"
SHAPE_PATH="labs/agent-bots/run/bot-run.sh"
SHAPE_OTHER="labs/agent-bots/run/sweep.sh"

shape_line() { git -C "$REPO" show "$SHAPE_SHA:$SHAPE_PATH" | sed -n "$1p"; }
SHAPE_L5="$(shape_line 5)"
SHAPE_L6="$(shape_line 6)"
SHAPE_L7="$(shape_line 7)"
SHAPE_N="$(git -C "$REPO" show "$SHAPE_SHA:$SHAPE_PATH" | wc -l | tr -d ' ')"
SHAPE_LAST="$(shape_line "$SHAPE_N")"
SHAPE_SHORT="$(printf '%s' "$SHAPE_L5" | cut -c1-4)"

shape_case() {   # shape_case <label> <expected> <verdict> <findings body>
    local rc=0
    cat > "$SHAPE_DIR/review.md" <<EOF
---
task: T-SHAPE
reviewed: $SHAPE_SHA
verdict: $3
---

# What I checked

Generated by sweep.sh, one property at a time.

# Findings

$4

# What I could not check

Everything else. This is a fixture, not a review.
EOF
    ( cd "$REPO" \
        && BOT_REVIEW="$SHAPE_DIR/review.md" \
           BOT_REVIEWED_SHA="$SHAPE_SHA" \
           BOT_TOUCHES="$SHAPE_PATH" \
           BOT_TASK_ID="T-SHAPE" \
           bash "$SHAPE" ) >/dev/null 2>&1 || rc=$?
    check "$1" "$2" "$rc"
}

shape_case "claim quoted" 0 changes-requested \
"- $SHAPE_PATH:5 — a finding that quotes what it is about.
  > $SHAPE_L5"

shape_case "claim unquoted" 1 changes-requested \
"- $SHAPE_PATH:5 — a finding that cites a real line and quotes nothing."

shape_case "quote from another line" 1 changes-requested \
"- $SHAPE_PATH:5 — cites line 5 and quotes line 7.
  > $SHAPE_L7"

shape_case "quote too short to identify" 1 changes-requested \
"- $SHAPE_PATH:5 — quotes something that is really there and proves nothing.
  > $SHAPE_SHORT"

shape_case "quote spans three lines" 0 changes-requested \
"- $SHAPE_PATH:5 — a finding about a paragraph rather than a line.
  > $SHAPE_L5
  > $SHAPE_L6
  > $SHAPE_L7"

shape_case "quote runs past the file" 1 changes-requested \
"- $SHAPE_PATH:$SHAPE_N — a three-line quote on the last line of the file.
  > $SHAPE_LAST
  > $SHAPE_L6
  > $SHAPE_L7"

shape_case "line past the file" 1 changes-requested \
"- $SHAPE_PATH:999999 — a real file, a line it does not have.
  > $SHAPE_L5"

shape_case "path not at that commit" 1 changes-requested \
"- labs/agent-bots/run/dispatch.sh:88 — a file that has never existed.
  > the lock is released before the live task is written"

shape_case "path outside touches" 1 changes-requested \
"- $SHAPE_OTHER:1 — a real file, outside the scope this task declared.
  > $SHAPE_L5"

shape_case "no findings, approved" 0 approve "No findings."

rm -rf "$SHAPE_DIR"

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
# A secret that is already in the history
#
# Holding a protected path back and deleting one are different acts, and
# `git rm --cached` on a tracked file is the second: it stages a
# deletion. A project that committed its .env years ago would have had
# this runner quietly remove it on the first run that touched anything
# - a destructive answer to a protective question, and one the
# protected-diff check downstream would only notice after the commit.
#
# The fixture commit is built with plumbing against a temporary index,
# so nothing here goes near the working tree.
# --------------------------------------------------------------------

SEC_BRANCH="bot-sweep/committed-secret"
SEC_TASKS="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep-sec.$$")"
mkdir -p "$SEC_TASKS" "$STATE/tmp"
SEC_IDX="$STATE/tmp/sweep-secret-$$.idx"
rm -f "$SEC_IDX"
SEC_BLOB="$(printf 'SECRET=committed-long-ago\n' | git -C "$REPO" hash-object -w --stdin)"
GIT_INDEX_FILE="$SEC_IDX" git -C "$REPO" read-tree HEAD
GIT_INDEX_FILE="$SEC_IDX" git -C "$REPO" update-index --add --cacheinfo "100644,$SEC_BLOB,.env"
SEC_TREE="$(GIT_INDEX_FILE="$SEC_IDX" git -C "$REPO" write-tree)"
SEC_COMMIT="$(git -C "$REPO" commit-tree "$SEC_TREE" -p "$(git -C "$REPO" rev-parse HEAD)" \
    -m "sweep fixture: a project with its .env in the history")"
rm -f "$SEC_IDX"
git -C "$REPO" branch -f "$SEC_BRANCH" "$SEC_COMMIT" >/dev/null 2>&1

cat > "$SEC_TASKS/T-995A.md" <<EOF
---
id: T-995A
title: Touch a file in a project whose .env is tracked
owner: fixer
status: todo
base: $SEC_BRANCH
branch: bot/fixer/T-995A
touches: labs/agent-bots/.sweep/*
verify: true
---

# Objective

Generated by sweep.sh. The agent edits the tracked .env on its way past;
the runner must put it back rather than commit its removal.
EOF

SECRC=0
BOT_AGENT_CMD="$STUBS/handless.sh" BOT_AGENT_ARGS="" \
    BOT_HANDLESS_FILE="labs/agent-bots/.sweep/mine.txt" \
    BOT_HANDLESS_MSG="Add a file, leave the .env alone" \
    BOT_HANDLESS_ALSO=".env" \
    bash "$RUN" --task "$SEC_TASKS/T-995A.md" --reset --repo "$REPO" >/dev/null 2>&1 || SECRC=$?
# `needs-review`, not `done`: nothing that never travels went anywhere,
# and an agent rewrote one of them anyway. Whether that was meant is not
# a question a verifier can answer.
check "tracked secret, edited" 2 "$SECRC"

# `ls-tree`, not `<rev>:<path>`: MSYS rewrites an argument holding both a
# slash and a colon into a Windows path list, so `a/branch:.env` reaches
# git as `aranch;.env`. The runner's own lookups are sha-based and have
# no slash, which is why they survive it and this did not.
SEC_NOW="$(git -C "$REPO" ls-tree "$SEC_BRANCH" -- .env 2>/dev/null | awk '{print $3}')"
SEC_TIP="$(git -C "$REPO" ls-tree "refs/heads/bot/fixer/T-995A" -- .env 2>/dev/null | awk '{print $3}')"
if [ "$SEC_TIP" = "$SEC_BLOB" ] && [ "$SEC_NOW" = "$SEC_BLOB" ]; then
    printf 'ok    %-28s left exactly as the base had it\n' "tracked secret"
else
    printf 'FAIL  %-28s base %s, branch %s, expected %s\n' \
        "tracked secret" "${SEC_NOW:-(gone)}" "${SEC_TIP:-(deleted)}" "$SEC_BLOB"
    FAILURES=$((FAILURES + 1))
fi

cleanup bot/fixer/T-995A
git -C "$REPO" branch -D "$SEC_BRANCH" >/dev/null 2>&1
rm -f "$STATE"/tasks/T-995*.md 2>/dev/null
rm -rf "$SEC_TASKS"

# --------------------------------------------------------------------
# Two tasks, one branch name
#
# The origin-ref check at dispatch cannot see this: a surface is a
# standalone clone, so refs/heads/<branch> only appears in the project
# at the push right at the end. Both runs would pass it, both would do
# all their work, and the loser would find out as a push failure.
# `touches:` does not catch it either - the collision is in the name.
# --------------------------------------------------------------------

DUP_TASKS="$(mktemp -d 2>/dev/null || printf '%s' "${TMPDIR:-/tmp}/bot-sweep-dup.$$")"
mkdir -p "$DUP_TASKS" "$STATE/tasks"
cat > "$STATE/tasks/T-996A.md" <<EOF
---
id: T-996A
title: In flight, holding the branch
owner: fixer
status: dispatched
base: labs/agent-bots
base_sha: $(git -C "$REPO" rev-parse HEAD)
branch: bot/fixer/shared
touches: labs/agent-bots/.sweep/theirs.txt
worktree: $WT_ROOT/bot-fixer-shared
---

# Objective

Generated by sweep.sh. Never dispatched; it only has to exist and say
which branch it is on.
EOF

cat > "$DUP_TASKS/T-996B.md" <<EOF
---
id: T-996B
title: A different task naming the same branch
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/shared
touches: labs/agent-bots/.sweep/mine.txt
verify: true
---

# Objective

Generated by sweep.sh. Disjoint files, same branch.
EOF

DUPRC=0
BOT_AGENT_CMD=true BOT_AGENT_ARGS="" \
    bash "$RUN" --task "$DUP_TASKS/T-996B.md" --reset --repo "$REPO" >/dev/null 2>&1 || DUPRC=$?
check "two tasks, one branch" 3 "$DUPRC"

rm -f "$STATE"/tasks/T-996*.md 2>/dev/null
rm -rf "$DUP_TASKS"

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

# ... and "blocked" has to mean the result went nowhere. A folder run
# writes its answer as a patch, so the branch not being pushed is only
# half the promise: the patch is the same publication through a
# different door.
if [ -z "$(ls "$FSTATE/patches" 2>/dev/null)" ]; then
    printf 'ok    %-28s nothing was published\n' "secret stays put"
else
    printf 'FAIL  %-28s a patch was written for a blocked run: %s\n' \
        "secret stays put" "$(ls "$FSTATE/patches")"
    FAILURES=$((FAILURES + 1))
fi

# The control plane cannot live inside a folder project: an import walks
# the whole folder, so state under it would be snapshotted, handed to
# the agent, and snapshotted again next run.
FSELF=0
folder_fixture
BOT_AGENT_CMD=true BOT_AGENT_ARGS="" \
    bash "$RUN" --task "$FOLDER_ROOT/T-992.md" --reset --repo "$FPROJ" \
        --state "$FPROJ/.state" >/dev/null 2>&1 || FSELF=$?
check "state inside the folder" 1 "$FSELF"

# A channel is a file the agent wrote, never a link to one: `-f` is true
# through a symlink and `mv` moves the link, so an artifact could be a
# link to any file on the host. Skipped where the shell cannot make one
# - Git Bash on Windows copies instead - because a check that silently
# passes is worse than one that says it did not run.
SYMOK=0
folder_fixture
if ln -s "$FPROJ/README.md" "$FOLDER_ROOT/symprobe" 2>/dev/null \
   && [ -L "$FOLDER_ROOT/symprobe" ]; then
    SYMOK=1
fi
rm -f "$FOLDER_ROOT/symprobe"
if [ "$SYMOK" -eq 1 ]; then
    SYMRC=0
    BOT_AGENT_CMD="$STUBS/linker.sh" BOT_AGENT_ARGS="" \
        bash "$RUN" --task "$FOLDER_ROOT/T-992.md" --reset --repo "$FPROJ" \
            --state "$FSTATE" >/dev/null 2>&1 || SYMRC=$?
    check "symlinked channel" 1 "$SYMRC"
else
    printf 'skip  %-28s this shell does not make symlinks\n' "symlinked channel"
fi

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
