# CodeScope — Session Handoff

> Keep this file honest and tight. Updated at the end of every session.
> It is the first thing a fresh Claude Code session should read after `CLAUDE.md`.
> **Intent:** cursor + last 1–2 sessions in depth, everything else a one-liner.
> Old detail lives in `git log` — don't duplicate it here.

> ### House rule for the Rust port
>
> **The Rust port (`codescope-rs/`) is a 1:1 functional port of the C# CodeScope build (`src/CodeScope.App/`, `src/CodeScope.Core/`, `src/CodeScope.AgentCli/`).** Before implementing any feature on the Rust side, **read the equivalent C# code first** and mirror its behavior, button labels, dialogs, data shapes, and persistence layout. Functional parity is the goal — we are not redesigning. If a `HANDOFF.md` entry, README line, or "next entry point" disagrees with what the C# code actually does, the C# code wins; update the doc. Genuine platform-forced deviations (gpui vs WPF idiom) get a one-line comment and an entry in `docs/DECISIONS.md`. "Cleaner" or "more elegant" is not a reason on its own. (Reinforced in session 33 after PR #56 invented a non-existent UX.)

**Last updated:** 2026-05-10 (session 37 — status bar wired + sidebar parity sweep)
**Branch:** `main` (PRs #98–#101 merged; #102 + #103 in flight pending review)
**Head:** main, in sync with origin
**Release:** `v0.2.5` shipped in session 36 — no new release this run
**Build status:** ✅ C# untouched. Rust workspace builds clean.
Tests: `codescope-core` (121 — claude_discovery added 19,
git status_label added 8, status-bar formatters added 14) +
`codescope-terminal` mouse-encoder (9) + sidebar-bin tests (29) —
**159 total**, all passing.
**Uncommitted work:** none on `main`. Open branches: `feat/sidebar-rebase-onto-default` (#102), `feat/sidebar-project-collapse` (#103).
**Open issues:** none on GitHub.

### Session 37 — status bar integration + sidebar parity sweep

Started with the cursor from session 36 ("wire register / unregister
into spawn / close, then integrating PR for the bar") and pivoted
to a broader autonomous sidebar-parity sweep after the user said
"ok misschien kan je hierna gaan zorgen dat de sidebar functioneel
het zelfde wordt als de c# versie. dit mag je helemaal autonoom
doen, zelf pr'en wachten op de review adressen en mergen."

**PR #98 — Claude session adoption + telemetry register/unregister.**
New `core/claude_discovery.rs` mirrors C# `ClaudeSessionDiscovery`
(poll-only, no `notify` dep). `Tab` gains `spawned_at`,
`adopted_session_id`, `fired_session_ids` — the latter mirrors
`WatchHandle._fired` so `/clear` rotations swap cleanly without
leaking stale tails. `start_claude_discovery_poll` stays armed
for the tab's full lifetime; on a new id it unregisters the
previous tail before registering the new one. Helper
`is_claude_auto_type` ported into core for unit testing the
agent-id heuristic.

**PR #99 — wire status bar to telemetry / git / notifications.**
Brings `render_status_bar` to functional parity with C#
`StatusBarView`. Left cluster: session dot (`signal_ok` /
`signal_warn` from new `theme::signal_*` helpers, exact
DesignTokens.xaml hex) + branch from `git_status_for` + `+N −N`
numstat (white added, dim removed, "changes" fallback) + remote
`↑/↓`. Hidden entirely on tabs without a git context. Right
cluster: short model name (via new `core::model_display_name`),
`tokens k tok pct%` (via new `format_tokens` / `format_context_pct`
that match C#'s `0.#` rule), `N turns`, last-turn duration,
`N busy · M idle` agent rollup, optional `N groups`, workspace
summary, tab counter, and a clickable bell button with a 4 px
unread dot. Segments are built as `Vec<AnyElement>` and
separators interspersed only between visible items — no stray
rules when an optional segment is `None`.

**PR #100 — worktree row right-aligned status slug.** New
`core::git::worktree_status_label` ports C#
`WorktreeViewModel.StatusLabel`: `chg` / `↑N ↓N` / `idle` (and
empty when no upstream + clean, deviating slightly from C# to
avoid a fresh standalone branch claiming sync state). Slug
renders right-aligned (sans 10 pt; switching the whole sidebar
to mono / sans is a follow-up). 8 new tests cover the slug
rules.

**PR #101 — `(no worktrees)` placeholder.** Dim row indented to
align with the worktree children, shown when a project has no
non-primary worktrees. Stable gpui id keyed off the project id
so it doesn't confuse element reuse when the user adds the
project's first worktree.

**PRs in flight (review pending):**

- **PR #102 — Rebase onto origin/<default>… menu row.**
  `core::git::rebase_onto`, worktree menu row hidden when on
  default branch, background-spawn run with toast surface.
- **PR #103 — Project chevron collapse / expand.** ▸ / ▾ glyph
  + click toggle, in-memory `collapsed_projects: HashSet<String>`,
  worktree children + placeholder skipped via `continue` when
  collapsed.

**Lessons / patterns:**

- The `address-review` autonomous loop turned out cheap: under
  a minute round-trip per PR. Copilot consistently flags concrete
  things (segment/separator interspersing, hard-coded vs
  themed colors, doc-vs-impl drift on ported behaviour) — most
  reviews resolved in one fix-commit.
- C# parity claims are easy to over-state in docstrings.
  Several review threads landed on "you say 'mirrors X' but it
  diverges in <small detail>" — fix is to spell out *what*
  diverges + *why*, not silently hand-wave.
- `Box::leak` is a tempting shortcut for dynamic
  `&'static str` labels in gpui menus; it leaks on every
  re-render. Better: build the row inline rather than going
  through helpers that need a static lifetime.

### Cursor — what's next

Two PRs are open and waiting on review (`#102` rebase, `#103`
project chevron). Once they merge the next sidebar parity work
falls into roughly four sizes:

1. **Tiny** — persist `collapsed_projects` to `layout.json` so
   expand state survives across launches. Builds on #103.
2. **Small** — port the worktree menu's "Copy branch" copy-PR-
   URL row once a PR detection model lands; today that needs a
   `gh` shell-out.
3. **Medium** — adopt the same mono / sans font split the C#
   sidebar uses (`Fig.Font.Mono` for branch labels and the
   status slug, `Fig.Font.Sans` for project names). Touches
   most sidebar render paths.
4. **Big — session-state tracking.** The biggest remaining gap
   per the parity audit. Live + closed history rows, session
   dots, history disclosure, soft-close + reopen flow. Depends
   on a Rust-side `SessionManager` that today doesn't exist
   (the Rust port treats every Tab as its own ad-hoc session).
   This is the structural work that unlocks busy-halo, agent
   rollup beyond Claude tabs, PR detection, etc.

The audit run earlier in the session lives in the conversation
transcript — full inventory at
`C:\Users\maui\.claude\projects\C--dev-codescope-public\<id>.jsonl`.

### Earlier sessions

### Session 36 — title bar merge + status bar / data-source foundations

Started from a `RefCell already borrowed` panic on title-bar
click (SendMessage re-entrancy via gpui's modal NC drag loop) and
ended with the four-PR foundation for a feature-complete status
bar.

**PR #92 — Chrome-style title bar.** Tabs merged into the same
40 px row as brand mark, sidebar toggle, and caption controls.
Caption controls absolute-positioned top-right so per-group tab
strips can span the same horizontal extent as the panes below
(divider alignment preserved). Rightmost group reserves
`CAPTION_CTRLS_W = 184 px` with `overflow_hidden` + opaque caption-
controls bg so tabs can't slide visibly under the buttons. Win32
PostMessageW everywhere (was SendMessageW — that's what caused
the original panic). Title-bar drag spots route through one
`AppShell::handle_titlebar_press` helper that time-discriminates
real human clicks (>10 ms apart) from the synthetic
`WM_NCLBUTTONDOWN` echo our own `start_drag` posts (gpui's
ClickState bumps click_count to 2 on the echo, which would
otherwise toggle maximize on every single click).
`DIVIDER_VISUAL_WIDTH = 1.0` constant — splitters / sidebar handle
are 1 px painted with a 6 px transparent absolute hit-overlay
extending into the adjacent panes for grabbing.

**PR #93 — status bar layout + workspace summary.** 32 px tall,
two-cluster shape mirroring C# `StatusBarView`. Active title left
(flex_grow + truncate), right cluster: `N worktrees · M dirty`
(via new `Sidebar::worktree_counts()`) + optional `N groups` (only
when >1) + tab counter. 1×14 vertical `SbSep` dividers between
items.

**PR #94 — per-worktree git status polling.** New
`core/git.rs::git_status(path)` returning `Option<GitStatus>` with
branch, numstat `(added, removed, has_changes)`, and ahead/behind
upstream. Early `git rev-parse --is-inside-work-tree` probe gates
the rest. `Sidebar::start_git_status_poll` runs every 5 s next to
the dirty-state poll. Accessor: `sidebar.git_status_for(&path) ->
Option<&GitStatus>`. `#[allow(dead_code)]` until the integrating
PR consumes it.

**PR #95 — Claude transcript tail.** `core/claude_telemetry.rs`
mirrors C# `ClaudeTelemetryService` / `ClaudeTranscriptParser` /
`ClaudeModelCatalog`. JSONL tail with stat-first incremental
read, per-instant DST-correct path through `process_new_lines`.
`turn_count` increments on assistant entries with usage (matches
C# `HasUsage`), not user prompts. `context_pct` clamped to
`[0.0, 1.0]`. Read errors preserve `last_pos` for retry. Adaptive
poll: 250 ms while any tail is Busy, 2 s while all idle, 30 s
when no tails registered (armed-only-when-needed). Accessor:
`app.telemetry_for(&session_id) -> Option<TelemetrySnapshot>`.
Register via `app.register_telemetry(session_id, working_dir)` —
returns early when neither USERPROFILE nor HOME is set.

**PR #96 — persistent notifications + popover.** New
`src/notifications.rs` (1:1 port of C# `NotificationService`).
Ring buffer of 50 entries, kinds = Generic / SessionWaiting /
SessionReady, popover render mirrors `BellPopup` XAML (360 wide,
max 420 tall, 14/10 padding, header / scrollable list / footer).
`format_hhmm` uses `FileTimeToSystemTime` →
`SystemTimeToTzSpecificLocalTime` for per-instant DST-correct
local time. Click on entry calls `activate_tab(group, idx)` if
the entry's session title matches an open tab. Popover
positioning currently snaps above the status bar via
`Edges{bottom: 38}`; integrating PR will swap for
`anchored().position(button_rect)` once the bell button exists.

**Build / test status after the four merges**

`cargo build --bin window` clean (3 warnings: `git_status_for`,
`register_telemetry`, `unregister_telemetry`, `telemetry_for` are
"never used" — deliberate, the integrating PR consumes them).
`cargo test --workspace` 112/112 passing.

### Cursor — what's next

The four data sources (`sidebar.git_status_for(path)`,
`app.telemetry_for(session_id)`, `app.notifications`) are ready
to be consumed by the status bar. The integrating PR should:

1. **Left cluster** — session-context dot (colour driven by
   `TelemetrySnapshot.state`: green = Idle, yellow = Busy /
   PendingToolUse) + branch name from `git_status_for`. Halo
   pulse when busy (animated via gpui timer entity) — optional,
   could ship without animation first and add it as a separate
   PR.
2. **Mid-left** — git dirty `+N −N` (white added, dim removed,
   fallback "changes" when has_changes && numstat zero) + remote
   `↑/↓` (`format!("↑{ahead}/↓{behind}")`).
3. **Right** — model name (icon + text), `tokens_used` + percent
   ("12345 tok 6.2%"), `turn_count` ("3 turns"), turn-duration
   clock-icon + `format_duration`, agent rollup
   `Σ busy / Σ idle` across all registered tails.
4. **Bell button** — small icon + 4 px accent unread dot when
   `notifications.has_unread()`. Click → `notifications.toggle()`.
   Replace the `Edges{bottom:38}` workaround in
   `render_notifications_popover` with
   `anchored().position(button_rect).anchor(BottomRight)` so the
   popover tracks the bell.

To wire telemetry data, callers also need to invoke
`app.register_telemetry(session_id, working_dir)` when a tab
spawns (and `unregister_telemetry` when it closes). That hook
isn't wired yet — currently `telemetry_tails` stays empty so the
poller idles at 30 s. Plumbing through `spawn_tab` /
`close_tab` is the prerequisite for the right-cluster slots
showing real data.

Ordering suggestion: (a) wire register / unregister into
spawn / close (no UI yet), (b) integrating PR for the bar
itself (left + mid-left + right + bell). The two could be one
PR but they're independent enough to split if it stays
reviewable.

### Session 35 — long autonomous run (PRs #68 → #88)

User said "ga maar autonoom door tot het klaar is" and stepped
away. Twenty-one PRs landed across the run; every Copilot review
addressed in a follow-up commit (or fix-PR for the merged ones)
with replies linked + threads resolved.

**Big features shipped (rough chronological + thematic):**

- **Worktree row context menu** (#68 + #70 fix) — Reveal / Open
  in WT / Copy path / Remove worktree… with id-based lookup and
  force-prompt on dirty.
- **Tab groups + drag-resize + persistence** (#69, #73). Flat
  `Vec<Group>` à la C# `EditorGroupViewModel`. Ctrl+\, per-group
  strip + pane, draggable splitters, weight persistence.
- **Native Windows titlebar** (#71, #79). gpui's `WindowControlArea`
  proved unreliable; switched to direct Win32 (`WM_SYSCOMMAND` /
  `WM_NCLBUTTONDOWN`) via a new `win32_titlebar` module. Same
  pattern Chrome / VS Code / Windows Terminal use. `windows = "0.61"`
  joined the deps as a `target.cfg(windows)` dep.
- **"New session" / "New Claude session" in sidebar menus** (#72).
  Project + worktree menu rows; auto-types `claude` after 250 ms.
- **Cold-start group rehydration** (#74). Persisted weights /
  focused index restore the work-area shape at launch.
- **Sidebar resize + Ctrl+B collapse** (#75). 6 px right-edge
  handle, chevron toggle in caption row.
- **Live theme reload** (#76). 1 s mtime poll on `settings.json`,
  hot-swaps `Arc<Theme>` and forwards to Sidebar.
- **Session restore** (#78). `LayoutState.open_tabs` records every
  tab; rehydrate on launch, drop missing-path entries silently.
- **Tab drag between groups** (#80). `on_drag` / `on_drop` wiring;
  cross-group reparent with no ConPTY teardown (gpui `Entity<T>`
  is Arc-backed, so just moving the `Tab` struct works).
- **Status bar** (#81) — 24 px bottom row, active title + tab N/M
  + group N/M.
- **Worktree git ops** (#82, #84). Pull (--ff-only), Copy branch,
  Open remote in browser. `core/git.rs` gains `pull_ff_only`,
  `remote_origin_url` (proper exit-code handling), and pure-parser
  `remote_url_to_browser` (4 unit tests). #84 fixed a
  command-injection vector (Windows `cmd /C start` →
  `ShellExecuteW`).
- **Group keybindings** (#83). Alt+Left/Right/1..9 for group
  focus, separate from the Ctrl-based tab chords.
- **Project menu git ops** (#85). Fetch all (prune) + Open remote
  in browser, with `spawn_open_remote_in_browser` extracted as a
  shared helper.
- **Tab right-click menu** (#86). Close / Close others / Close all
  to the right. `ink_ghost` for greyed-out no-op rows.
- **Worktree dirty-state polling** (#87). 5 s background poll runs
  `git status --porcelain` (HashSet-deduped, cache pruned each
  tick). 6 × 6 dot per worktree row: ink_ghost loading / accent
  clean / amber dirty.
- **Worktree menu — Discard changes…** (#88). Dirty-aware row,
  Critical-level confirm prompt, `git reset --hard HEAD` +
  `git clean -fd`. New `worktree_display_label` helper consolidates
  the "branch name or folder leaf" pattern across Remove / Discard
  prompts.
- **Toast notifications** (#90). Generic floating-stack overlay
  (bottom-right, anchored, deferred). `AppShell.toasts` VecDeque
  with two-rate poll (250 ms while visible, 8 s idle), cap of 5
  with oldest-evict, three severities (Ok / Err / Info — Info
  reserved for future use). Sidebar emits via new
  `SidebarEvent::Toast`; AppShell maps to its private `ToastKind`.
  Pull / Fetch all / Discard menu actions now toast their result.
  Mirrors C# `ToastHost`.

**Lessons:**

- gpui's `WindowControlArea` hit-test path is fragile in
  `appears_transparent: true` setups — direct Win32 was the right
  call. Don't repeat the "trust the framework first" mistake on
  shell-level integrations.
- `--admin` merge for solo work is fine; the Copilot review pass
  still happens — every PR landed got reviews addressed in a
  follow-up commit (or a fresh fix-PR when the parent had merged).
  Loop time ~20 s per thread (reply via REST → resolve via
  GraphQL). Worth doing.
- gpui `Entity<T>` Arc-backed means cross-group tab reparent is
  free — never had to mirror C#'s `SessionViewHostPool`.
- ID-based lookup beats indices for any state that survives an
  await point or a render boundary. Already true for worktree
  menu (#70); reinforced for tab drag (#80) and tab menu (#86).

### Session 34 — camelCase, context menu, dialog, titlebar, tests, end-to-end worktree flow (PRs #58–#62 + bundle)

The week's "next entry points" list (1–6 from session 33) is now
mostly walked. Five small PRs landed sequentially with reviews
addressed inline (Copilot reviewer flagged one issue per PR), then a
single larger bundle PR closed out the worktree flow end-to-end.

**Shipped (in merge order):**

1. **PR #58 — camelCase round-trip on `projects.json`** (`599b8d3`).
   `#[serde(rename_all = "camelCase")]` on `Project`, `Session`,
   `Worktree`, `ProjectsConfig`. Opaque `agents: Vec<serde_json::Value>`
   on the root so user agent-profile overrides survive a load/save
   cycle while the Rust port doesn't consume them yet. Three new
   tests pin the contract (C#-shape fixture, written-camelCase
   assertion, agents round-trip). Review feedback: backwards-compat
   for legacy snake_case Rust port files via per-field
   `#[serde(alias = "<snake_case>")]` (`f8820cf`, also in #58) plus a
   regression test (`loads_legacy_snake_case_fixture_without_data_loss`).
2. **PR #59 — platform-correct labels in the project context menu**
   (`3f01e2f`). The right-click menu itself piggybacked into PR #58
   during a branch split (an earlier mistake), so #59 ended up
   carrying just the platform fix. New `reveal_in_file_browser_label()`
   helper picks the label per `cfg!(target_os = …)`; "Open in Windows
   Terminal" row hidden off Windows via
   `.children(cfg!(target_os = "windows").then(|| …))` so a `None`
   yields zero children and the chain stays clean.
3. **PR #60 — titlebar interactions** (`109e1ce`). Drag region
   (`window_control_area(Drag)` + `start_window_move()`), double-
   click toggles maximise via `zoom_window()`, three caption buttons
   (Min/Max/Close, 46×40) with `WindowControlArea` annotations so
   Windows snap-layouts and accessibility see real controls.
4. **PR #61 — integration tests for `git::add_worktree` /
   `remove_worktree`** (`27ceab1`). End-to-end against a real `git`
   binary: per-test `TempDir` containing `<tmp>/repo/` and
   `<tmp>/repo.worktrees/` (review feedback: the original draft
   rooted under the OS-shared tempdir, which would have collided in
   parallel and leaked outside the cleanup guard). Three tests
   (add → list, add → remove → list, duplicate-branch error must
   carry stderr).
5. **PR #62 — "New worktree from branch…" dialog** (`6109eef`,
   subagent-built). Trimmed first cut: branch input with auto-derived
   folder under `<project>.worktrees/<sanitised-branch>`, HEAD as
   implicit base, no spawn-toggle / base-picker / editable folder
   yet. Modal rendered via `deferred(anchored().position(0,0).child(backdrop))`
   with click-out-to-cancel and Escape/Enter handling; key chars come
   from `event.keystroke.key_char`. New module
   `codescope-rs/src/new_worktree_dialog.rs` with
   `NewWorktreeDialogState` stashed on `Sidebar`. `Project::worktree_root_path()`
   added to `codescope-core`. 7 new unit tests on the sanitiser +
   folder derivation.
6. **Bundle PR (this session, end-to-end worktree flow):** worktree
   *child rows* now render under each project in the sidebar,
   clicking one emits `SidebarEvent::OpenSession { working_directory,
   title }` which `AppShell` catches via `cx.subscribe_in` and turns
   into a fresh tab pinned to the worktree path. The new-worktree
   dialog also emits the same event on successful create, matching
   the C# build's `SpawnSession = true` default — the user lands
   inside the new worktree immediately. New `git::list_branches`
   (`for-each-ref refs/heads + refs/remotes`, mirrors C#'s
   `GitService.ListBranchesAsync`; drops `<remote>/HEAD` symbolic
   rows) + 2 integration tests. `spawn_tab` refactored to
   `spawn_tab_in(working_directory, title_override, …)` so callers
   can pin both. Drive-by clippy cleanup (`(1..=9).contains(&n)`,
   collapsed nested `if let`).

**Process notes (worth carrying forward):**
- **PR splitting trap.** Stacking commits on the same local branch
  and then trying to "split" by creating a sibling branch + rebase
  works **only if you also reset the original branch** to drop the
  cherry-picked commit. I missed that on the camelCase / context-menu
  split, so PR #58 swallowed the context-menu commit and merged it to
  main; PR #59 then needed a rebase that auto-detected the dup
  ("patch contents already upstream"). Same dance had to happen for
  PR #60 / #62 after their bases (deleted feature branches)
  collapsed onto main. Lesson: after `git switch -c new-branch &&
  rebase --onto …`, also `git switch original-branch && git reset
  --hard origin/original-branch`. Or just use worktrees from the start.
- **Branch-protection merging.** main has a protection rule that
  blocks plain merges (`gh pr merge --squash`) and disallows
  `--auto` for this repo. `--admin` overrides cleanly — the user
  greenlit using it explicitly, the auto-mode classifier asked once
  per session escalation. For the next session, default to `--admin`
  on owner-merges of own PRs (with explicit per-session OK).
- **Subagent task on PR #62 worked well.** Briefed with the C# spec
  link, scoped to "trimmed first cut", isolation: worktree. Returned
  a clean PR + a punch list of deferred items (Tab cycle, IME,
  custom folder, base picker, spawn toggle). Worth doing again for
  any feature that's >150 LOC and self-contained.

**Suggested next entry points (priority order):**

1. **Worktree dialog polish toward C# parity.** The deferred items
   from PR #62's punch list — pair them with the new
   `git::list_branches` API:
   - **Base-branch dropdown** (filterable popup, LOCAL/REMOTE
     groups, `(HEAD)` pinned). The C# spec is in
     `src/CodeScope.Ui/Dialogs/NewWorktreeDialog.xaml.cs` lines
     190–212. `list_branches` already returns the data.
   - **Editable folder path** (live-syncs from branch but the user
     can override). C# `BranchBox.TextChanged` handler is the model.
   - **Spawn-session toggle** (defaults to true to match C# and the
     bundle PR's behaviour; off = user just gets the worktree row).
2. **Worktree row context menu.** "Remove worktree…",
   "Reveal in <file browser>", "Copy path", "Restart session"
   (later). Mirror `BuildWorktreeMenu` in
   `src/CodeScope.Ui/Views/SidebarView.xaml.cs`.
3. **Live theme reload.** Watch `settings.json` mtime via the
   `notify` crate, reload, resolve theme by name, swap `Arc<Theme>`
   on AppShell + Sidebar (the `apply_theme` plumbing already exists
   on Sidebar; AppShell just needs the same).
4. **Telemetry tail.** FSWatch on `~/.claude/projects/...` →
   `running / idle / error` per session, drives sidebar status dots.
   Heavy — multi-agent (Claude / Codex / OpenCode / Copilot / Pi),
   each with its own session-discovery layer in C#. Probably its
   own 1-2 session arc.
5. **Sidebar splitter / collapse.** `LayoutState.sidebar_visible` /
   `sidebar_width` are loaded but never re-saved because nothing
   changes them. Add a thin draggable splitter on the right edge +
   a collapse toggle in the heading.
6. **`appears_transparent: true` quirks.** Verify on a non-
   Windows host that the caption controls don't fight the native
   chrome (we never tested on macOS / Linux).

**Known small things (rolled forward from session 33):**
- Backend's `hyperlink_at` is unused (snapshot handles it).
- The "+ new tab" button still spawns in the *active project's*
  primary path. After the worktree click flow lands, consider
  making the "+" inherit the most-recent worktree of the active
  project instead so users don't keep re-clicking a worktree row.
- `codescope-terminal` has 8 preexisting clippy warnings (not
  touched this session). Cleanup PR if anyone gets bored.


### Session 33 — worktree-on-new-session was wrong; rolled back; data-loss incident + recovery

Aborted work in this session. Captured here so the next pass doesn't
walk back into the same trap.

**What I did and why it was wrong (PR #56, closed):**
On the strength of session 32's "Suggested next entry points" list
("Add a 'New session' affordance per project … run `add_worktree`,
save a `Session`, open a tab in the worktree path"), I built exactly
that: a `+ new session` row under the active project that
auto-created a `session-N` worktree, persisted Worktree + Session,
and emitted a `SidebarEvent::OpenSession` the AppShell caught via
`subscribe_in` to spawn a tab in the worktree path. PR #56 opened,
build clean, tests green (5 new on `Project::next_session_branch_name`,
`worktree_path_for`, `worktree_root_path` — those helpers are
still solid). User pushed back hard on first manual click:

> "hij lijkt nu meteen een nieuwe worktree aan te maken op een
> nieuwe sessie, dat is echt niet nodig hoor, als dat in de readme
> staat dan is dat ook fout."
>
> "worktree aanmaken alleen maar uit context menu doen."

**The rule (now also in auto-memory `feedback_session_vs_worktree.md`):**
A new session is just a tab opened in the project's *primary* path —
no worktree. Worktree creation is a separate, opt-in action that
lives **only** in a right-click context menu, prompts for a branch
name, and is named accordingly ("New worktree…"). Treat any spec doc
that conflates the two — including last session's "next entry
points" — as out-of-date.

PR #56 closed; branch deleted (local + remote); main is back to
`6cc89ee`. The `core::projects` helpers added in PR #56
(`worktree_root_path`, `worktree_path_for`, `next_session_branch_name`)
are *not* in tree — they came in on the closed branch. Reland those
under a context-menu-driven worktree action when the time comes.

**Data-loss incident — production `projects.json` overwritten:**
While trying to launch the dev build for the manual test, I called
the Bash tool with `$env:CODESCOPE_DEV = "1"; cargo run …` (PowerShell
syntax). The Bash tool runs `bash`, not pwsh, so it parsed `$env:`
as a stray identifier (`$env:CODESCOPE_DEV: command not found`) and
the env var was *never set*. The Rust build therefore resolved
`AppPaths` against **production** paths, not Dev. When the user
added a project + clicked the (then-still-present) `+ new session`
button, the Rust build wrote its snake_case `projects.json` over the
C# camelCase production file at `%APPDATA%\CodeScope\projects.json`.
Outcome: the user's full C# project list was overwritten down to
just `brmble` + 2 stray `session-N` worktrees on disk at
`C:\dev\brmble\brmble.worktrees\`. No backup at the usual paths
(`%LOCALAPPDATA%\CodeScope.Dev.backup-*` from session 32 was not
recreated this session).

**Recovery actions taken:**
- Closed PR #56 with a comment explaining the design miss.
- Deleted the feature branch (local + remote).
- `git worktree remove` for both stray worktrees in the brmble repo
  and `rmdir` for the now-empty (wrong-default) `brmble.worktrees`
  parent — left brmble's *real* `.worktrees/` (dot-prefix) intact.
- User restoring `%APPDATA%\CodeScope\projects.json` themselves from
  their own backup outside this system.

**Lessons captured (carry into every future session):**

1. **Env vars in the Bash tool: bash syntax only.** `CODESCOPE_DEV=1
   cargo run --manifest-path … --bin window`. Never `$env:NAME = …`
   in a Bash tool call. PowerShell syntax goes only in commands the
   user runs in their own shell.
2. **The Rust port silently overwrites a C#-shape `projects.json`.**
   Loading a camelCase file fails to deserialize, defaults to empty
   config, and the very next user mutation persists snake_case over
   the original. Until the camelCase round-trip lands (still on the
   follow-up list — see session 32 ⚠️ note), do not point a Rust
   build at production paths. Treat the `CODESCOPE_DEV=1` redirect
   as a **safety boundary**, not a convenience.
3. **Session ≠ worktree.** Repeated for emphasis. Memory file:
   `~/.claude/projects/C--dev-codescope-public/memory/feedback_session_vs_worktree.md`.
4. **Read the C# implementation before designing anything Rust-side.**
   The Rust port targets functional parity with the C# build, not
   reinvention. Grep `src/CodeScope.App/**/*.xaml{,.cs}` +
   `src/CodeScope.Core/**/*.cs` for the feature first; mirror UX
   terminology, data shapes, and persistence layout 1:1. PR #56
   invented a UX that doesn't exist in the C# build — that
   shouldn't have been possible if I'd looked at the C# tab strip /
   sidebar context menu first. House rule now codified at the top
   of this file and in
   `~/.claude/projects/C--dev-codescope-public/memory/feedback_csharp_parity.md`.

**Suggested next entry points (priority order, replacing session 32's list):**

1. **camelCase round-trip on `projects.json`.** Add
   `#[serde(rename_all = "camelCase")]` on `Project`, `Session`,
   `Worktree`, `ProjectsConfig`. Verify the C# build's top-level
   `agents: []` survives load (serde tolerance is default). Land a
   fixture-based round-trip integration test (the user's own
   restored production file is a good source — copy a sanitised
   subset into `core/tests/fixtures/`). **Without this the
   data-loss footgun above is still cocked.**
2. **Right-click context menu on a project row.** "Open in
   Explorer", "Reveal in shell", "Remove from sidebar", "**New
   worktree…**" (this last entry is the *only* place worktree
   creation lives). Needs a small popup-menu primitive — gpui has
   `cx.show_window` / overlay patterns; check `examples/`.
3. **Branch-name input** for "New worktree…". Real text input
   needed (gpui `examples/input.rs` is the reference, but it's a
   chunk of code). Pair with the sidebar filter input so the
   widget effort lands once.
4. **Live theme reload.** Watch `settings.json` mtime, reload,
   resolve theme by name, swap `Arc<Theme>` on AppShell + Sidebar.
5. **Telemetry tail.** FSWatch on `~/.claude/projects/...` →
   `running / idle / error` per session, drives status dots.
6. **Titlebar interactions.** Drag region, min/max/close caption
   controls. Carved out from session 32, still pending.

**Known small things (rolled forward unchanged from session 32):**
- Backend's `hyperlink_at` is unused (snapshot handles it).
- `LayoutState.sidebar_visible` / `sidebar_width` loaded but never
  re-saved — fine until a toggle / splitter lands.
- `git::add_worktree` / `remove_worktree` have no integration test
  hitting a real `git` binary; land it alongside "New worktree…".



### Session 32 — window/layout persistence, add-project, git primitives, sidebar drives tab cwd

Picked up the priority list left by session 31 and walked five items
in stacked commits on `feat/codescope-rs-window-state-save` (off main):

1. **`window.json` save wiring** (`48ccd19`). AppShell registers
   `cx.observe_window_bounds`; bounds + `is_maximized` flow into a
   `PendingWindowSave` slot, debounced for 500 ms before hitting
   disk. Restore-bounds (not live size) are serialised so an
   un-maximise on the next launch lands at the user's chosen size.
   Loop dies on AppShell drop — anything still pending at quit is
   genuinely stale.
2. **Sidebar `+` add-project** (`60f9c8a`). Native folder picker
   via `cx.prompt_for_paths(directories: true)`; on confirm a
   fresh `Project` is appended and `projects.json` written.
   `Project::new(path)` lives in `core`: leaf folder name → display
   name, UUIDv4 id, branch defaults to "main". Duplicate paths
   short-circuit (re-pick selects the existing row instead of
   adding a stale copy). Switched Sidebar to own `ProjectsConfig`
   directly (was `Arc<...>`) — simpler add/remove without
   `Arc::make_mut`.
3. **`codescope-core::git` worktree primitives** (`ca7c10d`).
   `list_worktrees` / `add_worktree` / `remove_worktree` shell out
   to the system `git`. Pure-stdlib (no libgit2 — see CLAUDE.md
   technology decisions). Porcelain parser handles
   primary/branched/detached/locked stanzas. 4 unit tests on the
   parser. Errors carry the trimmed stderr verbatim so a future
   error dialog can surface "fatal: 'foo' is already checked out
   at '...'" without guessing the cause. Not yet wired to UI.
4. **Sidebar drives new-tab cwd** (`1d2163d`). AppShell reads
   `Sidebar::active_project()` at spawn time, threads `path`
   through `SpawnConfig.working_directory`, labels the tab with
   the project name. Without a selection (cold launch / no
   projects) we fall back to inherited cwd + numeric label.
5. **Selection persists across launches** (`ee8d155`). Sidebar
   owns `LayoutState`, restores selection by id on construction,
   writes `layout.json` on every `select` / `add_project`. Saved
   id missing or stale → first project. Single writer (Sidebar);
   AppShell-side fields (sidebar_visible / sidebar_width — both
   unused so far) will land the same way once they become
   user-controllable.

**Architectural notes worth carrying forward:**
- `core` is now slightly impure: `git.rs` shells out to `git`. UI
  imports are still excluded (gpui, alacritty, windows-rs) — the
  layering pyramid still holds.
- Background-debounce + `Arc<Mutex<Option<PendingX>>>` works
  cleanly for both terminal-resize (terminal/view.rs) and
  window-state save (src/app.rs). Reach for it whenever a fast
  observer feeds a slow consumer.
- `Sidebar` is the writer for `layout.json`. Splitting writes per
  field-owner keeps the persistence surface obvious — when adding
  fields, prefer to put the writer next to the state owner rather
  than centralising.

**Suggested next entry points (priority order):**

1. **Worktree orchestration UI.** Add a "New session" affordance
   per project (button in sidebar, command-palette later). Prompt
   for a branch, run `add_worktree`, save a `Session` to the
   project, open a new tab in the worktree path. The "actually
   CodeScope" milestone — primitives in `core::git` are ready,
   the UI piece is what's left.
2. **Live theme reload.** Watch `settings.json` for mtime change
   (poll every ~1 s in a background task), reload settings,
   resolve theme by name, swap `Arc<Theme>` on AppShell + Sidebar,
   `cx.notify`. The two `theme::canvas(theme)` etc. helpers
   already accept `&Theme` so a swap repaints without rebuilding.
3. **Telemetry tail.** FSWatch on `~/.claude/projects/...` →
   `running / idle / error` per session, drives status dots
   (currently hardcoded green when active).
4. **Filter input at the top of the sidebar.** Needs a real text
   input — gpui's `examples/input.rs` is the reference but it's a
   chunk of code. Worth pairing with the new-session dialog so
   the input widget effort lands once.
5. **Right-click on project / tab.** "Remove from sidebar",
   "Open in Explorer", "Reveal in shell" — mirrors the C# build's
   tab-strip context menu.

**Manual-test results (this machine, dev build, end of session):**
- ✅ resize + reposition → close → relaunch reopens at the same
  bounds (`window.json` save wiring works)
- ✅ click `+` → folder picker → new row appended, `projects.json`
  written in Rust shape (snake_case)
- ✅ select project → close → relaunch restores selection
  (`layout.json.selected_project_id` wired)
- ✅ select project → `Ctrl+Shift+T` → new tab labelled with the
  project name, `pwd` lands in the project's path
- ⚠️ test plan claimed "v0.x C# `projects.json` round-trips" — it
  does **not** in practice. C# writes camelCase
  (`defaultBranch`, `worktreePath`, `agentSessionId`, …); Rust
  expects snake_case. C#-shape file fails to deserialize and we
  fall back to empty config. Fix in the next pass: add
  `#[serde(rename_all = "camelCase")]` on `Project`, `Session`,
  `Worktree`, `ProjectsConfig` (verify per-field — the C# file
  has a top-level `agents: []` we don't model yet, so a
  `serde(default)` fallthrough on extras is also needed). Then
  add a round-trip integration test with a fixture cribbed from
  `CodeScope.Dev.backup-*` so the schema break can't silently
  return.

**Known small things still to clean up (rolled into the "next
pass" lane the user explicitly carved out):**
- **Titlebar interactions broken.** `appears_transparent: true`
  claims the bar for our tab strip but we don't replace the
  drag region or the caption controls — so the window can't be
  dragged, can't be maximised by double-clicking the bar, and
  has no min/max/close glyphs. gpui's `examples/window.rs` and
  `window_shadow.rs` show the pattern: a `WindowControlArea`
  for the buttons + a hand-rolled drag handler on empty bar
  space (`window.start_window_move()` on Windows). Land
  alongside the next round of polish.
- Backend's `hyperlink_at` is unused (snapshot handles it).
- Layout fields `sidebar_visible` / `sidebar_width` are loaded
  but never re-saved because nothing changes them yet — fine for
  now, will update when a toggle / resize lands.
- `git::add_worktree` / `remove_worktree` have no integration
  test that hits a real `git` binary. The parser is tested; the
  shell-out wrapper is thin enough that the integration test can
  land alongside the new-session UI.

### Session 31 — codescope-core, settings + themes, sidebar + projects, window/layout state, mouse-mode, OSC 8 + URL detection

This session went from "working terminal in a window" to a real
app shell with a clean architecture, persisted state, and the
quality-of-life polish that makes the Rust port feel like a
product instead of a tech demo.

**Shipped (10+ commits on `feat/codescope-rs-architecture`, all on PR #54):**

1. **App shell + tab strip + Codescope theme tokens** (`95fe32f`,
   `c2f43f6`, `4a26ca7` — recovered via reflog after PR #53 squash
   landed without them; pushed cleanly on the new branch).
   `AppShell` Entity, 40 px tab strip in the titlebar zone, pill
   tabs (canvas-bg + 2 px Framer Blue top border on active, frost-
   on-hover for inactive), green status dot per tab, `+` new-tab
   button, custom titlebar via `appears_transparent`. Keybindings:
   `Ctrl+Shift+T` / `Ctrl+Shift+W` (Windows-Terminal convention so
   readline's word-bindings stay intact), `Ctrl+Tab` cycle,
   `Ctrl+1..9` direct select. View deliberately doesn't
   `stop_propagation` on app-level shortcuts so they bubble up.
2. **`codescope-core` workspace crate** (`d156734`). Pure-Rust,
   no gpui or alacritty deps. Hosts settings, themes, env-aware
   paths. Architecture pyramid is now `core ← terminal ← app/src`.
   Five built-in themes (`codescope-default`, `vs-code-dark`,
   `one-dark`, `solarized-dark`, `tokyo-night`); `settings.json`
   at `%APPDATA%\CodeScope\settings.json` with theme name, font
   chain, scrollback, cursor preset; `AppPaths` ports the C#
   `NoScope.CodeScope.Core.AppPaths` 1:1 (same folder names, same
   `CODESCOPE_DEV` redirect, same single-instance mutex naming).
   Terminal crate gained `ColorPalette::from_theme_palette` so a
   theme flows straight into the renderer + the OSC-query proxy
   without an interop seam in the binary.
3. **Project / Worktree / Session models + PROJECTS sidebar**
   (`e7126a7`). Direct port of
   `src/CodeScope.Core/Models/{Project,Session,Worktree,
   ProjectsConfig}.cs` — same field names so a `projects.json`
   written by the v0.x C# build round-trips. 240 px left rail with
   PROJECTS heading, project rows (accent rail + frost on active),
   empty-state prompt, placeholder `+` add button.
4. **window.json + layout.json persistence** (`674a985`). Load
   path wired: saved bounds (or `WindowBounds::Maximized`) flow
   into `WindowOptions::window_bounds`. Implausibly small saved
   sizes (< 320×240) are rejected so a 4K → laptop monitor swap
   doesn't break the next launch. Save path is a stub —
   `cx.observe_window_bounds` wiring is the follow-up.
5. **Mouse-mode reporting** (`0567b43`). New `terminal::mouse`
   module: SGR encoding when `?1006h` is set, X10 fallback. All
   click / drag / wheel / motion events route through
   `try_report_mouse` first; selection only kicks in when no TUI
   asked for the click (or the user held Shift to bypass). Right
   + Middle button listeners added so `tmux` context menus and
   `vim` middle-click work. 9 unit tests on the encoder.
6. **OSC 8 hyperlinks + plain-text URL detection** (`051cf3b`,
   `33083b2`). `StyledRun` carries `Option<Arc<str>>` hyperlink;
   snapshot reads `cell.hyperlink()` for OSC 8, then post-
   processes each line via `linkify` to retro-tag bare-text URLs
   (`claude-code`, `gh pr view`, `cargo --message-format json`).
   `TerminalSnapshot::hyperlink_at(row, col)` lets the View look
   up links without re-locking the live `Term`. Pointer cursor on
   hover, Ctrl/Cmd-click opens via the `open` crate; OSC 8 always
   wins over URL detection.
7. **Review fixes for PR #54** (`5d15ace`). Five copilot-reviewer
   threads addressed: settings doc honesty about unknown-key
   drop, accurate `build_font_config` "platform pick" comment,
   macOS state-dir doc match, `from_theme_palette` synthesizes
   the standard xterm cube/grayscale ramp for short tables (with
   `debug_assert_eq!`), `CursorStylePreset` doc no longer claims
   serde-friendliness it doesn't have.
8. **`+` button + hover cursor fixes** (`33083b2`). `+`
   collapsed to 0 px hit area because `h_full()` didn't always
   resolve before hit-testing — switched to explicit `h(40)` +
   `cursor_pointer()`. Hover-over-link now flips the root div to
   pointer cursor; we only `cx.notify` on URI changes so motion
   doesn't repaint.

**Confirmed working on Windows (manual test, this machine):**
multi-tab spawning + closing + cycling · `Ctrl+Shift+T/W` /
`Ctrl+Tab` / `Ctrl+1..9` from terminal focus · five themes
swap via `settings.json` and look right · sidebar shows
hand-edited `projects.json` entries · OSC 8 hyperlinks render
underlined and open on Ctrl+click with hand cursor · plain-text
URLs in claude-code output are clickable · pwsh + claude-code +
typing + scroll + selection + paste all still work cleanly.

**Architectural notes worth carrying forward:**
- `core` crate is the canonical home for any data the app needs
  to load / save / share. Adding gpui or alacritty there reverses
  the pyramid — push the new code into `terminal/` or `src/`
  instead.
- `Arc<Theme>` on `AppShell` is the swap point for live theme
  reload. `theme::canvas(theme)` etc. accept a `&Theme` so when
  the Arc swaps, the next render redraws against the new values
  without rebuilding the entity.
- The squash-merge / unpushed-commit incident at the start of
  the session: PR #53 squashed against the last *pushed* HEAD,
  not the live local HEAD. Three commits I'd made locally
  (app-shell + theme + style + shortcut bubble) were lost
  briefly. Recovered via reflog cherry-pick — the lesson is
  push after every meaningful commit, not after a sequence.
- `StyledRun` post-processing for URL detection is the right
  layer — keeps the per-cell loop clean and lets OSC 8 always
  win over heuristics. Wide-char runs are skipped on splitting
  to stay safe; URLs in the wild are pure ASCII so the fallback
  basically never trips.

(Next-entry-points list moved to the session 32 entry above —
those items are still accurate but session 32 added context.)

### Session 30 — pixel-accurate paint, paste, cursor blink, resize debounce, box-drawing alignment, OSC query handshake

This session took the working interactive terminal from session 29
and made it _feel_ like a real terminal — pixel-perfect rendering,
proper paste, blinking cursor, no-flicker resize, clean box drawings,
and inline answers to terminal capability queries.

**Shipped (5 commits on `feat/codescope-rs-terminal`, all on PR #53):**

1. `cd1665a` — pixel-accurate paint pass + bracketed paste.
   Replaced the flex-of-divs render with a single `canvas` element:
   measure phase shapes `│` for cell metrics, paint phase emits the
   default-bg quad, per-row merged non-default-bg quads, then one
   shaped TextRun per styled run at exact `col × cell_width`, then
   the cursor on top. `StyledRun` now carries `start_col` + `len_cols`;
   wide-char spacers filtered. Cursor shapes Block / HollowBlock /
   Beam / Underline rendered correctly (block redraws the cell glyph
   in `cell.bg` so the inverted fill stays readable). Adds
   `Ctrl+Shift+V` / `Cmd+V` paste, plus a smart `Ctrl+V` that pastes
   when the TUI has bracketed-paste mode on (claude-code, vim, modern
   shells) and falls through to `\\x16` when off (PSReadLine's own
   paste binding and readline's quoted-insert still work). CRLF
   normalised to LF so multi-line pastes don't run each line.
2. `59c7354` — cursor blink + Windows-Terminal default. Surface
   `term.cursor_style().blinking` onto the snapshot; View runs a
   530 ms blink timer (xterm convention) and snaps the phase back to
   "visible" on every keystroke. Term's `default_cursor_style` set to
   blinking bar so the cursor matches Windows Terminal even when
   PSReadLine doesn't emit DECSCUSR; TUIs that send DECSCUSR still
   override live.
3. `ad82486` — debounce resize + shape per cell. ConPTY on Windows
   dumps the active viewport into scrollback every time conhost is
   resized; resizing 60×/sec during a window drag filled scrollback
   with near-duplicate copies of the prompt, obvious as soon as the
   user scrolled up. Stage incoming sizes in `pending_size`; a 40 ms
   poll task applies after 120 ms of stability. One drag → one
   resize. Box-drawing characters (`╭╮╰╯─│`) used to break in
   claude-code's banner because `shape_line` over a whole run let
   glyph-advance differences in font fallbacks accumulate; now we
   shape one glyph at a time and paint each at exact `col ×
   cell_width`. gpui caches glyph shaping internally so the cost on
   a 100×30 grid is negligible.
4. `67a3b67` — answer OSC color + text-area-size queries inline.
   `Event::ColorRequest` (OSC 4 / 10-12) and `Event::TextAreaSizeRequest`
   (CSI 14 t) used to be silently dropped. Now resolved directly in
   `EventProxy::send_event` (event-loop thread) and forwarded as
   `Msg::Input` — round-trip is microseconds, not a frame. Added
   `Rgb` mirror of `ColorPalette` defaults + `resolve_rgb_no_overrides`
   that doesn't lock the term (the event-loop thread can't safely
   take the `Term` lock mid-parse). Per-terminal OSC 4 overrides
   aren't consulted; rare in practice.
5. `a2f1d18` — set `TERM_PROGRAM=CodeScope` in spawn env.
   Doesn't fix claude-code's minimal-UI fallback (that's a Node-side
   detection that also affects the C# CodeScope build), but is the
   right thing to advertise so other capability-aware TUIs (Ink apps,
   fzf, lazygit) recognise the host instead of treating us as a bare
   ConPTY.

**Confirmed working on Windows (manual test, this machine):**
pwsh + oh-my-posh banner with full colour + nerd glyphs · keyboard
input including `Ctrl+Shift+V` / `Cmd+V` paste with bracketed-paste
markers · smart `Ctrl+V` that pastes inside TUIs but stays
quoted-insert at the prompt · cursor blinks at 530 ms with proper
shape (matches DECSCUSR) and stays visible during typing · live
resize that doesn't fill scrollback with duplicates · box-drawing
characters render seamlessly (vim borders, claude banner — when
claude isn't in minimal mode).

**Known limitation:** claude-code falls back to its minimal UI in
both this terminal and the C# build's `EasyWindowsTerminalControl`.
TERM_PROGRAM, COLORTERM, OSC color/size response timing all
investigated; not the trigger. Likely a Node-side detection
(`isTTY`, `terminal-size`, or claude-internal heuristic). Out of
scope for our terminal layer; user accepted as long-standing.

**Architectural notes worth carrying forward:**
- The paint module (`terminal/src/paint.rs`) is the renderer of
  record. Per-cell shaping > per-run shaping for terminals. Block
  cursor needs to repaint the cell glyph in `cell.bg` to stay
  readable on top of the inverted fill — easy to forget.
- ConPTY resize-debounce is mandatory on Windows. Without it,
  scrollback fills with garbage on every drag. 120 ms of stability
  is the sweet spot — short enough to feel instant when the user
  stops dragging, long enough that a single drag doesn't fire 60
  resizes.
- OSC color / text-area-size queries _must_ be answered from the
  event-loop thread, not from the View's async drain. A frame of
  latency was enough to drop claude-code into minimal mode (though
  the trigger turned out to be elsewhere — the latency lesson stands
  for other TUIs).
- `EventProxy` carries a default `ColorPalette` clone and a
  `Mutex<WindowSize>` so it can answer queries without locking
  `Term`. The View pushes resize updates into the proxy via
  `Backend::resize` → `proxy.update_size`.

**Suggested next entry points (in priority order):**

1. **Settings file.** `settings.json` next to `projects.json` with
   `font.family`, `font.size`, `line_height`, `colors.*`, `scrollback`,
   `cursor.{shape,blinking}`. Replaces the `CODESCOPE_FONT` env var
   and hardcoded defaults. Reuse `AppPaths` for dev-mode separation.
2. **Hyperlinks (OSC 8) and URL/path detection.** Zed's
   `terminal_hyperlinks.rs` has clean heuristics to lift.
3. **Mouse-mode reporting.** TUIs like `tmux`, `htop`, `vim` want
   mouse events forwarded as escape sequences when they enable mouse
   mode. Currently we eat all clicks for selection.
4. **Tabs / multiple terminals in one window.** The View is stable
   enough now; scaling to N TerminalViews in a tab strip is the
   fastest route to actual workflow replacement.
5. **Box-drawing primitives.** Per-cell shaping fixed alignment, but
   for fonts that lack `╭╮╰╯`-style rounded corners we could draw
   them ourselves (`vendor/gpui-terminal/src/box_drawing.rs` is a
   working reference). Lower priority — current font fallback chain
   covers the common cases.

**Known small things still to clean up:**
- View's font defaults are still hardcoded `FiraCode Nerd Font` —
  settings file fixes this properly.
- `EventProxy` holds a default-cloned `ColorPalette`; if themes
  start updating the View's palette, we need an `update_palette`
  method on the proxy (or move the palette into a shared `Arc`).
- No tests on the View / paint layers; only `Backend` has the smoke
  example.

### Session 29 — Backend, View+Element-lite, scrollback, mouse selection, clipboard

This session turned the Rust port from "Backend stub + headless smoke
test" into a working interactive terminal window driven by our own code,
with a clear path forward.

**Shipped (3 commits on `feat/codescope-rs-terminal`, all on PR #53):**

1. `e469cc2` — `codescope-terminal::Backend` on alacritty's `EventLoop`.
   `EventProxy` forwards user-facing `Event` variants to a flume channel
   and routes `Event::PtyWrite` back via `Msg::Input` (the gpui-terminal
   Bug #1 we patched in vendor; here it's the canonical shape).
   `examples/smoke.rs` proves the pipeline headlessly: pwsh, `echo`,
   round-trip in the grid, clean exit. ~280 LOC, no UI dependency.
2. `fc03180` — `TerminalView` (gpui Entity, high-level div-based render,
   Element layer deferred). `colors::ColorPalette` resolves alacritty
   `Color` → `Hsla`. `input.rs` (lifted verbatim from vendor) maps
   keystrokes to bytes. Fonts: default `FiraCode Nerd Font` + 9-deep
   fallback stack so oh-my-posh glyphs render; `CODESCOPE_FONT` env
   override. Live resize via `canvas` overlay that measures cell width
   from `window.text_system().shape_line("│", …)` (Zed's pattern) and
   triggers `Backend::resize`. Cursor cell rendered with inverted bg/fg
   and gated on `TermMode::SHOW_CURSOR` + `CursorShape::Hidden` so TUIs
   that draw their own cursor (claude code, vim) don't double up.
   `cargo run --bin window` is the demo binary.
3. `127d1f2` — scrollback (`Config::scrolling_history = 10_000`),
   `Backend::scroll/scroll_page_up/page_down/reset_scroll/display_offset`
   wired through alacritty's `Term::scroll_display`. Snapshot translates
   `display_iter`'s absolute-line coords back to visible-row using
   `display_offset` (the bug that made scrolled rows render black). Mouse
   selection: `bounds_cache` Arc populated from the resize-probe canvas,
   `point_at(position)` maps window pixels → grid coords using measured
   cell metrics, `on_mouse_down/move/up` drive `Backend::start_/extend_
   /clear_selection`. Selected cells get fg/bg swapped in the snapshot.
   `Ctrl+C` is smart: copies + clears the selection if there is one,
   otherwise falls through to SIGINT. `Ctrl+Shift+C` and `Cmd+C` always
   copy. Typing clears selection and snaps back to active region.

**Confirmed working on Windows (manual test, this machine):**
pwsh + oh-my-posh prompt with full colour and nerd glyphs · keyboard
input including arrow keys / Ctrl+key combos · live resize tracking the
window edge · 10k-line scrollback via wheel and PageUp/PageDown ·
drag-select with visual inversion · clipboard round-trip via Ctrl+C
(with selection) and Ctrl+Shift+C (always) · plain Ctrl+C still kills
running commands when nothing is selected · cursor doubling in
claude-code resolved.

**Architectural notes worth carrying forward:**
- The `Backend` design exactly mirrors Zed's `Terminal` struct
  (`Arc<FairMutex<Term<EventListener>>>` + alacritty `EventLoop` +
  `Notifier` for writes + flume channel for upstream events). Validated
  by reading `crates/terminal/src/terminal.rs` from `zed-industries/zed`
  before writing ours — same shape, smaller.
- "Bug #2 conhost-vs-grid scroll-sync" from session 28 turns out to be
  a gpui-terminal *render* bug, not a ConPTY-level coordinate problem.
  Zed walks `display_iter` directly and does no translation. Our
  snapshot does the same now and the symptom is gone.
- The View layer skips the Element trait for now and uses high-level
  `div().flex().children(...)` with one styled run per cell-batch. This
  trades pixel-perfect alignment for ~150 LOC of paint code instead of
  several hundred. The Element layer goes in when alignment / cursor
  shape / sub-cell selection matter.

**Suggested next entry points (in priority order):**

1. **Element-level renderer.** Replace the per-line flex-of-divs with
   a real `Element` impl that paints background quads + batched
   `TextRun`s at exact `col × cell_width` offsets. Needed for: a real
   block/beam/underline cursor with blink, sub-cell selection
   highlighting, and to make the right-edge alignment perfect at any
   cell width. Reference: `gpui-terminal/src/render.rs` (vendored)
   and Zed's `terminal_view/src/terminal_element.rs`.
2. **Settings file.** A `settings.json` next to `projects.json` with
   `font.family`, `font.size`, `line_height`, `colors.*`, `scrollback`.
   Reuse `AppPaths` so dev-mode separation works.
3. **Hyperlinks (OSC 8) and URL/path detection.** Zed has a clean
   `terminal_hyperlinks.rs` we can lift the heuristics from.
4. **Mouse-mode reporting.** TUIs like `tmux`, `htop` want mouse events
   forwarded as escape sequences when they enable mouse mode. Currently
   we eat all clicks for selection.
5. **Tabs / multiple terminals in one window.** Once the View is
   stable, scaling to N TerminalViews in a tab strip is the fastest
   route to "actually replaces my workflow."

**Known small things still to clean up:**
- The View's font defaults are hardcoded `FiraCode Nerd Font`. Settings
  file would fix this properly; until then, `CODESCOPE_FONT` works.
- `fg_default(palette)` helper is private to `backend.rs` but mirrors
  `palette.foreground`; keep until colours come out of a theme struct.
- No tests on the View layer; only `Backend` has the smoke example.
  Element-layer rewrite is the right time to add unit tests.

### Session 28 — Rust port direction set; spike validates stack; terminal crate scaffolded

**Decision (this session):** start a Rust + gpui port of CodeScope. New work
lives under `codescope-rs/` at the repo root, alongside the C# / WPF source
which keeps shipping. UI/UX target inspired by Zed's editor (gpui's
distinctive feel). The C# stack stays the production target until the Rust
port reaches feature parity.

**Spike (`codescope-rs/`, workspace root with member `terminal/`):**
- One gpui window with one embedded terminal running `pwsh.exe` via
  `portable-pty` (ConPTY on Windows). Built with `gpui = "0.2.2"` and
  `zortax/gpui-terminal` (alacritty_terminal-based).
- `src/main.rs` is the spike binary. `src/bin/ptytest.rs` is a console-only
  diagnostic that validates portable-pty + ConPTY produce bytes from a
  spawned shell.
- `scripts/diagnose.ps1` enumerates VS installs, MSVC tool versions, and
  Windows SDK Lib paths — used to root-cause an `LNK1104 msvcrt.lib` link
  failure on this machine.
- `scripts/build.ps1` wraps `cargo` in a `vcvars64.bat` env from the
  VS 2022 BuildTools install. Was needed before adding the **C++ Desktop**
  workload to the existing VS 2026 (v18) Community install; redundant on
  this machine now but kept for portability.

**Build prereqs** (Windows): rustup, MSVC v143 (VS 2022 BuildTools _or_ VS 2026
Community with the **Desktop development with C++** workload), Windows 10/11
SDK, CMake. Vulkan SDK not required on Windows — gpui targets DirectX 11.1.

**Bug #1 (patched, in `codescope-rs/vendor/gpui-terminal/`):**
`Event::PtyWrite` was silently dropped by `GpuiEventProxy` (comment said
"handled internally by alacritty" — incorrect). alacritty emits this event
when it has a response for the embedder to write _back_ to the PTY (DSR
cursor-position responses, primary DA replies, etc.). Without the response,
cmd.exe / pwsh.exe hang at startup waiting for `ESC[1;1R`. Patch threads a
`PtyWriter` Arc into `GpuiEventProxy` and writes inline from `send_event`.
~30-line diff in `event.rs` + `view.rs`. Local-only; no upstream PR
(per `feedback_no_upstream_prs`).

**Bug #2 (architectural — _not_ patched in vendor):** PSReadLine emits
`ESC[<row>;<col>H` cursor-position sequences in conhost-screen coordinates,
but pwsh's startup clear-screen dance scrolls the alacritty grid by
~30 rows. Result: PSReadLine targets row 3 col 7 while the real prompt sits
at row 29-30 → cursor lands far above the input line. Classic ConPTY
conhost-vs-emulator scroll-sync problem; gpui-terminal makes no attempt
to bridge it. Caught via byte-level PTY logging in
`codescope-rs/src/main.rs` (`LoggingReader` / `LoggingWriter`).

**Decision:** stop investing in `gpui-terminal` patches. Pivot to a
roll-our-own terminal layer modeled on Zed's three-layer architecture
(`Backend` → `View` → `Element`). `gpui-terminal` is single-maintainer,
Linux-only-tested, missing scrollback, missing mouse selection, and now
two known fundamental bugs in two days of poking. Owning the code is
a multi-day investment but eliminates the patch-treadmill.

**Scaffolded (`codescope-rs/terminal/`):**
- Workspace member, crate name `codescope-terminal`.
- `src/lib.rs` reserves the three-layer module surface (`backend`, plus
  future `view` / `element`).
- `src/backend.rs` is a stub `pub struct Backend;` so the workspace
  compiles. Implementation lands next session.
- Direct dep on `alacritty_terminal = "0.25"` (matching what Zed uses);
  `portable-pty` stays only in the spike for now and may go away once
  the backend uses `alacritty_terminal::tty` directly (Zed's pattern).

**Spike status:** functional. `target/debug/spike.exe` opens, pwsh runs,
output renders, basic typing works. Up-arrow history-recall is broken
visually (Bug #2). Mouse selection and scrollback don't exist
(gpui-terminal `Planned`). `claude.exe` in the spike: visually fine,
keyboard interactions inherit Bug #2.

### Suggested next entry points

1. Implement `codescope-terminal::backend::Backend`:
   - Wrap `alacritty_terminal::Term<L>` in `Arc<FairMutex<...>>`.
   - Use alacritty's own `EventLoop` (executor-agnostic, runs on a worker
     thread). Drives both PTY reads and writes.
   - Track conhost scroll offset on Windows: subscribe to scroll events
     from the grid, maintain a `screen_origin_row` so we can translate
     incoming `ESC[<row>;<col>H` from conhost-coords to grid-coords.
2. After backend works, build the View layer (gpui) with input handling.
   The `keystroke_to_bytes` table from gpui-terminal is correct — the
   surrounding plumbing was the bug. Reuse the table, redo the wiring.
3. Element layer: batched-text-runs renderer, similar to Zed's
   `BatchedTextRun`.
4. Validate: pwsh + claude.exe must work cleanly with up-arrow history,
   scroll, mouse selection, and clipboard copy before declaring the
   terminal layer "done".

### Session 27 — fresh-install black-screen / no-terminals fix (PR #50, v0.2.5)

**Symptom:** user installed v0.2.4 on a clean Windows box, saw a black
workspace, and could not initialise any terminal. Same install on a dev box
worked fine.

**Root cause:** `EasyWindowsTerminalControl` → `Microsoft.Terminal.Wpf.dll`
→ `Microsoft.Terminal.Control.dll` (native, under `runtimes/win-x64/native/`)
is built with MSVC and depends on the Visual C++ 2015–2022 Redistributable
(`vcruntime140.dll`, `vcruntime140_1.dll`, `msvcp140.dll`). Velopack does
not bundle VCRedist, so on machines without it the native renderer silently
failed to load → empty `HwndHost` → black workspace, no terminals.

**Fix (ADR-0017, merged in `14be35b`, shipped as v0.2.5):** ship the three
DLLs (~709 KB total) app-local, beside `CodeScope.exe`. App-local DLL
resolution wins over `System32`, so the bundle works whether or not VCRedist
is installed and never interferes with system-wide installs.

- `src/CodeScope.App/native/vcredist/{vcruntime140,vcruntime140_1,msvcp140}.dll`
  committed (v14.50.35719.0). `NOTICE.md` next to them documents version,
  SHA-256, source, and Microsoft's redistribution license.
- `tools/refresh-vcruntime.ps1` re-pulls from `%SystemRoot%\System32` and
  prints fresh versions + hashes. Hard-fails on non-x64 OS or 32-bit
  PowerShell so wrong-arch DLLs never sneak into the win-x64 pipeline
  (Copilot review catch). Uses `System.Security.Cryptography.SHA256`
  directly so it works on stripped-down PS hosts without `Get-FileHash`.
- `CodeScope.App.csproj`: Content glob (`native\vcredist\*.dll`) parallel
  to the existing `conpty.dll` glob — copies for both `dotnet build` and
  `dotnet publish`. Verified: DLLs land next to `CodeScope.exe` in both
  Debug build and Release publish output.
- `tests/CodeScope.App.Tests/VcRuntimeBundleTests.cs` (5 tests) guards
  source DLLs in the repo, copied DLLs in build output, AND the structural
  csproj contract (`CopyToOutputDirectory` + `CopyToPublishDirectory` both
  `PreserveNewest`). A publish-only regression now fails CI.

**Shipped:** v0.2.5 release workflow ran green, all assets attached
(installer, portable, Velopack `full`/`delta` packages). Existing v0.2.4
installs auto-update via Velopack delta; fresh installs work out of the box.

### Sessions 26–28 — perf sweep, cleanup, UX fixes (PRs #38–#49)

A batch of memory/perf fixes, one feature removal, and two UX fixes landed
between sessions 25 and this update. All merged to `main` via PRs; no open
branches. Releases v0.2.0 → v0.2.4 shipped.

**Performance / memory (PRs #38–#46):**
- `43c6ffd` (#38) — **Status-bar hook cleanup.** `_statusBarHookedTabs` /
  `_statusBarHookedWts` HashSets now evict entries on `CollectionChanged.OldItems`
  so closed tabs / removed worktrees are GC-eligible. `WorktreePoller` prune
  path added for `PullRequestStatusPoller` parity.
- `126c2c5` (#40) — **WorktreePoller serialisation.** `SemaphoreSlim(1,1)` gates
  `RefreshAsync` vs. timer `PollAllAsync` to prevent double-probe or skip. Two
  new tests.
- `6057060` (#44) — **OverviewViewModel gating.** `IsActive` flag suppresses
  `RebuildOnUi` while overview is hidden; dirty flag triggers one rebuild on
  re-show. 120 lines of test coverage.
- `b7d9eec` (#45) — **Dev-mode MemoryWatchdog.** 5 min `WorkingSet64` poll,
  warns on ≥ 50 MB growth/tick. Scrollback-cap blocker documented (upstream
  `Microsoft.Terminal.Wpf` doesn't expose the setting).
- `d35191d` (#41) — **Error toast cap.** Hard-cap visible errors at 20; oldest
  drops. CI toasts use stable `"ci-<wt>-<pr#>"` ids → replace-in-place. 82-line
  test file.
- `1882a9b` (#46) — **Session retention policy.** `SessionRetentionPolicy`
  (100/worktree, 90-day TTL) enforced on load + after every soft-close. ADR-0015.
  290-line test file.
- `7349766` (#42) — **Telemetry timer arming.** Four telemetry services now start
  paused; arm/disarm on Register/Unregister. OpenCode gains 2 s "not found" TTL
  on `TryLocateMessageDir`. 89-line test file.
- `15d8e76` (#43) — **OpenCode running aggregates.** Unbounded `List<>` + full
  sort replaced with four running candidates (`LastEntry`, `LastUser`,
  `LastAssistantWithUsage`, `TurnCount`). 77-line test file.

**Feature removal (PR #48):**
- `30070e2` — **Diff panel removed.** `DiffPanelViewModel`, `DiffPanelView`,
  `IGitService.GetDiffAsync`, `Ctrl+D` binding, command palette entry, and the
  HTML design mock all deleted. ADR-0016. No consumers remained.

**UX fixes (PRs #47, #49):**
- `39899e2` (#47) — **Un-maximize on title-bar drag.** Maximized window now
  un-maximizes and follows cursor with correct horizontal fraction preserved.
- `bd6ba22` (#49) — **Dialog pixel snap.** `SnapsToDevicePixels` +
  `UseLayoutRounding` + `TextFormattingMode=Display` on NewWorktree,
  NewProject, CommandPalette dialogs — crisp text at fractional DPI.

**Other (already in session 25 handoff):**
- `2c37645` (#27) — Process tree kill on tab close + conpty.dll glob fix.
- `2437865` — README install-first rewrite.

### Session 25 — taskbar overlay badge (#18, PR #25)

Green/red taskbar overlay badge reflecting aggregate agent activity. ADR in
session 25 detail below; `ITaskbarBadgeService`, `MainViewModel.TaskbarBadge.cs`
partial, 6 tests. Shipped in `22f7982` (v0.2.3).

### Session 24 — Add project from a git URL (#20, PR #23)

"Add project" dialog with clone-from-URL mode; inline spinner + cancel +
inline error. `IGitService.CloneAsync`, `NewProjectDialog`, 4 tests.
Shipped in `e1ee4ea`.

### Session 23 — pi.dev agent support (committed in `26586d0`)

User-visible: a fourth agent ("π  Pi") in the new-session menu that launches
`@mariozechner/pi-coding-agent`, with the same busy/idle status dot, token
progress, and turn-duration read-out the Claude tab gets. Resume-by-id wires
through the existing `--resume`-style plumbing (Pi flag is `--session <uuid>`,
fallback `pi -c` for "continue most recent").

How: Pi's on-disk session-jsonl format is structurally similar to Claude's but
under `~/.pi/agent/sessions/--<encoded-cwd>--/<timestamp>_<uuid>.jsonl` and
with different field names (`message.usage.{input,output,cacheRead,cacheWrite}`,
`stopReason ∈ {stop, tool_use}`, `role ∈ {user, assistant, toolResult}`). Each
file's first line is a `session` header carrying the canonical `cwd`, so
discovery matches by header (canonicalised path comparison) instead of trying
to predict Pi's directory-naming scheme on Windows.

Files added:
- `src/CodeScope.Core/Services/PiTranscriptParser.cs` — pure parser; mirrors
  ClaudeTranscriptParser's contract. Includes `ExtractSessionIdFromFileName`
  that pulls the trailing UUID out of `<ts>_<uuid>.jsonl` names.
- `src/CodeScope.Core/Services/IPiTelemetryService.cs` + `PiTelemetryService.cs`
  — single recursive `FileSystemWatcher` over the entire sessions root so a
  fresh-launch race (Register before pi has written its first line) still
  picks up the file. Token + activity FSM identical to Claude's.
- `src/CodeScope.Core/Services/IPiSessionDiscovery.cs` + `PiSessionDiscovery.cs`
  — header-cwd matching with cross-platform path canonicalisation so
  `C:\dev\x` and `/c/dev/x` collapse to the same key. `since` filter, poll
  fallback, dispose semantics — all mirror `ClaudeSessionDiscovery`.
- 3 test files: `PiTranscriptParserTests` (10), `PiTelemetryServiceTests` (7),
  `PiSessionDiscoveryTests` (7) — all green.

Files touched:
- `src/CodeScope.Core/Services/AgentRegistry.cs` — Pi profile in `BuildDefaults`:
  `Command="pi"`, `ResumeArgs=["-c"]`, `ResumeByIdArgs=["--session"]`,
  `SessionIdFlag=null`, `Icon="π"`. Default agent stays Claude.
- `src/CodeScope.App/App.xaml.cs` — register `IPiTelemetryService` +
  `IPiSessionDiscovery`; pass to `MainViewModel`.
- `src/CodeScope.Ui/ViewModels/MainViewModel.cs`:
  - Optional `IPiTelemetryService` / `IPiSessionDiscovery` ctor params; both
    services emit `ClaudeSessionTelemetry` so `OnTelemetryUpdated` doesn't care.
  - `BeginClaudeAdoption` now branches on agent id (claude vs pi) and uses the
    appropriate discovery service. Same handle-management as before.
  - `ApplyAdoption` is the shared callback that persists the new id + (re-)
    registers the right telemetry tail.
  - `RegisterAgentTelemetry` / `UnregisterAgentTelemetry` route by agent id;
    callers (`HydrateFromLoaded`, `RestoreClosedWorktreeSessionsAsync`,
    `TryRestoreSessionAsync`, `CloseTabAsync`) no longer hard-code Claude.
- `tests/CodeScope.Core.Tests/AgentRegistryTests.cs` — Pi assertions added.

Open follow-ups:
- Pi's path-encoding on Windows is genuinely undocumented; we sidestep the
  problem by content-matching the header. If Pi ever drops the header line,
  this whole approach falls over — file an issue at that point, not before.
- `ContextWindowTokens` for Pi is left at 0 (model-agnostic CLI). The status
  bar's percent indicator stays blank for Pi until we surface a per-tab cap
  override, which is its own design problem.
- Committed in `26586d0` together with session 23b (shared files in
  AgentRegistry/MainViewModel/App.xaml.cs couldn't be split cleanly).

### Session 23b — opencode-cli telemetry parity (committed in `26586d0`)

User-visible: the OpenCode tab now gets the same status-dot + token + turn
read-out the Claude / Pi tabs have. Resume-by-id wired through the existing
descriptor plumbing (`opencode --session <id>`).

Why per-message JSON instead of JSONL: OpenCode persists each message as its
own file under `%USERPROFILE%\.local\share\opencode\project\<slug>\storage\
message\<sessionId>\msg_<id>.json`. The schema is upstream-defined in
`packages/opencode/src/session/message.ts` (Effect Schema.Struct):
`metadata.assistant.{tokens.{input,output,reasoning,cache.{read,write}}, modelID,
providerID, path.{cwd,root}, cost}` + `metadata.time.{created,completed?}`.
Tool-pending state flows from a part with `type:"tool-invocation"` whose
`toolInvocation.state` is `call` or `partial-call`.

Files added:
- `src/CodeScope.Core/Services/OpenCodeMessageParser.cs` — pure parser with
  `ExtractMessageIdFromFileName` for `msg_*.json` names and pending-tool
  detection on the `parts` array.
- `src/CodeScope.Core/Services/IOpenCodeTelemetryService.cs` +
  `OpenCodeTelemetryService.cs` — recursive root watcher; locates the right
  `message/<sessionId>/` dir via path-segment match (parent must be `message`),
  re-aggregates every Recompute by sorting in-memory entries on
  `metadata.time.created`. Activity FSM: latest message role/state drives
  Composing / PendingToolUse / Idle.
- `src/CodeScope.Core/Services/IOpenCodeSessionDiscovery.cs` +
  `OpenCodeSessionDiscovery.cs` — header-cwd matching reuses
  `PiSessionDiscovery.CanonicalizePath` (same cross-platform path problem).
  Adoption fires once per session id, on the first assistant message because
  that's where `metadata.assistant.path.cwd` first appears.
- 3 test files: `OpenCodeMessageParserTests` (8), `OpenCodeTelemetryServiceTests`
  (6), `OpenCodeSessionDiscoveryTests` (6) — all green.

Files touched:
- `AgentRegistry.cs` — opencode profile gains `ResumeByIdArgs=["--session"]`
  (was missing); Command/Icon/ResumeArgs unchanged.
- `App.xaml.cs` — register `IOpenCodeTelemetryService` +
  `IOpenCodeSessionDiscovery`; pass to `MainViewModel`.
- `MainViewModel.cs` — `BeginClaudeAdoption` gains an `opencode` branch;
  `RegisterAgentTelemetry` / `UnregisterAgentTelemetry` route opencode too.
  Same `ApplyAdoption` shared-callback shape as Claude/Pi.
- `AgentRegistryTests.cs` — opencode profile assertion added.

Open follow-ups:
- The Effect Schema upstream uses Unix-ms numbers for `time.created/completed`;
  the parser handles those as `DateTimeOffset.FromUnixTimeMilliseconds`. If
  OpenCode ever switches to ISO strings, parser returns null timestamps but
  the rest still works.
- OpenCode session-list browser (`opencode session list`) isn't surfaced in
  CodeScope — the user can still hit it from inside the tab via `/sessions`.
- Same `ContextWindowTokens` caveat as Pi (model-agnostic CLI), unless a
  Claude/known model lights up `ClaudeModelCatalog`.

---

## Cursor — current focus

**Performance sweep complete.** Sessions 26–28 landed a broad memory/perf
pass (11 PRs, #38–#49): telemetry timers only arm when watches exist,
closed-session retention is bounded (100/wt, 90-day TTL), overview rebuilds
are gated on visibility, OpenCode telemetry uses running aggregates instead
of unbounded lists, error toasts are hard-capped, status-bar hooks are
cleaned up on tab/worktree removal, and WorktreePoller refresh is serialised.
Diff panel removed entirely (ADR-0016). Two UX fixes: un-maximize on
title-bar drag and dialog pixel snapping at fractional DPI.

All open GitHub issues are closed. `main` is clean, 420 tests green,
v0.2.4 released.

### Session 22 — cross-group terminal drag pool (shipped)

User-visible: drag a tab from group A to group B → no flash, no respawn,
scrollback retained, prompt and agent state untouched. Hand-tested by the
user against the dev build; works.

Files touched:
- `src/CodeScope.Ui/Services/ISessionViewHostPool.cs` (new)
- `src/CodeScope.Ui/Services/SessionViewHostPool.cs` (new)
- `src/CodeScope.Ui/Views/SessionTabView.xaml.cs` — drop `Unloaded` teardown
  hook; rename `TeardownShell` → `internal Teardown`. Doc the why (Unloaded
  fires on every reparent — pool calls Teardown on Release instead).
- `src/CodeScope.Ui/Views/EditorGroupView.xaml` — replace per-tab
  `ItemsControl` with a single `ContentControl x:Name="ActiveSlot"`.
- `src/CodeScope.Ui/Views/EditorGroupView.xaml.cs` — subscribe to
  `EditorGroupViewModel.SelectedTab` change, resolve view via pool, set as
  `ActiveSlot.Content`. Detach (clear `Content`) on Unloaded — never tear
  down the view itself; the pool owns it.
- `src/CodeScope.Ui/ViewModels/MainViewModel.cs` — accept
  `ISessionViewHostPool?` ctor param; expose as `SessionViewPool`. Drop the
  agent-resume / telemetry-rebind block in `MoveTabToGroup`. Call
  `SessionViewPool?.Release(tab.Descriptor.Id)` in `CloseTabAsync` (covers
  soft-close, hard-remove, restart, and worktree-cascade since cascade routes
  through `CloseTabAsync`).
- `src/CodeScope.App/App.xaml.cs` — register
  `ISessionViewHostPool → SessionViewHostPool` singleton; pass to
  `MainViewModel`.
- `docs/DECISIONS.md` — ADR-0014.

Build/tests: `dotnet build` clean; `dotnet test` 288/288 green (no test
changes needed — `Core` doesn't see UI; `Ui.Tests` doesn't exercise
`MainViewModel`). The pool itself is a 30-line dictionary wrapper; relying
on the smoke test (the actual win is HWND survival, which only a real WPF
process can validate).

### Session 21 — per-worktree session history surface (shipped)

**Per-worktree session history surface.** Session 21 shipped a full
soft-close gate + history UI: every session (agent and shell) now soft-closes
on tab-close, auto-resume on "New session" was removed, and a collapsible
history disclosure appears under each worktree in the sidebar (only when
`count > 0`). Users reopen past sessions via double-click / right-click on a
history row, or via the worktree context-menu shortcut "Reopen most recent
closed session". Branch `feature/session-history-per-worktree` is local-only;
next step is push + PR to `main`.

### Session 21 — per-worktree session history surface (shipped)

Spec: `docs/superpowers/specs/2026-04-26-session-history-design.md`
Plan: `docs/superpowers/plans/2026-04-26-session-history-per-worktree.md`
ADR: `docs/DECISIONS.md` § ADR-0013 — auto-resume removed in favour of explicit history.

**Behaviour change for users:** "New session" always mints a fresh terminal —
it no longer auto-resumes the previous agent session. Past sessions accumulate
in a per-worktree history list (collapsed by default, shown only when
`count > 0`) in the sidebar disclosure row. Reopen options:
  - Double-click or right-click → "Reopen" on a history row.
  - Worktree context menu → "Reopen most recent closed session" (keyboard-friendly shortcut).

Commits (oldest → newest):

- `52f944d` — **Soft-close every session, including shells.** `SessionStore.SoftCloseSessionAsync` soft-closes every session on tab-close; all tab-close paths (single close, group close, worktree cascade) now call it. Shell sessions participate in soft-close alongside agent sessions.
- `ca3833a` — **Remove implicit auto-resume on New session.** `SessionManager.CreateShellSession` / `CreateAgentSession` always produce fresh sessions; the auto-resume lookup in `NewSessionAsync` was removed. Each new session is a blank slate. Enables predictable history accumulation.
- `b412046` — **WorktreeViewModel.History collection scaffold.** Adds `ObservableCollection<SessionTabViewModel>` (the same VM type reused for history rows) to `WorktreeViewModel`, wired from the store projection loop.
- `c6745b6` — **Project closed sessions into WorktreeViewModel.History.** Sidebar VM projection maps closed sessions onto the owning worktree's `WorktreeViewModel.History` list; `SessionSoftClosed` events demote live rows to history race-free; `SessionAdded` promotes them back on restore.
- `975ca53` — **Render closed-session history under each worktree.** Worktree `DataTemplate` became a `HierarchicalDataTemplate` with `ItemsSource={Binding History}`; history rows use a `DataTemplate` for `SessionTabViewModel` with dim opacity, an outline dot, and a relative timestamp. TreeViewItem expand/collapse drives the disclosure — no `Expander` control.
- `3fbe388` — **ReopenClosedSessionAsync command + shell-aware restore.** `MainViewModel.ReopenClosedSessionCommand` looks up the session by id, does pre-flight checks (agent profile, directory), and delegates to `TryRestoreSessionAsync` which spawns the tab with `--resume` for agents or a fresh `pwsh` for shells.
- `bb22c8a` — **Double-click + right-click reopen + manage closed sessions.** History row in sidebar responds to double-click (reopen) and right-click context menu ("Reopen" / "Rename…" / "Remove from history"). Drag-between-groups verified untouched — different code path, explicitly re-tested per spec requirement.
- `aef072a` — **Worktree menu shortcut for most-recent reopen.** Worktree context menu gains "Reopen most recent closed session" entry (visible only when `WorktreeViewModel.History.Count > 0`); bound to `ReopenClosedSessionCommand` with the first history row's id.

**Test coverage added/updated this session:**
- `52f944d` — `test(session-store): cover cascade-removal of closed sessions` (new `SessionStoreTests` covering soft-close, cascade on worktree removal, and re-open removal)
- Total 280/281 green (281 tests; 1 pre-existing FSWatcher flake on `main` — not a regression)

### Session 20 — post-v0.1.0 bugfixes + dev-mode side-by-side (shipped)

- `7e284ae` — **Six-in-one bundle** (separated here, kept together because
  the shared files `MainViewModel.cs` and `SessionTabView.xaml.cs` touch
  multiple concerns and splitting would have required hunk-level staging):
  * **Worktree delete cascade.** Right-click → Remove now closes any open
    tabs pinned to the worktree first (via a `CloseWorktreeSessionsAsync`
    callback the `MainViewModel` wires onto the sidebar VM) so pwsh
    releases its Windows cwd lock before `git worktree remove` runs.
    Failure surfaces as a toast plus an optional `--force` retry dialog.
    `SessionTabView` gained an `Unloaded` teardown
    (`DisconnectConPTYTerm` + `CloseStdinToApp` + `StopExternalTermOnly`)
    so the ConPTY child actually dies when the tab VM leaves its group.
    `IGitService.RemoveWorktreeAsync` + `ISessionStore.RemoveWorktreeAsync`
    grew a `bool force = false` param; existing NSubstitute stub widened.
  * **Terminal clipboard.** Windows-Terminal semantics in a
    `PreviewKeyDown` handler: `Ctrl+C` copies on a non-empty selection
    else falls through as SIGINT; `Ctrl+Shift+C` always copies;
    `Ctrl+V` / `Ctrl+Shift+V` paste with `\r\n` → `\r` normalisation
    (otherwise each CR is an Enter and multiline pastes emit blank
    commands). Clipboard contention gets one retry.
  * **URL open via selection.** `Ctrl+Shift+O` on a selected URL opens
    the default browser via `Process.Start(UseShellExecute=true)`.
    Multi-line terminal-wrapped URLs are stitched before validation.
    A WPF ContextMenu was prototyped for right-click access but is
    unreachable — see rough-edges below.
  * **Drag-resume Claude cross-group.** `AgentRegistry` restored
    `ResumeByIdArgs = ["--resume"]` for the claude profile so a
    cross-group drop relaunches `claude --resume <id>` instead of a
    bare session. `MainViewModel.MoveTabToGroup` preserves the
    telemetry tail when we can resume by id (same jsonl gets appended).
  * **Status-bar token semantics.** `ClaudeSessionTelemetry.TotalTokens`
    → `ContextTokens`, semantically the *latest* assistant turn's
    `input + cache_read + cache_creation + output` (overwrite, not
    accumulate). Claude is stateless per request so each turn's
    `input_tokens` already covers the prior conversation — summing
    double-counts and blew the % past 100 within a handful of turns.
  * **Dev-mode isolation.** New `NoScope.CodeScope.Core.AppPaths`
    resolves `CODESCOPE_DEV=1` at process start and redirects the
    single-instance mutex, `%APPDATA%\CodeScope\`, and
    `%LOCALAPPDATA%\CodeScope\` to `.Dev` variants. Window title
    suffix `[dev]`. CLAUDE.md documents the workflow.
- `3bef676` — **Drop dead WPF ContextMenu.** Right-click on the terminal
  couldn't open any WPF context menu: `Microsoft.Terminal.Wpf`'s HwndHost
  child captures `WM_RBUTTONUP` at the native layer, so WPF's routed
  mouse events (`Grid.ContextMenu`, `PreviewMouseRightButtonUp`, and a
  top-level `HwndSource.AddHook`) all see nothing. Confirmed via wpf-cli
  — right-click pasted the clipboard into pwsh via the terminal's
  own right-click-to-paste path. Keyboard shortcuts remain as the
  supported clipboard/URL surface.
- `a5aefea` — **Park right-click research** in the rough-edges list with
  four fix options (native HWND subclass / `Win32InputMode` flip /
  keyboard-only fallback / upstream event) so the next pass doesn't
  re-derive them.
- `d75097d` — **Model catalog: Opus 4.x defaults to 1M.** Real Claude
  Code CLI transcripts carry `message.model = claude-opus-4-7` with no
  `[1m]` tag — the 1M SKU is selected out of band. The old substring
  rule matched `claude && opus` and returned 200k, so the status bar
  divided ~30k context by the wrong cap and reported ~15% while Claude
  Code itself showed ~3% against the real 1M cap. Catalog now: explicit
  `1m` tag wins, sonnet/haiku stay 200k, `claude-3*` is 200k, every
  other `claude-opus-*` defaults to 1M.
- `bd882eb` — **Continuous adoption for `/clear` rotations.**
  `ClaudeSessionDiscovery.Watch` was one-shot — after the first
  adoption it disposed. Claude rotates its session id on `/clear` by
  writing a brand new jsonl in the same cwd dir, so every `/clear`
  stranded telemetry on the abandoned transcript and the status bar
  froze. `WatchHandle` now tracks fired paths in a set and keeps the
  watch alive for the tab's lifetime; `MainViewModel.BeginClaudeAdoption`
  short-circuits when the adopted id already matches the persisted one
  (stops churn on startup/poll re-fires) and otherwise unregisters the
  old telemetry, registers the new id, and persists. Single-tab
  `/clear` verified in the dev build against a real Opus 4.7 session.
  Multi-tab same-cwd is an accepted limitation: both tabs would see
  a rotation event for either; flag if it ever surfaces.

**Test coverage added/updated this session:**
- `SessionStoreTests` — mock signature widened for `force` param
- `ClaudeTelemetryServiceTests.ContextTokens` — expects latest-turn
  context size (was cumulative sum)
- `ClaudeModelCatalogTests` — Opus 4.x ids now expect 1M
- `ClaudeSessionDiscoveryTests.Callback_Fires_For_Each_New_Jsonl…` — new,
  covers the `/clear` rotation path
- Total 147/147 green (145 pre-session + 2 new)

### Session 19 — Velopack + GitHub Actions release pipeline (shipped)

- `a0df3cc` (PR #1) — **First distributable build.** Five-part landing:
  * `Velopack` 0.0.1298 package added to `CodeScope.App`;
    `VelopackApp.Build().Run()` runs in the `App` ctor before any WPF
    state so the installer/updater handoff args
    (`--veloapp-install/-uninstall/-obsolete/-firstrun`) short-circuit
    the main UI.
  * **`conpty.dll` publish bug fixed.** `Ci.Microsoft.Windows.Console.ConPTY`
    only ships `win10-x64` RID assets and .NET 10 no longer falls back
    from `win-x64` (NETSDK1206), so the DLL never reached the publish
    folder and installed builds would've killed every terminal tab
    instantly with `DllNotFoundException`. Swapped the fragile
    `AfterBuild` `Copy` target for a `Content` include pulled straight
    from `$(NuGetPackageRoot)`, with both `CopyToOutputDirectory` and
    `CopyToPublishDirectory=PreserveNewest`. Now flows through `dotnet
    build` and `dotnet publish` alike.
  * `.github/workflows/release.yml` — tag-triggered (`v[0-9]+.[0-9]+.[0-9]+`,
    plus pre-release tag pattern + `workflow_dispatch`). `windows-latest`,
    `dotnet-version: 10.0.x`, `dotnet tool install -g vpk`, publishes
    self-contained win-x64 loose files (`PublishSingleFile=false` — Velopack
    needs loose files for delta updates), downloads prior release
    (`vpk download github`, `continue-on-error` for first release),
    `vpk pack` with `--icon`, `--splashImage`, `--channel win`,
    `--packAuthors maui1911`, then `vpk upload github --publish`
    creates the GitHub Release.
  * `tools/release.ps1` — local counterpart to CI flow for iteration
    without tagging.
  * **Custom installer splash.** 640×400 PNG rendered from
    `src/CodeScope.App/assets/splash.html` via Playwright. Design-token
    matched: black bg, accent-blue radial glow + grid, 40 px brand mark
    with the cut-corner (matches sidebar `brand-mark` token), "INSTALLING"
    pill, "Spinning up a workspace for your parallel agent sessions"
    headline, terminal-style activity log bottom-right. Source HTML is
    checked in so the splash can be re-rendered when the visual language
    evolves — re-run Playwright with `setContent` against `splash.html`
    and overwrite the PNG.

  **Release timings:** 2 min 32 s wall-clock from tag push to release
  published. Release artefacts on v0.1.0 total ~220 MB across Setup,
  Portable ZIP, full nupkg, RELEASES, and `releases.win.json`.

**Claude telemetry → status bar + sidebar + tab strip** was a
six-session arc (10 → 16). Status-bar spec checklist is now
fully cleared — the notifications cluster (#11) landed this session.
Transcript JSONL
tail (`~/.claude/projects/<encoded-cwd>/<session_id>.jsonl`) feeds
per-session tokens, context-window %, turn count, wall-clock, and a
4-state activity FSM (Unknown / Idle / PendingToolUse / Composing).
Wait pulse now lights up on the tab dot, sidebar dot, sidebar status
label, project row, and status-bar dot in unison — all from one
`ClaudeTelemetryService` + one `ApplyActivityToStatus`. Polling
fallback closed the 100–500 ms FSWatcher latency, and the context
cap is now model-aware instead of a baked 1M default.

### Session 18 — ConPTY std-handle rebind + agent picker (shipped)

- `8edf392` — **The actual "Session Terminated" fix.** Symptom:
  shell + claude tabs both died instantly, terminal pane read
  "Session Terminated". Earlier `AllocConsole()` workaround was
  necessary-but-not-sufficient: `AllocConsole` creates `CONIN$` /
  `CONOUT$` but doesn't rebind the process's `STD_INPUT_HANDLE` /
  `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE` when the parent launcher
  redirected those pipes (bash pipes, wpf-cli launch, VS
  run-with-redirect). ConPTY children inherited the stale redirected
  handles, saw non-TTY stdin/stdout, and bailed — pwsh quietly,
  claude v2.1.118+ loudly with "Input must be provided… --print".
  `App.EnsureHiddenConsole` now `FreeConsole`s any inherited console,
  `AllocConsole`s a fresh hidden one, then `CreateFile("CONIN$"/"CONOUT$")`
  with `GENERIC_READ|WRITE` + `FILE_SHARE_READ|WRITE` and pushes both
  handles into the three std slots via `SetStdHandle`. Writes a
  `HH:mm:ss inOk=true outOk=true` breadcrumb to
  `%LOCALAPPDATA%\CodeScope\console.log` so regressions are
  diagnosable without a debugger. Ref:
  github.com/microsoft/terminal/issues/11276. Same commit fixes two
  collateral items: `SessionManager.CreateShellSession` gains
  `-NoExit -NoLogo` to match the agent-session launch (belt-and-braces
  so pwsh stays alive even if the std-handle fix ever regresses), and
  the sidebar's worktree-context "New session" / "Open in new group"
  are now agent-picker submenus (Default · Shell · Claude Code · Codex ·
  OpenCode). New `MainViewModel.OpenInNewGroupWithAgentCommand` +
  `BuildAgentChoices` helper in `SidebarView.xaml.cs`. Subtle gotcha
  noted in code: `Tag="primary"` on a submenu root overrides the
  default `SubmenuHeader` template via `Ctx.Menu.PrimaryTemplate` and
  strips the chevron — don't set it on submenu parents, only leaves.

### Sessions 13–18 (one-liners — see `git log` for detail)

| # | Gist |
|---|---|
| 18 | **ConPTY std-handle rebind + agent picker** (`8edf392`) — fresh `AllocConsole` + `SetStdHandle(CONIN$/CONOUT$)` so pwsh/claude don't die on inherited redirected stdio. Worktree-ctx "New session" / "Open in new group" turned into agent submenus. |
| 17 | **Claude CLI session-id adoption** (`86f874b`) — stop owning the id, adopt from disk via `IClaudeSessionDiscovery` FSWatcher tail. `AgentRegistry` drops Claude's resume flags (all bare launches); status bar wakes via `Register(adoptedId, workingDir)`. 6 discovery tests. *(Superseded in session 20: continuous adoption + resume-by-id restored.)* |
| 16 | **Notifications cluster** (`49fdd60`) — `INotificationService` ring buffer (cap 50), status-bar bell + 360 px popover, activity-driven pushes on `→PendingToolUse` / `→Idle`. Entry click jumps via Claude AgentSessionId match. |
| 15 | **Telemetry polish + chrome polish** (`abf4e45` poll fallback, `4b45770` per-model cap, `b97788c` caption margin, `5239ea0` empty-project placeholder, `b551730` Fig token pass, `39d2786` diff panel gutters, `071da29` single-rail tab motion). |
| 14 | **Wait-state propagation** (`792505f` sidebar → project → worktree rollup, `0400fda` top-bar tab-dot halo pulse). Note: 100–500 ms FSWatcher latency is accepted; poll fallback landed in 15. |
| 13 | **Transcript-driven activity FSM** (`2ee7b1d` + `3b0f5b4`) — user → Composing; assistant end_turn → Idle; assistant tool_use → PendingToolUse. |

### Earlier sessions (one-liners)

| # | Gist |
|---|---|
| 12 | **Context-window cap + wall-clock.** `AgentProfile.ContextWindowTokens` (Claude = 1M), `LastTurnDuration`, status bar renders `<used>/<cap> · <pct>%` + `<clock>`. |
| 11 | **Claude telemetry end-to-end.** `ClaudeTranscriptParser` + `ClaudeTelemetryService` (FSWatcher + tracked offset), per-session tokens + turn count on status bar. `e175a41` fix: encode `.` → `-` in transcript-dir name. |
| 10 | **Structured status bar** (spec §1-12, callouts #8/9/11 deferred) + persist Claude `session_id`, resume by it. |
| 9 | **Chrome visual pass** — titlebar caption buttons, tab-strip squaring, sidebar rail alignment, brand icon. |
| 8 | **New Worktree dialog overhaul** — base-branch picker, spawn-session toggle, `$`-prefixed path input, in-app classifier caption. |
| 7 | **Resizable GridSplitters** between editor groups + two sidebar ctx-menu bugfixes. |
| 6 | Multi-group **hardening**: drag-between-groups, agent resume, layout persistence, chrome iteration. |
| 5 | **Multi-group split v1** (`Ctrl+\`, split-right, `Ctrl+Shift+↵`, `Alt+←/→`, auto-collapse on last-close). |
| 4 | **Sidebar spec pass** — status dot + wait pulse, 240px width, rail flush, flatten to leaves, chevron + 40px foot CTA, top-row drag restored. |
| 3 | **Top-bar spec** — tab strip, hover close-×, AutomationIds, wheel-scroll, middle-click close. |
| 2 | **Fase 7 Framer token layer** + dialogs + refresh-pill fix; HTML-bundle iteration (overview grid, brand cell, sidebar parity, ctx-menu pixel parity). |
| 1 | Fases 1–6 (see roadmap below). |

## Roadmap state

| Fase | Scope | Status |
|---|---|---|
| 1 | WPF shell, tabs, persistence, Win32 job object | ✅ shipped |
| 2 | Sidebar, agent registry, rename, `ISessionStore` | ✅ shipped |
| 3 | Worktree CRUD, nested tree, branch-aware titles | ✅ shipped |
| 4 | Live git status, dirty/ahead/behind indicators | ✅ shipped |
| 5 | `gh` / `tea` PR integration, CI status, Create PR | ✅ shipped (Gitea CI rollup deferred) |
| 6 | Toasts, command palette, adaptive pollers, 50+ parallel sessions | ✅ shipped (session-exit toasts deferred) |
| 7 | Design overhaul using `docs/DESIGN.md` | 🔶 in progress |

## Repo shape

```
src/
  CodeScope.App        WPF host + DI + MainWindow + pollers
                       Styles/DesignTokens.xaml (Framer + Fig tokens)
                       Styles/ContextMenuStyles.xaml (sidebar ctx menu)
                       Diagnostics/MemoryWatchdog (dev-mode only)
                       Toasts/ToastService (hard-capped visible errors)
                       WindowGeometryStore, LayoutStore
  CodeScope.Core       ISessionStore, IGitService, IAgentRegistry,
                       IProjectStore, IPullRequestService (GH/Gitea/composite),
                       ISessionManager, IClaudeTelemetryService,
                       IPiTelemetryService, IOpenCodeTelemetryService,
                       ICopilotTelemetryService, SessionRetentionPolicy
                       Models: Project, Worktree, Session, WorktreeStatus,
                               AgentProfile, PullRequestInfo, CiStatus,
                               BranchInfo, ClaudeActivityState
                       Interop/ProcessTreeKiller (Win32 job)
  CodeScope.Ui         VMs: Main(+StatusBar +TaskbarBadge +Palette partials),
                            Sidebar, Project, Worktree, SessionTab,
                            CommandPalette, Overview*, EditorGroup
                       Views: SidebarView, SessionTabView, OverviewView,
                              GroupStripView, StatusBarView, EditorGroupView
                       Dialogs: RenameDialog, NewWorktreeDialog,
                                NewProjectDialog, CommandPaletteDialog
                       Services: ITaskbarBadgeService, ISessionViewHostPool
tests/
  CodeScope.Core.Tests 248 passing (1 FSWatcher flake) — Core only
  CodeScope.Ui.Tests   145 passing
  CodeScope.App.Tests   27 passing
docs/
  DECISIONS.md (16 ADRs), design/DESIGN.md
  HANDOFF.md (this file)
  screenshots/        README assets
  design/html/        HTML mocks + codescope-shared.css — pixel truth for design passes
```

## User preferences (non-negotiable)

- **Full autonomy:** no clarifying questions; sensible defaults; commit freely. (`memory/feedback_autonomy.md`)
- **Don't pause for inter-feature summaries** — only at real checkpoints. (`memory/feedback_keep_going.md`)
- **Working on `main`:** direct commits; one `git commit` call per commit (harness denies compound). (`memory/feedback_commit_messages.md`)
- **Never `git add -A`** — use `git add -u` + explicit paths (scratch files in repo root). (`memory/feedback_git_add.md`)
- GitHub PRs / issues / code / comments → **English**. User speaks Dutch/English.
- Absolute paths, never `cd`.
- .NET 10 / C# 14 / WPF. Wpf.Ui + EasyWindowsTerminalControl + shell-out to git + System.Text.Json + CommunityToolkit.Mvvm are non-negotiable (see CLAUDE.md).

## Known rough edges

- **"Remove from history" has no confirm dialog** — the action is destructive (entry cannot be recovered) but currently fires immediately on click. A follow-up should add a small inline confirm or an Undo toast. Low priority until the history feature is user-tested.
- **Gitea CI rollup is always `None`** — `tea pulls status`/REST integration deferred.
- **Session-exit toasts deferred** — `SessionManager` starts pwsh with `-NoExit`; detecting agent exit needs `SessionManager` refactor or pty-output parsing.
- **Terminal scrollback unbounded** — `Microsoft.Terminal.Wpf` 1.22 doesn't
  expose a public scrollback-line cap. Long-running sessions grow
  `WorkingSet64` without bound. `MemoryWatchdog` (dev-mode only) logs
  warnings when growth ≥ 50 MB/tick. Upstream change or fork required.
- **Terminal right-click opens no context menu** — parked for a dedicated
  research pass. `Microsoft.Terminal.Wpf`'s HwndHost child captures
  `WM_RBUTTONUP` at the native layer. Keyboard shortcuts (`Ctrl+Shift+O` /
  `Ctrl+Shift+C` / `Ctrl+Shift+V`) are the supported surface.
  Options: native HWND subclass, `Win32InputMode` flip, keyboard-only
  fallback, or upstream event. Context-menu XAML prototyped and reverted
  in `3bef676`.

## Dev loop quick-reference

```pwsh
# Build & test
dotnet build CodeScope.sln -c Debug
dotnet test  CodeScope.sln -c Debug

# Run (close any running instance first — DLL lock)
dotnet run --project src/CodeScope.App

# Smoke via wpf-cli (Windows-only)
wpf-cli launch "C:\dev\codescope\src\CodeScope.App\bin\Debug\net10.0-windows\CodeScope.exe"
wpf-cli screenshot shot.png
wpf-cli close

# Single-file release
dotnet publish src/CodeScope.App -c Release -r win-x64 `
    -p:PublishSingleFile=true -p:PublishTrimmed=true -p:PublishReadyToRun=true
```

## Next — suggested entry points

**Immediate:**

- **"Remove from history" confirm** — add an inline confirm or Undo toast so
  the destructive action is recoverable (see rough edges below).

**Multi-group / chrome polish:**

- **Tab drag-reorder motion spec** — `docs/design/html/CodeScope - Tab Drag.html`.
- **Tab Drag — floating drag chip adorner** — custom `Adorner` that
  follows the cursor, renders the tab replica with a −1.5° rotation +
  blue outer glow. 3 px blue drop-indicator between tabs needs a
  `DragOver` calc + dynamic `Rectangle` in the strip's ItemsPanel.

**Deferred / longer-horizon:**

- Session-exit toasts via `SessionManager` refactor.
- Real Gitea CI rollup (`tea pulls status` / REST).
- PR review-comments dialog (`gh pr view --comments`).
- Kanban overview of active sessions.
- **In-app update notifier** — `UpdateManager.CheckForUpdatesAsync` on
  startup (+ throttled poll), surface a status-bar pill / toast when a
  newer release is available, one-click apply via
  `ApplyUpdatesAndRestart`. The release pipeline is live — this is just
  wiring the client side to it.
- **Terminal scrollback cap** — upstream `Microsoft.Terminal.Wpf` doesn't
  expose a public scrollback-line limit; requires upstream change or fork
  (documented in `SessionTabView.xaml.cs` and `MemoryWatchdog.cs`).
