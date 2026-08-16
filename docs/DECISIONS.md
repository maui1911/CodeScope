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
pipeline ([ADR-0019](#adr-0019), [PRs #201 / #202](https://github.com/maui1911/CodeScope/pulls)).

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
* `docs/PARITY-AUDIT.md` is closed-stamped historical.
* The last commit where the C# source tree builds is tagged
  `legacy/v0.2.6-final` on the cutover-1 merge commit. The C# code is
  fully removed in cutover-2; the workspace is flattened and the
  Velopack pack id is renamed to `codescope` in cutover-3.
* Historical `v0.2.X` GitHub releases stay published; the
  `update_check` floor filter (see [ADR-0020](#adr-0020) and PR #206)
  keeps them out of the live update flow.
* Solo dogfooding: no preflight migration release is shipped. Users
  (= the developer) reinstall once between the `codescope-rs` and
  `codescope` Velopack pack ids; `%APPDATA%\CodeScope\projects.json`
  carries across unchanged.

