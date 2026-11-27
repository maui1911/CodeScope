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

---

## ADR-0018 — Rust port adds an in-app Settings dialog (forced deviation)

**Date:** 2026-05-11
**Status:** Accepted

The C# build configures the equivalent fields via hand-edited `settings.json`
(plus a handful of `App.config` entries); there is no in-product Settings UI on
that side. The Rust port adds one — `Ctrl+,` opens a centered modal that
surfaces the existing [`codescope_core::Settings`] fields (theme, default agent,
font family / size / line-height, scrollback, cursor shape + blink) through
form controls. This is a one-way deviation from the C# build: nothing about
the on-disk schema changes, the dialog merely renders the existing struct.

**Why deviate:** hand-editing JSON is a poor first-run experience and the
Rust port has no separate `App.config` surface to fall back on. The visual
idiom mirrors the in-app modals (`new_project_dialog.rs`,
`new_worktree_dialog.rs`) so the chrome stays consistent.

**Consequences:**
- The Rust port ships `src/settings_dialog.rs` with no C# counterpart.
- Saves go through [`Settings::save`] — the existing settings-file watcher
  in `app.rs` picks up the change on its next tick, and `apply_settings`
  is also called inline so the swap is instant.
- The dialog can preview the theme live as the user clicks through the
  list; Cancel explicitly reapplies the snapshot of `Settings` captured
  when the dialog opened, reverting any in-flight preview (the file-watch
  poller cannot do this on its own because `settings.json` is untouched
  during a preview). Font + scrollback edits only take effect for
  newly-spawned tabs (running terminals keep their baked-in palette /
  scrollback); the dialog surfaces a small "Applies to new tabs only."
  hint to make this explicit.
- No new schema fields — anything not yet representable in `Settings`
  (e.g. per-theme overrides for an external theme bundle) stays out of
  reach of the dialog until the underlying struct grows the capability.

---

## ADR-0019 — Rust port uses cargo-dist + Velopack-rs for its own release pipeline

**Date:** 2026-05-12
**Status:** Accepted (PR #154)

The Rust port (`codescope-rs/`) now ships through a separate
`rs--release.yml` workflow built on [cargo-dist][cargo-dist] +
[velopack-rs][vrs], running alongside the existing C# `release.yml`.
Both pipelines target the same GitHub repository and the same product
brand — the split is purely build-system.

[cargo-dist]: https://opensource.axo.dev/cargo-dist/
[vrs]: https://crates.io/crates/velopack

**Why deviate from the single-pipeline default?**

- **Different toolchains**: the C# `release.yml` shells out to `vpk`
  (the .NET CLI) which orchestrates MSBuild, Velopack's
  `dotnet publish` wrapper, and the Velopack uploader in one pass.
  Trying to run cargo-dist *inside* that workflow would mean pulling
  Rust + cargo-dist + MSI tooling into the .NET pipeline just to ship
  an artifact that's independent of the C# build.
- **Independent release cadence**: the Rust port is still pre-feature-
  parity in places (cross-platform, code signing, full Velopack
  channels). A combined pipeline would force both ports to rev tags
  together, which doesn't make sense yet.
- **Tag-namespace separation** (`rs-` prefix vs. `v` prefix). Both
  pipelines run `git describe --tags --match v*` so the C# build's
  informational version (and the Rust build's `CODESCOPE_VERSION_DISPLAY`)
  only ever see product-version tags, never the Rust-pipeline-internal
  `rs-vX.Y.Z` tags. Without that `--match`, `git describe` would pick
  whichever tag is closest in commit-distance regardless of prefix —
  exactly the failure mode Copilot caught on PR #162. See
  `Directory.Build.targets` and `codescope-rs/build.rs`.

**Consequences:**

- Two release-pipeline files live in `.github/workflows/`. The C#
  `release.yml` is unchanged; the Rust file is `rs--release.yml`.
- Two parallel release flows for the user:
  - `git tag v0.2.6 && git push --tags` → C# release (existing).
  - `git tag rs-v0.0.2 && git push --tags` → Rust release (new).
- The Rust pipeline ships **multiplatform** artifacts as of the
  multiplatform-release-pipeline PR: `x86_64-pc-windows-msvc` (MSI +
  zip), `aarch64-apple-darwin` + `x86_64-apple-darwin` (`.tar.xz`),
  and `x86_64-unknown-linux-gnu` (`.tar.xz`). The original
  cross-compile-from-Windows blocker (ring's build.rs needing
  `cc` / `clang` / `zig`) is sidestepped by running native builds
  on the GH Actions matrix (`windows-2022`, `macos-14`,
  `macos-15-intel`, `ubuntu-22.04`). Code signing / SmartScreen /
  Gatekeeper remain follow-ups on every platform.
- `velopack` crate replaces a hand-rolled apply path. The
  `codescope_core::update_check` module (which polls the GitHub
  releases endpoint and emits an `UpdateStatus::Available`) keeps
  its existing role; the bin-side `velopack_bridge.rs` consumes that
  signal and either auto-applies (Velopack-install path) or surfaces
  the URL (cargo-dist MSI / `cargo run` path).
- `dist-workspace.toml` carries `allow-dirty = ["ci"]` so the
  hand-edited "tag-push only" trigger in `rs--release.yml` survives
  future `dist generate` runs.

---

## ADR-0020 — Rust empty-state "Clone from Git URL" CTA is interactive (forced deviation)

**Date:** 2026-05-13
**Status:** Accepted

The C# `EmptyStateView` renders the "Clone from Git URL" button as
`IsEnabled="False"` with a tooltip `"Clone from Git URL (coming soon)"` —
the WPF clone flow was never wired up there.

The Rust New Project dialog already implements the full
`git clone <url> <parent>/<name>` flow via `DialogMode::Clone` (see
`codescope-rs/src/new_project_dialog.rs`). Leaving the empty-state
ghost disabled in the Rust port would be a strictly worse UX than the
underlying capability already supports.

**Decision:** the Rust empty-state Clone CTA is interactive. Clicking
it opens the New Project dialog already switched to the Clone tab via
`AppShell::open_new_project_dialog_clone`. Same `git clone` flow the
sidebar's "+ New Project → Clone from URL" tab uses today.

**Consequences:**

- The Rust empty-state hero diverges visually from the C# spec: where
  C# shows a faded disabled-looking ghost, Rust shows an enabled
  ghost button with hover state.
- One additional entry point (`open_new_project_dialog_clone` on
  `AppShell`) routes through the existing
  `Sidebar::set_new_project_mode` switch, so the divergence is
  isolated to one helper — no fork of the dialog state machine.
- If/when the C# build wires its clone flow, both ports converge
  on the same affordance.

## ADR-0021 — Rust rehydrate is driven by `projects.json`, not `layout.json`

**Date:** 2026-05-14
**Status:** Accepted

Until this ADR the Rust port had two sources of truth for "which
sessions are open across a restart": `projects.json` carried the live
`Session` rows (with `closed_at = None`), and `layout.json` carried a
parallel `open_tabs: Vec<RestoreTab>` that the cold-start path
(`AppShell::rehydrate_or_cold_start`) actually read. Any divergence
between the two — a crash before `save_layout` fired after a spawn, a
manual file edit, a partially-restored backup — left "live" rows in
`projects.json` that were invisible to the user because they weren't
in `layout.json`.

The C# build doesn't have this problem: `LayoutStore.Layout` carries
only `GroupCount`, `FocusedGroupIndex`, `Dictionary<string, int>
SessionToGroup` and `GroupWidths`. `MainViewModel.HydrateFromLoaded`
iterates every `Session` with `ClosedAt is null` and uses
`SessionToGroup[s.Id]` purely to decide *where* the tab lands, not
whether it lands at all.

**Decision:** mirror the C# split. `LayoutState.session_placements:
Vec<SessionPlacement>` replaces `open_tabs` as the placement record;
each entry binds a `Session.id` to its group index and an
`active_in_group` flag. The legacy `open_tabs` field stays
deserialise-only and is migrated in place on first load, then dropped
on next save. `rehydrate_or_cold_start` walks `self.projects.projects[].sessions`
where `closed_at.is_none()` and falls back to group 0 / non-active for
sessions without a placement.

**Consequences:**

- A session opened via `Ctrl+T` is durable from the moment
  `SessionManager::open` writes `projects.json` — even a process kill
  before the next `save_layout` keeps the tab on next launch (it just
  comes back in group 0 if the placement wasn't captured yet).
- `Tab.auto_type` is no longer persisted as separate state; the
  resume command line is rebuilt at rehydrate time from
  `Session.agent_id` + `Session.agent_session_id` via
  `build_resume_auto_type`, matching the C# hydrate path.
- Free-floating tabs (no project context at spawn time → no row in
  `projects.json`) no longer survive a restart. This matches C#'s
  invariant that every session belongs to a project (the "unsorted"
  bucket is the catch-all; `AppShell` already routes there via the
  Add Project flow).
- One-shot migration: pre-ADR `layout.json` files keep their tab
  groups via the `open_tabs → session_placements` conversion in
  `LayoutState::migrate`; entries without `session_id` are dropped
  (those predate resume-by-id and had no `projects.json` link
  anyway).

---

## ADR-0022 — Rust port becomes canonical CodeScope; C# build retired

**Date:** 2026-05-14
**Status:** Accepted

The Rust port (`codescope-rs/`) has matched the C# build's feature set
since `rs-v0.3.0-rc.5` (session 41) and has been the sole daily driver
since rc.4. The last twenty merged PRs are all `(rs)`-namespaced; the
C# tree under `src/CodeScope.{App,Core,Ui}/` has been untouched for
weeks. Carrying both implementations costs: duplicated release
pipelines, divergent build settings, a parity audit doc that goes
stale within days, and a "where is the canonical answer for X"
question on every change.

The Rust port is also platform-portable in a way WPF is not. Velopack
feeds now ship for Windows, macOS arm64/x64, and Linux x64 from one
pipeline ([ADR-0019](#adr-0019--rust-port-uses-cargo-dist--velopack-rs-for-its-own-release-pipeline), [PRs #201 / #202](https://github.com/maui1911/CodeScope/pulls)).

Earlier ADRs in this file describe the C# stack as canonical
(ADR-0001 .NET 10, ADR-0002 WPF, ADR-0003 EasyWindowsTerminalControl,
ADR-0004 Wpf.Ui, ADR-0006 CommunityToolkit.Mvvm, etc.). Those decisions
remain historically accurate for the `v0.2.X` line and are kept as
written. From this ADR onward, the Rust + GPUI stack is the active
record:

* GPUI for the application shell (was: WPF + `Wpf.Ui`).
* `codescope-terminal` (GPUI-native ConPTY/PTY) for terminal hosting
  (was: `EasyWindowsTerminalControl`).
* Cargo workspace at `codescope-rs/Cargo.toml` for builds (was:
  `CodeScope.sln` + `Directory.Build.{props,targets}`).
* `velopack-rs` for auto-update (was: Velopack .NET); pack id stays
  `codescope-rs` through cutover-2 and is renamed to `codescope` in
  cutover-3.
* `serde_json` for `projects.json` / `layout.json` (was:
  `System.Text.Json`). Same JSON shape — see
  [docs/MIGRATION-csharp-to-rust.md](MIGRATION-csharp-to-rust.md).

**Consequences:**

* The "mirror C# implementation 1:1" workflow rule (introduced in
  session 33 after PR #56 invented a non-existent UX) is retired
  together with the C# code. Future deviations from the v0.2.6
  behavior are normal product work, not parity violations.
* `docs/PARITY-AUDIT.md` was closed-stamped historical, then deleted
  on 2026-08-16 once every gap it listed had shipped — it survives in
  git history.
* The last commit where the C# source tree builds is tagged
  `legacy/v0.2.6-final` on the cutover-1 merge commit. The C# code is
  fully removed in cutover-2; the workspace is flattened and the
  Velopack pack id is renamed to `codescope` in cutover-3.
* Historical `v0.2.X` GitHub releases stay published; the
  `update_check` floor filter (see [ADR-0020](#adr-0020--rust-empty-state-clone-from-git-url-cta-is-interactive-forced-deviation) and PR #206)
  keeps them out of the live update flow.
* Solo dogfooding: no preflight migration release is shipped. Users
  (= the developer) reinstall once between the `codescope-rs` and
  `codescope` Velopack pack ids; `%APPDATA%\CodeScope\projects.json`
  carries across unchanged.


---

## ADR-0023 — Bots inbox is a read-only view of the lab control plane

**Date:** 2026-09-13
**Status:** Accepted

`labs/agent-bots` runs unattended coding bots against this repository.
Its state lives in a control plane directory
(`labs/agent-bots/.state/`: task files, an append-only `board.md`,
handoffs), written by the lab's runner scripts only. Until now the only
way to see what the bots did was reading those files by hand. The lab's
README §7.4 calls the next step "Stage 2 — the inbox".

**Decision:** CodeScope gets a **Bots inbox** panel that *reads* the
control plane of the selected project and never writes to it.

* **Where the data comes from:** `<project>/labs/agent-bots/.state`,
  found per selected project (`codescope_core::bots::lab_control_plane`).
  No settings, no product-level path, no AppPaths entry — the lab is
  still an experiment, and a project without that directory simply has
  no inbox (the footer button is hidden).
* **Slot:** the work-area slot shared with the Overview and the diff
  viewer; opening one closes the others. Entry points follow the
  Overview: a sidebar footer "Bots" button (with a count of tasks that
  are blocked or need review), `Ctrl+Shift+I`, and a palette command.
  `Ctrl+Shift+B` was the first choice but already toggles the sidebar.
* **Actions are navigation only:** open the handoff file, reveal a
  worktree that still exists, copy the branch name. Dispatching,
  approving memory notes and stopping runs stay with the runner
  scripts, so the runner remains the single writer of the control
  plane.
* **Refresh:** a poll loop re-reads on the background executor every
  2 s while the panel is visible and every 5 s otherwise (for the
  badge). The plane is a few dozen small files; no file watcher.

**Consequences:**

* The mockup's "Open diff" action is not built: the diff viewer shows
  a working tree against `HEAD`, not a bot branch against its base,
  and a finished run's worktree is already cleaned up.
* The inbox parser (`core/src/bots.rs`) follows the runner's formats
  rather than a schema of its own; a format change in the lab needs a
  matching change there.
* Moving the control plane out of `labs/` (a product path or a setting)
  is a later decision, when the bots graduate from the lab.

## ADR-0024 — Sidebar worktree rows follow `git worktree list`

**Date:** 2026-09-24
**Status:** Accepted

Worktree rows used to change only through the app: "Add project"
adopted what `git worktree list` showed at that moment, "New worktree"
added a row, "Delete worktree" dropped one. A worktree created
afterwards in a terminal, by an agent (Claude Code `--worktree`,
subagent isolation) or by the bots lab never showed up, not even after
a restart. A worktree removed outside the app left a row behind that
the in-app delete couldn't clear either: git answers
`is not a working tree`, and `--force` too.

**Decision:** the sidebar runs a discovery poll that keeps each
repository project's rows in step with git
(`ProjectsConfig::sync_worktrees`, driven by
`Sidebar::start_worktree_discovery_poll`).

* **Cadence:** first tick at launch, then every 5 s, the same interval
  as the dirty and git-status polls. One `git worktree list` per
  repository project, plus a stat of each stored path, all on the
  background executor. No file watcher, same as the other pollers.
* **Adding:** a listed worktree gets a row unless it is the primary,
  is prunable (folder deleted by hand, never pruned), or any project
  already tracks the path, whether as its root or as a worktree. Paths
  compare normalised *and* by resolved real path, because git lists
  paths with symlinks resolved and the New-worktree dialog stores what
  was typed. A folder that is gone resolves through its nearest
  existing ancestor, so it still matches its prunable entry.
* **Dropping:** a non-primary row goes only when git no longer lists
  it at all (a prunable entry still counts as listed, for an unplugged
  drive) *and* its folder is confirmed gone. That is the same edit the
  in-app delete makes: the row goes, and its session history stays in
  `projects.json`.
* **Add project:** runs the same sync once, synchronously, instead of
  its own adoption, so the same rules apply. A worktree another
  project already owns, or the new project's own path reached through
  a symlink, is not adopted a second time.
* **Writes:** a tick that changes nothing does not touch disk. A tick
  that does reloads `projects.json` first, the rule AppShell already
  follows, because the sidebar's copy can lag AppShell's session
  writes.
* **In-app delete:** when `git worktree remove` fails and git has
  already forgotten the worktree, the row is dropped without a force
  prompt. A folder still at that path is left alone.

**Consequences:**

* Short-lived agent and bot worktrees show up as rows while they
  exist, then disappear.
* An in-app add or delete that lands during a scan throws that scan
  away (`Sidebar::worktree_edits`). Otherwise a list taken before the
  delete finished would put the deleted row straight back.
* Stored paths stay as git prints them (forward slashes on Windows),
  as the add-time adoption already did. Consumers that need native
  separators convert them, as with the reveal helper (#298).

## ADR-0025 — Read OpenCode's `opencode.db` with `rusqlite` (bundled)

**Date:** 2026-10-30
**Status:** Accepted

OpenCode 1.x stopped writing one JSON file per message under
`storage/.../message/<sid>/` and keeps sessions, messages and message
parts in a WAL-mode SQLite database, `~/.local/share/opencode/opencode.db`.
The reader still walked the old layout, so OpenCode tabs got no
adoption, no status dot, no tokens and no title (#381). Every other
agent CodeScope tails writes plain files; this is the first one that
needs SQLite.

**Decision:** read the database in-process with `rusqlite`, `bundled`
feature, in `codescope-core` (`agents::opencode::db`).

* **Why not the `sqlite3` CLI:** shelling out is how CodeScope talks to
  `git`, but `git` is a prerequisite and `sqlite3` is not: most
  machines don't have it (Windows never ships it). A missing binary
  would silently disable OpenCode support. Spawning a process per
  query would also blow the poll budget below.
* **Why `bundled`:** no system SQLite to find or version-match on any
  of the three platforms; the library is compiled into the binary.
  The readers need WAL and the JSON functions, both in the bundled
  build.
* **Read-only, never blocking:** connections open with
  `SQLITE_OPEN_READ_ONLY` (no create, no URI parsing) and a zero busy
  timeout. `SQLITE_BUSY` / `SQLITE_LOCKED` and a missing database both
  mean "nothing new this tick"; a busy read is retried on the next
  tick, on the same connection. CodeScope never writes database pages.
  It does touch the side files the way any WAL reader does: it records
  read marks in `opencode.db-shm`, and if `-wal` / `-shm` are absent
  (OpenCode not running, its last connection closed) opening the
  database creates them and may run WAL recovery into `-shm`.
* **`data_version` before query:** both the adoption scan (every
  ~350 ms per OpenCode tab) and the session tail (250 ms – 2 s) keep a
  connection open and ask it for `PRAGMA data_version`, which changes
  whenever another connection commits. It reads the WAL-index header,
  not table pages: a few µs. Only a changed value runs the queries.
  File stats were tried first and miss commits: after an automatic
  checkpoint SQLite rewrites the WAL from offset 0, so its length stops
  changing, and on Windows consecutive commits often share an mtime.
  Discovery caches the session list per database path, shared by all
  tabs; each tail keeps its own connection.
* **One stat per tick, for identity only:** each probe also checks that
  `opencode.db` exists and, on Unix, that it is the same inode, and
  reopens if it was replaced. The stat is shared by every tab polled in
  the same tick (50 ms reuse window). On Windows an open connection
  stops OpenCode from deleting or renaming the file (sharing violation,
  error 32). Tails close their connection with their tab, but the
  discovery cache keeps its connection until CodeScope exits, so once
  an OpenCode tab has been open, deleting `opencode.db` (an OpenCode
  reset) fails until CodeScope is closed.
* **Adoption recency:** there is no per-session file to stat, so
  discovery returns the session's `time_updated` and the adoption tick
  compares candidates by it.

**Consequences:**

* `codescope-core` gains its first C dependency (`libsqlite3-sys`
  builds SQLite with the platform C compiler, which the GPUI build
  already requires).
* The reader follows OpenCode's schema (`session`, `message`, `part`
  and the JSON inside `data`), not a published API. A schema change in
  OpenCode reads as "no data" and is logged once (repeats of the same
  error are not printed), not a crash, and needs a matching change in
  `agents::opencode`.
* OpenCode versions older than the database (the JSON-file layout) are
  no longer read; that reader was removed.

## ADR-0026 — Restart Claude tabs onto a new Claude Code build in place

**Date:** 2026-11-17
**Status:** Accepted

A Claude Code update used to mean restarting CodeScope. The native
installer already swaps `~/.local/bin/claude.exe` while tabs still run
it: it renames the in-use image to `claude.exe.old.<ts>.<pid>`, which
Windows allows for a running executable. So the update lands, but every
`claude` that was already running keeps the old build ("Update
installed · Restart to update"). Closing all of CodeScope was the
blunt way to restart them all.

**Decision:** CodeScope restarts the tabs, not itself
(`src/app/claude_update.rs`, `agents::claude::{version, restart}`).

* **Detecting:** every 5 minutes (first check 30 s after launch)
  `claude --version` gives the installed build (20 s timeout). Each
  Claude tab's process reports its own build and `idle`/`busy` status
  in `~/.claude/sessions/<pid>.json`, which adoption already reads. A
  tab is outdated when its build is older. Outdated tabs get a `↑` in
  the tab strip and a "Restart on new Claude version" row in the tab
  menu. While any tab is outdated it is re-read every 15 s.
* **Restarting on its own:** an outdated tab is restarted without
  asking once it is idle, the user hasn't typed into it since their
  last Enter (a draft would be wiped), and it isn't the visible tab of
  a group in a focused window (the screen would redraw under them). A
  first version of this PR asked with a toast button; that was dropped
  as noise — the restart is invisible when done this way. Each tab gets
  one automatic attempt per installed build: a relaunch that comes
  back still outdated (its shell resolves `claude` to another install
  than CodeScope's PATH does) or a `claude` that won't exit is not
  retried every tick. The tab menu can always retry.
* **The sequence:** Ctrl+C (clears a draft, or just shows "press again
  to exit"), then `/exit` + Enter on the empty prompt, which shuts
  Claude Code down cleanly with no double-press timing involved. Then
  `claude --resume <session id>` is typed at the `pwsh -NoExit` prompt
  left behind. Before every keystroke the state is re-read: the
  process must still be under the tab's shell (a failed or empty
  process snapshot aborts, it never reads as "exited"), its pid file
  must still say idle, and the user must not have started typing. A
  session nobody sent a prompt in has an id but no transcript, so it
  gets a plain `claude` instead. "No transcript" requires both a
  missing file and no adoption of that id, so a missed path lookup
  can't swap a conversation for a blank one. The session id must be
  UUID-shaped before it is typed into a shell.
* **Three Ctrl+C instead of `/exit`:** tried first. It needs the
  presses inside Claude Code's double-press window, and a spare one
  can cut an already-running shutdown short.
* **Not installing updates ourselves:** Claude Code's own updater
  already handles the locked binary. Doing the swap ourselves would
  duplicate it and drift from it.
* **Not launching from `versions/<v>`:** running each tab from the
  versioned file would keep `bin/claude.exe` unlocked, but those files
  have no `.exe` extension (pwsh can't run them as a command) and it
  would change the launch path for every agent profile.

**Consequences:**

* A restart from the tab menu wipes a draft prompt, the same as
  pressing Ctrl+C by hand. Automatic restarts skip tabs that may have
  one. "Typed since the last Enter" is conservative: an arrow key
  counts too, so such a tab waits until its next prompt is sent.
* Whatever the idle `claude` owns dies with it: MCP servers come back
  with `--resume`, background shells or dev servers its tools started
  do not. A tab in a modal state that still reports `idle` (a dialog,
  an external editor) gets Ctrl+C and `/exit` typed into that, and the
  restart gives up after 10 s.
* The relaunch uses the registry's `claude` profile, like reopening a
  session, so extra flags the tab was started with are not repeated.
* Relies on undocumented Claude Code details: the pid file's
  `version`/`status` keys and `/exit`. If the keys change, tabs read as
  "not outdated" or "not idle" and are left alone. If `/exit` stops
  exiting, the restart gives up with one error toast per tab and
  build. Nothing is killed by CodeScope either way.

## ADR-0027 — A board on the host drives dispatch

**Date:** 2026-11-25
**Status:** Accepted

A project is a local folder, and work on it starts when the user opens
a tab. To move towards a software factory, work should come from a
board on the host, and CodeScope should carry it out as far as it can
on its own, in tabs the user can open and steer at any time. The terms
below (board, board item, trigger and progress column, dispatch,
phase, round, findings, decision point, needs you, turn, review round,
repair, board protocol) are defined in [GLOSSARY.md](../GLOSSARY.md).

**Decision:** a GitHub Projects board is CodeScope's control panel.
The user moves items into trigger columns; CodeScope dispatches them
and reports progress back on the board. Several people can share one
board, each with their own CodeScope or another tool that follows the
[board protocol](board-protocol.md).

* **Board, not repository, is the intake.** A board is its own
  concept, added by pasting its URL, not a property of a project: one
  board spans several repositories, and one repository can sit on
  several boards. An item finds its project through its repository; a
  project belongs to every repository any of its remotes points to
  (`origin` is often a fork, the issues live on `upstream`). When
  several projects match, the user picks once per board. A draft issue
  is not dispatched: it has no repository and no timeline that says
  who moved it. It waits, with that reason shown on it in the board
  panel, until a person converts it to an issue and moves it again.
* **Columns.** Two trigger columns, *Research* and *Ready*, are queues
  the user fills. CodeScope moves an item out of the queue into a
  progress column when it starts: *Researching*, *Needs decision*,
  *In progress*, *In review*. *Done* is left to GitHub's built-in
  workflows (item closed, pull request merged), which are on by
  default on a new board. Columns take their roles by name, the way
  the board protocol matches them; a board that no other tool works on
  can map other names in CodeScope's settings. Research is optional: an
  item moved straight to *Ready* skips it. The board panel names every
  role that has no column yet; the user adds it on GitHub, because
  rewriting the options of the status field through the API can
  regenerate their ids.
* **Writing rule.** CodeScope moves an item on its own in two ways
  only: out of a trigger column into the progress column of the round
  a person's move started, and on from the progress column CodeScope
  moved it into (*Researching* to *Needs decision*, *In progress* to
  *In review*), once per round. A person's move into a trigger column
  hands the item to CodeScope again; a move to any other column ends
  CodeScope's moves for that round. A move the user makes through
  CodeScope, such as *Ready* in the needs-you dialog, is the user's own
  move. An item GitHub has moved to *Done*
  is left alone. If a board has the older "Pull request linked to
  issue" workflow on, the board panel warns: it can move *In review*
  back to *In progress*, and CodeScope then leaves it there.
* **Several people, one board.** Each person's CodeScope works only
  for that person. Whatever others need to see lives on GitHub; only
  sessions and worktrees stay local. The assignee says who works on an
  item: on dispatch CodeScope assigns the issue or pull request to its
  user if nobody is, and the agent opens its pull request assigned to
  the user, so another person's tool leaves it alone. It never
  dispatches an item assigned only to others. With several assignees,
  only the tool of the assignee who moved the item starts; a move by
  anyone else waits, with that reason shown. A person runs one tool
  per board: two CodeScopes of the same person (on two computers, or
  an installed and a dev build) would both take the same moves,
  because GitHub shows both as that person. Another tool shares the
  board by following the same rules. The
  [board protocol](board-protocol.md) describes what a tool does to
  share a board: column roles, the assignee, the comment marker (named
  after no tool), draft PR means testing by hand, the linked branch.
  CodeScope reads the results of any tool that writes that marker.
* **Who starts a dispatch.** An automatic dispatch on a person's
  CodeScope starts only when the `ProjectV2ItemStatusChangedEvent` on
  the item's issue or pull request is not `wasAutomated` and either
  names that person as actor, or names someone with write access to
  the repository while the item is assigned to that person alone (a
  lead hands out work by assigning it and moving it to *Ready*). Only
  moves made after the board was added count: an item that already
  sits in a trigger column then waits until it is moved again.
  Anything else waits in its column with the reason shown; it
  *needs you* only when a choice of the user's is all that is missing,
  such as which of several projects. A dispatch hands issue text to an
  agent that can push and post as the user; a workflow or an outsider
  must not be able to start one.
* **One item, one worktree, two phases.** *Research* ends when the
  agent has written its findings to `.codescope/findings.md` in the
  worktree and the session is idle. CodeScope adds that file to
  `.git/info/exclude`, as it does for attachments, so it neither makes
  the worktree dirty nor ends up in a commit. CodeScope checks that
  the findings have the expected sections, tells the session what is
  missing if they don't, and posts them itself: a comment with a
  marker and a closing *Next step* line, on the issue or, for a pull
  request item, on the PR. Open choices are *decision points* (B1, B2,
  …, each with lettered options and the agent's advice). Posted
  findings move the item to *Needs decision*: the user answers the
  decision points, if there are any, and decides whether it goes to
  *Ready*. For an issue, the *implementation* phase starts with the
  implementation round, which ends when the agent's pull request is
  open; for a pull request item it starts with a review round. It goes
  on through review rounds, and repairs on a pull request the dispatch
  opened, until the pull request is merged or closed. Moving an item
  to *Ready* after research starts the next round in the same session,
  resumed if its tab was closed. A new session starts in the same
  worktree from the findings when the session is lost, or when the
  user chose a different CLI for implementation.
* **What a move starts.** A pull request item that one of the user's
  dispatches opened starts nothing in either trigger column: that
  dispatch already works on it, and its issue is the item to move.
  Otherwise a move into *Research* starts the research round, on an
  issue or a pull request, and a move into *Ready* starts the round
  that fits the item:
  * an issue without an open pull request: the *implementation round*
    (`implement`), which ends when the agent's pull request is open;
  * an issue whose open pull request this dispatch opened, or a pull
    request item that no dispatch of the user's opened: a *review
    round* (`address-review`) on that pull request, which ends when
    the agent has handled the review and its session is idle: it has
    pushed, replied in the review threads it handles, or written a
    reply to `.codescope/reply.md`, which CodeScope excludes from git
    as it does the findings; when the round ends, CodeScope posts that
    reply on the pull request, with the marker, and removes the file;
  * an issue with an open pull request from elsewhere: nothing starts,
    and the item waits with that pull request shown until it is moved
    again. To work on that pull request, a person adds it to the board
    and moves it to *Ready*.

  Both rounds move the item to *In progress* when they start, and on
  to *In review* once their pull request is ready: when the round ends
  with a ready pull request, or when its draft is marked ready later,
  by anyone. That is the round's one move on. A round that stops
  without its result (findings, a pull request, a push, a reply)
  leaves its session idle, and the dispatch *needs you* until the user
  steers it in the tab.
* **Answers.** The needs-you dialog shows decision points as choices
  with the advice selected. The answer is posted as a comment where
  the findings are ("B1 A, B2 B"), and the dialog can move the item to
  *Ready* afterwards. A conversation comment there after the findings,
  by anyone with write access to the repository, is an answer too, so
  a designer or a teammate can answer from GitHub. The findings and
  the answers reach the session with the prompt of the round that
  *Ready* starts, which lists each answer and who gave it. While the
  item waits in *Needs decision* the session gets nothing, so it does
  not start implementing before *Ready*. Comments by others are shown
  and start nothing.
* **Manual testing decides what comes after the PR.** When the agent
  could test everything automatically, it opens a ready pull request
  and the item moves to *In review*. When something needs testing by
  hand (UI, another OS), it opens a draft with a test instruction in
  the body; the item stays in *In progress* and needs the user. *Ready
  for review* in the item drawer marks the PR ready and requests the
  reviewers from the project's settings; the item then moves to
  *In review* as it does for any draft marked ready.
* **Turn.** Every dispatched item shows whose turn it is and since
  when, worked out from what is on GitHub alone, so it is the same on
  every teammate's screen: an agent (and whose), CI, a person (*you*,
  or a GitHub login), the reviewers, or nobody. *Needs you* adds what
  only the user's own CodeScope knows, such as a session waiting for
  input, so it shows only to them. The board panel filters on *Mine*
  or *All*; the toast and the bell fire only for the user's own
  dispatches.
* **Branch and link.** The branch is `<number>-<slug>`, the `gh issue
  develop` default, and can be edited in the manual dispatch dialog.
  Research uses it as a local branch. When implementation starts,
  CodeScope creates it on GitHub as a linked branch on the same commit
  (`createLinkedBranch`), so the PR links to the issue and merging it
  into the default branch closes the issue. GitHub ignores the link
  and `Closes #N` for a pull request into any other branch, so a
  stacked pull request leaves its issue open, and the item waits in
  *In review* until a person closes it. The worktree row in
  `projects.json` also records the item, so CodeScope knows the link
  without a query and after GitHub drops the branch link once the PR
  exists. A pull request item is worked on in a worktree on its own
  branch.
* **Skills, not prompt files.** Each round (the research round, the
  implementation round, a review round, a repair) runs a skill, named
  per project and by default
  `research`, `implement`, `address-review` and `repair`, in the open
  Agent Skills format (`SKILL.md`) that Claude Code, Codex, Gemini CLI,
  OpenCode and Copilot read. The agent finds it the way it finds any
  skill: in the worktree's repository skills, or in the user's
  personal skills for repositories that are not the user's to commit
  to. Without one, CodeScope's built-in prompt runs. A team shares how
  it researches and implements through its skills, whichever tool
  starts the session. CodeScope always appends its own handover rules
  (where the findings go, the decision-point format, when to open a
  draft, `Closes #N` and the user as assignee in the pull request, and
  no comments of the agent's own on the issue or the pull request,
  because CodeScope posts what a round produces; in a review round
  the agent may reply in the review threads it handles, and writes any
  other reply to `.codescope/reply.md`, as it does its answer to a
  forwarded comment), so a skill describes the work
  and cannot break the handover.
  CodeScope sets no agent per phase: agent definitions in the
  repository (such as `.claude/agents/`) reach the session through the
  worktree, as skills do, so a skill can hand work to them. The CLI
  per phase stays local, because it depends on what the user has
  installed.
* **Setup command.** A new worktree runs `.codescope/setup` from the
  repository's default branch in the project folder, else the one in
  `projects.json`, else nothing.
* **Skills come from the item's branch.** The agent loads skills and
  `CLAUDE.md` from the worktree, so a branch can change them for its
  own dispatch. A pull request item from a fork therefore needs the
  user before it is dispatched.
* **Polling, not push.** Each board is read every 30 s while the app
  runs, panel open or not, with a hand-written `gh api graphql` query
  (about 1 point per 100 items). The latest comments, mergeability and
  check status are read only for dispatched items: asking them for
  every item makes the query several times as expensive.
  `gh project item-list` fetches every field value
  and costs about 101 points per 100 items, so polling it once a
  minute would exceed the 5,000-point hourly limit. There is no push
  for a desktop app: the `projects_v2_item` webhook is organisation
  only, in preview, and needs a public URL. Triggers made while
  CodeScope is closed are dispatched at the next launch.
* **Limits and attention.** At most three of a user's dispatches run at
  once (a setting); the rest wait in their trigger column. The existing OS
  toast (`idle_notifier`) and an entry in the in-app bell fire when
  findings are ready, when a dispatch *needs you*, and when a session
  may be waiting on the user: in a pending tool call for over three
  minutes without its transcript growing. `notify_rust` cannot route a
  click on a Windows toast, so
  the bell entry is what opens the tab. A permission prompt writes
  nothing until it is answered, so it cannot be told apart from a long
  tool call
  (`agents::claude::telemetry`, `agents::codex::telemetry`). The
  permission mode comes from the phase's agent profile; CodeScope does
  not override it.
* **Repair.** On a red check, a merge conflict, or a base branch the
  pull request must catch up with, CodeScope sends the repair skill
  into the session. A repair handles only that and leaves review
  comments alone. At most two attempts in a row; a comment by someone
  else with write access starts the count again. The session stops and
  the dispatch *needs you* when a conflict involves a functional
  choice, or when the
  failure does not come from its branch (the base is red too, a flaky
  test). Repairs run only while the pull request has no submitted
  review, because a push after a review invalidates approvals.
* **After review.** Review comments are the user's to handle in the
  tab, or to hand back by moving the item to *Ready*, which starts a
  review round in the same session; it repairs whatever is red or
  conflicting as well. Until the first review is submitted, a
  conversation comment on the agent's pull request by someone else
  with write access is sent into the session. After that, every
  comment waits for *Ready*, because a push after a review invalidates
  approvals. "Someone else" leaves out the user: the agent posts as
  them, and they steer their own session in its tab. Repairs and
  forwarded comments are only for the pull request the dispatch
  opened: a pull request item that no dispatch of the user's opened is
  worked on only in the rounds a person's move starts.
* **Cleanup.** Merged: worktree removed, session closed, history kept
  ([ADR-0015](#adr-0015--closed-session-retention-policy-100-per-worktree-90-day-ttl)).
  Closed without a PR: removed only when the worktree is clean and has
  no commits of its own, otherwise the row is marked. Moved by the
  user to a column that is not a trigger column: no round starts,
  nothing more is sent into the session and nothing is removed; what
  the round already produced is still posted. CodeScope never stops an
  agent that is working.
* **GitHub first, other hosts behind one seam.** The first version
  supports GitHub through `gh` only. Board access (read a board, move
  an item, link an item to a branch) sits behind one seam that does
  not assume GitHub's *Done* workflow, linked branches, or a status
  field with option ids. Gitea 28 and later has a REST API for boards
  at repository, organisation and user level
  ([release notes](https://blog.gitea.com/release-of-28.0.0/)).
  Forgejo has no board API in a release yet. GitLab CE boards are
  label lists.

**Consequences:**

* CodeScope writes to GitHub for the first time: statuses, assignees,
  linked branches, findings, answers to decision points, the comment
  that reports a pull request, a review round's reply on the pull
  request, and marking a draft ready with its reviewers. Pull
  requests and pushes come from the agent, so implementation needs an
  agent that can run `gh`; research works with any agent that can
  write a file.
* A repository's skills steer every dispatch on it, for everyone on
  the board, whichever tool starts the session.
* Tools share a board safely as long as each follows the board
  protocol, so working together with another tool starts with
  agreeing on it.
* Linked branches are in public preview. If they change, the worktree
  row still holds the link, and the pull request says `Closes #N`
  anyway, which other hosts understand as well. Either way only a
  merge into the default branch closes the issue.
* Every added board costs one GraphQL call per 30 s for as long as the
  app runs.
* The Bots inbox
  ([ADR-0023](#adr-0023--bots-inbox-is-a-read-only-view-of-the-lab-control-plane))
  stays as it is and is not part of this; a bot that takes board items
  would be a decision of its own.
* Screens are designed before any of this is built.
