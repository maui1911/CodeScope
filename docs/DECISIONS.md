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
