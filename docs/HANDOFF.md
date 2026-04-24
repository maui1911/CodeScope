# CodeScope — Session Handoff

> Keep this file honest and tight. Updated at the end of every session.
> It is the first thing a fresh Claude Code session should read after `CLAUDE.md`.
> **Intent:** cursor + last 1–2 sessions in depth, everything else a one-liner.
> Old detail lives in `git log` — don't duplicate it here.

**Last updated:** 2026-04-24 (session 19)
**Branch:** `main`.
**Head:** `a0df3cc` — `Add Velopack distribution and v0.1.0 release workflow (#1)`
**Release:** `v0.1.0` shipped via GitHub Actions — https://github.com/maui1911/CodeScope/releases/tag/v0.1.0
**Build status:** ✅ `dotnet build` clean. `dotnet test` 145/145 green.
**Uncommitted work:** none.
**Unpushed commits:** none — pushed via PR #1.

---

## Cursor — current focus

**Distribution is live.** Session 19 wired Velopack end-to-end and cut the
first public release. Push a SemVer tag `v*.*.*` and CI handles the rest:
`dotnet publish` → `vpk pack` (auto-downloads the prior release first so
delta nupkgs compute) → `vpk upload github` creates the Release with
installer + full/delta packages + `releases.win.json` attached. For v0.1.1+
the flow is: commit → `git tag v0.1.1` → `git push origin v0.1.1`.

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

### Session 17 — Claude CLI v2.1.118 adoption fix (shipped)

- `86f874b` — **Claude Code v2.1.118 rotates session ids.** Two
  symptoms the user hit on cold hydrate: (a) `claude --continue`
  errored with `No deferred tool marker found in the resumed session`
  and exited, so every Claude tab terminated instantly; (b) persisted
  `agentSessionId` values in `projects.json` pointed at abandoned
  transcripts (Claude rotates the UUID on `/clear` and on resume), so
  the telemetry tail had nothing to watch — status dot, tokens, turns,
  activity FSM, Wait pulse, and notifications all went dark. New
  shape: **stop owning the id, adopt from disk.** `IClaudeSessionDiscovery`
  + `ClaudeSessionDiscovery` do a one-shot FSWatcher+350 ms poll over
  `~/.claude/projects/<enc-cwd>/*.jsonl`, filter by `CreationTimeUtc
  >= since`, fire once with `(adoptedId, path)`, then self-dispose.
  `AgentRegistry` drops Claude's `SessionIdFlag` / `ResumeArgs` /
  `ResumeByIdArgs` — `SessionManager` now launches a bare
  `pwsh -Command "& { claude }"` from every path (fresh, duplicate,
  cross-group drag, hydrate). `MainViewModel.BeginClaudeAdoption`
  starts a watch per tab, dispatcher-marshals the callback, calls
  `ISessionStore.UpdateAgentSessionIdAsync` to persist the adopted id,
  then `IClaudeTelemetryService.Register` so the status bar wakes up.
  Watches are torn down on adoption, tab close, and cross-group drop.
  Old persisted ids become harmless — overwritten on first launch.
  6 new unit tests cover fresh-create, pre-existing-via-poll, age
  filter, non-UUID rejection, once-only firing, dispose-before-discovery.
  Total 145/145 green.

### Session 16 — notifications cluster (shipped)

- `49fdd60` — **Status-bar bell + popover, activity-driven queue.**
  Core gains `INotificationService` / `NotificationService` —
  thread-safe in-memory ring buffer (cap 50) with `Push`, `MarkRead`,
  `MarkSessionRead`, `MarkAllRead`, `Clear`, and a single `Changed`
  event. `NotificationEntry` carries id + sessionId + kind
  (SessionReady / SessionWaiting / Generic) + title + detail +
  timestamp + IsRead. `MainViewModel` tracks each session's last
  `ClaudeActivityState` and pushes entries on semantic transitions:
  * → `PendingToolUse` → "Needs attention"; `PendingToolUse` /
  `Composing` → `Idle` → "Ready · Turn complete · <duration>".
  Suppresses the push when the transitioning tab is already the
  window-selected one; focusing a tab calls `MarkSessionRead` for
  its AgentSessionId so badges clear on reveal. `StatusBarView.xaml`
  gains the rightmost cluster — 1×14 separator + bell-glyph Button
  (12×12 path per spec §11) + 4 px Accent.Primary unread dot
  overlaid top-right. Clicking the bell opens a 360 px `Popup`
  (`Placement=Top`, fade, `StaysOpen=False`, 8 px radius,
  SurfaceDialog bg + drop-shadow) with "Notifications" header +
  "Clear" action, "No notifications yet." empty state, scrollable
  `ItemsControl` of entries (kind-coloured dot · title · detail ·
  session · HH:mm) and a "Click an entry to jump to its session"
  footer. Entry clicks invoke the VM's `ActivateCommand`, which flips
  `SelectedTab` via the Claude AgentSessionId match and closes the
  popover. 9 new unit tests cover ordering, MaxEntries trim, unread
  count, mark semantics, Clear, event firing. Total 139/139 green.

### Session 15 — telemetry polish + chrome polish (shipped)

- `abf4e45` — **250 ms poll fallback.** Shared `Timer` in
  `ClaudeTelemetryService` stats each watched file and re-reads when
  `Length > Offset` — zero-cost on quiet sessions, closes the
  FSWatcher latency gap so the Wait pulse feels instant. New
  three-arg ctor opts polling in; two-arg test-seam stays
  deterministic. Added `Polling_Picks_Up_Appended_Lines…` test.
- `4b45770` — **Per-model context window detection.**
  `ClaudeTranscriptParser` now extracts `message.model`; new
  `ClaudeModelCatalog.GetContextWindow` maps ids to 200k (standard)
  or 1M (any id containing `1m`). `ClaudeSessionTelemetry` carries
  `ModelId` + `ContextWindowTokens`; `SessionTabViewModel` mirrors
  the cap; `MainViewModel.StatusBar.ContextWindowFor` prefers the
  detected value over the baked `AgentProfile` default. 9 catalog
  cases + 1 end-to-end telemetry test.
- `b97788c` — **Caption-cluster margin on rightmost strip.**
  `RebuildGroupLayout` now stamps a 184 px right-margin (4 × 46 px
  `CaptionButton`) on the last `GroupStripView` so its tabs + `+`
  button can't sit under the window caption overlay. Zero margin on
  the others so column alignment to `WorkspacesHost` is preserved.
- `5239ea0` — **Empty-project placeholder.** `ProjectViewModel.HasNoWorktrees`
  + a `MultiDataTrigger` in the project `HierarchicalDataTemplate`
  render a muted "(no worktrees)" row beneath an expanded empty
  project. Collapsed stays a one-line row.
- `b551730` — **Fase-7 token polish pass.** `Fig.Style.Dialog` +
  the three dialog outer borders point at `Fig.Radius.Medium` (8 px,
  was `Radius.Panel` 15 px / hard-coded). `Fig.Style.Button.*` swap
  `Framer.FocusVisual` → `Fig.FocusVisual` (dashed 2,2). Tab-strip
  titles drop to variable-weight 340 unselected with a Medium trigger
  on `IsSelected`, `TextFormattingMode=Ideal` for the sub-integer
  weight. "Pill on hero CTAs" pile item dropped — current hero buttons
  match HTML mocks pixel-for-pixel.
- `39d2786` — **Diff panel: stats header + line-number gutters + row
  tints.** `DiffPanelViewModel.RefreshAsync` now walks the patch once
  and tracks old/new counters seeded from each hunk header; `DiffLine`
  grew `OldLine` / `NewLine`. New `Summary` + `HasSummary` feed a muted
  mono "N files · +A −B" stats pill next to the panel's DIFF label.
  View rebuilt to a 44 / 44 / * grid (old gutter, new gutter, code)
  with per-kind row backgrounds (Added / Removed / Hunk); Context rows
  get an explicit `Fig.Brush.Ink` foreground to stop them falling
  through to SystemColors black. Also dropped the empty-state status
  duplication — `StatusBarText` now returns "" when `wts == 0` so it
  no longer collides with `StatusEmptyMessage`.
- `93cde45` — **Focused-group hairline accent (later reverted in `071da29`).**
  Shipped a bottom-border accent on the focused strip; user called it out as
  a redundant double-line against the already-present selected-tab rail.
  Reverted with the Tab Motion pass.
- `071da29` — **Tab Motion single-rail animation.** Per-tab `TabRail`
  border replaced with one shared `Border` in a `Canvas` above `StripList`.
  `SelectionChanged`/`Loaded`/`SizeChanged` measure the selected
  `ListBoxItem` via `TranslatePoint` and animate `Canvas.Left` + `Width`
  with a 200 ms `CubicEase` ease-out. First paint snaps without animation;
  `DispatcherPriority.Loaded` retry handles the "container not realised
  yet" race. Same commit reverts the `93cde45` hairline and drops the
  outer strip border's bottom thickness (`0,0,1,1` → `0,0,1,0`) so
  unselected tabs don't show a leftover grey line.

### Session 14 — wait-state propagation (shipped)

- `4455c42` — Single-page deck recapping sessions 10–13 (four
  CSS-rebuilt status bars, flow diagram, four idea cards, live
  shot, commit log). *(Deck file removed during 2026-04-24 cleanup;
  still reachable via the commit.)*
- `792505f` — Sidebar propagation. `WorktreeViewModel.DotState` +
  `StatusLabel` now reflect child `TabStatus.Wait`; subscribes
  per-session `PropertyChanged(Status)`. `ProjectViewModel.HasWaitingChild`
  lives via per-child `PropertyChanged(DotState)`. Cross-instance fix:
  `ApplyTelemetry` stamps both the tab-strip VM *and* the sidebar
  mirror (distinct refs per the sidebar-flatten pattern).
- `0400fda` — Top-bar §3 pulse. Halo `Ellipse` behind the 6px tab
  dot, 1.4s scale 1→2.4× + opacity .55→0, `DataTrigger` on
  `Status == Wait`. Same timing as the sidebar so they read as one
  system.

**Known latency:** `FileSystemWatcher` + JSONL flush boundaries mean
Wait surfaces 100–500 ms after Claude emits the `tool_use` line.
Acceptable; a 250 ms poll fallback over the watcher would close the
gap if it ever feels laggy.

### Session 13 — transcript-driven activity state (shipped)

- `2ee7b1d` — `TranscriptEntry.StopReason` +
  `UserCarriesToolResult`; new `ClaudeActivityState` enum + FSM
  (`user` → Composing; `assistant end_turn` → Idle;
  `assistant tool_use` → PendingToolUse).
- `3b0f5b4` — `MainViewModel.ApplyActivityToStatus` projects
  activity onto `TabStatus` (PendingToolUse → Wait, etc.).
- `86291f2` — handoff update.

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
                       WindowGeometryStore, LayoutStore
  CodeScope.Core       ISessionStore, IGitService, IAgentRegistry,
                       IProjectStore, IPullRequestService (GH/Gitea/composite),
                       ISessionManager, IClaudeTelemetryService
                       Models: Project, Worktree, Session, WorktreeStatus,
                               AgentProfile, PullRequestInfo, CiStatus,
                               BranchInfo, ClaudeActivityState
                       Interop/ProcessTreeKiller (Win32 job)
  CodeScope.Ui         VMs: Main(+StatusBar partial), Sidebar, Project,
                            Worktree, SessionTab, CommandPalette,
                            Overview*, EditorGroup, DiffPanel
                       Views: SidebarView, SessionTabView, DiffPanelView,
                              OverviewView, GroupStripView, StatusBarView,
                              EditorGroupView
                       Dialogs: RenameDialog, NewWorktreeDialog,
                                CommandPaletteDialog
tests/
  CodeScope.Core.Tests 116 passing — Core only; no UI tests by convention
docs/
  DECISIONS.md (12 ADRs), design/DESIGN.md
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

- **Gitea CI rollup is always `None`** — `tea pulls status`/REST integration deferred.
- **Session-exit toasts deferred** — `SessionManager` starts pwsh with `-NoExit`; detecting agent exit needs `SessionManager` refactor or pty-output parsing.
- **Drag tab between groups restarts pwsh** — agent resumes via `--continue`/`--resume` but the scroll buffer is lost. Fix needs a shared SessionTabView host pool (see `memory/project_terminal_lifecycle.md`).
- **Terminal right-click opens no context menu** — parked for a dedicated
  research pass. Short version: `Microsoft.Terminal.Wpf` hosts a native
  Win32 child HWND (`TerminalContainer : HwndHost`) that intercepts
  `WM_RBUTTONUP` at the native layer, so WPF's routed mouse events never
  fire and `Grid.ContextMenu` / `PreviewMouseRightButtonUp` / the
  top-level `HwndSource.AddHook` all see nothing. Tested three tunneling
  routes via wpf-cli — the right-click instead pasted the clipboard into
  pwsh (the terminal's own right-click-to-paste path). `PreviewKeyDown`
  is unaffected because it rides tunneling at the keyboard layer, which
  is why the current build ships `Ctrl+Shift+O` / `Ctrl+Shift+C` /
  `Ctrl+Shift+V` via the keyboard handler in `SessionTabView.xaml.cs`.
  Options to investigate later:
  * Subclass the terminal's native HWND via `SetWindowLongPtr(GWLP_WNDPROC)`
    to peek `WM_RBUTTONUP` and forward → most robust but fragile across
    `Microsoft.Terminal.Wpf` versions; P/Invoke heavy.
  * Flip `Win32InputMode="False"` and rebuild keyboard forwarding
    ourselves → recovers WPF mouse events but is a big rewrite of the
    keyboard path.
  * Keyboard-only fallback: `Shift+F10` / Apps-key handler that opens a
    WPF ContextMenu programmatically (small, but no mouse UX).
  * Upstream a right-click event in `Microsoft.Terminal.Wpf` so no
    subclassing is needed.
  Context-menu XAML + click handlers were prototyped and reverted in
  `3bef676` (see diff — can be resurrected once a reach route lands).

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

**Multi-group / chrome polish:**

- **Preserve terminal HWND across drag-between-groups** — shared
  SessionTabView host pool.
- **Tab drag-reorder motion spec** — `docs/design/html/CodeScope - Tab Drag.html`.

**Fase 7 — remaining design token / mock consumption:**

- **Diff Panel right-side dock** — current panel is bottom-docked with
  full width; mock has it right-docked at ~400 px replacing the terminal.
  Requires MainWindow layout rework (swap a `RowDefinition` for a
  `ColumnDefinition` in the workspace Grid) + new splitter wiring.
- **Diff Panel Unified/Split toggle + file tabs + Stage/Revert** —
  feature work on top of the polish that landed in `39d2786`.
- **Tab Drag — floating drag chip adorner** — custom `Adorner` that
  follows the cursor, renders the tab replica with a −1.5 ° rotation +
  blue outer glow. 3 px blue drop-indicator between tabs needs a
  `DragOver` calc + dynamic `Rectangle` in the strip's ItemsPanel.
  (Session 15 closed: Empty State polish `5239ea0`, Diff Panel polish
  `39d2786`, Tab Motion single rail `071da29`.)

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
