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

**Concurrency control without locks** *(designed, not built)*. Each
task declares `touches:` globs; the runner should refuse to dispatch two
tasks with overlapping globs from the same base, and on a collision
rebase, re-verify, and route a failed rebase to a human. None of that
exists yet — today nothing reads another task's `touches:`.

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
| Covered | the file contract; one bot; one task; contract-exists-at-base check; worktree create; agent run; the `.bot-blocked` refusal channel; verifier; evidence capture incl. scope check; handoff write; append-only board; no-op cleanup |
| **Not** covered | scheduling/routines, multi-bot handoff, the approval inbox, per-bot memory, cross-task `touches:` overlap checks, rebase-on-collision, resume after a crash, pushing, opening PRs, any GPUI surface |

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

A task that already finished will not re-run; pass `--reset` to discard
the live task and start over. Exit codes are `0` done, `1` blocked,
`2` needs-review.

**Which CLI runs is data.** The task's `agent:` wins, otherwise the
charter's; the profile lives in `contract/agents/<id>.md`. There is no
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
   optional.** Two bots working one repo *will* collide. Today nothing
   reads another task's `touches:` (§3.4), which is survivable with one
   bot and not with four.

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
`contract/agents/<id>.md` now carries the command, a headless template
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

**One bug found in the making.** The old `BOT_AGENT_ARGS="${BOT_AGENT_ARGS:---permission-mode auto}"`
default survived the rewrite. Since `BOT_AGENT_ARGS` being *set* is the
signal that a stub is being injected, every run silently took the
override path and dropped the profile's `-p {prompt}` — so the agent
would have been invoked with no prompt at all. Caught by reading the
resolved argv in the plan output, which is precisely why the plan
prints it.

