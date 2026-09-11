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
| **Control** | outside git | the runner | board, tasks, handoffs, inbox, runs, per-bot memory |
| **Work** | worktrees + branches | one bot per branch | the actual code |

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

```
dispatch  ->  add_worktree(path, "bot/<owner>/<task>", base = origin/main)
              record the base SHA on the task file
work      ->  launch the agent in that worktree with a deterministic prompt
verify    ->  run the task's own verifier command on that tree, exit 0
              and the diff touches only the task's declared `touches:` globs
gate      ->  push + PR  (never straight to main - a hard invariant in code,
                          not an instruction in a prompt)
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

**Concurrency control without locks.** Each task declares `touches:`
globs. The runner refuses to dispatch two tasks with overlapping globs
from the same base. If it happens anyway: rebase, re-verify; if the
rebase fails, the task goes to the inbox for a human.

As a side effect, crash recovery is free. Nothing lives in memory — on
restart you read the task files and re-derive each phase from git.

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
- **Secrets.** Gate before the push, not after. A key in a pushed
  commit has already been seen.

---

## 4. What the prototype covers

| | |
|---|---|
| Covered | the file contract; one bot; one task; worktree create; agent run; verifier; evidence capture; handoff write; append-only board |
| **Not** covered | scheduling/routines, multi-bot handoff, the approval inbox, pushing, opening PRs, any GPUI surface |

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

`BOT_AGENT_CMD` and `BOT_AGENT_ARGS` select the agent; the defaults
target Claude Code headless mode. Check them against your installed CLI
version first. See the script header for the full flag list.

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

Until then it stays in `labs/`.

---

## 7. Findings

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
