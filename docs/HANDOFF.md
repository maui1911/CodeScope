# CodeScope — Session Handoff

> Keep this file honest and tight. Updated at the end of every session.
> It is the first thing a fresh Claude Code session should read after `CLAUDE.md`.
> **Intent:** cursor + last 1–2 sessions in depth, everything else a one-liner.
> Old detail lives in `git log` — don't duplicate it here.

**Last updated:** 2026-04-30 (session 25)
**Branch:** `iconbubbely` (off `main`); taskbar overlay badge feature, rebased onto `main` after #22 + #23 merged. PR #25 open.
**Head:** `51229ee` (will bump after this HANDOFF update commits) — see session 25 below.
**Release:** `v0.1.0` shipped via GitHub Actions — https://github.com/maui1911/CodeScope/releases/tag/v0.1.0
**Build status:** ✅ `dotnet build CodeScope.sln` clean. Full solution `dotnet test` 385/385 green (Core 233, Ui 135, App 17), modulo two known FSWatcher flakes (`ClaudeSessionDiscoveryTests.Callback_Fires_For_Each_New_Jsonl…` and `PiSessionDiscoveryTests.Discovers_New_Session_File_With_Matching_Cwd` — pass in isolation).
**Uncommitted work:** this HANDOFF cursor tweak.
**Unpushed commits:** `iconbubbely` rebased onto `origin/main` (10 commits, all post-`e1ee4ea`); needs `git push --force-with-lease` because the rebase rewrote SHAs. PR #25 will pick up the new tip automatically.

### Session 25 — taskbar overlay badge (#18, PR #25)

User-visible: the Windows taskbar icon now carries a small overlay that
reflects aggregate agent activity across the whole app — green dot when ≥1
agent tab is registered and all idle, red disc with the busy count digit
otherwise. 10+ busy renders `9` with a small superscript `+` so the badge
stays one digit wide. No overlay when the workspace has zero agent tabs (or
only shell tabs). `Description` text is set in parallel for screen readers
(`"All agents idle"` / `"3 agents working"` / empty). Hand-verified against
the dev build; user-confirmed the green-idle state and the
top-right-of-icon position (Windows positions the overlay closest to screen
content, so a top-of-screen taskbar gets the badge anchored top-right of
the icon — that placement is Windows-controlled, not configurable).

How:

- New `ITaskbarBadgeService` (UI singleton) owns
  `Window.TaskbarItemInfo.Overlay` + `.Description` on the active main
  window. Default impl `TaskbarBadgeService` rasterises a 16×16
  `BitmapSource` via `DrawingVisual` + `RenderTargetBitmap`: filled disc
  (radius 7), 1 px contrast ring (radius 7.5, 40 % black) for legibility on
  light AND dark taskbars, optional centred white digit (Segoe UI Variable,
  weight 700, em 10) and optional super-scripted "+" (em 6) at top-right.
  Brushes resolve from theme (`Signal.Ok` / `Signal.Warn`) — same tokens
  the in-app status dots use; `Brushes.Red` defensive fallback.
- New `MainViewModel.TaskbarBadge.cs` partial: `BusyAgentCount`,
  `AgentTabCount` derived properties + private `RecomputeTaskbarBadge`.
  `IsAgentTab` excludes shell sentinel (case-insensitive); shell-only tabs
  and soft-closed history rows don't move the badge. The recompute fires
  from `RaiseStatusBarChanged()` — every per-tab `Status` change and every
  `Tabs.CollectionChanged` already hits that path, so no new subscriptions.
  Backing fields initialise to `-1` so the first recompute always fires;
  subsequent calls early-out via a change flag, so the rasteriser is **not**
  invoked on token/turn telemetry ticks (fixed in review).
- DI + XAML wiring in `App.xaml.cs` (singleton registration + ctor arg
  passthrough to `MainViewModel`) and `MainWindow.xaml` (declares
  `<TaskbarItemInfo />` once so the property is non-null on first paint).

Files added:
- `src/CodeScope.Ui/Services/ITaskbarBadgeService.cs`
- `src/CodeScope.Ui/Services/TaskbarBadgeService.cs`
- `src/CodeScope.Ui/ViewModels/MainViewModel.TaskbarBadge.cs`
- `tests/CodeScope.Ui.Tests/MainViewModelTaskbarBadgeTests.cs` — 6 tests:
  empty workspace, idle agent, 2-busy-of-3, 12-busy raw count, shell
  exclusion, and a status-flip case that drives the real
  `HookStatusBarSources → RaiseStatusBarChanged → RecomputeTaskbarBadge`
  pipeline (not the test bypass). `Apply` rasteriser is hand-verified
  per project convention (no UI tests).

Files touched:
- `src/CodeScope.Ui/ViewModels/MainViewModel.cs` — new field + optional
  `ITaskbarBadgeService? taskbarBadge` ctor param.
- `src/CodeScope.Ui/ViewModels/MainViewModel.StatusBar.cs` — single
  `RecomputeTaskbarBadge();` call appended to `RaiseStatusBarChanged()`.
- `src/CodeScope.App/MainWindow.xaml` — `<ui:FluentWindow.TaskbarItemInfo>`
  block.
- `src/CodeScope.App/App.xaml.cs` — singleton registration; new ctor arg
  on the `MainViewModel` factory.

Spec / plan: `docs/superpowers/specs/2026-04-29-taskbar-overlay-badge-design.md`,
`docs/superpowers/plans/2026-04-29-taskbar-overlay-badge.md`. Visual mockup
locked at `scratch/badge-mockup.html` (gitignored — re-render with the
HTTP-server route in the spec if tweaking the design).

Three review passes (in order), all fed back into the same branch:
1. Internal review found a perf nit — counts walked `AllTabs` twice per tick
   and fired `PropertyChanged` on every recompute. Fixed with a single
   `foreach` and change-only notifications.
2. `/codex:review` flagged the `StatusFlip_TriggersRecompute` test as
   verifying the test bypass instead of the production wiring it claimed.
   Replaced with a version that calls `HookStatusBarSources()` and asserts
   on the real `PropertyChanged → RaiseStatusBarChanged` pipeline.
3. GitHub `copilot-pull-request-reviewer` flagged that `Apply()` was firing
   on every `RaiseStatusBarChanged` tick (incl. token/turn telemetry) even
   when counts hadn't moved → redundant rasterisation. Fixed by gating
   `Apply()` behind a change flag inside `RecomputeTaskbarBadge`.

Open follow-ups:
- **Concern flagged but accepted as design**: `TaskbarBadgeService` resolves
  `Application.Current.MainWindow` on every call; if the app ever spawns a
  modal/splash that becomes the foreground window, the badge could be
  applied to the wrong window. Single-window app today; revisit when adding
  a real splash screen.
- **High-DPI `9⁺` legibility** unverified — only exercised at 100 % scaling.
  If blurry on a 200 %+ taskbar, follow-up is a 32×32 super-sample render
  (the bitmap stays 16×16 logical; we just render at twice the resolution
  and let Windows downscale).
- **Bonus matrix item** (10+ concurrently busy → `9⁺`) not yet hand-tested
  in production; covered by unit tests but visual-only.

### Session 24 — Add project from a git URL (#20, shipped in `e1ee4ea`)

User-visible: the "Add project" entry-point now opens a `NewProjectDialog`
with two modes — *Existing folder* (today's behaviour) and *Clone from URL*.
Clone mode shows an inline indeterminate spinner + "Cloning…" caption while
`git clone` runs, and lets the user Cancel mid-flight (cancels the
`CancellationToken`, kills git, cleans the partial target). On failure the
dialog re-enables and renders git's stderr inline beneath the URL field — no
toast, the user is still looking at the dialog.

How:
- `IGitService.CloneAsync(url, parentDir, folderName, ct)` shells out to
  `git -C <parent> clone -- <url> <name>` via the existing `ProcessRunner`.
  Pre-validates empty inputs, refuses non-existent parents, refuses non-empty
  targets. `OperationCanceledException` propagates (consistent with every other
  method on the service). 4 tests in `GitServiceCloneTests.cs`.
- `NewProjectDialog` is a self-contained WPF Window mirroring `NewWorktreeDialog`'s
  style. Owns its `CancellationTokenSource` for the duration of the clone;
  Cancel during clone cancels without closing, Cancel when idle closes with
  `DialogResult = false`. Failed/cancelled clones run a best-effort
  `TryDeleteDir` so a retry isn't blocked by the destination-exists check.
- `SidebarViewModel.AddProjectAsync` rewired through the dialog. New helper
  `DefaultCloneParent()` walks recent-project parent → `%USERPROFILE%\source\repos`
  → `%USERPROFILE%`. Drag-drop path (`AddProjectByPathAsync`) untouched.
- `App.xaml.cs` registration of `SidebarViewModel` switched to named args
  (positional was getting fragile with four picker/service params); the new
  `pickNewProject` lambda captures `IGitService` once at registration.

Spec: `docs/superpowers/specs/2026-04-29-add-project-from-git-url-design.md`.
Plan: `docs/superpowers/plans/2026-04-29-add-project-from-git-url.md`.

Files added/touched:
- `src/CodeScope.Core/Services/IGitService.cs` (CloneAsync signature)
- `src/CodeScope.Core/Services/GitService.cs` (CloneAsync implementation)
- `tests/CodeScope.Core.Tests/GitServiceCloneTests.cs` (4 tests)
- `src/CodeScope.Ui/Dialogs/NewProjectRequest.cs` (records)
- `src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml` + `.xaml.cs` (dialog)
- `src/CodeScope.Ui/ViewModels/SidebarViewModel.cs` + `.Commands.cs` (wiring)
- `src/CodeScope.App/App.xaml.cs` (registration + named args)

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

**Cross-group terminal drag without restart — shipped (session 22).** A new
`ISessionViewHostPool` (UI-singleton) owns one `SessionTabView` per session id;
each `EditorGroupView` is a single `ContentControl` that pulls its content
from the pool. Dragging a tab between groups now reparents the *same* view
under the new `ContentControl`, preserving the inner `EasyTerminalControl`
HwndHost — pwsh / claude / codex stay alive, scrollback intact, telemetry
uninterrupted. The agent-resume / telemetry-rebind hack in `MoveTabToGroup`
that masked the old respawn was deleted (~25 lines). Spec
`docs/superpowers/specs/2026-04-26-cross-group-terminal-drag-design.md`,
plan `docs/superpowers/plans/2026-04-26-cross-group-terminal-drag.md`,
ADR-0014. The "Drag tab between groups loses scroll buffer" rough edge is
closed.

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
                            Overview*, EditorGroup
                       Views: SidebarView, SessionTabView, OverviewView,
                              GroupStripView, StatusBarView, EditorGroupView
                       Dialogs: RenameDialog, NewWorktreeDialog,
                                CommandPaletteDialog
tests/
  CodeScope.Core.Tests 156 passing (1 FSWatcher flake) — Core only; no UI tests by convention
  CodeScope.Ui.Tests   108 passing
  CodeScope.App.Tests   17 passing
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

- **"Remove from history" has no confirm dialog** — the action is destructive (entry cannot be recovered) but currently fires immediately on click. A follow-up should add a small inline confirm or an Undo toast. Low priority until the history feature is user-tested.
- **Gitea CI rollup is always `None`** — `tea pulls status`/REST integration deferred.
- **Session-exit toasts deferred** — `SessionManager` starts pwsh with `-NoExit`; detecting agent exit needs `SessionManager` refactor or pty-output parsing.
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

**Immediate:**

- **Push + PR for `feature/session-history-per-worktree`** — branch is local-only; 9 feature commits + HANDOFF ready. `git push -u origin feature/session-history-per-worktree` then open PR to `main`.
- **"Remove from history" confirm** — add an inline confirm or Undo toast so the destructive action is recoverable (see rough edges above).

**Multi-group / chrome polish:**

- **Tab drag-reorder motion spec** — `docs/design/html/CodeScope - Tab Drag.html`.

**Fase 7 — remaining design token / mock consumption:**

- **Tab Drag — floating drag chip adorner** — custom `Adorner` that
  follows the cursor, renders the tab replica with a −1.5 ° rotation +
  blue outer glow. 3 px blue drop-indicator between tabs needs a
  `DragOver` calc + dynamic `Rectangle` in the strip's ItemsPanel.
  (Session 15 closed: Empty State polish `5239ea0`, Tab Motion single
  rail `071da29`. Diff Panel removed entirely — see ADR-0016 / #37.)

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
