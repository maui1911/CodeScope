# Architecture Decision Records

Format: one section per decision. Lightweight ADR.

---

## ADR-0001 — Target .NET 10 / C# 14

**Date:** 2026-04-21
**Status:** Accepted

.NET 10 is LTS (released Nov 11 2025). C# 14 adds `field` keyword, extension members, ref-readonly improvements. No reason to use .NET 8 or earlier.

**Consequences:** users must install .NET 10 SDK to build. Runtime deployment uses `win-x64` single-file publish with `PublishReadyToRun=true`.

---

## ADR-0002 — WPF, not WinUI 3 / MAUI / Avalonia

**Date:** 2026-04-21
**Status:** Accepted

WPF has the lowest memory floor, the most stable HWND hosting story (critical for `EasyWindowsTerminalControl`), and 20 years of ecosystem stability. WinUI 3 and MAUI still have HWND-interop quirks. Avalonia would force cross-platform abstractions we don't need — Windows-only is the product.

**Consequences:** no Fluent-by-default controls — we layer `Wpf.Ui` on top. No XAML Islands / WinUI.

---

## ADR-0003 — `EasyWindowsTerminalControl`, not xterm.js + WebView2

**Date:** 2026-04-21
**Status:** Accepted

`EasyWindowsTerminalControl` wraps Windows Terminal's official renderer (DirectX atlas engine). It is native WPF, HWND-composable, and matches the product's "native Windows" positioning. The alternative (xterm.js in a WebView2) would add 100–150 MB of Chromium per window and cross a JS interop boundary for every keystroke.

**Caveat:** package targets `net6.0`. Forward-compatible with `net10.0-windows`. If build fails or rendering is broken on .NET 10, plan B is fork to `/vendor/EasyWindowsTerminalControl` and retarget.

---

## ADR-0004 — Wpf.Ui (`WPF-UI` 4.2.0) for Fluent chrome

**Date:** 2026-04-21
**Status:** Accepted

Provides Fluent styles, dark mode, Mica backdrop, and Win11-native title bar on WPF. Actively maintained. Package targets `net8.0` — forward-compatible with `net10.0-windows`.

**Consequences:** accept the package's style surface; theme tweaks go through its `ApplicationThemeManager` API.

---

## ADR-0005 — Shell out to `git`, not LibGit2Sharp

**Date:** 2026-04-21
**Status:** Accepted

`git` on PATH is guaranteed on the target developer machine. LibGit2Sharp lags behind modern git features (worktree UX, partial clones, commit graph) and bundles a native DLL we'd rather not ship. All git operations are IO-bound anyway — Process.Start overhead is negligible.

---

## ADR-0006 — `System.Text.Json` for config, not Newtonsoft

**Date:** 2026-04-21
**Status:** Accepted

Built-in, zero-dep, fast, source-gen friendly. Newtonsoft only wins on exotic scenarios we don't have.

---

## ADR-0007 — Single-instance app (named mutex), for Fase 1

**Date:** 2026-04-21
**Status:** Provisional — revisit in Fase 2

Simpler state model and no surprises when two windows race to write `projects.json`. Can be lifted when multi-window support lands.

---

## ADR-0008 — xUnit + FluentAssertions + NSubstitute

**Date:** 2026-04-21
**Status:** Accepted

Team-default combo. No UI tests in Fase 1 (would require a heavy harness for WPF + HWND-hosted terminal control).

---

## ADR-0009 — Config schema versioning from v1

**Date:** 2026-04-21
**Status:** Accepted

`projects.json` always writes a `version` field. Load path checks version, runs migrations in order, writes back upgraded file. Unknown fields are preserved on roundtrip so downgrades don't lose data.

---

## ADR-0010 — Default theme: Mica dark with system accent

**Date:** 2026-04-21
**Status:** Accepted

Dark-first aesthetic consistent with the rest of the IDE-class tools our users already live in (VS Code, Rider). Users can switch later through Wpf.Ui's theme manager (wired in Phase 2 settings).

---

## ADR-0012 — Design system reference: `figma` DESIGN.md via getdesign.md

**Date:** 2026-04-21
**Status:** Accepted — design overhaul tracked as Fase 7

A project-wide `docs/DESIGN.md` was installed via `npx getdesign@latest add figma --out ./docs/DESIGN.md`. This is a curated DESIGN.md derived from Figma's marketing / product UI language (variable fonts, pill/circle geometry, black-and-white chrome with colorful product content).

**Why:** CodeScope's current Fluent/Mica look is functional but generic. Picking an opinionated style guide up front gives any future UI work a concrete reference — colors, spacing, type weights, component geometry — so agent-authored UI changes converge on one aesthetic instead of drifting. The Figma system in particular emphasizes typography-driven hierarchy, which plays well with a dev-tool density.

**Consequences:**
- Any Fase 7 UI pass should read `docs/DESIGN.md` first and mirror its color tokens and geometry rules in the Wpf.Ui theme.
- Not every element of the Figma system is portable (custom variable font license, web-specific effects); Fase 7 scope must include a "what we're keeping vs adapting vs dropping" table.
- DESIGN.md is a reference, not a contract — if the system conflicts with Windows platform conventions (e.g. keyboard shortcuts, Mica backdrop), Windows convention wins.

---

## ADR-0011 — `net10.0-windows` monikers, `PerMonitorV2` DPI awareness

**Date:** 2026-04-21
**Status:** Accepted

Per-monitor v2 is required for the app to look correct on mixed-DPI setups. Declared via `app.manifest`.

**Consequences:** `CodeScope.App.csproj` references an `app.manifest`; all child windows inherit.

---

## ADR-0012 — Partial-class split threshold for ObservableObject view-models

**Date:** 2026-04-22
**Status:** Accepted

ViewModels backed by `ObservableObject` split into `<Name>.cs` (state + constructor),
`<Name>.Commands.cs` (`[RelayCommand]` surface), and `<Name>.StoreSync.cs` (event-projection
into observable collections) when the single file crosses ~400 LOC or mixes three distinct
responsibilities. Below that threshold, keep the single file.

Applied to: `SidebarViewModel` (was 640 LOC). Not yet applied to: `MainViewModel` (964 LOC),
which is scheduled to follow the same split once the layout-persistence code moves out of
the VM into `LayoutStore` directly. Tracking under the mid_level_elegance review cluster.

**Consequences:**
- Fresh contributors grep for `<Name>.Commands.cs` to find a context-menu action; this is the convention.
- Reviewers should not flag a single-file VM under the threshold as "inconsistent with the split" — size matters.

---

## ADR-0013 — Auto-resume on `New session` removed; per-worktree history is the explicit surface

**Date:** 2026-04-26
**Status:** Accepted

`MainViewModel.NewSessionAsync` previously matched any soft-closed session for
`(worktree, agent)` and restored it transparently in place of minting a fresh
session. The implicit behaviour shipped because there was no UI to see closed
sessions. Side-effect: a second closed conversation on the same worktree was
shadowed by `FirstOrDefault`.

**Decision:** Drop the auto-resume block. *New session* always mints fresh. Reopening a
closed session is an explicit action driven by the per-worktree history
surface (sidebar disclosure under each worktree + a "Reopen most recent
closed session" item in the worktree context menu).

**Consequences:**
- Returning users learn one new affordance (history disclosure) and the
  one-keystroke worktree-context-menu shortcut for the most-recent-closed
  case.
- The `TryRestoreSessionAsync` helper stays as the implementation of explicit
  reopen.
- The drag-between-groups path (`MoveTabToGroup`) is unaffected — it never
  routed through the auto-resume block.
- `projects.json` now accumulates `Session` entries for closed shells too;
  pre-this-change, shells were always hard-removed on close so `projects.json`
  never carried shell rows.

---

## ADR-0014 — Shared `SessionTabView` host pool for cross-group drag

**Date:** 2026-04-26
**Status:** Accepted

Dragging a session tab between editor groups used to respawn the underlying
ConPTY child. Root cause was a WPF lifecycle interaction, not a terminal-control
limitation: each `EditorGroupView` bound an `ItemsControl` to
`EditorGroupViewModel.Tabs` with a `DataTemplate` for `SessionTabViewModel`. A
cross-group VM move destroyed the source `ContentPresenter`, which unloaded
`SessionTabView`, which destroyed the inner `EasyTerminalControl` HwndHost,
which killed pwsh / claude / codex along with the scrollback.

**Decision:** Introduce `ISessionViewHostPool` (UI-singleton) that owns one
`SessionTabView` per session id. Each `EditorGroupView` is a single
`ContentControl` whose `Content` is resolved from the pool keyed by
`SelectedTab.Descriptor.Id`. Reparenting the *same* `SessionTabView` between
two `ContentControl`s preserves the inner HwndHost (WPF documents this:
"removed HwndHost is not destroyed, just reparented under a non-visible
window"). The pool calls a renamed `SessionTabView.Teardown()` on `Release`
(close / restart / cascade) — we removed the `Unloaded` teardown hook because
it fires on every reparent, which is the exact bug we were fixing.

**Decision considered and rejected:**
- *TermPTY-only transfer* (`DisconnectConPTYTerm` → assign to a fresh
  `EasyTerminalControl`'s `ConPTYTerm` property): preserves the process and
  agent state but the renderer's scrollback lives in the
  `Microsoft.Terminal.Wpf` HWND, not in TermPTY. The new HWND boots empty. A
  replay buffer would be approximate at best for full-screen TUIs.
- *Win32 `SetParent` on the inner ConPTY HWND*: HwndHost has no supported way
  to swap its child HWND between two host instances. Workarounds are fragile
  across `Microsoft.Terminal.Wpf` versions.

**Consequences:**
- `MoveTabToGroup` lost ~25 lines of agent-resume / telemetry-rebind plumbing
  that existed to mask the respawn — no respawn now, no need to mask.
- `SessionTabView` is no longer free to manage its own ConPTY lifecycle; the
  pool is the single owner. Any new code path that closes a session must call
  `pool.Release(id)` (otherwise pwsh lingers and locks its cwd, breaking
  `git worktree remove`).
- Pool is UI-only (lives in `CodeScope.Ui.Services`); `Core` knows nothing
  about it. Dispatcher-affined, no locking.
- Spec: `docs/superpowers/specs/2026-04-26-cross-group-terminal-drag-design.md`
- Plan: `docs/superpowers/plans/2026-04-26-cross-group-terminal-drag.md`

---

## ADR-0015 — Closed-session retention policy: 100 per worktree, 90-day TTL

**Date:** 2026-05-01
**Status:** Accepted
**Issue:** #33

Soft-closed sessions accumulate in `projects.json` and in the sidebar History
disclosure for the lifetime of an install. Without a bound, long-running
installs grew hundreds of closed sessions per worktree, slowing config load
and growing the sidebar tree. ADR-0013 introduced the explicit history surface
but deliberately deferred the retention policy.

**Decision:** Two complementary cuts, applied per-worktree on every prune sweep:

1. **TTL** — sessions whose `ClosedAt` is older than 90 days are dropped.
2. **Cap** — if the per-worktree closed count is still above 100, the oldest
   entries beyond the cap are dropped (newest-first ordering preserved).

Pruning runs on `LoadAsync` (one-time migration of pre-policy state) and on
every `SoftCloseSessionAsync` (so the cap stays enforced as new closes arrive).
Each pruned row emits `SessionStoreChange.SessionRemoved` so VM History rows
disappear in lockstep. Live sessions (`ClosedAt is null`) are never touched.

**Decision considered and rejected:**
- *Manual purge only* (per-worktree "Clear history" action): accumulates
  silently between user actions and never bounds the long tail. Useful as a
  complementary affordance but not a sufficient policy on its own.
- *Per-project retention* instead of per-worktree: a project with one busy
  worktree would crowd out the closed-session history of its quieter siblings.

**Consequences:**
- Constants live in `SessionRetentionPolicy` (`MaxPerWorktree = 100`,
  `MaxAge = 90 days`). Bumping them requires a code change + new ADR amendment;
  not user-configurable for now.
- Behaviour change: on first launch after this lands, any pre-existing closed
  session older than 90 days OR beyond the cap-100 of its worktree is
  irrecoverably dropped. The migration is logged as a warning if persistence
  fails so the user can re-run.
- Live sessions, sessions with `ClosedAt = null`, and orphan sessions
  (`WorktreeId = null`) are still bounded — orphans collapse into a single
  "no-worktree" bucket so the cap still bounds them.
- A future "Clear history" UX (mentioned as a complementary action in #33)
  doesn't conflict with this policy; it'd just be a manual variant of the
  automatic sweep.

---

## ADR-0016 — Diff panel removed entirely

**Date:** 2026-05-01
**Status:** Accepted

The bottom-docked diff panel (`Ctrl+D`) was never used in practice — every
real review surface (status bar git stats, sidebar dirty markers, in-terminal
`git diff`, the agent CLI's own diff view) covers the same ground better. The
panel had a known memory issue (#32: triples large patches in memory + bursts
duplicate work on every selection change) that we'd otherwise have had to fix.
Cheaper to drop than to maintain.

**Removed:**
- `DiffPanelViewModel`, `DiffPanelView` (`.xaml` + `.xaml.cs`)
- `IGitService.GetDiffAsync` + the `git diff --no-color HEAD` runner in
  `GitService` (no other consumers)
- DI registration in `App.xaml.cs`, the `MainViewModel.Diff` property +
  `AttachDiffPanel` + `ToggleDiffPanel` command
- `MainWindow.xaml`: `Ctrl+D` `KeyBinding`, the `GridSplitter` + panel mount,
  the now-unused `BoolVisible` window-resource converter (other Views
  redeclare it locally), and the `WorkspaceLayer` Grid's row definitions
  (single child, no rows needed)
- `SidebarViewModel.WorktreeSelected` event (only consumer was the
  diff-panel bridge)
- Command palette entry "Toggle diff panel"
- `README.md` `Ctrl+D` row
- `docs/design/html/CodeScope - Diff Panel.html`

**Decision considered and rejected:**
- *Keep the panel and fix #32*: investment without a user; the panel was
  never integrated into anyone's actual review flow.
- *Replace with an inline diff-on-hover affordance*: out of scope; no
  current demand and would re-introduce the same per-selection work.

**Consequences:**
- Closes #37 (this ADR) and supersedes #32 (the perf fix is moot once the
  feature is gone).
- One fewer `IGitService` surface for tests/mocks to track.
- `Ctrl+D` is now free for future bindings.

---

## ADR-0017 — Bundle Microsoft Visual C++ runtime DLLs app-local

**Date:** 2026-05-04
**Status:** Accepted

A fresh-install user reported a black workspace and no terminals ever
initialising. Root cause: `EasyWindowsTerminalControl` →
`Microsoft.Terminal.Wpf.dll` → `Microsoft.Terminal.Control.dll` (in
`runtimes/win-x64/native/`) is a native binary built with MSVC. It depends on
`vcruntime140.dll`, `vcruntime140_1.dll`, and `msvcp140.dll`. On a Windows
box without the Visual C++ 2015–2022 Redistributable installed, the native
DLL silently fails to load — the `HwndHost` stays empty and every terminal
tab is a black rectangle. Velopack does not bundle VCRedist.

**Decision:** ship the three DLLs **app-local**, beside `CodeScope.exe`.
App-local DLL resolution wins over `System32`, so the bundle works whether
or not the user has VCRedist installed, and never interferes with the
system-wide install for other apps.

**Implementation:**
- DLLs committed under `src/CodeScope.App/native/vcredist/` (~709 KB total —
  `msvcp140.dll` 541 KB, `vcruntime140.dll` 121 KB, `vcruntime140_1.dll`
  47 KB). Source: System32 on a developer box where the latest VCRedist is
  installed (bit-identical to the redist payload).
- `CodeScope.App.csproj` Content glob copies them to output + publish, in
  the same style as the existing `conpty.dll` glob (ADR notes / PR #27).
- `tools/refresh-vcruntime.ps1` re-pulls from `%SystemRoot%\System32` and
  prints fresh versions + SHA-256s for `NOTICE.md`.
- `VcRuntimeBundleTests` (4 tests) guards both the source DLLs in the repo
  and the copied DLLs in the build output — silent regression if anyone
  removes the Content glob is now a build break.
- `NOTICE.md` next to the DLLs documents version, SHA-256, source, and the
  Microsoft redistribution license terms.

**Decisions considered and rejected:**
- *Startup-probe + friendly download dialog*: shifts the work to the user,
  doesn't help offline installs, and a black workspace is a brutal first
  impression even with a dialog. Kept on the shelf as a possible additional
  safety net but not needed once the runtime is bundled.
- *Velopack prerequisite stage that installs VCRedist*: requires admin,
  re-runs Microsoft's installer, ~25 MB download per fresh install, and
  ties our installer flow to a third-party MSI. The bundled DLLs are
  ~3% of that size, run with no admin, work offline.
- *Pull DLLs from a NuGet package at build time*: no official Microsoft
  package ships these for app-local redistribution; community packages
  exist but are unmaintained. Committing the binaries is more honest about
  what we depend on.

**Consequences:**
- Repo grows by ~709 KB of binary content. Acceptable — same order of
  magnitude as the icon set, smaller than several screenshot assets.
- The DLL versions need a manual refresh when Microsoft ships an important
  security update to the VC runtime. The refresh script + NOTICE table
  make this a 30-second operation.
- Velopack installer payload grows by ~709 KB.
