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
| **Work** | a repository per task + branches | one bot per branch | the actual code |

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
| per-bot isolation | a clone + branch | `git::add_worktree(repo, path, branch, base)` |
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

The derived task lands in `.state/proposed/`, **and nothing will run
it until somebody approves it.** `run/bot-approve.sh` with no arguments
is the inbox - what is waiting, whose it is, and the one command that
moves it; with `--id` it writes `approved_by:`, `approved_at:` and
`approved_body:` onto the proposal. The runner refuses a proposal
without those, and `bot-tick.sh` shows it as `inbox` rather than
queueing it.

Two details carry the weight. `approved_body` is a hash of the task
with the approval lines removed, so **the approval is of the bytes and
not of the name**: edit the proposal afterwards and the runner says so
and sends you back to read it again. And `bot-approve.sh` refuses to run
inside a bot run, which is the part that catches the loop reaching for
its own gate through a verifier or a hook. The agent's real containment
is different and stronger - the control plane is not in its worktree, so
an approval in the control plane is not the agent's.

`--chain` still dispatches in the same breath, and now has to open the
gate to do it: it writes the approval itself, with
`approved_by: --chain (nobody read this)`, and puts `approval-bypassed`
on the board. The gate's value is not that a machine cannot open it. It
is that a machine opening it leaves a mark.

None of this applies to a task committed to the repository. That one
went through a person, a review and a merge, which is the thing an
approval is trying to establish and a stronger claim than a line in a
file.

**Per-bot memory goes through the same gate, and that is the whole
design.** A run may write one fact to `.bot-memory` — something that
would have made it easier had the bot known it at the start. The runner
stores it under `.state/bots/<name>/memory/` **unapproved**, and
`memory_block` only ever assembles notes carrying a valid approval, so
a bot asking to remember something and a bot deciding to are different
events with a person in between.

The reason to be this careful about a feature that sounds like a cache:
**a note is the only thing in this loop where an agent's own prose comes
back to it as input.** That is rules 1 and 2 of §3.6 at once, and it is
an injection channel the loop would otherwise have built for itself, for
free. So `run/memory.sh` is mostly limits — one fact, 400 bytes, twenty
notes per bot, and a refusal for any note containing a `---` fence or a
heading, because a note that closes the section it is quoted inside and
opens another one rewrites the prompt around it. That check is about
shape, not opinion: a note that says something *wrong* is a note nobody
should approve, and no amount of reading catches a structural breakout
reliably.

The cap is refused rather than rotated. `docs/HANDOFF.md` in this
repository grew to 3600 lines because nothing ever said no, and it was
deleted rather than read — a bot with two hundred notes has a diary, not
a memory. `bot-approve.sh --forget <bot>/<note>` is how one goes.

And the notes arrive in the prompt labelled: *claims, not contract*,
with the charter named as the winner and the run that produced each one
named next to it, so the bot can go and check instead of taking its own
word for it. F-23, applied to a bot's own past.

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
ignored.

The other half is the base moving. A branch is cut from one commit and
merged against another, so once the work is done the base ref is read
again; if it moved, the commits are replayed onto the new tip and
verified there. Only a clean rebase that is still green keeps its
`done` — a conflict, a verifier that now fails, or a branch the new
base has emptied all go to a human with the branch put back exactly
where it was. `--no-rebase` turns it off. See F-25, and F-16 for what
the dispatch-time check cannot see.

That covers a base that moves *during* the run. The longer window is
after it: a branch that finished, was pushed and cleaned up, and then
waits for a human while the base moves on. The tick reads that too -
a `done` task whose branch is in the project, not yet in its base ref,
and whose base ref no longer points at the recorded `base_sha` is
`stale-base` - and dispatches `--recheck`: the same run with the
agent's turn taken out, started from the pushed tip, ending in the
same rebase step and the same four verdicts. The record is kept; the
branch and its `base_sha` move only on a clean, green replay. See F-52.

Crash recovery is *cheap*, not free. Nothing lives in memory, so the
state is all on disk — but there is no resume path: a run that dies
mid-flight leaves the live task at `dispatched`, and the runner refuses
to start again until the worktree and branch are gone and you pass
`--reset`. Re-deriving the phase from git is the design; the code only
detects that it happened.

### 3.5 Where it gets messy

- **Surface sprawl.** One checkout per task is gigabytes fast, and a
  `--shared` clone is no cheaper than a worktree was — the objects are
  shared, the working tree is not. Cap concurrency low (4, not 50) and
  remove a surface as soon as its result is pushed back. The
  `session.rs` `RetentionPolicy` (TTL + cap per worktree) already has
  the right shape.
- **Windows path length and file locks.** Deep surface paths plus
  rust-analyzer and `target/` holding handles make removal fail.
  `sidebar.rs` already has the force-fallback; the bot layer must
  inherit it rather than reinvent it. Removal is `rm -rf` now, which
  refuses nothing — hence the marker file the removal helper checks
  for. See F-28.
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
  whoever can land a commit in `tasks/` or `examples/` controls a
  shell command with your privileges. That is acceptable while the task files are
  your own; it becomes a hard gate the moment a task could arrive from
  a pull request. See F-6.
- **Secrets.** Gate before the push, not after. A key in a pushed
  commit has already been seen.

---

### 3.6 Threat model

The list above is where this section used to live, and it is why twelve
rounds of review kept finding the same thing in twelve shapes: it stops
at *an agent might be sloppy or wrong*. The sentence it was missing:

> **The agent's output is an input to the runner's tools.**

Everything the agent produces is read back by a program that runs
outside the sandbox, with the runner's privileges, in reach of the
control plane and of the user's own project. The diff is the part
everyone remembers to check. It is not the only thing being read.

#### Who this defends against

Not a determined attacker who already has a shell on this machine —
that game is over before it starts. The adversary here is an agent that
is confused, over-helpful, trained on a habit that does not apply, or
steered by text it read while working. None of those is distinguishable
from malice at the boundary, so the mechanisms are written as if it were
malice: *it probably meant well* is not a property a runner can measure.

#### What the agent controls, and what reads it back

| Surface | What the agent can do with it | What holds it |
|---|---|---|
| The worktree | write anything anywhere, in scope or not | scope check against `touches:`, protected paths, `PROTECTED_IN_DIFF` — F-17, F-34 |
| `.git` | name programs the runner's own git then runs; answer *which repository is this*; move the working tree somewhere else | the disarm: hooks emptied, command-naming config enumerated and unset, `.git`'s identity compared against the one recorded at creation — F-36, F-37 |
| The channels | make `.bot-blocked`, `.bot-commit-msg` or the report a symlink or a hard link to a host file, and have it harvested into the control plane | the channel guard: links are removed unread, not followed — F-36 |
| Its own prose | assert findings nobody checked, describe work it did not do | the claim verifier, evidence read off the tree rather than off the summary — F-23, F-33 |
| Its exit code | claim success | the verifier, in a clean checkout the agent never touched — F-13 |
| Its commit | put a secret in the tree itself, past every pathspec | the diff is checked after the fact, not only the `add` — F-34 |

#### The five rules the mechanisms come from

1. **Nothing the agent wrote decides what the runner executes, or
   where.** Hooks, `core.hooksPath`, `diff.external`, `filter.*.clean`,
   `url.*.insteadOf`, `core.worktree`, the identity of `.git` itself.
2. **Nothing the agent wrote is believed about the tree.** The tree is
   read. The summary is a claim that explains a verdict; it never is
   one.
3. **What leaves the surface is content the runner copied, never a path
   the runner followed.** A symlink is a name for somewhere else, and
   nowhere else is this run's to carry out.
4. **Every claim carries something checkable back to the blob.** A
   citation proves a file was opened; the quote proves the line was
   read.
5. **A measurement that could not be taken is not an empty
   measurement.** A diff that failed is not a diff with nothing in it, a
   push that exited 0 is not a push that landed, and a restore that ran
   is not a restore that worked. This is the one that hides, because
   `|| true` spells *unknown* and *nothing* identically.

#### Knowingly open

- **The agent's process is not confined.** It runs with the runner's
  privileges. The sandbox is a worktree convention, not a kernel
  boundary — everything above is about what the *runner* will act on,
  not about what the agent can reach.
- **The project's object store is readable from the surface.** That is
  what `--shared` means; `objects/info/alternates` still names the path
  after the remote is removed.
- **`verify:` is host execution sourced from repo content** (F-6), and
  so is the charter's `agent:` argv.
- **Content filters** need a `filter.<n>.clean` *and* a `.gitattributes`
  naming it. The config half is disarmed; the tracked half arrives
  through the diff, where the scope check sees it — #349.
- **The agent reads more than the charter** and the runner cannot
  enumerate it (F-7).
- **The agent's turn is assumed to end when its process does.** Every
  channel read, every evidence read and the disarm itself are
  `[ -f ]` then read, which is two opens of a path the agent owned a
  moment ago. Nothing stops an agent backgrounding something that keeps
  writing. `tree_state` catches a worktree that moves across the
  verifier, and the channels are removed after being read, so the
  exposure is narrow rather than closed - and closing it means
  snapshotting the surface before measuring it, which is a different
  design.

#### When a finding is finished

When you can say which of the five rules it serves, and what reads the
thing it produced. A new finding is *the same one again* when it is one
of those five at a site nobody had looked at yet — which is what the
last three rounds were, and why they felt endless. The rules end the
list; the sites do not.

---

## 4. What the prototype covers

| | |
|---|---|
| Covered | plain-folder projects as well as git ones; the file contract; **two bots** (`fixer`, `reviewer`), two deliverables (`produces: commit`, `produces: report`), the handoff between them and a scheduler that reads the board; contract-read-at-base (existence *and* argv); a serialised dispatch claim with a cross-task `touches:` overlap refusal; worktree create; agent run; the `.bot-blocked` refusal channel; the report channel (`artifact:`, `shape:`, harvested into `.state/artifacts/`) with citations *and* quotes checked against the blob; verifier, in a clean checkout of the branch tip; the surface disarm (hooks, command-naming config, `core.worktree`, and the identity of `.git` itself); the approval gate on derived tasks *and* on per-bot memory, hashed over the body, with `--chain` as a recorded bypass; per-bot memory itself - one fact per run, capped, refused if it would restructure the prompt it is quoted in; evidence capture incl. scope and TODO checks; handoff write; append-only board; no-op cleanup; rebase onto a base that moved, re-verified there; retiring a finished record (`bot-forget.sh`), which appends to the board rather than editing it and leaves the handoff alone; re-verifying a branch that is *waiting* to be merged when its base moves (`--recheck`, dispatched by the tick) |
| **Not** covered | resume after a crash, any trigger other than "someone ran a tick", pushing, opening PRs, any GPUI surface |

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
checks each verdict, starts two colliding tasks at the same moment to
prove exactly one can claim, and reports any worktree or lock left
behind. It is the one command that says whether the loop still holds:

```bash
bash labs/agent-bots/run/sweep.sh
```

### Letting it decide for itself

`run/bot-tick.sh` reads the world, prints a decision for every task it
can see, and dispatches the ones that are ready:

```bash
# what would happen, and why - changes nothing
bash labs/agent-bots/run/bot-tick.sh --dry-run

# actually run up to two, two at a time
bash labs/agent-bots/run/bot-tick.sh --max 2 --parallel 2

# keep going
bash labs/agent-bots/run/bot-tick.sh --watch 300
```

The table below is a tick over the *fixtures*, which is the only place
this many decisions exist side by side. The no-argument form above
reads `tasks/` and prints what is really queued:

```bash
bash labs/agent-bots/run/bot-tick.sh --dry-run \
  --tasks labs/agent-bots/examples
```

```
TASK           OWNER     DECISION     RUNS  LAST         WHY
T-0004         fixer     manual          0  never        no schedule: auto
T-0005         reviewer  cooling         5  blocked      blocked 2 time(s), last one under 30m ago
T-0006         reviewer  due             6  done         every: 1d elapsed
T-0006-fix     fixer     ready           0  never        never run
```

`RUNS` and `LAST` come off the board — they are the two questions only
an event log can answer, since a dispatch that was *refused* never
wrote a handoff. `DECISION` does not: what is true right now comes from
the live task and from whether its worktree is on disk, because a run
that died mid-flight appended no closing row. F-23 has the reasoning.

**Scheduling is opt-in.** A task runs unattended only with
`schedule: auto`, or `every: <duration>` which implies it. Everything
else reports `manual` and is left alone — the first tick ever run
offered to dispatch the overlap *fixture*, which is what a directory of
task files looks like to something that cannot tell a fixture from a
job.

**Real work lives in `tasks/`, fixtures in `examples/`**, and the
default set a tick reads is `tasks/` plus `.state/proposed/`. Opt-in
scheduling was the first answer to the fixture problem and it is not
the whole one: `examples/T-0006` is a routine on `every: 1d`, written
to demonstrate recurrence, so a default that included `examples/`
would re-run a demo review daily on nobody's request. Pass
`--tasks labs/agent-bots/examples` to exercise the shapes on purpose.
F-44.

A task that already finished will not re-run; pass `--reset` to discard
the live task and start over. Exit codes are `0` done, `1` blocked,
`2` needs-review.

A task whose files another in-flight task already claims is refused
before it costs a worktree; the plan prints the overlapping paths, and
`--allow-overlap` overrides it. `examples/T-0004-overlap-fixture.md`
reproduces both that refusal and the stale-dispatch case by hand.

**Two things a task can produce.** `produces: commit` is the default
and is what T-0001 and T-0002 are: the work lands in the tree and gets
the branch, the verifier and the rebase. `produces: report` lands
beside it — the bot commits nothing, writes one file named by
`artifact:` in the shape of the template named by `shape:`, and the
runner harvests that into `.state/artifacts/` and points the handoff at
it. A commit from a report task is a failed run.

The reviewer is the first role to use it, not the definition of it: its
defaults are `.bot-review.md` and `templates/REVIEW.md`, and a second
reading role brings its own pair. The owner's charter declares the same
field, and a task that disagrees with it is refused at dispatch — a bot
is what its charter says it is, and a task does not get to reassign it.

```bash
# a review run - the reviewer reads, judges, and commits nothing
labs/agent-bots/run/bot-run.sh   --task labs/agent-bots/examples/T-0005-review-overlap-check.md
```

Its verifier is `run/review-shape.sh`, which checks the review against
the tree it claims to be about: every cited `path:line` is a real file
and a real line at that commit and falls inside `touches:`, **and every
finding quotes that line, with the quote matched against the blob**.
Substring after whitespace is normalised, at least eight characters of
content, and a multi-line quote has to match consecutive lines from the
one cited.

That last part is the difference between a reviewer that opened the
file and one that read the file listing. A citation resolving proves
somebody knew a path; only the quote proves they were looking at the
line. What none of it can check is whether the review is *right* — see
F-19 for where that ceiling sits, and F-33 for why it moved as far as
it did.

A review task that also declares `on_changes_requested:` and
`derived_verify:` hands its verdict on. `changes-requested` then makes
the runner write a task for that bot into `.state/proposed/` and
address the handoff to it by name:

```bash
# review, then hand the findings to the fixer as a scoped task
labs/agent-bots/run/bot-run.sh   --task labs/agent-bots/examples/T-0006-review-telemetry.md

# the inbox: proposed tasks and notes nobody has agreed to yet
labs/agent-bots/run/bot-approve.sh
labs/agent-bots/run/bot-approve.sh --id T-0006-fix
labs/agent-bots/run/bot-approve.sh --memory fixer/<note-file>
labs/agent-bots/run/bot-approve.sh --forget fixer/<note-file>

# ... or skip the reader and say so on the board
labs/agent-bots/run/bot-run.sh   --task labs/agent-bots/examples/T-0006-review-telemetry.md --chain
```

**Retiring a record.** A live task under `.state/tasks/` outlives its
run on purpose — it is how a later dispatch knows the task finished,
and what a human reads when something went wrong. `bot-forget.sh`
removes one, and it is the supported way because the only other one
was `rm` under the directory whose whole job is being the record:

```bash
labs/agent-bots/run/bot-forget.sh T-0010
labs/agent-bots/run/bot-forget.sh T-0010 --surface   # and the clone
```

Two separate gates, and conflating them was a bug. A **run marker**
under `.state/running/` says whether a process is in there, carrying
its pid so a killed run is distinguishable from a live one; the
**status** says whether the run reached a verdict. Neither answers the
other's question: the runner writes `done` *before* its cleanup and
rewrites it if that cleanup fails, so a finished-looking status is
normal while there is still work to do. `--force` is the way past
either gate. And both gates, plus every removal after them, run while
holding the runner's own `dispatch.lock`, so a claim cannot start
between the checks and the act.

It removes nothing *from* the handoff, the artifacts or a bot's
memory, and it appends a `forgotten` row to the board rather than
editing it — retiring a record is itself an event, and the board is
append-only either way. `--surface` removes all three parts of a
surface, the way the runner's own `drop_surface` does: the verify
checkout beside it, the clone, and then the `refs/bot-base/<id>` pin
in the project. F-46 and F-47.

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

Codex is the one agent that sandboxes its own shell calls, and the
first run under it proved that this buys less than it sounds. A linked
worktree's git directory is outside the worktree, so worktrees isolate
the *checkout* and never the repository (F-26) — the work surface is a
standalone clone now, for that reason and because a plain folder has no
worktree to add (F-28).

That did rescue Codex, once the profile stopped hoping and started
saying so. Codex denies writes to `.git` wherever `.git` is, and a
standalone clone puts it inside the one directory the agent is granted
— so `codex.agent.md` passes it explicitly as
`--add-dir {git_dir}`, which the runner substitutes with the surface's
own `.git`. Codex commits and completes the loop under that (F-30).

The runner's own commit stayed, and it is a fallback rather than the
plan: an agent that leaves its work uncommitted still gets measured,
with the handoff saying who made the commit and, when the agent wrote
no message, that the subject is the runner's. Work that cannot be
measured cannot be judged — and which of the two happened is not
something to guess from an empty diff.

**`base:` must be a ref that contains the contract.** The agent reads
its charter and context from its own checkout — the base commit — not
from your working tree. While this branch is unmerged that means
`base: labs/agent-bots`, not `origin/main`; the runner refuses up front
otherwise. It also means contract edits only reach the agent once they
are committed.

**Unless the project is a plain folder**, in which case `base:` is the
literal word `folder`. There is no ref to cut from, so the runner
snapshots the folder into a bare repository under state, grafts the
contract in at `.bot-contract/`, and the work is cut from that. See
F-29 — including what the snapshot deliberately leaves behind.

### Where things land

| Path | What |
|---|---|
| `.state/board.md` | append-only event log |
| `.state/tasks/<id>.md` | the live task — the repo copy is only a definition |
| `.state/handoffs/<ts>_<bot>__to__human__<id>.md` | the handoff |
| `.state/artifacts/<ts>_<bot>_<id>_<name>` | a report, harvested off the worktree |
| `.state/proposed/<id>.md` | a task one bot's run wrote for another — needs an approval before it runs |
| `.state/bots/<bot>/memory/<ts>_<id>.md` | one fact that bot asked to remember — read back into its prompts only once approved |
| `.state/tmp/dispatched-<id>.md` | the bytes a proposal was dispatched with, snapshotted at the gate |
| `.state/REPO` | which repository this control plane belongs to |
| `.state/runs/<day>/<id>-<ts>.log` | agent + verifier output |
| `<repo>.worktrees/bot-<owner>-<id>/` | the work surface — a clone, with its own `.git` |
| `.state/snapshot.git` | plain-folder projects only: the folder, committed, and the base the work is cut from |
| `.state/patches/<ts>_<id>.patch` | plain-folder projects only: the result, as something a folder can apply |

`.state/` is git-ignored, and a clean no-op run removes its own
worktree and branch.

### Why this is bash

It is the first question anyone asks after reading nearly 7,000 lines
of `set -euo pipefail`, so it belongs here rather than in someone's head.

Three reasons it was the right call. **The work is orchestration** —
start a process, move a file, call `git`, read an exit code — which is
the one domain a shell is actually for. **CodeScope shells out to git
by policy** (no libgit2; see the root `CLAUDE.md`), so every git
sequence in `bot-run.sh` transfers to Rust as-is: the runner is a
transcript of the calls the product will make, not a parallel design.
And **a lab with no build step can be thrown away**, which is the
status this has: nothing ships, nothing depends on it.

What it cost is smaller than it looks. Read the findings below and
almost every one of them is about git semantics and trust — a citation
is not a read line, a check before the claim is advice, the verifier
was measuring the wrong tree, the agent writes `.git`. Those would have
arrived exactly as late in Rust or Python, because they are questions
about what the runner *believes*, and no type system has an opinion
about that.

The genuinely language-specific defects never earned a finding of their
own. They are bullet points inside findings whose lesson is about
something else: `${a[@]}` under `set -u` on the bash macOS ships,
`find -maxdepth` as a GNU extension, backticks inside a double-quoted
string, unguarded pipelines under `pipefail`, MSYS rewriting
`branch:.env` into a Windows path list. Cheap, every one caught by the
sweep or a review, and not one of them changed the design.

**The exception is the one worth carrying forward.** `|| true` spells
*unknown* and *nothing* identically, which is rule 5 of §3.6 and took
three rounds to see. In a language with `Result` that rule is a
compile error instead of a review round — and that is an argument for
the graduation in §6, not for a rewrite now. Sixteen review rounds and
89 green checks are the asset here; porting resets the regression suite
and re-finds the same design findings in a new dialect. The language
changes when this moves into `codescope-core`, and the thing that moves
is the contract, not the script.

## 6. What would have to be true to graduate this

1. The loop runs unattended and green three times on a real issue in
   this repo. *(**One of three, and the first on a real issue.**
   T-0010 — a scheduler tick, no babysitting — produced the fix for
   issue #343, verifier green in a clean checkout, one commit inside
   `touches:`, and that commit is now on `main` as part of #354. Two
   to go, and F-45 is what the first one actually proved. Earlier runs
   under Claude Code and Codex completed on fixtures: F-26, F-27 and
   F-30 are what those took.)*
2. The verifier catches at least one agent run that *claimed* success
   and was wrong. If that never happens, the verifier is not verifying.
3. A handoff between two bots survives a rebase. *(**Done.** A branch
   is rebased onto a base that moved during the run and re-verified
   there — F-25 — and a branch that is merely waiting is now re-read
   by the tick and replayed the same way when its base moves — F-52.
   Both halves are in the sweep: four in-flight cases, and three
   waiting ones — clean, conflict, and nothing moved.)*
4. Worktree cleanup works on Windows with a build running.
5. `verify:` no longer runs through `eval` on the host, or task files
   are provably trusted input. A product feature cannot ship a shell
   command sourced from repo content. See F-6.

Until then it stays in `labs/`.

The runner itself does not graduate. What crosses over is the contract
- the `TASK.md` fields, the three planes, the verdict chain, the disarm
rules and the five rules in §3.6 - as Rust in `codescope-core`,
reusing the worktree, process and retention machinery that is already
there. The bash is a transcript of the calls that code will make. See
"Why this is bash" in §5 for why it was written that way and what it
cost.

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
idle toast have to work with no visible tab.

**Checked, and they do not — for a reason that improves the design.**
The tail itself is tab-independent: `telemetry_tails` is a map keyed by
agent session id, polled by one task in `start_telemetry_poll`. What is
tab-bound is *discovery*. `register_telemetry` is called from exactly
one place, the adoption scan, and that scan walks `groups[].tabs[]` and
matches a transcript against an open tab's working directory. A session
with no tab is never scanned, so it is never adopted, so it is never
registered. No tail, no `Idle`, no toast.

The fix is not to make adoption work without a tab. It is to notice
that **the inbox should never have been waiting for a session to go
idle.** In this design the runner decides a run is over — that is the
whole point of it writing the handoff — and it says so by creating a
file. So the event the UI wants is *a handoff appearing in
`.state/handoffs/`*, which CodeScope can watch exactly the way it
already watches the transcript directories. Telemetry only matters for
a bot you are actually watching, and by then you have opened a tab and
adoption works normally.

Two things follow.

Stage 1's "a bot run is just a session CodeScope opened for you" holds
only for a bot you opened a tab for. An unattended run is not a session
in CodeScope's sense at all: it is a process the runner owns, and the
product's entire stake in it is the state directory.

Which makes the integration surface smaller than this section assumed.
Not "teach telemetry to work headless" but **one watcher on one
directory, plus a renderer for documents that already have a fixed
shape.** The handoff, the review and the board are all already parsed
by `bot-tick.sh` with twenty lines of `awk`; `core/src/overview.rs` is
the precedent for where the pure-data half belongs.

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
   optional.** Two bots working one repo *will* collide. Both halves
   are built now: the dispatch refusal (§3.4), and the rebase onto a
   base that moved during the run, re-verified there, with anything
   short of clean-and-green routed to a human (F-25). With one bot that
   was a rare annoyance; with four it is the normal case — and the
   collision worth having built it for is the one with no file in
   common, which no merge would ever have shown.

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

And the board is read now. `bot-tick.sh` (F-23) decides what should run
from three sources it is careful to keep separate: the live task for
what is true, the board for what has happened, file mtimes for how long
ago. That is what makes "this has ended `blocked` three times in a row,
leave it to a human" expressible at all.

What is still not scheduling: there is no daemon, no trigger on a git
event, nothing that survives the process. A tick is a thing you run and
`--watch` is a `sleep` in a loop. The only reason that is enough is
that the whole state is on disk and every tick re-reads it from
nothing — which is the same property that made crash recovery cheap,
arriving twice for the price of one design decision.

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
rebase-on-collision half — built since, as F-25 — and it is the piece
that actually matters once tasks outlive a single sitting.

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

---

### F-23 · A log tells you what happened; it must not be asked what is true

*2026-09-11, the first thing that reads the board.*

The board had been write-only for the whole experiment. `bot-tick.sh`
is the first reader, and the first question it had to answer was not
"what should run" but **which facts may be taken from a log at all**.

Three kinds of fact, three different sources, and mixing them up is the
whole trap:

| question | source | why not the others |
|---|---|---|
| what is true now | the live task, plus whether its worktree is on disk | a run that died mid-flight appended no closing row, so the board's last word about it is a lie by omission — and status already has a single writer (§3.2); deriving it from the log would create a second answer |
| what has happened | the board, and only the board | a dispatch that was *refused* never wrote a handoff, so the handoffs directory cannot count attempts even in principle |
| how long ago | file mtimes | the board's timestamps are ISO strings, and turning those into epoch seconds portably is a worse problem than this one deserves; `find -mmin` is on both GNU and BSD |

That split is what makes loop detection possible at all. "This task has
ended `blocked` three times in a row" is a question only the event log
can answer, and it is exactly the question that stops a scheduler
burning an afternoon re-running something that needs a human.

Four things the first tick got wrong, all of them instructive.

**A directory of task files is not a queue.** The first run offered to
dispatch `T-0004` — the *overlap fixture*, a file whose entire purpose
is to sit there looking like a task. A scheduler that runs everything
it can see will run the first fixture anybody drops in `examples/`, and
this repo has three. So `schedule:` is **opt-in**: `auto` or nothing
happens. Everything else is reported as `manual` and left alone.

**Recurrence is decided by the clock, not by the status.** The first
version asked the status first, so clearing a live task made a routine
that had run minutes ago look like one that had never run — "no live
task" reads as `todo`. For a task with `every:`, the interval *is* the
question; status only says whether it is running right now.

**Refusal is not failure, and needed its own exit code.** An overlapping
claim means nothing ran and nothing was decided. Counting that as a
failed run would have the backoff and the give-up counter park a task
that was never attempted. Exit 3 now means "never started", distinct
from blocked and from needs-review.

**Permission travels with the work, one hop.** A derived task inherits
`schedule:` from the review that produced it: a task a human allowed to
run unattended may produce a follow-up that runs unattended; a task a
human starts by hand produces one that waits for a hand. The
alternative — letting whichever bot wrote the file decide — is the same
mistake as letting it choose its own scope (F-21).

And then the part that was worth all of it. `sweep.sh` now generates
two tasks that declare the same file and starts them **at the same
moment**, against one control plane:

```
dispatch  T-990A.md
dispatch  T-990B.md
  T-990A  ->  refused - nothing ran, try again
  T-990B  ->  done
```

Exactly one claimed. The board carries both runs' rows, interleaved and
intact. That is F-14 (the board survives concurrent writers), F-16 (the
overlap is real and named) and F-17 (the claim is serialised) all being
load-bearing at once — and until this run, every one of them was a
guard against something that had never actually happened.

What is still not scheduling, and should not be mistaken for it: there
is no daemon, no trigger on a git event, no queue that survives the
process. A tick is a thing you run, `--watch` is a `sleep` in a loop,
and the only reason that is enough is that the entire state is on disk
and the next tick re-reads it from nothing.

---

### F-24 · By the fifth review, nothing new in kind

*2026-09-11, the last round before this merged.*

Eleven more findings, all real, all fixed — and not one of them a new
*kind* of mistake. Every one is a shape already written down here,
appearing somewhere it had not been looked for yet. That is worth
recording, because it is the first sign this list has converged.

**F-4's shape — an escalation reported as success.** A review with
`verdict: blocked` makes no commit, so it fell through to the generic
zero-commit branch and came back `done`. The task went terminal, the
scheduler never returned to it, and the only record of the refusal sat
inside a file nobody had been told to open. F-4 was a crashed agent
reported as a clean no-op; this is a reviewer saying *I could not do
this* and being thanked for it. `run/stubs/stumped.sh` is the
regression.

**F-18's shape — a promise nothing checks.** `verified:` was printed in
the plan, described in the profile as the safety boundary, named in an
error message telling authors to set it before dispatching, and gated
nothing at all. A field that documents a boundary and enforces none is
the boundary not existing.

**F-17's shape — a guard that stops at the first happy path.** The
stale-lock takeover refused to break a lock with no `owner` file, which
is precisely the lock left by a run killed between `mkdir` and writing
its token: the one crash the ten-minute recovery exists for was the one
it could not recover. Similarly, cleanup failure at the end of a no-op
run printed a warning and still recorded `cleaned` — so a live task
claimed a clean tree while its worktree sat there waiting to block the
next run.

Two are worth naming on their own.

**The advice contradicted the check.** The linked-worktree guard tells
you to point every checkout at one `--state`; the ownership stamp then
compared `git rev-parse --show-toplevel`, which differs per checkout,
so the second one would reject the shared state as another repo's. The
documented safe path was the one path the code refused. It compares the
common git directory now — the thing that is actually per-repository.

**The regression suite was a hazard.** `sweep.sh` force-deletes seven
fixed branch names. A developer with real work on `bot/fixer/T-0001`
would have lost it to running the tests. It now enumerates exactly what
it may destroy, refuses to start if any of those already exist, and
will not clean up a branch outside that list. A test harness that can
eat your work is worse than no harness, and "it only deletes bot
branches" is an assumption about somebody else's naming.

What this round did not find: a new category. After twenty-three
findings the failure modes here are a short list — *self-reported
evidence, a check that runs before the thing it guards, a criterion
nobody evaluates, a log asked what is true* — and the remaining work is
recognising them in one more place, not discovering another one.

---

### F-25 · A clean merge and a working merge are not the same claim

*2026-09-11, building the other half of F-16.*

Every handoff until now ended on the same sentence: the verifier passed,
here is the branch. It was true, and it was a claim about a commit that
had already stopped being the tip. The branch was cut from base *X*, the
verifier ran against base *X*, and by the time a human read the handoff
the base was *Y* — usually because the very task this one was told to
wait for had landed. Nothing in the loop noticed. Nothing was *wrong*,
either: every individual step was honest. The word "verified" was just
attached to a world that no longer existed.

So the base is read a second time, after the work is done, and if it
moved the commits are replayed onto the new tip and verified *there*.
Only "clean rebase, still green" keeps the `done`.

The interesting part is what the four outcomes turn out to be, because
one of them is not a merge problem at all.

**Conflict.** The work no longer applies. Git names the paths, the
rebase is aborted, and the branch is left exactly where it was — still
the tree that passed. `needs-review`, because choosing between two
changes is a judgement, and the one thing a runner must never do is make
one up.

**Emptied.** The rebase succeeds and nothing is left: every patch was
already upstream. Somebody else did this work. Git's default is to drop
such a commit silently, which is right for a human at a keyboard and
wrong here — afterwards, "already fixed" and "silently lost" are the
same picture. It is reported rather than cleaned up.

**Red.** The rebase is clean — *no file is contested* — and the
verifier now fails. This is the one the overlap check cannot see by
construction. The overlap check asks whether two tasks name a file in
common; here they name none, and they still disagree: a rename on one
side and a caller on the other, a signature changed under a passing
test, an invariant one side relied on and the other removed. Two tasks
that pass every gate the loop has, merge without a conflict marker, and
produce a broken tree. Git has no opinion about this and never will,
because it is not a text problem. Only running the verifier on the
merged result finds it, and that is the whole reason this step exists.

**Clean and green.** The branch moves, and every number in the handoff
is re-read from the rebased tree — base, head, commit count, numstat,
touched paths — because they all described the old base and a reader has
no way to tell which of them moved.

Three decisions worth keeping.

**The branch only ever moves on success** — and the first version of
that sentence was not true. `git rebase` moves the checked-out branch as
its opening act, so replaying on the branch itself left `TASK_BRANCH`
pointing at an unverified tip for as long as the second verifier took,
and a run killed inside that window left it there for good. Resetting it
afterwards is a rollback, and a rollback is a promise about a code path
that runs after the damage. So the replay happens on a **detached
head**: the branch does not move until something has verified, and on
conflict, red or emptied there is nothing to undo because nothing was
done. `checkout -B` is the only thing that ever advances it.

That keeps an invariant a reader can rely on without qualification:
*the branch named in a handoff is a tree that verified*. What did not
work is in the handoff as prose, not as a tip somebody has to bisect
for. It also means the verifier's own result has to be saved and
restored around the second run, or the handoff would describe the branch
using a measurement of a tree that was thrown away — F-13's mistake,
re-committed one layer up.

**It is gated on the verdict, not on a copy of the verdict's
conditions.** The rebase runs only when the run would otherwise be
handed off as `done`. The first draft re-tested the eight conditions
that lead there, which is two lists free to disagree about what "done"
means — and the failure mode is a rebase burying a reason that was
already established.

**The window narrows; it does not close.** The base can move *again*
while the second verifier runs, and re-reading until it holds still
would never terminate in a busy repo. So the ref is read once more at
the end, and if it moved the handoff says so and keeps its `done` — the
claim being made is about a commit, which is printed, not about a tip.
A runner that quietly implied otherwise would be doing the thing this
finding exists to stop.

**No fetch.** The base ref is re-read locally, so `origin/main` moves
only because something else fetched it. A runner that reached the
network in order to measure would be changing the world it is
describing, and the same task would get different answers depending on
when it ran. What the runner promises is narrow and checkable: *this
verified against the base as this machine currently understands it*.

The sweep checks three things per case and not one, which is the part
worth copying elsewhere. The exit code and the board event are both the
run's own account of itself; the **branch ref** is not. Every check but
that one would stay green if the invariant above quietly stopped
holding, which is precisely how a rollback rots.

`run/stubs/mover.sh` is the regression — an agent that does its work and
moves the base out from under itself while it is at it, driven by four
environment variables so the same stub produces all four outcomes. It
builds the competing commit with plumbing (`read-tree`, `commit-tree`,
`update-ref`) rather than a second checkout, because the runner is
holding the only worktree it knows about and a test that needs its own
is a test that leaks one.

What is still missing: the *other* other half. This catches a base that
moved before the handoff was written. It does nothing about a base that
moves afterwards, while the branch sits waiting for a human — and that
window is longer than the run by a wide margin. Answering it means
re-verifying on a trigger rather than at the end of a run, which is the
scheduler's job and not this one's.

---

### F-26 · The sandbox that made Codex the safe choice is what stops it working

*2026-09-11, the first run under a second CLI.*

The Codex profile had been written, checked against `codex exec --help`,
and never run. §7.2 called Codex "the one agent here that bounds its own
blast radius rather than relying on the worktree being throwaway" — and
that sentence is exactly why its first real run produced nothing.

    sandbox denies writing
    .git/worktrees/bot-fixer-T-0007/index.lock;
    baseline verifier passed all 542 tests and no source files
    were changed

**A linked worktree's git directory is not inside the worktree.** Under
`-s workspace-write` the writable roots are the working directory,
`/tmp` and `$TMPDIR`. The checkout is at `$WT`; its git directory lives
under the *main* repository. So an agent that sandboxes itself by
directory can read every file it was given, edit every file it was
given, and not write a single commit. The runner reads its evidence off
commits — so a correct, careful, honest run produces, by the runner's
own rules, nothing at all.

It is worth being precise about how nearly this went unnoticed. The task
was the smoke test, which needs no change; the tree was clean either
way, and a runner that only counted commits would have called it a
green no-op. What made the difference is that Codex used
`.bot-blocked` — the refusal channel — and said *why*. F-4 built that
channel for an agent that crashes. Its first real user was an agent that
worked perfectly and could not finish.

**The fix is wider than it looks.** `--add-dir <git dir>` lets the
commit through, and what it grants is the whole object store and every
ref in the repository. There is no narrower grant: for a linked
worktree, objects and `refs/heads/*` are in the common directory, so
"let this agent commit on its own branch" and "let this agent rewrite
`main`" are the same permission. That is not a Codex flaw. It is what a
linked worktree *is*, and it means the isolation this prototype claims
from worktrees is isolation of the *checkout*, never of the repository.
The narrower answer is a per-task `git clone --shared`, where the git
directory is inside the sandbox and only a push at the end crosses the
boundary — a design change, not a flag, and the first real argument
against worktrees this project has produced.

**And on Windows it is still not enough.** With the git directory
granted, the run got one step further and stopped on
`CreateFileMapping Win32 error 5`: Git Bash's fork emulation needs
shared memory the sandbox denies, so Codex could not run the verifier it
had been told to run before editing anything. The sandbox policy is
per-OS; `autonomy:` is one line in one file shared by every machine that
checks out this repo. That is a gap in the contract, not a detail — it
is the only field here whose correct value depends on the host.

Two smaller things from the same run, both worth keeping:

**`AGENTS.md` is not in this repo, and Codex reads `AGENTS.md`.** The
runner warned and ran anyway, which is right — an agent without the
house conventions is degraded, not unsafe. But it means the second bot
was working from the task and its charter alone while the first works
from those *plus* `CLAUDE.md`. F-7 said the two bots would be identical
because they share host instructions. The truth is worse and more
interesting: they differ, and not in any way anybody chose.

**The prompt said "exactly one commit" with no exception.** The runner
has always treated zero commits as a legitimate no-op; the prompt told
the agent to produce one regardless, and Codex — reading the rule as
written on a task that needed no change — tried to. Two statements of
the same rule, in two places, disagreeing. The prompt now says a no-op
is a result.

What this run did *not* do is complete. Criterion 1 in §6 is still
unmet for any agent but Claude Code, and the reason is a Windows
sandbox rather than anything in the file contract. The contract itself
came out of this intact: a different CLI, a different argv shape, a
different instruction file, one refusal channel, and a handoff a human
can act on without reading a log.

---

### F-27 · Three walls behind the first one, and each one looked like the agent failing

*2026-09-11, making Codex actually run.*

F-26 ended on "the sandbox kills Git Bash, unresolved". Working through
that turned out to be four separate problems wearing the same coat, and
the useful part is not any one of them — it is that all four produced
the same symptom: *the agent did nothing and did not say why*.

The first move was to stop guessing. `codex sandbox` runs a command
inside the same Windows restricted-token sandbox with no model call at
all, which turns an expensive question into a free one:

    cmd          starts
    powershell   hello-from-ps
    git          git version 2.52.0.windows.1
    bash         *** fatal error - CreateFileMapping ... Win32 error 5

So git is fine under the sandbox, and PowerShell is fine. The only
casualty is Git Bash, and not because of git: MSYS fork emulation wants
shared memory a restricted token denies. That reframes the whole thing.
Codex was not blocked by its sandbox; it was blocked by *choosing* a
shell, and it chooses by looking at PATH.

**Wall 1: the shell.** `shell: posix|native` in the profile. `native`
means the runner takes every PATH entry carrying a POSIX shell off the
agent's environment — stated as the intent, not a list of directory
names, because a hardcoded `/usr/bin` is a guess about somebody else's
install. Codex then picks `pwsh.exe`, and a real run confirmed it.

**Wall 2: the launcher is a shell script.** `command: codex` resolves
to npm's extensionless shim, which is written in `sh` and dies calling
`sed` before the agent exists. Taking the shell away breaks the thing
that starts the agent. So under `shell: native` the command is resolved
against the *runner's* PATH — the one that still has a shell on it —
and a Windows executable sibling wins over a POSIX script. `codex.cmd`
runs on the stripped PATH; `codex` does not.

**Wall 3: 8191 characters.** `codex.cmd` is a batch file, and cmd.exe
truncates a command line at 8191 characters. The prompt carries the
whole task file. So the agent got a prompt cut off mid-sentence *and
lost the flags that came after it* — the log's banner read
`workspace-write [workdir, /tmp, $TMPDIR]` with the `--add-dir` simply
gone. Codex replied "Send me the issue to fix." Nothing errored.
Nothing warned. It was handed half a job and answered accordingly.

That one is worth dwelling on, because the diagnosis was only possible
by accident: the log happens to echo the prompt back. The prompt is the
one input to a run that was written down nowhere — the log holds
whatever the agent chose to repeat, which is a different thing. It is
saved beside the run log now, as sent. `prompt: argv|stdin` is the fix
for the truncation itself, and stdin is the better default for any
agent whose prompt can grow: argv limits are per-OS, per-launcher, and
silent.

**Wall 4, still standing: the git directory.** With all three fixed,
Codex read the contract, did the work, wrote the file — and could not
commit:

    Cannot commit because Git cannot create
    C:/.../.git/worktrees/bot-fixer-T-0008/index.lock: Permission denied

`--add-dir` had been granted and the banner confirms the sandbox
accepted it. It does not make that path writable in practice, and the
free probes could not establish why without spending more runs to find
out. So F-26's conclusion stands and is now load-bearing: on Windows,
under a sandbox, an agent in a *linked worktree* cannot commit. The
narrower fix is not a flag — it is giving each task a `git clone
--shared` instead, where the git directory sits inside the workspace
the sandbox already grants, and only a push at the end crosses the
boundary.

What all four have in common is the thing to keep. A truncated prompt,
a dead launcher, a missing shell and a denied lock are four different
faults, and every one of them arrived as *an agent that produced
nothing*. Three were invisible in the runner's own evidence, because
the runner measures the tree and all three happened before the tree was
touched. The one that was visible was visible only because Codex wrote
`.bot-blocked` — a channel built in F-4 for agents that crash, which
has now diagnosed three separate infrastructure faults and zero crashes.

A last one, smaller and the same shape: the prompt told every agent to
run `verify:` itself. A verify string is a POSIX command by convention
here, and an agent with no POSIX shell cannot run one — `cargo test …`
is shell-neutral, `test -f …` is not. The prompt now says what was
always true instead: the runner runs the verifier afterwards, in a
clean checkout, and that is the result that counts.

---

### F-28 · The work surface stopped being a worktree, and Codex still could not commit

*2026-09-11, rebuilding the isolation.*

Two unrelated-looking problems turned out to have one shape.

F-26: a linked worktree's git directory lives under the *main*
repository, so an agent that sandboxes itself by directory can edit
every file it was given and cannot write a commit.

And: a project need not be a git repository at all. CodeScope opens
plain folders — `core/src/git.rs` has `is_work_tree()` precisely so the
UI can drop the git surface for them — and there is no worktree to add
to a folder. Everything in this runner is built on `git worktree add`,
so the honest answer for half the projects the product supports was
"no bots for you".

Give the work surface **its own `.git`, inside it** and both answers are
the same answer. The surface is a standalone repository now: a
`--shared` clone of the project when the project is a repo, and (next)
a snapshot imported from the folder when it is not. `--shared` keeps the
objects in the project's store and reads them through alternates, so a
clone per task costs a checkout and not a copy of the history — the same
price a worktree charged. The alternates are live rather than a
snapshot, which is what still lets the rebase step replay onto a base
that landed after the clone was made.

Three things fell out of it that were not the point and are worth
keeping:

**The result has to be pushed back.** A clone under `.worktrees` is not
where anybody looks for a branch, so the branch is pushed into the
project at the end. That is not the push this loop refuses to make — the
refused one goes to a *remote*, where work becomes visible to other
people and hard to take back. This one moves a ref inside the repository
the task already named, and it is the only way a result outlives the
surface, because cleanup deletes the clone. A push that fails downgrades
a `done` to `needs-review`: a result nobody can reach from the project
is not a result yet, however green the verifier was.

**Cleanup became `rm -rf`.** `git worktree remove` refuses to delete
something that is not a worktree; `rm -rf` refuses nothing. So the
surface gets a marker file written into its `.git` at creation, and the
removal helper checks for it first. A path this script computed is not
by itself a reason to delete a directory tree — F-24's regression suite,
one layer down.

**The leftover check had been measuring the wrong thing.** `sweep.sh`
counted bot worktrees with `git worktree list`. A clone is not
registered anywhere, so that check would have passed by construction
forever. It counts directories now.

**And the part that did not work — at first.** The whole detour began
with Codex being unable to write `index.lock`. With the git directory
now *inside* the workspace the sandbox grants, it still could not:

    Cannot commit because Git cannot create
    C:/...worktrees/bot-fixer-T-0008/.git/index.lock: Permission denied

Same sandbox, same message, different path. At which point I concluded
that Codex denies writes to `.git` wherever it finds it — a deliberate
carve-out no arrangement of directories could get around.

**That conclusion was wrong, and the way it was wrong is the finding.**
See F-30. The carve-out is real: `.git` inside the workspace *is*
denied by default. What is not true is that nothing lifts it. Naming
that directory with `--add-dir` lifts it exactly — and the reason the
run above still failed is that `--add-dir` was pointed at the
*project's* git directory, because the substitution feeding it was
still computed from the project. It had never been pointed at the
surface's own `.git` at all.

The clone stands on its own regardless. It is what makes a plain folder
possible at all, and it removes an isolation model that was only ever
isolating the checkout.

---

### F-29 · A folder has no base, so the runner makes one

*2026-09-11, the other half of F-28.*

Once the work surface is its own repository, a project that is not a
repository stops being a special case and becomes a *seeding* question:
where do the files in the surface come from? For a git project, a
`--shared` clone. For a plain folder, a snapshot.

The snapshot is a bare repository under state with one branch,
`refs/heads/folder`, and it is three things at once:

- the **base** the work is cut from, which a folder does not otherwise
  have,
- the **tree** the scope check and the overlap check expand their globs
  against, which they were always going to need,
- and an **undo**, which is the part that matters most and was not the
  goal. A folder with no version control has no restore point. Pointing
  an autonomous agent at one and hoping would have been the reckless
  part of this whole design; now the first thing that happens to a
  folder is that it gets committed.

Four decisions, each of which had an obvious wrong answer.

**"The base moved" means "the folder changed", and the only way to ask
is to snapshot it again.** That sounds expensive and is not: an
unchanged folder produces the same tree, the runner notices and reuses
the existing commit rather than making an identical one. So the check
is a stat walk. Reusing the commit is also what lets two runs against an
untouched folder share a base — without that, every run would invent its
own base and the overlap check could never tell that two tasks were
talking about the same tree. Once that is in place the entire
rebase-on-collision path from F-25 works on folders unchanged, with all
four outcomes; only the meaning of "the base" differs.

**The exclude list lives in the snapshot repo, not in the folder.** A
folder has no `.gitignore` discipline — that is most of what makes it a
folder — so without a list the first snapshot of a node project is
`node_modules` and the first of a rust one is `target/`. That list is
this runner's opinion, and the user's folder is not this runner's to
write to, so it goes in the snapshot's `info/exclude`. A `.gitignore`
that *is* in the folder is honoured on top of it: the folder's own
opinion about what is not source outranks a default written by a
stranger.

The second half of the list is not about size. `.env`, `*.pem`, `*.key`,
`.netrc` — an agent that cannot read a file cannot leak it, and a
directory that was never a repository has never had a reason to keep
those out of itself. Making one into a repo without that list would take
a project whose secrets were safe by being un-versioned and commit them
into a store an agent then reads.

**The contract travels with the snapshot.** The rule that an agent reads
its charter from its own checkout does not get an exception for folders;
it gets a path. The contract is grafted in at `.bot-contract/` rather
than `labs/agent-bots/contract/`, because that second path means
something in this repository and nothing at all in somebody's folder.

**A folder gets a patch, not a branch.** The result is pushed into the
snapshot repo, so nothing is lost, but a folder has nowhere to pull a
branch *to*. The artifact a human can act on is a patch file, and the
useful thing about it is `git apply --check`: it answers "does this
still fit the folder as it stands now", which a branch in a repository
the folder has never heard of cannot.

What this does not do is merge. For a git project the loop ends at
"here is a branch, verified against this base"; for a folder it ends at
"here is a patch, verified against this snapshot". The second is
strictly weaker, and honestly so — there is no history to merge into,
and manufacturing one would be inventing a project structure the user
did not ask for.

---

### F-30 · Two failures, one message, two causes

*2026-09-11, the correction.*

F-28 ended on a confident sentence: Codex denies the model writes to
`.git` wherever `.git` is, so no arrangement of directories gets around
it. The evidence was two runs producing the same error at two different
paths, plus a permissions table inside the binary with `/.git` in it.
It read like a carve-out, and half of it was.

It was wrong, and the shape of the mistake is one this document already
has a name for.

The two runs did fail for the same *reason* in the sense that both were
denied a write to a `.git`. They did not fail for the same *cause*. In
the first, `.git` was outside the workspace. In the second it was
inside — and still denied, because a sandbox that grants a workspace
does not thereby grant the `.git` in it. What I did not check is what
`--add-dir` was actually pointed at, and the answer is: the *project's*
git directory, both times. The substitution feeding it was computed
from the project, and it stayed that way after the surface stopped
being a worktree. The grant had never once named the directory the
agent was being denied.

Point it at the surface's own `.git` and Codex commits. Clean run,
`done`, verifier green, branch pushed back. The first completed loop
under a second CLI.

So the carve-out is real — `.git` inside the workspace is denied by
default, which is a sensible thing for a sandbox to do — and it is
liftable by naming it, which is also sensible. Both halves obvious in
hindsight; the confident wrong half came from generalising to "wherever
`.git` is" on evidence that only supported "in the two places it was
tried".

This is F-23's rule pointed at myself. *A log tells you what happened;
it must not be asked what is true.* Two log lines that match are two
log lines that match. The thing that would have caught it is the thing
the runner does to its agents on every run and I did not do here:
**read the invocation, not the outcome.** The argv was printed in the
plan on every one of those runs, and it said `--add-dir
C:/dev/codescope-public/.git` while the failure said
`C:/dev/codescope-public.worktrees/bot-fixer-T-0008/.git`. Two different
paths, on screen, three inches apart.

### What stays

The runner-commit, built on the strength of the wrong conclusion, is
kept — because the reasoning that led to it survives the correction
intact. A sandboxed agent *can* write files and not history; that Codex
turns out to be grantable does not make every agent grantable, and an
agent that can only edit is now a first-class citizen of this loop
rather than a run that produces nothing. It is additive: an agent that
commits its own work still does, and the runner only ever touches what
is left over. Nothing in the evidence changes, because a commit is a
mechanical act that asserts nothing — `git add -A` puts out-of-scope
files in the diff where the scope check already catches them, instead
of dropping them quietly.

What the agent still owns is the message, and that is the right split.
`.bot-commit-msg` joins `.bot-blocked` and `.bot-review.md` as a channel
out of the worktree. A change committed with no message is committed
anyway — work that cannot be measured cannot be judged — and reported
`needs-review`, because no verifier reads reasons.

### And one thing the completed run exposed

Codex's own commit was signed `maui <maui.wind@gmail.com>`. It inherited
the ambient git identity and attributed a bot's work to the person
sitting at the machine. That is F-17's ambient identity, third
appearance, finally in the path that matters: the meddling verifier had
it, `mover.sh` had it, and now the real agent. The surface sets
`user.name` and `user.email` at creation, so it holds whoever ends up
committing — the bot as author, the runner as committer when the runner
does it, which is what git's split is for.

---

### F-31 · Replacing a mechanism hands back its guarantees, silently

*2026-09-11, four review rounds on the work surface.*

Sixteen findings across four rounds, all real. Most of them are one
thing said in different places, and it is a thing worth naming: **git
was enforcing rules nobody had written down, and swapping the mechanism
returned them without returning the checks.**

The work surface used to be a linked worktree. That came with
guarantees that were load-bearing and invisible:

- `git worktree remove` refuses to delete anything that is not a
  worktree. `rm -rf` refuses nothing — so the sweep's allowlist, which
  protects *branch names*, was suddenly guarding a path, and a stale
  directory named after an allowed branch would have been deleted. The
  runner had a marker file for exactly this; the sweep did not check
  it, and neither did the removal of the verify checkout.
- `git worktree add -b` creates a branch and hands you a checkout of
  it. A clone comes with an `origin` that lives in `.git/config` —
  which the agent can write, since the sandbox is granted that
  directory. `git remote set-url origin` would have redirected the
  runner's post-verification push anywhere. It pushes to the path now,
  which is a variable that has never been inside the worktree.
- `git worktree list` made every leftover visible from the project. A
  clone is registered nowhere, so the sweep's leftover check would have
  passed by construction — *and* nothing removed a surface after a
  successful push, so every finished task left a full clone behind
  while the handoff called it throwaway.

The same shape twice more, away from worktrees. A folder's
`info/exclude` is overridden by a `.gitignore` negation, so the secret
list needed a second layer that does not care how a path got into the
index — and a third, in the verdict, because neither binds an agent
that commits by itself. And `git clone` does not copy `info/exclude`,
so the surface had no protection at all against a secret the agent
*created*.

Then a cluster where the evidence and the artifact could describe
different trees:

- Nothing checked that HEAD was still on the task branch. Every number
  in the handoff is read from HEAD and the push names the branch; an
  agent that switched would have produced a green handoff for a branch
  still sitting at the base. The charter forbade switching and nothing
  enforced it — F-18's shape, again.
- `reattach move` recorded its failure and the evidence was overwritten
  anyway, so a failed `checkout -B` produced a handoff about the
  rebased tree while the epilogue said the branch had not moved. The
  order was wrong: move first, believe second.
- The scope check ran on the pre-rebase diff and was never re-run on
  the replayed one. A rename-aware replay can land a patch on a path
  the original never touched, so "green" was being read as permission
  to move the branch when scope had not been asked.
- And a verify *checkout* that could not be created was reported as
  `rebase-red` — "does not verify" about a tree nothing had measured.

Plus one honest race: snapshotting a folder moves a shared ref, and two
folder runners could read the same parent and overwrite each other. It
takes a lock now, held across the snapshot only — it is not the
dispatch claim.

What to take from it. Every one of these was invisible while the
mechanism underneath was doing the work, and every one became a gap the
moment the mechanism changed. The question that would have found them
in one pass is not "is the new thing correct" but **"what was the old
thing quietly guaranteeing?"** A worktree guaranteed ownership,
identity and visibility. A clone guarantees none of the three, and a
checklist would have said so faster than four rounds of review did.

---

### F-32 · The exception tells you the abstraction is wrong

*2026-09-11, on the observation that the bots should not all be code
writers.*

There were two kinds of task, `change` and `review`, and the second was
spelled into twenty-odd branches of the runner. Every one of them said
some version of *unless this is a review*: skip the overlap check, do
not harvest the commit message, do not commit what was left behind, do
not rebase, a commit is a failure here, zero commits is success here,
name a file instead of a branch in the handoff.

Twenty-odd exceptions is not a special case. It is an abstraction
being wrong in one place and paid for in twenty-odd.

What the runner actually branches on is **where the result lands**, and
that has nothing to do with reviewing. A commit lands in the tree and
needs the branch, the verifier and the replay. A report lands beside
it: one file, harvested into the control plane, and a commit is a
failed run. So the field is `produces: commit | report`, and the
reviewer becomes an instance of the second — `artifact:` names the file
and `shape:` names the template it must match, both defaulting to the
reviewer's because that is the role that got here first.

The test of whether a rename is real is whether the word survives
anywhere load-bearing. `.bot-review.md` is now a default, not a
constant. `templates/REVIEW.md` is now a default, not a path in the
prompt. `.state/reviews/` is `.state/artifacts/`. `run/review-shape.sh`
kept its name because it is genuinely the reviewer's verifier — one
shape among several, and the next reading role writes its own.

Three things came out of it that were not the point.

**A charter's silence is not a claim.** The obvious enforcement is that
the bot's charter declares `produces:` too, and a task that disagrees
is refused — a fixer cannot be handed a report. But charters are read
*at base*, so an absent declaration read as `commit` would refuse every
report task in the world until an unrelated PR landed. Silence has to
mean *no claim*, exactly as an undeclared `agent:` already falls
through to the task's. Declaring it is what binds it.

**A contract change and a runner change cannot be verified together on
a branch.** The verifier runs inside the verify checkout, so the script
that reads the runner's environment is the copy at base, not the copy
being edited. Renaming `BOT_REVIEW` to `BOT_ARTIFACT` turned the sweep
red for three checks — not because either side was wrong, but because
they were at different commits. That is contract-read-at-base working
exactly as designed, and it means the last review-shaped names in the
runner can only move once both halves are merged. Left alone, with the
reason written down, as #348. The alternative was exporting both names:
green today, dead code the day it lands.

**The reserved namespace was already there and unnamed.** `artifact:`
had to be validated — it is a filename the runner will `mv` out of a
worktree — and writing the rule down surfaced that `.bot-` had been a
namespace all along, with `.bot-blocked` and `.bot-commit-msg` in it.
An artifact must live there, which makes collision with a project file
impossible rather than unlikely.

What generalises: **count the exceptions before believing the type.**
One `if` for a special case is a special case. Twenty of them, all
saying *unless*, are the shape of the thing you should have modelled,
and every future role arrives as one more exception until you do.

---

### F-33 · A resolving citation is not a read line

*2026-09-11, the claim verifier.*

`review-shape.sh` checked that every finding cited a path that is a
file at the reviewed commit, at a line that file has, inside
`touches:`. That is a real check and it catches the obvious fabricator
— F-19's `fabulist.sh` invents a filename and the run goes red.

It also leaves the more convincing failure completely unguarded. A
reviewer that lists real paths, real line numbers, correct frontmatter
and a filled-in blind-spot section, and describes code that is not on
those lines, passes every check. Everything mechanical about it is
right. It is the shape a model produces when it is summarising what a
file like that usually contains instead of reading the one in front of
it, and it is *more* dangerous than the invented path, because nothing
about it looks wrong.

So a finding now quotes the code it is about, and the runner reads the
blob and checks the quote:

```
- core/src/telemetry.rs:142 — the span is opened and never closed.
  > let span = info_span!("emit", id = %id);
```

The rules that mattered, and why each one is where it is:

- **Substring, not equality.** Quoting a fragment of a 120-character
  line is the honest way to cite one. A fragment cannot be guessed
  either, so nothing is lost.
- **Whitespace normalised on both sides.** Re-indenting a quote is not
  inventing one, and refusing a review over two spaces would teach
  exactly the wrong lesson about what the check is for.
- **At least eight characters of content, whitespace excluded.** `{`
  occurs on half the lines of any Rust file. A quote that matches
  everything identifies nothing, and a minimum is the only thing
  standing between "quote the line" and "quote a brace".
- **A multi-line quote is consecutive lines from the one cited.** It
  lets a finding be about a paragraph rather than a line, and it makes
  the cited coordinate load-bearing instead of decorative: the quote
  has to start where the citation says it does.

What this does *not* do is move F-19's ceiling. Nothing here can tell
you whether the finding is correct — only that the reviewer was looking
at the line it says it was looking at. The gap between "cited" and
"read" was the one that could be closed mechanically, so it was worth
closing; the gap between "read" and "right" still needs a human.

### The second thing this exposed

The verifier could not be tested from the branch that changes it.
`verify:` runs inside the verify checkout, which is a checkout at the
task's `base:` — so a full run exercises the copy of `review-shape.sh`
sitting at base, never the one being edited. Every check of it went
through three stubs and a whole dispatch, and all of them were checking
last week's verifier.

`review-shape.sh` is a pure function of (review, commit, touches).
Nothing about testing it needs a runner, a surface or a task. Ten
direct checks now call the working-tree copy with the environment set
by hand — one per property, including the three failure modes the
quoting introduced — and they are the only part of this that could be
proven before the merge.

The general form, and it is the same one as #348: **a script that is
read from the tree under test can only be tested as a unit.** Anything
that reaches it through the full loop is testing whatever is at base.
That is contract-read-at-base doing its job, not a defect — but it
means "the sweep is green" answers a narrower question than it looks
like it does, and it is worth knowing which of the two it answered.

---

### F-34 · The same command, protective or destructive, depending on state nobody checked

*2026-09-11, two review rounds on `produces:` and the surface.*

Seven findings, and the one worth keeping is four characters long.

`strip_protected` holds secrets back by running `git rm --cached` on
every protected path in the index. On a path the agent just created
that unstages it, which is the whole intent. On a path the project
committed years ago — some repositories do have a `.env` in their
history — the same command stages a **deletion**, and the runner's
fallback commit records the removal of a file nobody asked it to touch.
The protective operation is the destructive one, and which of the two
it is depends on a fact the function never looked up.

Nothing about it reads as dangerous. "Drop the protected paths from the
index" is exactly what it does. It is the index that has two meanings —
*staged addition* and *staged difference from HEAD* — and the code was
written against the first while running against both.

Its twin, one screen further down: the blocker for a protected path in
a commit ends with the sentence **"Nothing is pushed."** That sentence
was not true. The push was gated on a broken tree and on the commit
count, never on `PROTECTED_IN_DIFF`, so a run could print the promise
and publish the branch in the same breath — and a folder run would
write the secret into `.state/patches/` instead, which is the same
publication through a different door. The prose was the specification
and nobody had run it.

So: anything the base already carries is put back exactly as the base
has it, only genuinely new paths are dropped, and a run that held
something back says so and comes back `needs-review` rather than
`done`. A rewritten `.env` is not necessarily malice — a formatter, an
install, a rotated key — but it is not a verifier's call either.

The other five, briefly, because each is a smaller version of something
already written down here:

- **The control plane inside a folder project.** An import walks the
  whole folder, so `--state` under `$REPO` puts this run's logs,
  prompts and the snapshot repository itself into the surface, hands
  them to the agent, and snapshots them again next run. It compounds.
  Refused at dispatch now. A git project was safe only by accident.
- **`PROTECTED_IN_DIFF` was not recomputed after a rebase**, exactly as
  the scope check was not one round earlier. A base-side rename can
  land a patch at a path the original diff never had. Fixing the same
  shape twice means the lesson was "re-run the scope check", when it
  should have been "re-run *every* diff-derived check".
- **A branch name is not a reservation.** The dispatch check asks the
  project whether `refs/heads/<branch>` exists — but a surface is a
  standalone clone, so the ref only appears there at the push. Two task
  ids naming one branch both pass, both do all the work, and the loser
  finds out last. `touches:` does not catch it: the collision is in the
  name, not in the files.
- **A channel is a file the agent wrote, never a link to one.** `[ -f ]`
  is true through a symlink and `mv` moves the link, so
  `.bot-review.md` could be a link to anything on the host — stored as
  this run's evidence, read by the verifier through the link. Removed
  unread now.
- **`--shared` means the base objects live in the origin.** A surface
  kept as evidence borrows them, and the base ref is free to move or be
  deleted while it waits. The commit is pinned at `refs/bot-base/<id>`
  for as long as the surface exists.

What generalises: **ask what a command does in the state you did not
test, not in the state you had in mind.** The index entry that is an
addition in one repository is a deletion in another; the branch name
that is free in the project is taken in a sibling clone; the file that
is a file is a link. Every one of these was correct in the case that
was in front of me while writing it.

---

### F-35 · The check was not where the code ran

*2026-09-11, the round after F-34.*

The runner has had a meddling-verifier check since F-6: `verify:` is
arbitrary code, so after it runs the runner re-hashes the tree and
blocks the run if anything moved. `meddler.sh` is the regression test —
a verifier that reaches sideways into the agent's worktree and commits
there, then exits 0.

The hash was taken of `$WT`. The verifier runs in `$VERIFY_WT`.

So the one thing a verifier can do without reaching anywhere at all —
rewrite the source in the clean checkout it was handed, then pass —
went unwatched, because the check was standing in the tree the attack
does not need to visit. `meddler.sh` had to reach for the sibling
worktree to be caught; a verifier that stays home never triggered it.
The test proved the check worked against the attack the test performed.

Fixed by hashing both, with one distinction that had to be got right:
in the verify checkout, *untracked* files are ordinary. A test writes a
fixture, a tool leaves a cache. Calling that meddling would fail honest
verifiers, so only tracked content counts there — `git diff HEAD`, no
porcelain. Rewriting source in place has no innocent reading;
scribbling a cache does. `selfmeddler.sh` is the other half of the pair
now.

Three smaller ones from the same round, each a variant of *the guard
and the thing it guards are in different places*:

- **`mkdir -p "$STATE"` ran before the check that refuses a state
  directory inside a folder project.** The invariant is "the runner
  writes nothing inside a plain folder"; the implementation created the
  directory and then declined to use it. Now checked before the mkdir,
  and again after canonicalisation — `$REPO/../proj/.state` is inside
  the project by any honest reading and is not a prefix of it by string
  comparison.
- **The symlink guard covered `$TASK_ARTIFACT` on every task.**
  `artifact:` is validated for a report task and ignored for a commit
  one, so a commit task could name `../../somewhere` and have the guard
  itself `rm -f` a symlink outside the surface. A guard that reaches
  further than the thing it guards is not a guard.
- **The push was not gated on the verdict.** `blocked` means the result
  cannot be trusted — a red verifier, a tree that moved, a scope
  violation — and the branch was pushed into the project anyway, under
  a name the next dispatch then refuses as "already exists". It was
  gated on a broken tree, on the commit count, and (since F-34) on
  protected paths: three specific reasons, where the general one was
  sitting in `$STATUS` the whole time.

What generalises: **a check is a claim about a place.** "The tree did
not move" means nothing until you say which tree, and the answer is not
"the one I had a variable for" — it is wherever the thing being checked
actually happens. Three of these four were guards pointed one directory
away from the code they were guarding.

---

### F-36 · The sandbox ends where the runner's tools begin

*2026-09-11, a second reviewer on the whole stack.*

A different model was pointed at the same branch, told what the lab
claims and what was already known, and asked for defects with a failure
scenario attached. Thirteen, ten of them new. Two of those changed what
this prototype is.

**The agent writes `.git`. `.git` is where git keeps the names of
programs it runs.**

The surface hands the agent its own `.git` on purpose — a sandbox that
denies it cannot commit, which is F-28. What follows from that had not
been thought through: a hook in `.git/hooks`, or `core.hooksPath`, or
`core.fsmonitor`, or `diff.external`, or a `filter.*.clean`, is a
string in a file the agent may write, and *every runner git call after
the agent's turn executes it*. Outside the sandbox, as the runner, with
the control plane and the real project in reach. `git status` is
enough; it does not need the commit.

The whole design rests on the agent being contained and the runner
being the one that measures. The measuring instruments were inside the
containment.

The agent's turn now ends with a disarm: the hook directory is emptied,
`core.hooksPath` is forced to a runner-owned empty directory through
`GIT_CONFIG_*` (which behaves like `-c` and outranks the surface's
config), and local config keys that name a command are enumerated from
what is actually there and unset — enumerating beats a deny-list of
guesses. `saboteur.sh` drops a `pre-commit` and the sweep checks the
marker file it would have written. With the disarm switched off, that
marker appears: the check is real, and so was the hole.

**The prose about the push was a guess about git.**

Two rounds ago the push moved from `origin` to a path, on the argument
that `origin` lives in a config the agent can write. `url.<x>.insteadOf`
rewrites the *argument* to `git push`, so one config line redirects
`git push /path/to/project` at an attacker's URL, the push reports
success, and the handoff says a branch landed in a project that never
received it. The key is unset by the disarm, and the ref is now read
back out of `$ORIGIN_REPO` afterwards — a push is believed because the
destination has it, not because the command exited 0.

And the clone still had `origin`: live, writable, pointing at the
user's repository. Checked against a throwaway pair rather than
reasoned about — a `--shared` clone can create a branch in its origin
*and delete an existing one*, no `--force`, no refusal. The remote is
removed at creation now. It is a guardrail and not a sandbox (the path
is still in `objects/info/alternates`), but the accident is the case
that happens, and "nothing here ever pushes" is true by default instead
of by hope.

Eight more, each a real scenario:

- `strip_protected` walked the whole index instead of the staged
  change, so a project with a committed `.npmrc` — registry config,
  entirely ordinary — would have every single run report a secret held
  back that nobody touched, and never reach `done`. A staged *deletion*
  had no index entry at all, so it was never restored.
- The verdict for a held path sat below the generic "N uncommitted
  file(s)" branch, which fires first by construction: holding a tracked
  file back is what leaves the worktree dirty.
- `rev-list`, `status` and `diff` ran unguarded after the broken-tree
  check under `set -e`. An agent that removes `objects/info/alternates`
  leaves HEAD resolvable and the base unreachable — the script exits
  128 with no handoff and the live task stuck on `dispatched`.
- `"${AGENT_ARGV[@]}"` without the `${a[@]+…}` guard: on bash before
  4.4, which is what macOS ships, `set -u` kills the script on an empty
  array. Every stub run in the sweep passes an empty argv.
- The sweep cleared *every* live task in the control plane, not its own
  — taking a real in-flight run's task file out from under it.
- The derived task scraped citations from the whole review, including
  "What I could not check", where the charter explicitly tells the bot
  to record observations about files it was not asked to review. Doing
  what the charter says killed the handoff.
- A derived task on a folder project carried `base: <sha>`, which
  dispatch refuses — a handoff naming a next step nobody can take.
- Four "nothing ran" outcomes exited 1, which `bot-tick.sh` counts as
  `blocked` and feeds to `--give-up`. Exit 3 exists for exactly this.
- `--dry-run` said "changes nothing on disk" while stamping the state
  directory with a repository identity.
- `review-shape.sh` counted lines with `wc -l`, so a file whose last
  line has no newline came back one short and a finding citing that
  line was refused as past the end.

What generalises, and it is not "review harder": **a second reader with
a different prior finds a different class of thing.** Ten rounds of
review had produced order-of-operations bugs, guards that proved too
little, and prose that disagreed with code — all real, all the same
family. The first question from somewhere else was *what does the
attacker do with what you handed them*, and that question had never
been asked here, because the threat model in the README stops at "an
agent might be sloppy or wrong". It does not say "an agent's output is
an input to my tools", and that sentence is the one this finding is
about.

---

### F-37 · A measurement that could not be taken is not an empty measurement

*2026-09-12, the twelfth round.*

Nine findings, and they read as a direct continuation of the one before:
the reviewer had picked up the thread and started pulling. Two of them
were structural.

**`.git` need not be a directory.**

The disarm empties the hooks and unsets the config through
`git -C "$WT"`, and the first thing git does with `-C` is ask `.git`
where the repository is. A one-line file reading
`gitdir: /somewhere/else` is a valid `.git` — it is the form every
linked worktree uses, so it is not a trick git tolerates by accident.
Write one before the turn ends and the hook cleanup, the config
enumeration, the evidence, the commit and the push all run against a
repository the agent picked. The disarm was standing on the one thing
it had not checked.

The clone's git dir is recorded at creation now and compared before
anything is read through it. A pointer that has changed is removed
unfollowed rather than resolved, which leaves git unable to answer
anything about the surface — and every evidence read is guarded, so
that arrives as an unreadable tree and a blocked run with a verdict that
names the swap, instead of measurements of somebody else's repository.
`hijacker.sh` does it with a `gitdir:` file, which needs no symlink
privilege and therefore runs on Windows too.

**`core.worktree` names no program, and that is why it was missed.**

The disarm's deny-list was built by asking *which keys name something
git will execute*. `core.worktree` names a directory. Set it and the
runner's own `status`, `add`, `diff` and `commit` read and publish files
from wherever it points — nothing executed, no hook, no filter, the
whole measurement just quietly about somewhere else. The rule was too
narrow by one word: it is not *what git runs*, it is *what git does and
where*.

`core.bare` went on the same list and came off it in the same session.
A clone is born with `core.bare = false`, so unsetting it fired on every
run, and `config-disarmed` — an event that is supposed to mean *the
agent wrote something it should not have* — appeared on the board every
single time. A signal that is always on is not a signal.

**And the class the other seven belong to.**

- `$dir.break` is the mutex that serialises breaking a stale lock, held
  for two reads and an `rm -rf`. A run killed inside that window left it
  behind for ever: every later `mkdir` of it failed, so the stale path
  was never entered again and the abandoned lock outlived every waiter.
  The recovery mechanism had a permanent failure of its own.
- `rebase --abort` and `update-ref` both had their exit status
  discarded, and the handoff said "it has been put back" either way. The
  ref is read back now; if it is not the tree that verified, the run
  says so and names the command to fix it by hand.
- The rebased diff was read with `|| true`, so a damaged object store
  produced an empty diff — and an empty diff means no path is out of
  scope and none is protected. Both gates open, on the branch-moving
  path.
- `git diff --shortstat` and `git status --porcelain` were unguarded
  under `pipefail`: the script exits 128 with no handoff and the live
  task stuck on `dispatched`. The second of those was the one that
  turned the new `.git` check from a blocked run into a crash, the first
  time it fired.
- `find -maxdepth` is GNU, in a guard on a lab that says it supports BSD
  `find` — and 20 lines above it, `take_lock` uses the portable
  `-prune ... -print` for exactly this reason.
- A failed cleanup downgraded the task to `needs-review` while the
  handoff's frontmatter still said `status: done`. One run, one record,
  two answers.
- `drop_sweep_tasks` took a task id as proof of ownership, and
  `T-0006-fix` is what the runner derives from `T-0006` — an id a human
  running the shipped examples has too. The sweep now refuses to start
  while any of its ids already exist, rather than deleting them: the
  only sound claim to a file is having created it.

**And one that was found while fixing them,** which is the finding in
miniature. The new blocker text read *"goes through `git -C $WT`"*, with
backticks, inside a double-quoted shell string. Backticks are command
substitution. The sentence ran git, pasted its help output into the
blocker, and exited 1 under `set -e` — killing the run, with no handoff,
at the precise moment it had just caught an agent redirecting the
runner's tools. Grepping for the rest turned up one that had shipped:
*"excluded from every `git add` this runner performs"* ran a bare
`git add` in the runner's own working directory on every
blocked-by-a-secret run, and went unnoticed only because git answers
that with "Nothing specified, nothing added." and exit 0.

Prose that looks like documentation was an input to the shell. Which is
the threat model with the agent taken out of it, and the reason it is
now written down as §3.6 rather than implied by thirty-seven findings:
**a measurement that could not be taken is not an empty measurement**,
and the same `|| true` that hides a broken diff hides a broken restore,
a broken push, and a broken `add`. The sweep is 67 checks now; four of
them are this round, and one of those is a lock left deliberately
wedged.

---

### F-38 · An identity you compare is a race; an identity you name is not

*2026-09-12, the backlog on the PR underneath.*

Twenty-two review threads were still open on the parent PR while the
branch above it was on its twelfth round. Checking them one by one
against the code: **eleven were already fixed** - on the branch that has
not merged down yet, so from the parent's diff they are as open as they
ever were. Two more are F-6, which is not a bug but graduation
criterion 5. Nine were real.

That ratio is the first finding, and it is about process rather than
code. **A stacked PR's review comments describe a tree that no longer
exists**, and nothing on the page says which ones. The reviewer is not
wrong and the author is not ignoring them; the comments are simply
pinned to a commit the work has moved past. Half the backlog was a
merge that had not happened. Read it as a queue of unfixed defects and
you will fix eleven things twice.

**The one worth keeping is four lines of locking.**

Releasing a lock was: read the owner file, compare it to this run's
token, and if it matches remove the file and `rmdir`. Every word of
that is careful and it still has a window - the lock is broken as
stale, a new run creates the same directory and writes *its* token, and
the old process, still holding a comparison it made a moment ago,
deletes the new holder's marker and rmdirs its lock. The critical
section is then open from the outside, which is precisely the thing a
lock exists to prevent.

The fix is to stop comparing. The token becomes the *name* of the file:

```bash
: > "$dir/owner.$RUN_TOKEN"            # take
rm -f "$dir/owner.$RUN_TOKEN"          # release - can only be ours
rmdir "$dir" 2>/dev/null || true       # fails while anyone else's is there
```

There is no window because there is no comparison. `rm -f` names a file
only this run could have created, and `rmdir` refuses a directory that
still holds somebody else's. The same change makes "are we still the
holder?" a file test instead of reading back contents another process
may have rewritten.

Its twin, one round late: **ten minutes of mtime is a guess about the
holder, and the mtime is never refreshed while the critical section
runs.** A legitimately slow dispatch - a large checkout, a folder
snapshot - looked exactly like a crash and had its lock taken away
while it was still inside. The token starts with the holder's pid and
the holder is on this machine, because the lock is a directory on it,
so the question has a real answer: `kill -0`. A pid that is gone is a
run that is gone. A reused pid costs one missed recovery and a clear
timeout message; breaking a live lock costs two runners in one critical
section, and those prices are not close.

The other seven:

- **`tree_state` hashed untracked *paths*, not untracked *contents*.**
  `status --porcelain` names the file and says nothing about what is in
  it, so a verifier rewriting an untracked file it did not create left
  every byte of the hash where it was. Ignored paths stay out on
  purpose - a build cache moving is not meddling, which is the line the
  verify checkout already drew.
- **`awk -v v="$2"` runs the value through awk's escape processing**,
  so a worktree root of `C:\tmp` was stored in the live task as
  `C:<TAB>mp` and the overlap scan then read the live surface as stale.
  On the platform this lab is developed on. Through `ENVIRON` now.
- **`--dry-run` created the control plane** - `.state/` and every
  directory under it, in a clean checkout - before reaching the branch
  that prints the plan and stops. Third time this flag has been caught
  changing something it says it does not.
- **`bot-tick.sh` always forwarded `--state`.** `bot-run` reads the
  *presence* of that option as proof somebody coordinated the control
  plane deliberately, and waives its refusal to run from a linked
  worktree on the default plane. Forwarding it unconditionally made the
  proof automatic: two linked checkouts would each resolve their own
  default state, each be told it was deliberate, and dispatch over each
  other.
- **The surface marker proved provenance, not ownership.** The leaf is
  the branch with `/` turned into `-`, which is not injective:
  `bot/a-b/c` and `bot/a/b-c` name one directory. Both tasks would find
  a marked surface there and each believe it was its own. The marker
  names the task; now it is read rather than merely counted.
- **A branch committed to and then reset back is not a branch nothing
  happened on.** `merge-base --is-ancestor` is true when HEAD *is* the
  base, so the run reads as zero commits, in scope, clean - the no-op
  shape, and the no-op path deletes the surface and the branch, which
  is where the commit still was. The reflog is the only witness that
  survives a reset, so it is asked how many distinct tips the branch
  has had.
- **The `.state/REPO` stamp was check-then-write.** Two first runs for
  two repositories sharing a fresh control plane both saw no file and
  both walked into the same task and lock namespace. `set -C` makes
  creating the file the claim, and the loser reads back what landed.

Four new checks: a reset that must not be cleaned up, the surface it
must leave behind, and a dry run against a fresh state directory that
has to still not exist afterwards. Sweep is 71.

---

### F-39 · The gate was a flag, and the flag was not the hole

*2026-09-12, building the thing the README had been promising.*

`--chain` was off by default and the README said the gap it left is
where the approval inbox goes. That sentence had been true for eleven
rounds and it described the wrong gap.

**`bot-tick.sh` reads `.state/proposed/` alongside the repository's own
examples.** A derived task landed there and the scheduler picked it up
like anything else — so the gate was not "off by default", it was
"absent, with a flag sitting next to where it should have been". And
one line makes it worse than that: the derived task carried
`schedule: $TASK_SCHEDULE`, inherited from the review that wrote it. A
recurring review produced a **recurring fix task**. The parent's
recurrence is a statement about reading telemetry every day; it says
nothing about committing code every day, and it was being read as
though it did.

So the inbox is not a feature that was missing from a design that held
without it. It is the missing half of a claim the README was already
making.

What it is:

- A proposal carries `approved_by:`, `approved_at:` and
  `approved_body:` or it does not run. The runner refuses it before the
  lock and before the surface — a refusal that has already created
  something is not a refusal — and exits 3, so a scheduler reads
  "nothing happened" rather than "this failed".
- `approved_body` is a hash of the task with the approval lines
  removed. **The approval is of the bytes, not of the name.** Approve
  `T-0006-fix`, edit `T-0006-fix`, and the approval no longer describes
  it: `stale` is its own verdict with its own sentence, because "never
  approved" and "approved, then changed underneath you" send a reader to
  do different things.
- One definition, in `run/approval.sh`, sourced by all three scripts.
  Three copies of six lines would be three chances to disagree about
  what approved means, and the one that drifts is the one that lets
  something through. First shared file in the lab, and it earned it.
- The derived task's `schedule:` is now `manual`, and an approved
  proposal is dispatchable whatever it says — the approval *is* the
  scheduling decision for a one-shot task.

And what it is not, which took longer to get right than the code. **An
approval is not proof that a human did it**, and nothing on this side of
the filesystem can be. What it is is proof that something outside a work
surface did it, and that is a real boundary: the agent only ever has the
surface, the control plane is not its to write, so an approval in the
control plane is not the agent's. `bot-approve.sh` also refuses when
`BOT_RUN_ACTIVE` or `BOT_CHAIN_DEPTH` is set, which catches the loop
reaching for its own gate through a verifier or a hook — not the
containment, the sanity check on the containment.

`--chain` keeps working and now has to open the gate rather than walk
around it: it writes `approved_by: --chain (nobody read this)` and
boards `approval-bypassed`. That is the honest shape of a bypass. A gate
a machine cannot open is not what this needs; a gate a machine can open
*silently* is.

Four checks: an unapproved proposal is refused, the loop cannot approve
for itself, approving then running works, and a task edited after
approval is refused again. Sweep is 75.

**The one to carry into per-bot memory**, which is next and is the same
shape: memory is text an agent wrote that the runner later feeds back
into a prompt. It is an approval gate problem before it is a storage
problem, and building the store first would be building the injection
channel and leaving the gate for round thirteen.

---

### F-40 · A guardrail with `|| true` on it is a comment

*2026-09-12, the thirteenth round.*

Four, and two of them are about the same mistake made in two different
ways: writing down what a mechanism is *for* and then not checking that
it happened.

**`git remote remove origin || true`.** The block above it explains, at
length, that a clone is born with a writable remote pointing at the
user's repository and that removing it takes away the one push an agent
could make by habit. Then it swallows the failure. A config lock is
enough, and the run carries on and starts an agent in a surface with
exactly the capability the paragraph says it removed — quietly, because
the whole point of `|| true` is that nothing is said. The remove is
followed by `git remote` now, and a remote that is still there is a
failed dispatch: the surface comes down, the base pin is dropped, and
no agent runs.

Which generalises past this line. **A guardrail is a claim about state,
so it ends with a read, not with a command.** The push already worked
this way — believed because the destination has the ref, not because
`git push` exited 0 (F-36) — and this is the same sentence about a
different verb.

**A verdict already written cannot be corrected by a flag.** The
previous round guarded the post-rebase `--shortstat` by setting
`TREE_BROKEN=1`, which reads exactly like the three guards above it.
Except those run *before* the verdict and this one runs after. So the
flag did reach the publish gate, which skipped the push in silence,
while `STATUS` stayed `done`, the handoff reported success and the run
exited 0 — with no branch in the project. The guard turned a crash into
something worse than a crash: a false report. It rewrites `STATUS` and
`BLOCKERS` now, and it keys off the command's exit status rather than an
empty result, because an empty `--shortstat` is also what commits that
change nothing produce and F-22 says that is an answer.

Two smaller ones, both about a claim being checked in a different place
from where it is used:

- **The sweep's ownership preflight read `id:` from the frontmatter;
  the cleanup deletes by file name.** A pre-existing
  `proposed/T-0006-fix.md` whose id is missing or says something else
  passed the scan and was deleted anyway — the exact hole the preflight
  had been added to close, one round earlier. Now both questions are
  asked, and the one that decides is the one that deletes.
- **The usage section still carried F-28's conclusion** that granting
  the git directory "did not rescue Codex", two rounds after F-30
  recorded that it did: `codex.agent.md` passes the surface's own
  `.git` as `--add-dir {git_dir}` and Codex commits under it. Prose that
  contradicts the code it documents is a defect with a slower failure
  mode, and this file is long enough that the only way it stays true is
  for a reader to keep catching it.

---

### F-41 · Memory is an approval problem before it is a storage problem

*2026-09-12, building the last thing on the list.*

Per-bot memory had been on the "not covered" line since the first
draft, described as a thing bots would obviously want: a run learns
something, later runs know it. Written that way it is a cache, and a
cache is a morning's work.

It is not a cache. **A note is the only thing in this loop where an
agent's own prose comes back to it as input**, on a later run, pasted
into a prompt by the runner. Threat model rules 1 and 2 at once — and an
injection channel the loop builds for itself, for free, unless something
stands in front of it. Build the store first and you have built the
channel and left the door for round fourteen.

So the store is four files' worth of limits and one directory:

- **Nothing is live until a human approves it.** Same three fields as a
  proposed task, same hash over the body, same inbox. A run writing to
  `.bot-memory` is a bot *asking*.
- **One fact, 400 bytes, twenty notes.** And the cap is refused, not
  rotated: `docs/HANDOFF.md` in this repository grew to 3600 lines
  because nothing ever said no, and was deleted rather than read. A bot
  with two hundred notes has a diary. `--forget` is how one goes, and a
  person chooses which.
- **A note may not restructure the prompt it is quoted in.** No `---`
  fence, no `#` heading, nothing that repeats one of the runner's own
  section markers. That is checkable; "is this note honest" is not, and
  the two failure modes want different mechanisms. A note that says
  something wrong is a note nobody should approve. A note that closes
  its section and opens a new one is a note no reader reliably catches.
- **They arrive labelled.** *Claims, not contract*, charter wins, and
  each one names the run that produced it so the bot can go and look.
  F-23 turned on a bot's own past: prose is a claim about what
  happened, and what happened is somewhere else.

The sweep drives the whole order: the note is stored and not live, an
unapproved note is **absent from the resolved prompt**, an approved one
is **present**, and a prompt-shaped note never gets stored at all. The
two prompt assertions are the ones that matter — everything else is
about files, and the prompt is the thing that actually reaches a model.

---

### F-42 · The thing you validated has to be the thing you use

*2026-09-12, the fourteenth round, on the approval gate itself.*

Four, and the first is the gate defeating itself.

**The approval hashed the proposal and then the runner read the file
again.** Parse the frontmatter, build the prompt, copy to the live task
— three more reads of a path in a directory anybody may edit, and an
edit in that window changes what runs without touching the approval that
was checked. The gate had the shape of `stat` then `open`. So the first
thing it does now is copy the bytes to `.state/tmp/dispatched-<id>.md`
and point `$TASK` at the snapshot; every read after that, the live copy
included, is of the file that was validated. This is F-38's sentence
again — *an identity you compare is a race* — arriving at a feature
built two hours after F-38 was written, which is the honest measure of
how easily it comes back.

**`BOT_RUN_ACTIVE` did not cover the path its own comment claimed.** It
was exported around the agent, and the comment said it catches the loop
reaching for its own gate "through a verifier or a hook". `run_verify`
did not export it. `verify:` is a shell command sourced from repo
content (F-6) running inside the loop, so a verifier — including one the
agent had just edited on its branch — could call `bot-approve.sh` and
approve the next proposal. Exported there too, and the sweep drives it
through a real `verify:` rather than by setting the variable by hand,
because the variable being set is the thing under test. The fixture is
chosen so the negative shows: `bot-approve.sh` with no arguments lists
the inbox and exits 0, so without the guard the verifier passes.

Two more:

- **`git hash-object` takes its object format from wherever it runs.**
  Approve from inside a SHA-256 repository and list from outside it and
  the same bytes hash two ways — an approval reported `stale` for being
  read from a different directory. All three callers now hash through
  the lab's own repository.
- **An inbox listed everything ever proposed.** A proposal is kept after
  it runs, because it is the record of what was dispatched, so the count
  stopped meaning "waiting" the moment the first one completed.
  Terminal live tasks are now counted separately rather than hidden:
  *empty* and *nothing left to do here* are the same sentence only if
  you can see the difference.

---

### F-43 · Fixed at the site it was reported, not at the shape it has

*2026-09-12, the fifteenth round - and one question from the user.*

Six findings, and two of them were the sentence from F-42 arriving
again: *the thing you validated has to be the thing you use.*

**The dispatch snapshot had one name per task, not per invocation.** Two
runs of `T-x`: the first snapshots and validates, the second overwrites
that file, and the first then parses and copies bytes nothing checked.
The fix for a check/use race, with a check/use race in it. Unique per
run token now, created with `set -C` so the create is the claim, removed
on the way out — and the shared `dispatched-<id>.md` record is written
only *after* the gate passes, because writing it before meant every
refusal overwrote the record of the last thing that actually ran with
something that never did.

**`memory_block` validated each note and then opened it again to
render.** A note replaced in between goes into the prompt carrying
somebody's approval of different text. That is the whole mechanism
defeated by a race, in the feature built specifically to have a
mechanism.

Then the user asked the question that made this finding worth its own
entry: *you said it keeps finding the same thing in different places —
can you check whether these six occur anywhere else in the PR, so we get
ahead of it?*

They did. **The task half of `bot-approve.sh` had the identical defect
and nobody reported it:** `approval_state "$TASK"`, then
`approval_body_hash "$TASK"`, then `awk ... "$TASK"` — three opens of a
file in a directory anybody may edit, so the hash was of one version and
the approval got stamped onto another. The review found it in
`memory.sh` and stopped there, and I had fixed `memory.sh` and stopped
there. Two readers, both anchored on the line they were shown.

So the fix is not per-site. **Both `approval.sh` and `memory.sh` now
take text rather than a path**, with thin `_file` wrappers for callers
that only have a name and are only asking one question. A caller that
needs two answers has to read the bytes itself, which makes the right
shape the easy one instead of the remembered one.

The rest of the audit, for the record, because a negative result is
worth as much as a finding:

- **Predictable temp paths written before a claim.** Clean elsewhere —
  the folder-import index files use the pid, the handoff rewrite uses
  the pid, the sweep uses `mktemp`.
- **A contract of N fields checked as fewer.** This was the finding
  (`approved_at` was never required, on both halves, so a truncated
  approval passed and then rendered a blank date). Checked the others:
  `review-shape.sh` requires all three of its frontmatter fields and
  the runner's required-task-field loop is complete.
- **An early exit that hides a later section.** This was the finding
  (no `.state/proposed` meant the shared inbox reported empty while
  notes sat in it). No other multi-part listing has a guard at the top.
- **A count whose label stopped being true.** This was the finding
  twice over — the inbox counted completed proposals as "waiting", and
  then in-flight ones too. Also removed a `memory_counts` helper that
  returned a waiting count nobody called: a queue whose contents nobody
  can see is a number.
- **Check-then-read on a path the agent owns.** Five of those, all in
  the evidence phase, all resting on the agent's turn having ended when
  its process did. Nothing stops an agent backgrounding something. That
  is now written down under "knowingly open" in §3.6 rather than
  pretended away — closing it means snapshotting the surface before
  measuring it, which is a different design and not a two-line fix.

Two new checks, both of which the review would otherwise find next
round: a truncated approval is refused at dispatch, and a note whose
approval lost its date stops reaching the prompt. Sweep is 87.

What generalises: **a review comment names a line, and the defect has a
shape.** Fixing the line is what gets asked for and it is half the job;
the other half is grepping for the shape and finding the instance
nobody pointed at. Fifteen rounds in, that is the only thing that has
reliably reduced the next round.

---

### F-44 · Opt-in scheduling was half the answer, the default directory was the other half

*2026-09-12, writing the first real task.*

F-23's lesson was that a directory of task files is not a queue, and
the fix was `schedule:` — opt-in, `auto` or nothing happens. That
closes the case of a fixture with no schedule, which is what `T-0004`
is.

It does not close the case of a fixture *with* one.
`examples/T-0006-review-telemetry.md` is a routine on `every: 1d`,
written precisely to demonstrate that recurrence works, and the
scheduler's default task set was `examples/` plus `.state/proposed/`.
A tick left on `--watch` would have re-run a demo review every day,
correctly, on nobody's request. Opt-in was not violated: the fixture
opts in, because opting in is the thing it exists to show.

So real work goes in `tasks/` and that is the default; `examples/` is
reachable only by naming it. `tasks/T-0010` is the first file in it —
the client-side slash command latching the busy dot, issue #343.

What generalises: **a safety rule and a default are not the same
control, and the rule cannot cover for the default.** The rule said
"only tasks that asked for it". The default said "and here is a
directory of tasks that asked for it, as a demonstration". A
demonstration of a feature is indistinguishable from a use of it to
everything downstream of the file, which is why the two belong in
different directories rather than behind one more field.

The same sentence is worth carrying to the product: a fixtures folder
shipped next to a jobs folder is a loaded gun the moment anything scans
for work, and `verify:` in §3.5 is what it is loaded with.

The review added the half I had missed, and it is the same shape one
turn further out: **every tick in the sweep named its directory with
`--tasks`, so nothing exercised the default at all.** A suite that
always passes the argument cannot see a bad default, which is why the
bad default survived twelve rounds of it. Two checks now — the ids a
no-argument tick decides about are exactly the ids in `tasks/`, and no
fixture id is among them — and they fail by name rather than by
arithmetic.

Writing those down turned up a smaller version of the same thing.
"The sweep is *N* checks" had been a number in prose for three
findings running, and the two assertions at the bottom — no worktree
left, no lock left — printed no verdict line, so the same suite could
honestly be called 85 or 87 depending on whether you counted them.
They have labels now and the sweep prints `checks: 89 ok, 0 failed, 1
skipped`. A count nobody can run is the check-count version of rule 5
in §3.6: it does not distinguish *measured* from *remembered*.

---

### F-45 · The first real run, and what it did not prove

*2026-09-12. Sixteen review rounds, and then a task.*

`tasks/T-0010` went through a scheduler tick with nobody watching:
surface created and disarmed, prompt assembled from charter plus task
plus context, `claude -p` with `--permission-mode auto`, channels
harvested, evidence read out of the tree, verifier run in a clean
checkout of the branch tip. Three minutes fifty-two seconds, `done`,
one commit, diff inside `touches:`. The commit is on `main` now as
part of #354, authored by `fixer (via claude) <fixer@bots.invalid>`
because rewriting that would have erased the only interesting thing
about its provenance.

Then the part that matters more than the verdict: **`done` is a claim,
so it was checked against something else.** The narrow verifier the
task declared is deliberately narrower than the crate (F-1), so it
cannot see a regression elsewhere. Independently: 551 crate tests
green, clippy clean on the changed file, and the diff read line by
line. It held.

The work was good in a way a verifier cannot measure. The rule was
implemented on the two answer shapes the task quoted and not one step
further; the guard arm was placed above the `EntryKind::User` arm
rather than inside it; `last_user_ts` was left alone; every test drove
the parser from the captured constants rather than from lines written
to match the prose. And the commit message noted, unprompted, that a
pending background agent still forces `Busy` afterwards — an
interaction with the test from #299 that nothing in the task
mentioned. It had gone and read the neighbours.

**What this does not prove, and it is most of the list.**

*That the contract is right.* One task, written by the same person who
wrote the runner, in a file the runner's author knows the parser of.
The interesting failure is a task written by somebody who has not read
`bot-run.sh`.

*That the loop is unattended.* The scheduler dispatched it; a human
wrote the task, captured the transcript lines the task quotes, took it
through two review rounds, and then reviewed the output. The agent's
turn was unattended. The loop was not.

*That the verifier verifies.* Criterion 2 wants a run that claimed
success and was wrong. This one claimed success and was right, which
is the outcome that teaches least. Twenty stubs lie on purpose and the
runner catches all twenty; no real agent has lied yet.

*That it saves time.* Writing the task, capturing the evidence and
reviewing the diff cost more than writing the patch would have. That
is the honest shape of a first run and it is not an argument either
way — the question is whether the second and third cost less, and
there is no data.

What generalises is smaller than the event: **the human half of the
contract is the half that decides.** #343 insisted the discriminator be
checked against real captured transcript lines. Doing that ruled out
`isMeta` and turned one answer shape into two, which are precisely the
two things a bot handed the issue text alone would have got wrong.
The agent wrote all the code and none of the decisions.

---

### F-46 · The suite refused to run because the lab had been used

*2026-09-12, minutes after F-45.*

T-0010 finished, wrote `status: done` into `.state/tasks/T-0010.md`,
and the next `sweep.sh` refused to start:

    sweep: refusing to run - these already exist and this script
    force-deletes every one of them:

      live task T-0010 (.../.state/tasks/T-0010.md)

The refusal is right in general and wrong here. It exists because the
sweep dispatches into this control plane, and a run still in flight is
dispatching into it too — the old behaviour cleared every live task on
its way past, which took another run's file out from under it and
ended that run with no status at all. What it could not tell was *in
flight* from *finished*. A live task carries `status:`, and `done`,
`blocked` and `needs-review` are verdicts: a record, not a collision.
`dispatched` still refuses, deliberately, because a run that died
mid-flight left exactly that and wants a human.

The decision is a function rather than an inline `case`, for the
reason F-44 gave one turn earlier: the preflight runs before anything
in the file can assert about it, so a check has to be able to reach
the decision. Five checks call `live_task_verdict`; the preflight
calls the same thing.

**The second half was worse, and it is why this has its own finding.**
The way out of that refusal was to delete the live task. There was no
command for it — nothing removed a record and nothing offered to — so
the documented route was `rm -f` under `.state/`, which is the
directory whose entire purpose is being the record. A recovery
procedure that reads *delete the evidence of the only real run the
loop has completed* will eventually be followed by somebody in a
hurry. And the pressure is in the wrong direction: the suite is what
tells you the loop still holds, so anything that makes the suite
unrunnable after real work gets cleared out of the way rather than
understood.

Hence `run/bot-forget.sh`: removes the live task, optionally the
surface, refuses on `dispatched` without `--force`, refuses from
inside a run the way `bot-approve.sh` does, removes a surface only on
a `.git/bot-surface` marker that names *that* task, and appends
`forgotten` to the board. Nine checks, and they are about what it
refuses rather than what it removes — including a directory whose
marker names a different task, because `rm -rf` on the strength of a
task file's own field is how a tool like this takes somebody's
checkout with it.

What generalises: **a regression suite and the thing it tests cannot
share a mutable directory without one of them eventually being
sacrificed.** Here they share `.state/`, and the suite won on the
strength of being easier to run. Giving real work its own control
plane would be the deeper fix — `--state` already takes a path, so it
is a default and a paragraph away — and it is the same sentence as
F-44 one level down: a fixture directory next to a jobs directory,
except this time the fixtures and the jobs are *runs*.

---

### F-47 · A status is where the run got to, not whether anybody is in there

*2026-09-12, the review of F-46's own fix.*

`bot-forget.sh` gated on the status: `done`, `blocked` and
`needs-review` meant finished, so the record could go. The review
pointed at the runner and asked what order it does things in.

    set_status "$STATUS"        # done
    board "handoff" "$STATUS"
    ...
    drop_surface || CLEAN_FAILED=...
    if [ -n "$CLEAN_FAILED" ]; then
        STATUS="needs-review"
        set_status "$STATUS"    # and the handoff's frontmatter too

A terminal status is on disk *while the runner still has work to do*.
Delete the live task in that window and the run's last `set_field`
fails on a missing file, and the run ends with no status at all —
which is **exactly** the bug `sweep.sh`'s preflight was built around,
one commit after I fixed it there. I had written the rule into the
suite and then walked back into it in the tool the suite made
necessary.

The status could never have answered it. It records where the run
*got to*; the question was whether a process is still in there, and
nothing on disk said. So the runner now writes
`.state/running/<id>.<run-token>` holding its pid, created where the
task becomes `dispatched` and removed in the exit trap — so it
survives a `die`, a refusal and a Ctrl-C, and what it leaves behind on
a `kill -9` is a stale marker that anything reading it tests with
`kill -0`. A file is a claim; a process is a fact. The lock protocol
had already learned that (F-38); nothing else had.

Two gates now, because there are two questions: *is anybody in there*
(the marker) and *did the run reach a verdict* (the status).

**And the same review found the decision stated twice.** `sweep.sh`
treated a missing status as unsafe; `bot-forget.sh`, written a commit
later, let it through and deleted the file. One rule in two places is
two rules, and the reader picks — so `run/live-task.sh` is now the one
definition, text-first for the reason F-43 gave, and both callers
source it. That makes three: `approval.sh` for "approved",
`memory.sh` for "remembered", `live-task.sh` for "finished". Every one
of them exists because two readers of the same file disagreed in
production.

**A surface turned out to be three things.** `--surface` removed the
clone and stopped. The runner's `drop_surface` removes the sibling
`-verify` checkout first, then the clone, then deletes
`refs/bot-base/<id>` from the project — and the order is load-bearing,
so a removal that refuses still leaves the pin pointing at what the
surface was cut from. Retiring a surface without the pin leaks a ref
that holds the base objects alive for ever, and without the verify
checkout leaves a directory nothing will ever clean up. Mirrored now,
order included.

What generalises: **when you write a second tool that acts on another
tool's file, read what the first one does to that file and in what
order — not what its field names suggest.** `status: done` reads like
a fact about a finished run. It is a field one process writes twice.

---

### F-48 · Three EXIT traps in sequence, and the last one dropped what the first one promised

*2026-09-12, found by a check written for something else.*

F-47's fix added two lines to the sweep's leftover report — no run
marker should outlive its run, no task snapshot should outlive its
dispatch. Both went red immediately: **23 markers and 15 snapshots on
disk.**

The runner installs the EXIT trap three times:

    trap on_exit EXIT              # locks, snapshot, marker
    ...
    trap rollback_dispatch EXIT    # the half-dispatch window
    ...
    trap release_locks EXIT        # from the agent run onward

Each one replaces the last, and the last one drops everything except
the locks. So from the dispatch onward — which is every run that gets
as far as calling an agent — the private task snapshot and the run
marker were never removed at all. `rollback_dispatch` had the same
hole on its failure path.

The comment on `on_exit` is the part worth reading twice:

> The private copy of the task goes with the locks: it is this
> invocation's and nobody else may read it, so leaving it behind would
> turn `$STATE/tmp` into a pile of half-dispatched tasks that look
> like records.

That is exactly what had happened, in the directory the comment names,
while the comment sat above a function the later traps had taken out
of the exit path. The rule was written down, was correct, and was not
running.

Both later paths go through `on_exit` now. And because a killed run
cannot run any trap at all, a dispatch also clears *its own task's*
dead markers and snapshots — pid from the marker, pid from the run
token in the snapshot's name, `kill -0` either way. Only its own id:
another task's leftovers are not this run's to judge, which is the
same line the sweep's preflight draws.

What generalises, and it is not "test your cleanup": **a trap is a
variable, not a declaration.** `trap X EXIT` reads like a statement
about the program and behaves like an assignment — the last write
wins, from anywhere, including from a function three hundred lines
away. Every one of the three was locally correct; the bug was in the
sequence, which no single reading of any one of them shows. The two
leftover checks are the only thing that could have found it, and they
existed for eleven minutes before they did.

---

### F-49 · Three ways to read a file wrong, all in the safety code

*2026-09-12, the second round on F-47's fix.*

Every one of these was in the part of the code whose job is deciding
whether something may be deleted.

**A silent failure in front of a destructive step.** `--surface`
deleted `refs/bot-base/<id>` with `update-ref -d ... 2>/dev/null &&`,
and then deleted the live task regardless. If the ref was locked, the
pin survived, the record naming it did not, and no later invocation
could retry - the permanent leak this path exists to close, created by
the path itself. The runner's `drop_surface` can afford `|| true` on
the same call because its live task survives and the next run tries
again; this one cannot, so every step is fatal now. Rule 5 of the
threat model with a `git` command in it: **a removal that could not be
attempted is not a removal.**

The same reordering applies to the board row. It was appended *after*
the live task was removed, so a board that could not be written left
no trace of who removed the record. The mark goes down first now, and
a failed append stops the removal.

**A parser laxer than the runner's.** `live_task_status` was
`sed -n 's/^status:...' | head -n1` over the whole document, so a task
with no frontmatter status and a line starting `status: done` anywhere
in its prose - a findings list, a quoted example - read as a finished
record, which is the answer that permits deletion. Frontmatter only
now, counting fences, which is what the runner always did.

Then the grep for the shape found two more, neither reported.
`drop_sweep_tasks` read `id:` the same lax way *and deletes on it*, so
a file whose prose quoted `id: T-0001` was one of the sweep's own. And
`bot-run.sh` read the live status that way to decide whether a task
was in flight, finished, or free to dispatch. Three sites, one shape,
and only one of them was in the file the review pointed at.

**A record format that splits on spaces.** `live_task_running` printed
`<path> <pid> alive|gone` and both callers parsed it with
`read -r mark pid state`. One space in the state directory -
`C:/Users/Some Name/...`, which is most of Windows - shifts every
field, and no caller ever removes a dead marker again. Silently: the
loop runs, the comparison fails, nothing happens. The path is last
now, because the last field of a `read` absorbs the remainder.

And one more leftover check, which is the round's own lesson turned
into something that runs: **no `refs/bot-base/*` may survive a
sweep.** It went red immediately - the `.git`-hijack fixture cleans up
by hand, because `cleanup` refuses a surface whose marker the fixture
deliberately destroyed, and the hand-rolled version forgot the pin.
The same defect the review had just found in `--surface`, in the file
that tests for it, found by the check written for the other one.

What generalises: **the code that decides whether to delete something
is read by more people than it is written by, and every one of these
was a reading error in a line that looked right.** A `sed` that gets
the common case, a `read` that works until a path has a space, an
`&&` that swallows the answer. None of the three would show up in a
run that went well, which is the only kind of run anybody watches.

Sweep is 118.

---

### F-50 · The marker arrived after the damage it was meant to prevent

*2026-09-13, the third round on the same PR.*

F-47 put a run marker down where the task becomes `dispatched`. The
review read the lines above that and listed what happens first:

    take_lock dispatch.lock        # the claim
    ...create the surface
    ...write refs/bot-base/<id>
    ...on --reset, discard the old live task
    marker                         # <- F-47 put it here
    set_status dispatched

Every destructive step of a claim ran with the *old* record still
visible — a terminal status on it and no marker anywhere — which is
exactly the combination `bot-forget.sh` accepts as "finished". A forget
landing in that window would pass both gates and delete the new
surface out from under the `--reset` that was building it, because the
new surface has the same path and the same marker text. The marker is
taken the moment the claim is held now, and `on_exit` removes it on
every path out, a refusal included.

**That still left a check-then-act race**, and the review offered the
fix: `bot-forget` holds `dispatch.lock` across every check and every
removal. It is a client of the runner's lock protocol rather than a
copy — owner file named `owner.<pid>-...`, which is how `take_lock`
already tells a live holder from a dead one — and it never breaks
anybody else's lock: it waits briefly, then refuses and names the
holder. F-38 again, for the third time in this lab: two readers of one
resource who each check before acting, and the answer is always that
one of them has to hold something.

**The sweep made the same mistake one file over.** Its preflight
accepted a live task with a verdict as a record and started the suite
beside it — while that run could still be in its cleanup. And the
leftover checks written last round counted *every* marker, pin and
`bot-*` surface on disk, so a real run that ended `blocked` and kept its
evidence — which is what `blocked` is for — would fail the suite. Both
questions are one function now, `live_task_settled` → `active |
settled | unfinished`, with the process asked first, and the leftover
checks vouch only for what this sweep made.

The comment on `live_task_verdict` had to change too. It said a
`record` is something "nothing is going to rewrite out from under"
you, and two callers believed it. It is a record of a verdict and
nothing more.

**And a plain-folder project keeps its pin somewhere else.** The plane
is stamped with the folder, but the surface is cut from
`$STATE/snapshot.git` and the pin lives there — the runner's
`ORIGIN_REPO` rule. `--surface` deleted the clone, then rejected the
folder as not a repository and stopped: the record and the pin both
survived, minus the one part that showed what they were for. The
repository is resolved and validated *before* anything is removed now,
by the runner's own precedence.

What generalises is the order-of-operations version of F-43: **a check
that can refuse has to run before the first thing it would have
stopped.** The marker was checked after the claim had already done
its damage; the pin repository was checked after the clone was already
gone. Both were correct checks in the wrong place, and neither place
looks wrong until you read the lines above it.

The next review found the rule broken twice more in the fixes for it.
`bot-forget`'s board mark — the mark that is supposed to stop every
removal when it cannot be written — sat after `--surface` and before the
live task, so an unwritable board said "nothing was removed" with the
verify checkout, the clone and the pin already gone. It goes down before
the first removal now, and a refusal after it leaves a
`forget-stopped` row so the board does not claim a record that is still
there. And the dispatch rollback appended to the board before calling
`on_exit`, under `set -e`: if the failure being rolled back was a full
state directory, that append ended the trap and leaked exactly the lock,
snapshot and marker the trap exists to release. A cleanup is only
guaranteed if nothing before it can stop it.

A Codex review then found three more, and each came with a failing
sweep case before its fix. `bot-forget` checked the verify checkout's
ownership, removed it, and only then read the surface marker that
could refuse — so an old record whose path a colliding branch leaf had
reused lost another task's verify checkout. Every ownership check now
runs before the board mark. The pin was looked up with `rev-parse
--verify` and the delete skipped on any failure, but a broken ref
answers that exactly like a missing one; the delete is now attempted
unconditionally, because `update-ref -d` already tells absent (success)
from broken (failure). And `live-task.sh` had brought back the early
`exit` that the runner's own parser documents as a SIGPIPE: a task body
larger than a pipe buffer killed its caller with 141 under `pipefail`.
The removals themselves also answer through `stop` now, rather than
letting `set -e` leave `forgotten` as the board's last word. That last
one has no sweep case; a removal that fails halfway is hard to stage
portably.

A second Codex pass confirmed those four and found the same classes
elsewhere. A marker whose pid could not be read came back `gone` — a
failed read standing in for a finished process — and is `unknown` now,
which every caller treats as possibly alive; only `--force` clears one.
The runner's own `drop_surface`, the function `bot-forget` was written to
mirror, still alternated check and removal, and since every caller runs
it under `||` a verify checkout that would not go was reported as a clean
removal. `on_exit` could stop at a snapshot that would not go before the
marker went. And the sweep's own pin checks used `rev-parse --verify`, so
"no base pins left" passed with a broken pin on disk; they assert
absence through `update-ref --stdin` now, and a check proves the helper
sees a broken ref.

The third pass found one: `bot-forget` looked for the pin in
`snapshot.git` whenever the plane had one, which is not the runner's
rule — the runner uses it only for `base: folder`. A plane that once ran
a folder task would have a later task's pin looked for in the wrong
repository, found absent, and the record deleted with the real pin left
behind. The runner now records `origin_repo:` in the live task, and
`bot-forget` reads it; older records fall back to the task's `base:`,
not to what the directory happens to contain. Provenance is recorded,
not inferred.

The fourth pass showed the fallback for older records was inference
too: the runner's rule is a probe of the project, not the task's
`base:`, and a git repository with a branch called `folder` is cut from
the project. The fallback now asks the runner's own question, and where
the answer could have changed since — the project is a repository now
and the plane also holds a snapshot — it refuses and asks for `--repo`
instead of picking one of two repositories to delete a ref from.

The fifth pass found the other half of that probe: a failed probe was
read as "a plain folder", but a project that moved, or one git refuses
to read, fails it too. The snapshot is chosen only when the stamped
folder is there and has no repository in it; anything else refuses and
asks for `--repo`. A failed lookup is not a folder any more than it is a
dead process.

An Opus review then showed that fallback was still wrong for the one
case it was written for: the runner stamps the *git dir*,
`<project>/.git`, so "a folder with no `.git` in it" is true of a
repository git refuses to read — and the sweep's fixtures stamped the
work tree, a shape the runner never writes. Three fallbacks in a row had
reconstructed provenance from the disk and each was wrong in a new case. The rule now
is only what is certain: the runner cuts from the stamp or from
`snapshot.git`, and the snapshot exists only if a folder was imported.
No snapshot, the stamp; a snapshot, refuse and ask for `--repo`. The
fixtures stamp what the runner stamps.

The same review found two more. A removal a Windows file lock stops
halfway had already deleted `.git` — `rm -rf` goes in name order — so
every retry refused the tree as not a bot surface; removals now take the
ownership proof last and keep it if anything else failed. And a task
whose frontmatter was never closed still runs, but `set_field` never
inserts `worktree:` into it, so `--surface` skipped its block and deleted
the only record naming the clone and the pin. The runner reads its
fields back and rolls back if they did not take; `--surface` refuses a
record without `worktree:`.

A second Opus pass reproduced the case that fix still missed: when the
*directory* is what is locked — on Windows, any process whose working
directory is the tree, like a terminal opened on a blocked surface —
every entry goes and the folder does not, so the final `rm -rf` took the
proof and then failed. The folder is now renamed away and back before
the proof goes; that rename is refused in exactly that case. The sweep
stages it for real (a `cmd.exe` sitting in the tree on Windows, an
unwritable subdirectory elsewhere) and failed against the previous
helper with "proof gone".

A third Opus pass found nothing significant, and two small edges of that
rename: a probe name that already exists makes `mv` move the tree inside
it, and a rename back that fails leaves the tree under the probe name,
where a retry of `bot-forget` found nothing, skipped both removals and
said `forgotten`. The probe name is checked first, the rename back is
retried and named if it still fails, and `bot-forget` refuses a surface
with a stranded removal beside it.

A Copilot review on top found four more. `remove_proof_last` walked
through links: a surface, or its `.git`, that is a symlink had the
target's entries removed before the proof was reached; it refuses a
linked root or proof directory now. The rename back could move the tree
*into* a directory recreated meanwhile; the destination is re-checked
before every attempt. `bot-forget` read a file or symlink where the
surface should be as an absent surface and reported success; it refuses
a wrong-type path. And the sweep's lock fixtures used `mkdir -p` on the
real control plane's `dispatch.lock` and removed it afterwards, so a
real claim overlapping the suite could lose its owner file; both take
the lock the way a runner does and give back only what they put there,
skipping when it is really held.

That last one was a symptom. The sweep ran in `.state` beside real work,
and every preflight, id scope and lock courtesy added over five rounds
was a patch on sharing a control plane with runs it cannot see start. It
has its own now, `.state-sweep`, named with `--state` on every runner
invocation; only the repository's branch and ref namespace is still
shared, and that stays limited to the sweep's listed names.

Sweep is 155, with the symlink case a skip on a Windows shell that
cannot make links.

### F-51 · The shim was not dead code; it was the proof

*2026-09-13, #348 step two.*

F-32's note on the verifier rename said exporting both names would be
"green today, dead code the day it lands", and left the rename for
later. When it came to it, the two-step version was the only one that
could be checked at all. `verify:` runs in a checkout of `base:`, so a
branch never tests its own verifier against its own runner — it tests
the new runner against the old script. Step one (#353) exported both
names and the sweep was green against the base script asking for the
old ones. Step two removes the old exports, and the sweep is green
because base now carries the script that reads only the new ones.

A shim that lives for exactly one merge is not compatibility; it is
the intermediate state made verifiable. What generalises: **when the
checker runs at a different commit from the change, a rename has to
pass through a state both commits accept** — and that state is worth
one merge, not a note saying it cannot be proven.

---

### F-52 · The longer window was the one after the run

*2026-09-14, graduation criterion 3.*

F-25 closed the window between dispatch and handoff: a base that moves
while the agent works is read again, and the work is replayed and
re-verified there. It left the other window open and said so. A
finished branch is pushed, its surface is removed, and then it waits -
for a review, for a merge, for somebody's afternoon - and the base
keeps moving the whole time. Every day of that wait the handoff's
"verified against *X*" describes a commit further from the tip, and
nothing in the loop looked at it again. In a repository with one
person merging, the wait is the longer of the two windows by a margin
that makes the first one look like a rounding error.

The tick now asks the question, because the tick is the thing that
reads the world on a schedule. For a `done` task with no `every:` it
reads three facts from git: is the branch still in the project, has
the base ref left the recorded `base_sha`, and is the branch already
part of that ref. Landed branches and branches on a base that has not
moved are nothing to do. The rest are `stale-base`, and the tick
dispatches the runner with `--recheck`.

**What `--recheck` is, is the ordinary run with the agent's turn taken
out.** That was the design decision, and it was made against the
alternative of a separate script. A recheck needs a surface, the
evidence read, the verifier in a clean checkout, the rebase step with
its four verdicts, the push, the handoff and the cleanup - which is
every step the runner already has, each of them carrying a finding
that was paid for once. A second script doing "just the rebase" would
have been a second copy of F-25's verdict table, free to disagree with
the first (the argument of F-43, one level up). So the runner takes a
flag: the surface is cut at the pushed tip instead of the base, the
recorded `base_sha` is the commit to replay from, the agent is skipped,
and everything from Evidence on runs unchanged. The rebase step does
not know it is in a recheck.

Three things had to be different, and each is small.

**The push is not a fast-forward.** An ordinary run pushes a branch the
project has never seen. A recheck moves one it has, so the push carries
`--force-with-lease` pinned to the tip the run started from: if
anything else moved the branch meanwhile, the push is refused and the
run says so, rather than overwriting somebody's work with a replay of
an older version of it.

**The record is kept.** An ordinary run copies the task definition over
the live task. A recheck must not - the live task *is* the record of
the run that finished, and its `base_sha` is the only thing that says
what the branch stands on. The recheck writes the new surface path into
it and, only when the branch moves, the new `base_sha`. That last
sentence is what stops the tick from rechecking the same branch on
every tick: the sweep checks that the record follows the branch,
because without it the loop would replay forever and every replay
would be green.

**A recheck is `dispatched` while it runs.** It was tempting to leave
the status at `done`, since that is what is true of the work. But the
overlap scan reads `dispatched` plus a worktree on disk as "somebody is
editing these files", and a replay is exactly that. So a recheck claims
like any run, and a recheck killed mid-flight reads as a stale dispatch
- with its branch in the project untouched, because nothing moves
before the push.

What it refuses, before the lock and at no cost: a branch already in
its base (it landed - retire the record with `bot-forget.sh`), a base
that has not moved, a report, a folder project (its result is a patch,
not a branch), and a `--reset` on the same command line, since one
keeps the record and the other discards it. And a definition that
disagrees with the record about `branch:`, `base:` or the repository
(Codex, reviewing this): the record names the branch that finished,
the definition is a file anybody may have edited since, and a recheck
that took the branch from the definition would replay whatever it now
names and force-push onto it - with a lease taken from that same
branch, so the lease would hold. The record is the identity.

A base ref that no longer resolves is the one case the tick does not
dispatch. The runner resolves the base before it knows it is
rechecking and exits there with nothing written, so a tick that
dispatched it would hold a `--max` slot on every tick and record
nothing (the same review). It is reported as `base-gone` instead: the
branch verified against a commit that has no name any more, and where
it lands is somebody's decision.

**And the first dry run found three stale branches in the real control
plane.** T-0010, T-0011 and T-0012 verified against `labs/agent-bots`
and their work landed on `main`, not on that ref, so by the tick's
reading they are waiting on a base that moved - which is true, and
also not what anybody would want replayed. That is not a defect in the
question; it is the answer to a question nobody had asked in a month.
Those records want retiring, and `bot-forget.sh` is what that is for.
The rule the runner cannot know is "landed somewhere other than its
base", and it is a rule for a person.

What generalises: **a claim about a commit ages at the rate its base
moves, and a loop that only checks at the moment of writing has
verified the past tense.** The re-read costs a verifier run per moved
base; the alternative is a handoff that is quietly less true every
morning.
