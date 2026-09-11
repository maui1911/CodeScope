# agent-bots

**Status:** prototype · **Ships:** no · **Product code touched:** none

An experiment in giving CodeScope persistent, *named* agents that pick
up tasks, do the work in their own git worktree, and prove what they
did — coordinating through plain files instead of a runtime.

The prototype here deliberately uses **no CodeScope code at all**. It
is a shell script plus a folder of Markdown. That is the point: if the
idea only works once it is wired into the GPUI shell, the idea is not
ready. Prove the contract first, port it second.

---

## 1. Prior art: xAI Grok Bot

Grok Bot launched in beta on 2026-08-11. It is not the `@grok` chatbot
on X — it is a *computer-use agent* that operates software the way a
person does, clicking and typing until a job is done.

### The concepts it ships

| Concept | What it means |
|---|---|
| **Bot** | One persistent, named agent with a single primary job, a charter description, and durable memory across conversations. Up to 50 per account. |
| **Cloud computer** | An always-on Linux VM with browser, filesystem and terminal. Work under `/workspace` survives sessions. |
| **Memory** | Per-bot role, preferences and summaries of earlier work. Context compounds instead of resetting. |
| **Skill** | A reusable procedure — purpose, inputs, workflow, validation. Taught by *demonstration*: screen-record a workflow once and the bot replays it. |
| **Routine** | A skill bound to a schedule or a trigger. |
| **Teams / Chief of Staff** | Bots message each other, share context in threads, and pass ownership. The viral pattern is one router agent delegating to named specialists. |
| **Approvals** | Send, publish, pay, delete and production changes sit behind a gate. |

The recommended progression is worth stealing verbatim: **run once by
hand → refine → save as a skill → automate as a routine.**

### What the marketing gets wrong

Two things you will not learn from a promo video:

**"Every bot gets its own computer" is false.** The docs say the
opposite: all of one user's bots share a single VM — files, browser
sessions *and* logins. Each bot gets its own *screen*, which is a work
surface, not a boundary. The documentation states plainly that separate
bots must not be used as a security boundary. A research bot and an
invoicing bot sit on the same credential pool.

**There is no task model.** No task object, no status field, no
dependency graph, no board. Action-level audit logging was listed as
"coming" at launch.

### The part that actually matters

Because there is no task model, every serious multi-bot design in the
wild rebuilt the same thing on disk:

- a **board file** with exactly one writer
- **one file per task**
- a **`handoffs/` folder** of structured transitions
- a **`context/` folder** (architecture, conventions, glossary) that
  stops a bot "inventing a plausible-sounding wrong name"

The single-writer rule is not stylistic. Two bots writing one file lose
one of the edits *with no error and no warning*. The handoff format
that emerged carries six fields — **objective, artifact, evidence,
status, blockers, next action** — and the message carries a *path*, not
the content.

The summary of the whole discovery:

> *"Handoff quality is a file format problem, not a prompting problem."*

That sentence is why this experiment exists. The valuable half of Grok
Bot is a file contract, and a file contract is free.

### Sources

Primary (xAI):
[Introducing Grok Bot](https://x.ai/news/introducing-grok-bot) ·
[Grok Bot docs](https://docs.x.ai/grok-bot/overview) ·
[Grok Bot and X](https://x.ai/news/grok-bot-and-x)

Secondary:
[Flavio Copes deep dive](https://flaviocopes.com/grok-bot/) ·
[The Architecture DeepDive](https://thearchitecturedeepdive.blog/articles/grok-bot-architecture.html) ·
[Build your own (AI Builder Club)](https://www.aibuilderclub.com/blog/how-to-build-your-own-grok-bot) ·
[CellCog on the shared-computer security model](https://cellcog.ai/blog/grok-bot-security/) ·
[Composio guide](https://composio.dev/content/guide-to-frok-bot)

Only the first three are primary. The secondary sources disagree with
each other on pricing, so nothing here rests on them alone; the
architecture claims above are ones that multiple independent sources
and the official docs agree on.

---

## 2. Why CodeScope is unusually well placed

Most of the hard half already exists in the product:

| Grok Bot concept | Already in CodeScope |
|---|---|
| Cloud computer | the local machine, plus a **git worktree per session** |
| Launching an agent | `agent_registry::AgentProfile` — argv, `resume_args`, `session_id_flag` |
| "The turn finished" | `telemetry::SessionState` → `Idle` / `Busy` / `PendingToolUse`, from the transcript tail |
| Feeding a prompt in | `codescope_terminal::backend::write_input` |
| Persistence + retention | `projects.json`, `session.rs` |
| Verification surface | `git.rs`, `diff.rs`, `pr.rs` (incl. `CiStatus`) |

One gap in that row, found the hard way (F-10): every field on
`AgentProfile` describes how to start an *interactive* session for a
human to type into — `resume_args`, `new_session_args`,
`session_id_flag`. There is no headless invocation in it, because
nothing in the product has ever needed one. A bot layer does.

`SessionState::Idle` plus `write_input` is, between them, a complete
agent loop. The motor is built.

And on two axes CodeScope is *ahead* of the thing being imitated: the
worktree is a real isolation boundary, and everything lands in git, so
the audit trail xAI was still promising is a `git log`.

---

## 3. Design

### 3.1 Three planes

The naive port of `/workspace` breaks immediately, and the reason is
worth stating because it is not obvious: **worktrees have separate
checkouts.** If `board.md` lives in the repo and bot A updates it on
its branch, the orchestrator on `main` does not see it. The file is not
*conflicting*, it is *invisible*. Single-writer does not save you.

So state is split by lifetime and by writer:

| Plane | Where | Written by | Contents |
|---|---|---|---|
| **Contract** | in git, on `main` | humans only, via PR | `context/`, `skills/`, `bots/*/BOT.md` |
| **Control** | outside git | the runner | board, tasks, handoffs, runs *(inbox and per-bot memory: designed, not built)* |
| **Work** | worktrees + branches | one bot per branch | the actual code |

"Humans only" on the contract plane is a convention, not something the
code can enforce — the runner only catches a bot that *commits* an edit
there, and only because `touches:` happens to exclude `labs/`.

The contract plane belongs in git because changing a bot charter or a
project convention *is* a reviewable act. The control plane must stay
out of git because mutable coordination state inside branches is the
bug described above. Grok Bot puts both in `/workspace`; the
single-writer rule is the bandage on that wound.

In the product, the control plane would go through `codescope_core::paths`
so dev/installed separation keeps working. In this prototype it is a
`.state/` directory (git-ignored).

### 3.2 The board is append-only

One refinement on the published pattern. Instead of a mutable board
that one writer owns, this design splits it in two:

- **`board.md` is an append-only event log.** Timestamp, task id,
  event, ref. Appending never conflicts and never silently loses a
  write.
- **Status lives on the task file**, whose single writer is that
  task's runner.

Same guarantee, no central mutable document, and the log doubles as the
audit trail.

Append-only is a property of the *writes*, though, not of the file —
creating it is still a race, and the first version lost rows to it
(F-14). The lock that fixed that then needed a release path of its own,
and the file has to *appear* atomically as well as be created
atomically, or a waiter appends into a half-written header (F-17).

### 3.3 What git gives you for free

| Grok Bot had to build | git already has | CodeScope already wraps it |
|---|---|---|
| per-bot isolation | worktree + branch | `git::add_worktree(repo, path, branch, base)` |
| approval gate | pull request | `pr::fetch_for_branch` + `CiStatus` |
| audit log *("coming")* | `git log`, reflog | `diff.rs`, `git::git_status` |
| rollback | delete the branch | `git::remove_worktree(.., force)` |
| evidence for a claim | commit SHA + `--numstat` | `GitStatus { added, removed, ahead, behind }` |
| conflict handling | rebase | `git::rebase_onto(repo, base_ref)` |

The sharpest line in that table: the xAI docs concede that an approval
*gates the action but does not reverse completed work*. **Approvals
give you stop. Git gives you undo.**

### 3.4 The loop

This is the **design**. What the prototype actually implements is in
section 4; the lines marked *(designed)* have no code behind them yet.

```
dispatch  ->  assert the contract exists at the base commit, or refuse
              add_worktree(path, "bot/<owner>/<task>", base)
              record the base SHA                          (designed:
                                                    on the task file;
                                          today it goes to the board
                                                     and the handoff)
work      ->  launch the agent in that worktree with a deterministic prompt
              the agent may write .bot-blocked to report a refusal
verify    ->  run the task's own verifier command on that tree, exit 0
              and the diff touches only the task's declared `touches:` globs
gate      ->  push + PR  (never straight to main - a hard invariant in code,
                          not an instruction in a prompt)     (designed)
done      ->  merge -> remove worktree, delete branch, task Done
```

Three properties this buys:

**Evidence is a SHA, not prose.** The handoff `evidence` field is
`bot/a/T-0142@abc1234` plus a numstat. The runner does not believe the
agent summary; it reads the tree.

**A handoff is a branch handoff.** Bot A passes to bot B by naming a
*ref*. B branches off it. That is "the message carries a path" in git
form — stronger, because a ref is verifiable and cannot silently go
stale.

The built version of this is narrower and more interesting than the
sketch. A review that ends `changes-requested` makes the runner write
the follow-up task: `owner` and `verify` from the review task, which is
contract; `touches` from the paths the findings cite, each re-checked
against the tree; `base` from the commit the reviewer actually had; and
an objective that is a **path to the review**, never a copy of it. The
bot that found the problem does not get to say what may be touched to
fix it — that would be self-reported scope, which is the one thing
this design refuses everywhere else. See F-21.

The derived task lands in `.state/proposed/`, and running it is the
acceptance. `--chain` dispatches it in the same breath, and is off by
default: a task written by a machine and started by a machine with
nothing in between is a different risk class, and in the product that
gap is where the approval inbox goes.

**Concurrency control without locks.** Each task declares `touches:`
globs, and the runner refuses to dispatch a task whose files another
in-flight task already claims. Both sides are expanded against the base
tree and intersected, so the answer is a list of real paths rather than
an opinion about two patterns; `--allow-overlap` is the escape hatch
for a collision you mean to resolve by hand. A refusal costs nothing —
it happens before the worktree exists — and it lands on the board,
because two tasks written to collide is worth a row.

The check and the claim are one step, not two: a `mkdir` lock is held
from the last read of the control plane until this run is visible in
it, and the scan inside that lock is the one that counts. A check that
runs before the claim is advice (F-17).

"In flight" means status `dispatched` **and** a worktree still on disk.
Believing the status field alone would wedge every later task behind a
run that crashed; a dispatch with no worktree is reported as stale and
ignored. What is still missing is the other half: rebase-on-collision
and re-verify when the base moves under a finished branch. See F-16 for
what the check cannot see.

Crash recovery is *cheap*, not free. Nothing lives in memory, so the
state is all on disk — but there is no resume path: a run that dies
mid-flight leaves the live task at `dispatched`, and the runner refuses
to start again until the worktree and branch are gone and you pass
`--reset`. Re-deriving the phase from git is the design; the code only
detects that it happened.

### 3.5 Where it gets messy

- **Worktree sprawl.** One worktree per task is gigabytes fast. Cap
  concurrency low (4, not 50), prune on merge, sweep `git worktree prune`
  at startup. The `session.rs` `RetentionPolicy` (TTL + cap per
  worktree) already has the right shape.
- **Windows path length and file locks.** Deep worktree paths plus
  rust-analyzer and `target/` holding handles make `worktree remove`
  fail. `sidebar.rs` already has the force-fallback; the bot layer must
  inherit it rather than reinvent it.
- **Contract drift per branch.** A bot editing `CONVENTIONS.md` changes
  the truth for nobody until merge. Hence: bots never write to the
  contract plane.
- **Prompt injection.** An agent reading an issue or a web page can
  find instructions in it. `context/` and the task file are the only
  instruction sources; everything fetched is data. Anything
  irreversible goes behind the gate.
- **The task file is a host-execution surface.** `verify:` is run with
  `eval` on the *runner's* machine, outside the worktree and outside
  any agent permission model. Task definitions are repo content, so
  whoever can land a commit in `examples/` controls a shell command
  with your privileges. That is acceptable while the task files are
  your own; it becomes a hard gate the moment a task could arrive from
  a pull request. See F-6.
- **Secrets.** Gate before the push, not after. A key in a pushed
  commit has already been seen.

---

## 4. What the prototype covers

| | |
|---|---|
| Covered | the file contract; **two bots** (`fixer`, `reviewer`), two task kinds (`change`, `review`) and the handoff between them; contract-read-at-base (existence *and* argv); a serialised dispatch claim with a cross-task `touches:` overlap refusal; worktree create; agent run; the `.bot-blocked` refusal channel; the `.bot-review.md` output channel; verifier, in a clean checkout of the branch tip; evidence capture incl. scope and TODO checks; handoff write; append-only board; no-op cleanup |
| **Not** covered | scheduling/routines, the approval inbox, per-bot memory, rebase-on-collision, resume after a crash, pushing, opening PRs, any GPUI surface |

Two deliberate omissions:

**The runner calls the agent headless, not through a PTY.** The product
path is `write_input` plus the telemetry tail; the prototype swaps in a
headless call so the file contract is tested in isolation from terminal
plumbing. Same contract either way — that is the thing being proven.

**It never pushes and never opens a PR.** The loop stops at "branch is
ready, here is the SHA". Outward-facing actions stay a human call until
the contract has earned trust.

## 5. Running it

```bash
# 1. See the resolved plan and the exact prompt. Changes nothing.
labs/agent-bots/run/bot-run.sh \
  --task labs/agent-bots/examples/T-0001-smoke-test.md --dry-run

# 2. Full loop with the agent call stubbed out. Proves worktree,
#    verifier, evidence and handoff on their own.
labs/agent-bots/run/bot-run.sh \
  --task labs/agent-bots/examples/T-0001-smoke-test.md --skip-agent

# 3. For real.
labs/agent-bots/run/bot-run.sh \
  --task labs/agent-bots/examples/T-0001-smoke-test.md
```

Work through those in order on the first run — step 2 is where a wrong
path or a broken verifier shows up, and it costs no tokens.

`run/sweep.sh` drives every stub in `run/stubs/` through the runner and
checks each verdict, then reports any worktree or lock left behind. It
is the one command that says whether the loop still holds:

```bash
bash labs/agent-bots/run/sweep.sh
```

A task that already finished will not re-run; pass `--reset` to discard
the live task and start over. Exit codes are `0` done, `1` blocked,
`2` needs-review.

A task whose files another in-flight task already claims is refused
before it costs a worktree; the plan prints the overlapping paths, and
`--allow-overlap` overrides it. `examples/T-0004-overlap-fixture.md`
reproduces both that refusal and the stale-dispatch case by hand.

**Two kinds of task.** `kind: change` is the default and is what T-0001
and T-0002 are. `kind: review` runs the `reviewer` charter instead: it
must commit nothing, it writes `.bot-review.md`, and the runner
harvests that into `.state/reviews/` and points the handoff at it.

```bash
# a review run - the reviewer reads, judges, and commits nothing
labs/agent-bots/run/bot-run.sh   --task labs/agent-bots/examples/T-0005-review-overlap-check.md
```

Its verifier is `run/review-shape.sh`, which checks the review against
the tree it claims to be about — including that every cited `path:line`
is a real file and a real line at that commit, and falls inside
`touches:`. What it cannot check is whether the review is *right*; see
F-19 for where that ceiling sits.

A review task that also declares `on_changes_requested:` and
`derived_verify:` hands its verdict on. `changes-requested` then makes
the runner write a task for that bot into `.state/proposed/` and
address the handoff to it by name:

```bash
# review, then hand the findings to the fixer as a scoped task
labs/agent-bots/run/bot-run.sh   --task labs/agent-bots/examples/T-0006-review-telemetry.md

# ... and dispatch that task too, rather than printing the command
labs/agent-bots/run/bot-run.sh   --task labs/agent-bots/examples/T-0006-review-telemetry.md --chain
```

**Which CLI runs is data.** The task's `agent:` wins, otherwise the
charter's; the profile lives in `contract/agents/<id>.agent.md`. There is no
built-in default — a bot says what it runs on. `claude`, `codex`,
`gemini`, `copilot` and `pi` are filled in and verified; `opencode` is
a blank stub that dispatch refuses. A task can pin `model:` too, and
dispatch refuses rather than silently dropping the pin when the
profile has no model flag. See F-10.

`BOT_AGENT_CMD` and `BOT_AGENT_ARGS` override the profile; they exist
for the stubs in `run/stubs/`.

**Claude Code runs under `--permission-mode auto`, not `acceptEdits`.**
The prompt asks it to run the verifier and to commit; both are Bash
calls, and `acceptEdits` covers edits only. An unattended run under
`acceptEdits` cannot finish its job — it makes no commit, which is
precisely the shape F-4 describes. Worth being blunt about what that
costs: the worktree bounds what the agent is *meant* to touch, not what
it *can*. It is a work surface, not a security boundary — the same
criticism this document levels at Grok Bot in section 1. What actually
contains the blast radius here is that the branch is throwaway and
nothing in this loop ever pushes.

**`base:` must be a ref that contains the contract.** The agent reads
its charter and context from its own checkout — the base commit — not
from your working tree. While this branch is unmerged that means
`base: labs/agent-bots`, not `origin/main`; the runner refuses up front
otherwise. It also means contract edits only reach the agent once they
are committed.

### Where things land

| Path | What |
|---|---|
| `.state/board.md` | append-only event log |
| `.state/tasks/<id>.md` | the live task — the repo copy is only a definition |
| `.state/handoffs/<ts>_<bot>__to__human__<id>.md` | the handoff |
| `.state/reviews/<ts>_<bot>_<id>.md` | a review, harvested off the worktree |
| `.state/proposed/<id>.md` | a task one bot's run wrote for another |
| `.state/runs/<day>/<id>-<ts>.log` | agent + verifier output |
| `<repo>.worktrees/bot-<owner>-<id>/` | the work |

`.state/` is git-ignored, and a clean no-op run removes its own
worktree and branch.

## 6. What would have to be true to graduate this

1. The loop runs unattended and green three times on a real issue in
   this repo.
2. The verifier catches at least one agent run that *claimed* success
   and was wrong. If that never happens, the verifier is not verifying.
3. A handoff between two bots survives a rebase.
4. Worktree cleanup works on Windows with a build running.
5. `verify:` no longer runs through `eval` on the host, or task files
   are provably trusted input. A product feature cannot ship a shell
   command sourced from repo content. See F-6.

Until then it stays in `labs/`.

---

## 7. What production would look like

None of this is built. It is written down because the prototype's own
layout is an artefact of prototyping, and the differences matter.

### 7.1 Where bots run

**Locally, on the user's machine, in a worktree of the project.** No
cloud VM. That is the deliberate inverse of Grok Bot and it is mostly
gain: your credentials, your git, your network, and a real isolation
boundary per bot instead of one shared machine.

It costs one thing Grok Bot sells, though, and the product has to be
honest about it: **"always-on" does not hold.** With CodeScope closed,
nothing runs. A routine on a schedule means "runs while the app is
open", which is a weaker promise than the word suggests. That is
defensible for coding agents — you want to review the results anyway —
but it has to be said in the UI rather than discovered. The alternative
is a headless service alongside the app, which is a much larger
commitment than it first appears.

### 7.2 Where the contract lives

In the project's repo, with a per-user layer above it. F-3 constrains
this more than it looks.

**Per project, in the project's repo:** `.codescope/` at the root,
committed — `context/`, project-specific bots and skills. It travels
with the branch, every worktree checkout has it automatically, and it
is reviewable through a PR. Exactly the role `CLAUDE.md` already plays.

For this repo that means `labs/agent-bots/contract/` eventually becomes
a plain `.codescope/` at the root. Its current location is scaffolding.

**Per user, global:** `%APPDATA%\CodeScope\bots\` — bots the user wants
across projects. Here is the tension: such a bot is in no commit, so
the F-3 check cannot cover it and no handoff can prove which version of
the charter ran.

The resolution: **global is a template, project is an instance.** A
global bot is materialised into that project's `.codescope/bots/` on
first use and becomes versioned from then on. That is what Grok Bot
does with templates — a one-way copy, no live link — and it keeps the
F-3 property intact. Accepting unversioned global bots instead only
works if the handoff records the charter's hash, which is half the work
for less of the benefit.

**The control plane stays out of the project repo** (§3.1). It goes
through `codescope_core::paths` into the state directory —
`%LOCALAPPDATA%\CodeScope\bots\<project-id>\` — one per project, with
board, tasks, handoffs and runs. Routing it through `paths` is what
gives dev/installed separation for free, which `CLAUDE.md` requires of
any new on-disk state.

Worktrees need nothing new: `Project::worktree_root_path()` already
resolves to `{project}.worktrees`, so a bot task lands in
`{project}.worktrees/bot-<owner>-<taskid>`.

### 7.3 Resolving F-7

In production this gets *better*, not worse: the bot runs in the
project's worktree, so inheriting that project's `CLAUDE.md` is exactly
what you want. A bot that does not know the project conventions is
useless.

So: **declare, do not suppress.** Three layers, stated explicitly:

| Layer | Scope |
|---|---|
| `CLAUDE.md` | true for every agent in this project |
| `.codescope/context/` | true for every *bot* — glossary, architecture |
| `BOT.md` | what makes *this* bot different: role, scope, escalation |

And the cheap part that makes it auditable: the runner records which
instruction sources were loaded, with their hashes, in the handoff
evidence. When two bots behave identically, you can see why. That is
roughly five lines and it resolves the honest half of F-7 without
throwing away the project conventions.

`BOT.md`'s claim that "Nothing else is an instruction" has to go. It
was simply wrong.

### 7.4 The UI

The first version needs almost no new UI, and **the handoff is the
UI.**

*Stage 1 — no new concepts.* A bot run is a session CodeScope opened
for you, in a worktree, with a marker. Tab, telemetry badge, idle
toast, diff viewer, PR status: all of it exists and all of it already
works. The only new thing is *why* the session started. The sidebar
already renders bot worktrees with `+N -M`, ahead/behind and CI — they
need a label, not their own tree. Starting one goes in the project's
right-click menu next to worktree creation, plus the command palette,
behind a dialog that mirrors `NewWorktreeDialog`.

*Stage 2 — the inbox.* This is the part that genuinely needs new
surface, and it is a queue rather than a dashboard: *"2 bots done, 1
blocked."* A sidebar badge, a list, and the existing toast path
underneath. A handoff is already a structured document — objective,
artifact, evidence, status, blockers, next action. Rendering one well,
with the SHA linked into the diff viewer and the branch into the
worktree, is most of the value.

*Stage 3 — the board.* See §7.5; this is less optional than it looks.

**The pitfall to design around:** bot runs must not open tabs while
running unattended. Four bots means four tabs shoving the user's own
work aside. A bot session should be *background* by default and get a
tab only when opened — which in turn means the telemetry tail and the
idle toast have to work with no visible tab. Whether that holds today
is worth checking before the design sets.

### 7.5 The destination is multiple bots

The position taken for this project, 2026-09-11: **the power is in the
orchestration and the board, which means multiple bots.** A single bot
doing one task in a worktree is a thin delta over what CodeScope
already does when you open an agent session there — automated dispatch,
verification and handoff are real, but incremental. Parallel bots with
distinct scopes, a board showing who owns what, and handoffs between
them is the point where hosting becomes orchestration. It is also what
made the Grok Bot pattern spread: the Chief of Staff, not the
single agent.

That is almost certainly right about the destination. It does not
change the order — every multi-agent system that works was a
single-agent loop that worked first, and an adversarial review found
seven findings in *one* bot's loop, which multi-bot would multiply
rather than avoid. The single loop only stopped lying about its verdict
today, and only on a no-op task.

What it does change is the priority *within* the plan. Two items move
from "someday" to "next", because they are multi-bot blockers rather
than single-bot polish:

1. **F-7 becomes blocking.** Two bots on one machine inherit identical
   host instructions, so everything the contract does not pin down is
   identical by construction. If differentiated roles are the value
   proposition, F-7 attacks it directly. §7.3 is the fix.
2. **The `touches:` overlap check and rebase-on-collision stop being
   optional.** Two bots working one repo *will* collide. The overlap
   refusal is now built (§3.4) — the half that is not is what happens
   when the base moves under a branch that already passed: rebase,
   re-verify, and route a failed rebase to a human. With one bot that
   is a rare annoyance; with four it is the normal case.

The second bot now exists — `reviewer`, which reads and judges and
commits nothing — and the first thing it did was find four open races
in the dispatch path that dispatched it (F-20). That is the argument
for this direction in one run: a second bot is not twice the throughput,
it is a different pair of eyes on work the first one cannot see.

The handoff between them is built too (F-21): a review that ends
`changes-requested` becomes a scoped task for the fixer, written by the
runner rather than by either bot. The first time that ran end to end,
the fixer read the review, checked its single finding against the
source, and refused it with a reason — which turned out to be the one
outcome the runner had no verdict for (F-22).

What is still missing is everything about *when*. Nothing schedules a
bot, nothing notices that a task has become ready, and `--chain` is one
link long by construction. The board records what happened; nothing yet
reads it to decide what happens next.

And the board stops being a maybe. It was designed append-only from the
start (§3.2) precisely so it would survive concurrent writers, so the
single-bot phase should be treated as scaffolding for it rather than as
a destination of its own.

## 8. Findings

Running log. Each entry is something a run taught that the design did
not predict — that is the only reason this folder exists.

### F-1 · A verifier must be narrower than, or equal to, `touches:`

*2026-09-11, first `--skip-agent` run.*

T-0001 shipped with
`cargo clippy -p codescope-core --all-targets -- -D warnings` as its
verifier and `core/src/telemetry.rs` as its only `touches:` glob. The
run came back `blocked` with exit 101 — on ~12 pre-existing clippy
findings in `settings.rs` and elsewhere, files the task was explicitly
forbidden to edit.

Nothing was wrong with the loop. The loop reported exactly what was
true. The *task* was unsatisfiable: no change inside `touches:` could
ever make that verifier pass.

**Rule:** a verifier must be a predicate the owner can satisfy inside
its own scope. If the verifier is wider than the task, every run is
blocked on somebody else's debt, and the bot cannot tell the difference
between "I failed" and "it was already broken".

This generalises past clippy. Any whole-crate gate — a test suite with
an unrelated flaky test, a lint pass, a type check across a boundary —
has the same failure mode.

**Consequence for the product port:** dispatch should refuse a task
whose verifier demonstrably fails on the *base commit*, before the
agent is ever launched. That is one cheap `git`-clean run, and it turns
a confusing blocked handoff into a clear "this task is malformed".
Grok Bot has no equivalent, because it has no verifier at all.

*Side note, not a finding:* those ~12 clippy findings on `main` are
pre-existing and out of scope here. `CLAUDE.md` asks for clippy clean
on *changed files*, so this is accepted debt, not a regression.

### F-2 · `git worktree remove` needs the force fallback on Windows

*Same run.*

The verifier created `target/` inside the worktree. Plain
`git worktree remove` refused; `--force` succeeded. The script already
chains the two, which is why this cost nothing — but it confirms that
the product port must inherit `sidebar.rs`'s existing two-step remove
rather than reinventing it.

### F-3 · The contract must exist at the *base commit*, not in your checkout

*2026-09-11, adversarial review of the first commit.*

The design says the contract plane lives in git so it travels with the
branch. The prototype then pointed the agent at
`labs/agent-bots/contract/...` while creating the worktree from
`origin/main` — where `labs/` does not exist at all
(`git ls-tree origin/main labs/` returns nothing). The first real run
would have launched an agent into a tree with no charter, no
conventions and no glossary, and the runner would have had no idea.

Nothing about the *design* was wrong. The runner just never checked a
precondition it depends on.

**Rule:** assert the contract exists at the base commit before spending
a worktree and an agent run on it. Implemented; a bad base now fails in
under a second with the ref named.

This is F-1 again in a different costume — prove the precondition at
base, not halfway through. Two instances of the same class in two runs
suggests the general rule for the product port: **everything the run
depends on gets checked against the base commit at dispatch.** The base
is the only tree that exists before the work starts, so it is the only
place a cheap check can live.

A corollary worth stating: contract edits reach the agent only once
committed. Editing `BOT.md` in your working tree changes nothing about
the next run. That is correct — a versioned contract is the point — but
it surprises you the first time.

### F-4 · A crashed agent reported `done`, and the cleanup deleted the evidence

*Same review.*

The runner captured the agent's exit code, logged it to the board, and
then never read it again. The verdict looked only at the verifier, the
scope check, the dirty count and the commit count. So a wrong CLI flag,
a missing binary or an auth failure produced: no commits → verifier
passes at base → `done` → *no-op* → worktree and branch deleted →
exit 0. The script header claimed a wrong flag "fails loudly". It
failed as a green no-op.

Worse, this was the failure mode most likely to happen on the very
first real run, because `--permission-mode` friction in headless mode
produces exactly that shape: an agent that cannot commit.

Two fixes, and the second is the interesting one:

1. The exit code now enters the verdict, ahead of the verifier — a
   crashed agent explains a failing verifier, not the other way round.
2. **The agent had no channel to report a refusal at all.** The charter
   told it to escalate with `status: blocked`; the prompt forbade it
   from writing a handoff (rightly — self-reported evidence is not
   evidence); and nothing read its prose or its exit code. A bot told
   to stop had no way to say so. It now writes one line to
   `.bot-blocked` in the worktree, which the runner reads, deletes
   before anything counts the dirty files, and turns into a `blocked`
   handoff.

The general shape: *forbidding self-reported evidence is right, but it
obliges you to provide a channel for self-reported **intent**.* Those
are different things, and the first design collapsed them.

Verified with two stub agents: one writing `.bot-blocked` and exiting
0, one exiting 3 silently. Both now land on `blocked` with the worktree
kept; both were `done` with the branch deleted before the fix.

### F-5 · The scope check trusted `git diff --name-only` too much

*Same review.* Three ways the subset check could be wrong:

- Non-ASCII paths came back quoted (`"core/src/\303\244.rs"`), so any
  such file looked like a violation. Fixed with `core.quotepath=off`.
- Rename detection listed only the destination, so an agent could move
  an out-of-scope file *into* scope and delete it invisibly. Fixed with
  `--no-renames`, which reports the source deletion too.
- `touches:` globs are shell `case` patterns, so `*` crosses `/`:
  `core/*.rs` silently scopes the entire crate. Not a code change —
  documented in `templates/TASK.md`, because the fix is for task
  authors to name files.

### F-6 · The verifier is host execution sourced from repo content

*Same review. Open — not fixed.*

`verify:` runs through `eval` on the runner's machine, outside the
worktree and outside any agent permission model. The task file that
supplies it is repo content. Today that is fine: the task files are
yours, and the runner is a script you invoke by hand.

It stops being fine the moment a task file could arrive from a pull
request, which is exactly what "CodeScope dispatches bots from tasks in
the repo" would mean. Logged as graduation criterion 5 rather than
patched, because the honest fix is an allowlist of verifier commands or
a sandbox, and that is a design decision, not a one-line change.

Same run also hardened `owner:` and `id:`, which were being spliced
into filesystem paths unsanitised.

**Re-raised in the review on #346, and the answer is unchanged.** One
thing did narrow, via F-13: the verifier now runs in a detached
checkout of the branch tip rather than in the agent's worktree, so it
can no longer quietly rewrite the tree the handoff describes. That is a
smaller surface, not a closed one — it is still arbitrary repo-sourced
code running with the runner's privileges, and it stays criterion 5.
### F-7 · The charter is not the agent's only instruction source

*2026-09-11, first real agent run.*

The run itself was clean: the agent read its charter, ran the verifier
before and after, noticed that the only clippy hit matching
`telemetry.rs` was in `core/src/agents/opencode/telemetry.rs` and
therefore outside `touches:`, made no commit, wrote no `.bot-blocked`,
and explained why. Exactly the behaviour the contract asks for.

It reported in Dutch.

`context/CONVENTIONS.md` says English only. Nothing in the prompt or
the contract asked for Dutch — the nested agent inherited it from the
host: `~/.claude/CLAUDE.md`, the project `CLAUDE.md`, and the user's
output-style and language settings all load into a `claude -p` session
before the prompt is read.

So `BOT.md`'s "Nothing else is an instruction" is false as written. A
bot is not a clean room; its charter is *additive* to whatever the host
config already says, and the host wins on anything it states more
specifically. Harmless here — this project's `CLAUDE.md` agrees with
the contract. It stops being harmless when the whole premise is
narrow, differentiated roles: two bots on one machine inherit the same
host instructions, so the part of their behaviour the contract does not
pin down is identical by construction.

**Resolved in design, not yet in code — see §7.3.** Suppressing the
host config was rejected: it would cost the project conventions a bot
genuinely needs. The decision is to declare the layers instead
(`CLAUDE.md` → `context/` → `BOT.md`) and have the runner record which
instruction sources were loaded, with hashes, in the handoff evidence.
`BOT.md` has been corrected already; the evidence recording has not
been written.

This was promoted from "worth deciding eventually" to a multi-bot
blocker once the destination was settled (§7.5): if differentiated
roles are the value, then instructions the contract does not pin down
being identical across every bot attacks the premise directly.

The narrow lesson for the product port: whatever launches a bot has to
know exactly which instruction sources that process will load. Today
the runner does not.

### F-8 · Criterion 2 cannot be manufactured, so the mechanism was tested instead

*2026-09-11, T-0002 — the first task with real work.*

T-0002 cleared the 12 clippy findings `codescope-core` actually
carried. The agent produced one commit across five files, +44/−38,
clippy and all 542 tests green, diff inside `touches:`. Verdict `done`.

Reviewing the diff by hand, it was *right*, including on both judgement
calls:

- `should_implement_trait` on `AgentId::from_str` — kept the
  `Option`-returning inherent method behind an `#[allow]`, reasoning
  that an unknown id is an ordinary `None` rather than an `Err` and
  that the `projects.json` round-trip depends on the signature. That is
  the correct call and it is the one the skill sanctions.
- `too_many_arguments` on a `#[cfg(test)]` fixture — `#[allow]` with a
  one-line reason, rather than inventing a params struct for test data.

Both allows carry the justification the task asked for. The apparent
whitespace-only lines in the diff are nested struct fields reindented
by the change itself, not reformatting.

So **graduation criterion 2 is not met, and could not be** — the run
that was supposed to claim success while being wrong was simply
correct. That criterion cannot be scheduled; it is satisfied by
accumulating real runs until one goes wrong, and two real runs have now
gone right.

What *can* be established on demand is that the mechanism works, so a
third stub agent was written: it commits a deliberately failing test,
prints *"Done. Cleaned up telemetry.rs and verified the full suite
passes."*, and exits 0. The runner returned `blocked`, verifier exit
101, worktree kept.

That is the property criterion 2 is really about — **the verdict comes
from the tree, not from the claim** — and it now has a regression test
rather than a hope. Criterion 2 stays open as written, because a stub
proves the runner is honest, not that a real agent ever isn't.

One small thing worth recording: the task file said the crate carried
21 findings. It carried 12 — the 21 came from counting `-->` spans,
which include clippy's `note:` lines. The agent silently used the right
number and never flagged the discrepancy. Harmless here, but at scale a
task file that has drifted from reality is exactly the kind of thing
nothing in this design currently reconciles.

### F-9 · Nobody chose the model, and nothing recorded it

*2026-09-11, noticed while cherry-picking T-0002 for review.*

The bot's commit carried `Co-Authored-By: Claude Fable 5.1`. The run
that produced it was launched as plain `claude -p` with
`--permission-mode auto` and no model flag, so the nested session
resolved whatever the host's configured default happened to be at that
moment. It was not a decision, and the handoff does not mention it.

Three consequences, in ascending order of importance:

1. **Runs are not reproducible.** Re-running the same task tomorrow may
   use a different model if the host default has changed. The evidence
   block records the SHA, the numstat and the verifier exit code, and
   none of that identifies what produced the work.
2. **`BOT_AGENT_ARGS` is global.** It is one environment variable for
   every bot, which contradicts the premise that bots are individually
   configured. A charter can describe a role but cannot say what to run
   it on.
3. **Model choice is per-bot leverage, and it is being left on the
   table.** A triage bot that reads and routes does not need what a bot
   rewriting a parser needs. In a multi-bot setup (§7.5) that is
   straightforwardly the difference between a loop you can afford to
   run often and one you cannot.

The fix is small and shaped like something CodeScope already has:
`AgentProfile` in `agent_registry` is exactly a per-agent argv record.
The charter should be able to name an agent profile, the runner should
pass it explicitly instead of inheriting a default, and the handoff
evidence should record what actually ran.

That last part is the same fix as F-7's — the evidence block should
say which instruction sources and which model produced this tree.
Together they are what makes a handoff auditable rather than merely
plausible.

*Recording half now implemented — see F-10. Choosing a profile per bot
is done; pinning a model per bot is available via `model:` on a task
but has no charter-level equivalent yet.*

### F-10 · The loop had quietly assumed Claude Code

*2026-09-11.*

CodeScope is CLI-agnostic — `claude`, `codex`, `copilot`, `opencode`,
`pi`, `gemini` all have profiles in `agent_registry`. The bot loop did
not. It hard-coded `claude -p "$PROMPT"`.

The contract itself turned out to be portable already: plain Markdown,
and `.bot-blocked` is just a file. Exactly two things were
Claude-shaped, and they are worth separating because they fail
differently.

**The invocation.** Codex settles the argument on its own: its headless
mode is a *subcommand*, `codex exec <prompt>`, not a `-p` flag. There
is no single argv shape to hard-code, so the invocation has to be data.
`contract/agents/<id>.agent.md` now carries the command, a headless template
with a `{prompt}` placeholder, the fragment that lets the CLI work
unattended, a model flag and the instruction files it reads — same
frontmatter parser as tasks and charters. Five were verified against
the CLIs installed here:

| Agent | Headless | Unattended |
|---|---|---|
| `claude` | `-p {prompt}` | `--permission-mode auto` |
| `codex` | `exec {prompt}` | `-s workspace-write` |
| `gemini` | `-p {prompt}` | `--approval-mode auto_edit` |
| `copilot` | `-p {prompt}` | `--allow-all-tools` |
| `pi` | `--print {prompt}` | *(none needed)* |

`opencode` is a deliberately blank stub — it is not installed here, so
every field would have been a guess. An empty `headless` means
*unfilled*, and dispatch refuses with an explanation. A plausible guess
would instead have failed inside an agent run, after a worktree had
been spent on it.

`pi` is the reason these fragments stay opaque strings. Its flags in
this area (`--no-tools`, `--tools <allowlist>`) *restrict* rather than
permit, because its tools are on by default. So "let it work
unattended" maps to adding a flag for four CLIs and adding nothing for
the fifth. A boolean like `autonomous: true` could not express that,
and a normalised enum would have to grow a case per CLI forever.

**The instruction sources.** Codex reads `AGENTS.md`, Gemini reads
`GEMINI.md`, Claude Code reads `CLAUDE.md`. A contract that states its
conventions in the wrong file is *invisible* to that agent — which
makes this contract data, not trivia. The runner now records which of
an agent's declared instruction files were actually present in the
worktree, and warns on a miss, because a `MISSING` there means every
judgement the bot made was worse-informed than it looked. That is the
part of F-7 the evidence block was missing, and it also closes F-9's
recording half: the handoff carries the agent id, the resolved argv,
and whether a model was pinned.

**A product-level gap this exposed.** `AgentProfile` in
`agent_registry` has `resume_args`, `new_session_args`,
`session_id_flag`, `resume_by_id_args` — every field is about starting
an *interactive* session for a human to type into. There is no headless
invocation anywhere in it, because nothing in the product has ever
needed one. A bot layer does. Whatever ports this will either extend
`AgentProfile` or carry a parallel record, and that is a real design
decision rather than a detail.

**Parked: no second CLI has actually run.** Only `claude` has been
through a real run. The other four profiles are flag-verified against
their `--help` and exercised as far as the resolved argv, which proves
the *dispatch* path but not the loop. Re-running T-0002 under
`agent: codex` was the intended next test and is postponed on quota.

Worth noting for whoever picks it up: the test does not need Codex
specifically. Any second CLI proves the profile mechanism end to end.
Codex is merely the most informative one, because its subcommand shape
differs most from the template the loop was originally written around —
so if the contract is only agnostic on paper, Codex is where that shows
first. `gemini`, `copilot` and `pi` are the cheaper substitutes if the
point is just to see a non-Claude agent complete a task.

### F-11 · A contract file got swallowed by instruction-file discovery

*2026-09-11, minutes after the profiles were committed.*

The profiles were named `contract/agents/<id>.md`, so the Claude Code
profile was `contract/agents/claude.md`. That file was then loaded into
a running session **as a `CLAUDE.md` instruction file** — the discovery
matches case-insensitively, and on Windows the filesystem does too.

It was caught by seeing the profile's own text appear as project
instructions in a session that had no business reading it. Nothing
broke, because the file happens to contain accurate prose about the
agent. That is luck, not design: a profile is a *description of a tool*
and was being served to that tool as *orders*.

`gemini.md` had the same collision waiting against `GEMINI.md`.
`codex.md` and `copilot.md` did not, because those CLIs read
`AGENTS.md` — which is exactly what makes this hard to spot by
inspection. The landmine only exists for agents whose instruction file
is named after the agent, so five of six profiles looked fine.

Profiles are now `<id>.agent.md`. The lookup stays mechanical and the
name cannot be mistaken for an instruction file by any of them.

This is F-7 for the third time, and by now the pattern is the point:
**the contract does not get to decide what the host treats as
instructions.** F-7 was the host config leaking *in*. This is a
contract file leaking *out*, into the host's own discovery. Both come
from the same missing idea — nothing in the design declares the
boundary between "files the bot reads because we told it to" and "files
the runtime picks up on its own".

For the product port, the concrete rule: **no file inside `.codescope/`
may be named such that any supported agent's instruction discovery
would claim it.** That list grows every time a CLI is added, so it
belongs next to `instruction_files` in the profile rather than in
someone's memory.

**One bug found in the making.** The old `BOT_AGENT_ARGS="${BOT_AGENT_ARGS:---permission-mode auto}"`
default survived the rewrite. Since `BOT_AGENT_ARGS` being *set* is the
signal that a stub is being injected, every run silently took the
override path and dropped the profile's `-p {prompt}` — so the agent
would have been invoked with no prompt at all. Caught by reading the
resolved argv in the plan output, which is precisely why the plan
prints it.


### F-12 · Pinning half a contract is not pinning

*2026-09-11, from the review on #346.*

F-3 made the runner prove that the charter, the profile and `context/`
all exist at the base commit before dispatching. It then read the
profile out of the runner's own working tree to build the argv.

So the check and the use disagreed about which tree the contract lived
in. An uncommitted edit to `claude.agent.md` — or simply running the
script from a branch holding a newer profile than `base:` — sends one
CLI, with one set of autonomy flags and one set of `instruction_files`,
into a worktree built from a commit that describes a different one.
Nothing errors. The plan output even looks right, because it prints
what the runner resolved, and the runner is the half that is wrong.

Existence was the easy half to check and the useless half to have.
Every value the runner takes from the contract now comes out of
`BASE_SHA` through `git show`, and the plan prints the profile as
`<base>:<path>` so the tree that answered is visible.

The rule F-3 stated and the code then quietly broke: **the contract is
whatever the agent can see.** Read it from anywhere else and the two of
you are working from different documents.

---

### F-13 · The verifier was measuring the wrong tree

*2026-09-11, from the review on #346.*

The verifier ran in the agent's worktree. Two things follow from that,
and neither is visible in a green run.

An uncommitted edit can be the thing that makes it pass. The handoff
then reports `verify: … -> exit 0` about a branch the verifier was
never run against. The runner did disclose this — the evidence block
carried a parenthetical saying the run was on the working tree — which
is honest and useless: it hands the problem to the one person who is
reading the handoff *because* they did not watch the run.

And `verify:` is arbitrary code (F-6). Every piece of evidence — HEAD,
commit count, dirty count, touched paths, numstat — was read *before*
it ran. A verifier that commits, or writes a file, changes the tree the
handoff claims to describe after the description was taken. A run could
report one clean in-scope commit and hand over something else.

Both collapse into one fix: verify a detached checkout of `HEAD` in a
throwaway worktree. The verifier sees exactly the commits the branch
carries — nothing uncommitted, nothing left ignored by the agent's run
— and it has no path to the tree the evidence describes. The runner
then re-reads HEAD and the dirty count afterwards and blocks the run if
either moved, because "it cannot reach that tree" is a claim, and this
file is about not trusting those.

The cost is a second checkout per run. `CARGO_TARGET_DIR` points at a
runner-owned cache under `.state/`, so dependencies build once instead
of per run; that is the only tool-specific line in the runner, and it
is a cost decision rather than a semantic one. It lives in state rather
than in the agent's worktree so that a bot cannot seed the cache that
judges it.

What this does not fix: the verifier is still host execution sourced
from repo content (F-6). It is now execution against a clean tree,
which is a smaller surface, not a closed one.

---

### F-14 · Append-only is a rule about intent, not a guarantee

*2026-09-11, from the review on #346.*

The board was designed append-only precisely so that concurrent bots
could not lose each other's writes (§3.2). It was created like this:

```sh
if [ ! -f "$BOARD" ]; then
    { …header… } > "$BOARD"
fi
```

Two runs starting together both see no board, both build the header,
and the second `>` truncates the first one's rows. The audit trail
loses evidence in exactly the case it exists for — a single bot never
hits it, and a single bot was all that had ever been run.

Creation is now guarded by `mkdir`, the portable atomic primitive:
exactly one process creates the directory, the loser waits for the file
instead of racing it, and the winner re-tests before writing. The
appends need no lock — one short line through `>>` is an `O_APPEND`
write well under `PIPE_BUF`, so rows interleave but never tear.

The lesson generalises past this file. "Append-only" describes what the
writers promise and says nothing about the file's lifecycle: creation,
rotation and truncation are each a separate race, and each one can
discard exactly the history the format exists to keep. Before the first
genuinely concurrent run — which is where this design is headed — every
state file needs that question asked of it individually, not answered
once by the word *append-only*.

---

### F-15 · The parts that report failure were the parts that failed

*2026-09-11, from the review on #346.*

Two unrelated bugs, both sitting in the path whose entire job is to
report a bad run:

- `set_status` used `sed -i` with no argument and the GNU-only `0,/re/`
  address. On BSD sed — macOS, which this project ships — it fails, and
  it fails *after* the handoff is written, so `set -e` ends the run
  with the live task still saying `dispatched`. The next run then
  refuses to start a task that had in fact finished.
- Reading `.bot-blocked` as `tr -d '\r' < file | head -c 2000` SIGPIPEs
  `tr` once the file passes 2000 bytes. Under `pipefail` that is exit
  141 with `set -e`: an agent whose refusal ran long is precisely the
  case where the promised `blocked` handoff never gets written. The
  same shape sat in the frontmatter parser, whose `| head -n1` survived
  only because the inputs are short.

Both are now written to survive their own failure mode: an `awk`
rewrite-and-rename for the status, `head` before `tr`, and a parser
that reads its input to the end rather than quitting early.

F-4 and F-8 were about the runner reaching a verdict honestly. These
are the layer under that: **the verdict path has to be the most boring
code in the script, because it is what runs once everything else has
already gone wrong.** Anything clever there — a GNU-only flag, a
pipeline that can be cut short — turns a reportable failure into a
silent one.

---

### F-16 · Two globs have no cheap answer, so the tree answers instead

*2026-09-11, building the overlap check.*

The obvious implementation compares the patterns: does `core/src/*.rs`
overlap `core/src/telemetry.rs`? For two arbitrary shell globs, "can
these match a common string" is decidable and not cheap, and every
shortcut is wrong in a direction you only discover later.
Literal-against-literal is easy, literal-against-pattern is one `case`,
pattern-against-pattern is where it turns silly — and a checker that
has to answer "maybe" must refuse, which makes the ordinary case
unusable.

So it does not compare patterns. It expands both sides against the base
tree and intersects the results. The question becomes "name a file both
tasks may edit", the answer is a list of real paths a human can read in
the refusal, and the matcher is the same `case` loop the scope check
already uses — so the overlap check is exactly as correct as the rule
it is protecting, with no second set of semantics to learn.

Two things that costs.

**A glob only overlaps if a file already exists to overlap on.** Two
tasks that will both create `core/src/new_thing.rs` expand to nothing
and sail past each other. Comparing the declared patterns for string
equality patches the narrow case — identical declarations do collide —
but `core/src/*.rs` against `core/src/new_thing.rs` does not, because
the file is not in the base tree. The honest fix is the pattern
comparison this finding opens by avoiding, and it is only worth
building once a bot is allowed to create files. Today's tasks are not.

**It is a check at dispatch, not a lock.** It answers "are these two in
flight together", not "will these two merge cleanly". A task can be
dispatched, finish, and be merged after a second task has already gone
out from the same base; nothing here notices. That is the
rebase-on-collision half, still unbuilt, and it is the piece that
actually matters once tasks outlive a single sitting.

One thing the implementation had to get right on the first try: **"in
flight" means status `dispatched` *and* a worktree still on disk.** A
crashed run leaves its live task saying `dispatched` forever, so a
check that trusted the status field alone would wedge every later
overlapping task behind a ghost — and the failure would look like the
overlap check working. It reports the stale dispatch and proceeds.

---

### F-17 · A check that runs before the claim is advice, not a rule

*2026-09-11, from the second review on #346.*

The overlap check (F-16) read the control plane, decided nothing
collided, and then went off to create a worktree and a live task. In
between those two steps there is nothing. Two runners started together
both see no in-flight neighbour, both pass, and both dispatch — and the
refusal that exists specifically for the concurrent case is the one
thing that does not survive it.

This is F-14 again in a different costume. The design was written for
concurrency; the implementation was written as though the runner were
the only process on the machine, which it was, right up until the
feature whose entire premise is that it is not.

The claim is now serialised. One `mkdir` lock is held from the last
read of the state to the moment this run is visible in it: inside it,
the task's own status is re-read and the overlap scan runs again, then
the worktree and the live task are created, then the lock drops. The
scan outside the lock still runs, because the plan has to print
something before the human decides — it is a preview, and only the
locked pass is the answer. The lock does not span the agent run;
serialising the bots themselves would defeat the point.

Two more instances of the same shape came out of the same review, both
in guards that looked finished:

- **The F-14 board lock had no release path.** A run killed between
  `mkdir` and `rmdir` leaves the lock behind forever, and every later
  run then waits and dies — one crash wedging the whole control plane,
  which is worse than the race it was added to fix. Now: an `EXIT` trap
  releases whatever is held, and a lock older than ten minutes is
  treated as a crash and broken.
- **The board's creation was atomic; its *appearance* was not.** `>`
  creates an empty file and then fills it, so a waiter that checks
  `-f "$BOARD"` can start appending rows into a half-written header.
  The header is now written to a temporary file and renamed.
- **The post-verify tree guard (F-13) compared head plus a file
  count.** A verifier that rewrites an already-modified file, or swaps
  one dirty path for another, leaves both numbers identical and slips
  through the check that exists to catch exactly that. It now compares
  a hash over head, `status --porcelain` and `git diff HEAD`. Untracked
  *content* is still outside it.

The question worth asking of every guard in this runner, and the one
that was not asked: **what can happen between reading this and acting
on it?** Where the answer is "another run", the check is a comment.

---

### F-18 · A criterion the runner cannot check is decoration

*2026-09-11, from the second review on #346.*

`fixer/BOT.md` listed four things that make a run successful. The
runner checked three. The fourth — no new `TODO`/`FIXME` without a
linked issue number, which is a hard rule in this repo's `CLAUDE.md` —
existed only as prose, so a bot that satisfied the verifier, stayed in
scope and made exactly one commit came back `done` with an unlinked
`TODO` in it.

This is F-1 seen from the other end. F-1 was a verifier *wider* than
the task, which blocked work the bot was not allowed to do. This is an
acceptance criterion *narrower* than the code, which passed work the
charter says is a failed run. Both are the same gap: the distance
between what the contract says "done" means and what the loop can
actually prove.

The check is now in the runner — added lines only, since pre-existing
debt in a file the bot had to touch is not the bot's to answer for —
and `run/stubs/sloppy.sh` is the regression: a stub that does the job
correctly, passes the verifier, stays in scope, and leaves one unlinked
`TODO` behind. All four of the fixer's criteria are now evaluated.

The rule that comes out of it: **every line in an acceptance section is
either executable by the runner or deleted.** A charter is not
documentation that happens to sit near the code; it is the
specification the verdict is derived from. A criterion nobody evaluates
is worse than a missing one, because it teaches the reader that the
ones beside it are checked.

---

### F-19 · A review cannot be verified the way a patch can

*2026-09-11, building the second bot.*

The reviewer was meant to be a second charter and nothing more. It
turned out to break the two rules the loop was built on, which is the
most useful thing it could have done.

**"No verifier, no dispatch" met a task with no executable predicate.**
A patch has one: run the tests. A judgement does not — there is no
command that exits 0 when an opinion is correct. The rule could have
been dropped for review tasks, which would have made it a rule about
convenience. Instead the *subject* changes and the rule stands:
`run/review-shape.sh` is an executable that must exit 0, and what it
checks is the review.

What it can prove, and does:

- the review exists, parses, and names a verdict in the vocabulary
- it is about the commit the runner actually checked out — a review of
  the wrong tree reads as current and describes something else
- every finding cites a path that **exists at that commit**
- every cited path is inside the task's `touches:`
- it says what it could not check

What it cannot prove is whether any of it is *right*. That ceiling is
the finding. The check that earns its keep is the citation one: a
reviewer's characteristic failure is not lying about its verdict, it is
naming a file that does not exist, and that is mechanically decidable.
`run/stubs/fabulist.sh` is the regression — a well-formed, confident,
correctly-signed review whose findings point at
`labs/agent-bots/run/dispatch.sh`, a file this repo has never had. It
lands on `blocked` with the invented path quoted in the handoff.

**Zero commits is the success shape.** A reviewer that commits has
failed, even when the change is an improvement — the whole value of a
second bot is that it has no stake in the diff. That inverted three
verdict rules at once, and the split it exposed is worth keeping:
everything about the *loop* (worktree, base pinning, evidence off the
tree, the runner writing the handoff, `.bot-blocked`) was already
kind-agnostic. Only the acceptance model was not, because it had been
written from one example. A `kind:` on the task, four verdict branches
and a second prompt were the entire cost.

The overlap check (F-16) falls out for free: a review claims nothing,
so it can never conflict at merge, so review tasks are exempt in both
directions. What it does get is a note — if another in-flight task is
editing the files under review, the review describes a tree that has
already moved, and the handoff says so rather than pretending the
verdict is about the branch.

---

### F-20 · The reviewer's first run found four real bugs in the code that dispatched it

*2026-09-11, the first real run of a second bot.*

T-0005 pointed the reviewer at the dispatch path of `bot-run.sh` and
asked one question: can two runs started together both dispatch tasks
that touch the same file? It answered no, correctly, and named what
closes the window — the worktree and the live task are both visible
before `drop_lock`, so there is no state in which one run has passed
its check but not yet claimed. Then it found four other windows that
were open. All four verified; all four fixed in the same commit as this
finding.

1. **Breaking a stale lock was itself a race.** Two runs could both
   judge the same directory stale, both remove it, and both walk in —
   the takeover added in F-17 to stop one crash wedging the control
   plane had reintroduced the race F-17 was about. It is now serialised
   by a second `mkdir`, and the owner token is re-read after winning
   that: a fresh holder cannot appear without the directory first going
   away, so any change of identity shows up in the token.
2. **`drop_lock` removed a lock without checking it still owned it.** A
   run whose lock was broken out from under it deleted the *next*
   holder's lock on the way out, leaving that one inside the critical
   section with the door open. Every release now checks the token.
3. **`waited` was not incremented on the stale branch**, so a lock that
   could not be removed spun at full speed forever. Both the counter
   and the sleep are now unconditional.
4. **"Is that dispatch still alive" was answered with a guess.** The
   live task recorded `branch:` but never where its worktree was, so
   the scan recomputed the path from *its own* `--worktree-root`. A run
   started with a different root finds nothing there, reads a live
   claim as a ghost, prints it as ignored and dispatches over it. The
   runner now records `worktree:` on the live task and the scan reads
   it.

A fifth finding is correct and untestable here:
`LOCKS_HELD=("${kept[@]}")` on an empty array is an unbound-variable
error under `set -u` on bash before 4.4, which is what macOS ships as
`/bin/bash`. The local bash is 5.2, where it is legal; the reviewer
said so itself rather than claiming a repro it did not have. The guard
went in anyway — F-15 already committed this script to running on
macOS, and every other array expansion in the file had the same shape.

One finding was open at the time and is now guarded rather than
solved. The control plane defaults to a directory beside the *script*,
so two checkouts of the same repository take different `dispatch.lock`
directories and scan different `tasks/` trees while creating branches
in one shared object store: **the lock is scoped to the state
directory and the conflict it prevents is scoped to the repository.**
The runner now refuses to use the default state from a linked worktree
and asks for an explicit `--state`, which closes the case it can
detect. Where the control plane should actually live is still §7.2's
question for the product port.

Two things worth recording about the run itself, beyond the bugs.

**The blind-spots section did the work it was added for.** The reviewer
wrote that its probe script for the lock interleaving was refused by
the sandbox, so three findings were read off the code and not
reproduced — "I would not call the race observed until someone has run
it". A review that had listed only findings would have read as five
verified defects. The section that is hardest to get a model to write
is the one that made the rest usable.

**It read the findings it was reviewing against.** F-16 and F-17 were
in the task's context as *claims to test*, and the review opens by
confirming F-17's window is genuinely closed before saying where the
mechanism still fails. That is the behaviour the `context/` folder was
copied from Grok Bot's community pattern to produce, and it is the
first time in this experiment that it has visibly paid.

Criterion 2 is still not met. Nothing here was an agent claiming
success while being wrong — the reviewer was right, carefully, and said
where it wasn't sure.

---

### F-21 · A handoff between bots is a task the runner writes, not a message a bot sends

*2026-09-11, closing the loop between two bots.*

Until now every handoff ended `to: human`, because there was nobody
else to address. A review that says `changes-requested` is the first
thing in this design with an obvious next recipient, and the obvious
implementation is to let the reviewer emit the task.

That fails for exactly the reason the agent does not write its own
handoff. A task is a *scope*: it says which files may be edited and
what will judge the result. Letting the bot that decided what is wrong
also decide what may be touched to fix it is self-reported scope, and
the loop's one rule is that self-reported anything is not evidence.

So the runner writes it, and every field comes from somewhere that is
not the review's prose:

| field | source |
|---|---|
| `owner`, `verify` | the review **task**, which is contract |
| `touches` | the paths the findings cite, re-checked against the tree |
| `base` | the commit the worktree was actually at |
| objective | a **path** to the review — never a copy of it |

That last row is the Grok Bot lesson taken literally: *the message
carries a path*. Inline the findings into the derived task and there
are two copies free to disagree, and the bot reads the copy.

Three consequences fell out, none of them anticipated:

**"No verifier, no dispatch" applies one level up.** A review task that
declares `on_changes_requested:` must also declare `derived_verify:`,
and dispatch refuses without it — checked before the review runs, not
after, because the moment to discover that a handoff cannot be
delivered is before producing the thing to deliver. The verifier cannot
come from the review for the same reason the scope cannot.

**The citations are re-checked even though the verifier already checked
them.** `verify:` is data; a task is free to name a different one. A
path that will scope another bot is not something to take on trust from
the file it came out of.

**A stable derived id is a fork, not a queue.** Re-reviewing the same
task would write over a follow-up that is still open, so the runner
refuses to derive while one exists. Same reasoning as the overlap
check, one level up.

The dispatch of the derived task is a separate decision, and `--chain`
is off by default. A task written by a machine and started by a machine
with nothing in between is a different risk class from one a human read
first; in the product that gap is where the approval inbox goes. The
flag exists so the chain can be demonstrated without pretending the
gate does not matter.

**One bug found by the chain itself.** Running it with a stub, the
child inherited `BOT_AGENT_CMD` and the *fixer* ran the reviewer stub —
which wrote `.bot-review.md`. The runner harvested it unconditionally,
so the file left the worktree, never counted as uncommitted, and a
change run that produced a review instead of a commit came back `done`.
Harvesting is now gated on `kind: review`; on a change task the file
stays where it is and the run is `needs-review`, which is what a bot
acting outside its charter should look like.

---

### F-22 · An empty commit is an answer, and no verifier reads answers

*2026-09-11, the first real bot-to-bot chain.*

The reviewer handed the fixer a finding. The fixer read the review by
the path it was given, read `core/src/telemetry.rs` end to end, decided
the finding asserted nothing about the code — no defect, no incorrect
behaviour, nothing at the cited line to be wrong about — and answered
with an **empty commit** whose message is the argument, because the
derived task names the commit message as the channel for a finding the
fixer disagrees with.

That is the right answer, arrived at the right way. The runner reported
`done`.

It was not wrong by accident so much as by construction. The verdict
rules are "one commit, verifier green, diff inside `touches:`", and an
empty commit satisfies all three without the runner ever learning
whether anything was decided. Every other `done` in this design means
*a verifier proved something about a diff*. Here there is no diff, and
what needs judging is a piece of reasoning.

So a commit with an empty diff is now `needs-review`, with the commit
subjects quoted in the blocker. Not because the answer is suspect —
`run/stubs/refusenik.sh` is the regression and it is modelled on the
real run — but because the thing that has to be evaluated is an
argument, and the runner cannot read arguments. The same rule as F-18:
an outcome the runner cannot check does not get reported as proven.

Two things worth keeping from the run itself.

**The handoff worked as designed and the evidence says so.** The fixer
was given a path, not a copy; it opened the review, took its claim
seriously enough to check it against the source, and refused it with a
reason a human can audit. It also noticed, and said, that the review's
own text admits to being a stub — and then explicitly declined to rest
its rejection on that, resting it on having read the file instead. That
is the distinction between corroboration and evidence, unprompted.

**Disagreement has to be a first-class outcome or the chain is a
rubber stamp.** If the only shapes a derived task can end in are "fixed"
and "blocked", every finding becomes a change, and a reviewer's
mistakes get written into the code by a second bot that had no way to
push back. The derived task says so in as many words: *a finding you
disagree with is not a finding you skip; say so, with the reason.
Silence reads as agreement, and the next reader cannot tell the
difference between fixed and missed.*
