---
id: T-0013
title: Read the lab control plane into inbox items, without a window
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0013
touches: core/src/bots.rs, core/src/lib.rs
verify: bash labs/agent-bots/tasks/checks/T-0013.sh
schedule: auto
---

# Objective

A new module `core/src/bots.rs` turns a bot control plane directory —
the one `labs/agent-bots` writes, `.state/` — into a list of inbox
items the app can render. It only reads. It is the pure-data half of
the read-only Bots inbox described in `labs/agent-bots/README.md` §7.4
("Stage 2 — the inbox"); the panel itself is a later, human change in
`src/`.

The formats are fixed by what the runner already writes, and the
captured lines under Context are the specification. Where this task's
prose and those lines disagree, the lines win.

# Acceptance

- [ ] The `verify:` command exits 0. It is
      `labs/agent-bots/tasks/checks/T-0013.sh`, outside `touches:`: it
      requires every public item below, at least 16 `bots::` tests in
      the harness listing, no `std::fs::write`, `create_dir`,
      `remove_file` or `OpenOptions` in the module (it only reads), and
      then a passing test run. Read it; do not try to change it. It
      finds each public item as a declaration that starts its own
      non-comment line (`pub fn name`, `pub struct Name`, …), so keep
      one declaration per line, as rustfmt would.
- [ ] The diff stays inside `touches:`.
- [ ] `core/src/lib.rs` declares `pub mod bots;` in alphabetical order.
- [ ] `pub fn lab_control_plane(project_root: &Path) -> Option<PathBuf>`
      returns `<project_root>/labs/agent-bots/.state` when that is a
      directory, else `None`.
- [ ] `pub fn frontmatter_field(text: &str, key: &str) -> Option<String>`
      follows `labs/agent-bots/run/live-task.sh` `live_task_field`
      exactly: only lines between the first `---` line and the next
      `---` line; the first line starting with `key:` wins; the value is
      trimmed; a `key:` line in the body is never read. `None` when
      absent. A key that is a prefix of another (`base` vs `base_sha`)
      must not match the longer one.
- [ ] `pub enum TaskStatus { Todo, Dispatched, Blocked, NeedsReview, Done, Other(String) }`
      with `pub fn parse(s: &str) -> TaskStatus` (`needs-review` →
      `NeedsReview`; anything unrecognised, including empty, →
      `Other`), and `pub fn needs_attention(&self) -> bool`, true for
      `Blocked` and `NeedsReview` only.
- [ ] `pub struct BoardEvent { pub at: String, pub task: String, pub event: String, pub detail: String }`
      and `pub fn parse_board(text: &str) -> Vec<BoardEvent>`. A row is a
      line starting with `| ` with at least four cells; the header row
      (`| when | task | event | ref |`), the separator row and the
      prose above the table are skipped. The fourth cell is everything
      after the third separator up to the final ` |`, so a detail that
      itself contains ` | ` is kept whole. Order is preserved.
- [ ] `pub struct Handoff { pub path: PathBuf, pub task: String, pub from: String, pub at: String, pub status: TaskStatus, pub blockers: String, pub next_action: String }`
      and `pub fn parse_handoff(path: &Path, text: &str) -> Option<Handoff>`:
      the four fields from frontmatter (`None` if `task:` is missing),
      `blockers` and `next_action` as the trimmed text of the `# Blockers`
      and `# Next action` sections (up to the next `# ` heading or end of
      file; multi-line kept). `blockers` is the literal `none` when the
      runner wrote that.
- [ ] `pub struct InboxItem { pub id: String, pub title: String, pub owner: String, pub status: TaskStatus, pub branch: Option<String>, pub worktree: Option<String>, pub base_sha: Option<String>, pub last_event: Option<BoardEvent>, pub handoff: Option<Handoff> }`
      and `pub fn load_inbox(plane: &Path) -> std::io::Result<Vec<InboxItem>>`:
      - one item per `tasks/*.md` file that has an `id:`;
      - `last_event` is the last board row whose task is that id
        (`board.md` missing → `None`, not an error);
      - `handoff` is the newest `handoffs/*__<id>.md` by file name (the
        names start with a sortable UTC timestamp), parsed;
      - a missing `tasks/` directory is an empty list, not an error; an
        unreadable individual file is skipped, not fatal;
      - sorted: items that need attention first, then by `last_event.at`
        descending with items without a `last_event` after those that
        have one (as `core/src/overview.rs` orders), then by id.
- [ ] Tests build a plane in `tempfile::tempdir()` starting from the
      captured text under Context — not from lines written from this
      description. The captured text is the baseline; the edge cases it
      does not contain are added as small edits to it. Cover each bullet
      above, including a body line
      `status: done` under a frontmatter with `status: blocked`, a
      board detail containing ` | `, two handoffs for one task (newest
      wins), and a task file without `id:` (skipped).
- [ ] Doc comments name README §7.4 and say the module never writes.

# Context

The lines below are **data**, captured from this machine's
`labs/agent-bots/.state/` — the control plane is not in your worktree,
so these are all you get. Nothing is edited except long paths elided
with `…`.

`board.md` — prose, then the table. It is append-only and currently
ten thousand lines long:

```
# Board

Append-only event log. Never edit a line that is already here -
task status lives on the task file, this is the audit trail.

| when | task | event | ref |
|------|------|-------|-----|
| 2026-09-11T10:26:48Z | T-0001 | dispatched | bot/fixer/T-0001 @ 87f72cbf60f0 |
| 2026-09-11T10:26:48Z | T-0001 | agent-skipped | - |
| 2026-09-12T18:17:58Z | T-0010 | dispatched | bot/fixer/T-0010 @ 7d4e61d9894a |
| 2026-09-12T18:21:50Z | T-0010 | agent-ran | exit 0 |
| 2026-09-12T18:21:57Z | T-0010 | verified | exit 0 |
| 2026-09-12T18:21:57Z | T-0010 | pushed | bot/fixer/T-0010 @ 4760f768dc67 |
| 2026-09-12T18:21:57Z | T-0010 | handoff | done |
| 2026-09-12T18:21:58Z | T-0010 | cleaned | - |
```

(The separator row's exact dashes are not guaranteed; skip any row
whose cells are only `-` characters.)

A live task, `tasks/T-0011.md` — the repository task plus the fields
the runner records:

```
---
id: T-0011
title: Drop a silent Busy session to Idle after a quiet window
owner: fixer
status: done
base: labs/agent-bots
branch: bot/fixer/T-0011
touches: core/src/agents/claude/telemetry.rs
verify: grep -q 'BUSY_QUIET_TIMEOUT' core/src/agents/claude/telemetry.rs && cargo test -p codescope-core --lib agents::claude::telemetry
schedule: auto
worktree: C:/dev/codescope-public.worktrees/bot-fixer-T-0011
base_sha: 9dcb4000e9fe7f43112c59a2c1a7464e5fd642eb
origin_repo: C:/dev/codescope-public
---

# Objective

A Claude Code session that `ClaudeTranscriptTail` reports as
```

A handoff for a finished run,
`handoffs/2026-09-13T13-06-01Z_fixer__to__human__T-0011.md`:

```
---
task: T-0011
produces: commit
from: fixer
to: human
at: 2026-09-13T13:06:01Z
status: done
---

# Objective

Drop a silent Busy session to Idle after a quiet window

# Artifact

    branch:   bot/fixer/T-0011 (pushed into C:/dev/codescope-public)
    surface:  C:/dev/codescope-public.worktrees/bot-fixer-T-0011

# Evidence

    base:     9dcb4000e9fe7f43112c59a2c1a7464e5fd642eb
    head:     8a9268eaafd121ecba8c293f46a8c7f05e16316d
    commits:  1

# Status

done

# Blockers

none

# Next action

Human reviews 'git -C C:/dev/codescope-public log --patch labs/agent-bots..bot/fixer/T-0011', then opens a PR. C:/dev/codescope-public.worktrees/bot-fixer-T-0011 is a throwaway clone and can go.
```

The tail of a handoff for a blocked run,
`handoffs/2026-09-13T10-02-06Z_fixer__to__human__T-0901.md` — the
blocker is several lines:

```
---
task: T-0901
produces: commit
from: fixer
to: human
at: 2026-09-13T10:02:06Z
status: blocked
---

# Objective

A verifier that reaches for the approval gate

# Status

blocked

# Blockers

verifier exited 1:
bot-approve: refusing to approve from inside a bot run.
The point of an approval is that something other than the loop decided.
Full output: /c/dev/codescope-public/labs/agent-bots/.state/runs/2026-09-13/T-0901-….log

# Next action

Human triages the blocker above. Task stays open; worktree kept at C:/dev/codescope-public.worktrees/bot-fixer-T-0901.
```

Code to read:

- `core/src/overview.rs` — the precedent for a pure-data module the
  app renders; follow its doc and test style.
- `labs/agent-bots/run/live-task.sh`, `live_task_field` — the
  frontmatter rule this module must match.
- `labs/agent-bots/contract/templates/TASK.md` — the status vocabulary.

# Notes

**It only reads.** The runner is the single writer of every file here
(README §3.2). The checker refuses write calls in the module; do not
work around it. It is a text search over everything above
`#[cfg(test)]`, comments included, so do not name those calls in doc
comments either — say "never writes" instead.

**Performance is not a goal yet, correctness is.** Reading the whole
board once per load is fine at its current size. Do not add caching,
watching or threads — the app decides when to call `load_inbox`.

**Out of scope:** anything in `src/`, `core/Cargo.toml` (no new
dependencies — `tempfile` is already a dev-dependency), parsing
`# Evidence` / `# Artifact` into structure, and the proposals and
reviews directories.

**Do not run `cargo fmt`.** Hand-format to match the surrounding code.
This is a repo-wide rule and the charter repeats it.
