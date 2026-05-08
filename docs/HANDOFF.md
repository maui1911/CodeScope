# CodeScope — Session Handoff

> Keep this file honest and tight. Updated at the end of every session.
> It is the first thing a fresh Claude Code session should read after `CLAUDE.md`.
> **Intent:** cursor + last 1–2 sessions in depth, everything else a one-liner.
> Old detail lives in `git log` — don't duplicate it here.

**Last updated:** 2026-05-08 (session 28)
**Branch:** `feat/codescope-rs-terminal`
**Head:** _uncommitted_ — `codescope-rs/` spike + terminal-crate scaffold
**Release:** `v0.2.5` shipped — https://github.com/maui1911/CodeScope/releases/tag/v0.2.5
**Build status:** ✅ C# side untouched, still green. Rust workspace at
`codescope-rs/` builds (`cargo build --workspace --manifest-path codescope-rs/Cargo.toml`).
**Uncommitted work:** entire `codescope-rs/` directory (3 commits planned).
**Open issues:** none on GitHub.

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
