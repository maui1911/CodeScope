# CodeScope Rust Port: Parity Audit

> **Closed 2026-05-14.** The Rust port reached daily-driver parity at
> `rs-v0.3.0-rc.5` (session 41) and the C# build was retired the same
> day. See [ADR-0022](DECISIONS.md) and
> [MIGRATION-csharp-to-rust.md](MIGRATION-csharp-to-rust.md). The
> "mirror C# 1:1" rule that drove this audit is retired with the C#
> code; gaps listed below are historical context, not active TODOs.

## Executive Summary

The Rust port has completed 4 major subsystems: window-state & layout persistence, Claude telemetry, sidebar context menus, and the new-worktree dialog. The remaining work breaks into three tiers: missing agent telemetry services (Copilot, OpenCode, Pi) and single-feature dialogs (15 tiny items), cross-cutting system features (crash logging, keyboard shortcuts, notifications) totaling about 6-8 engineer-weeks of medium-difficulty work, and one structural blocker (session-state tracking) that gates everything UI-facing. This memo catalogs 25+ gaps discovered by file-by-file comparison of the C# implementation against current Rust code.

---

## Gaps by Size (Tiny First)

### TINY (0-1 day each, single-file)

- **Copilot Telemetry Service** (CopilotTelemetryService.cs) - NOT PORTED: Parse events.jsonl; mirror Claude 1:1; 250ms poll fallback.
- **OpenCode Telemetry Service** (OpenCodeTelemetryService.cs) - NOT PORTED: Parse ~/.opencode/.sessions; transcript parsing; model discovery.
- **Pi Telemetry Service** (PiTelemetryService.cs) - NOT PORTED: Parse ~/.pi/.sessions; activity state.
- **Crash Log Handler** (App.xaml.cs LogFatal) - NOT PORTED: Write %LOCALAPPDATA%/CodeScope/crash.log on unhandled exception.
- **Command Palette Dialog** (CommandPaletteDialog.xaml.cs) - NOT PORTED: Searchable action list; Ctrl+K / Ctrl+Shift+P.
- **Confirm Dialog** (ConfirmDialog.xaml.cs) - PARTIAL: Used by UpdateService; generic yes/no/cancel.
- **Rename Dialog** (RenameDialog.xaml.cs) - NOT PORTED: Inline rename for projects/worktrees via context menu.
- **Memory Watchdog** (MemoryWatchdog.cs) - NOT PORTED: Dev-only log of working-set growth; warns if >50MB per 5-min.
- **Idle Toast Notifier** (WindowsIdleToastNotifier.cs) - NOT PORTED: WinRT toasts when window minimized + session done; 2s dedup window.
- **PR Status Poller** (PullRequestStatusPoller.cs) - NEEDS-VERIFICATION: Polls gh/tea every 30s; backoff cadence.

### SMALL (2-4 days each, 1-3 files)

- **Keyboard Shortcuts** (MainWindow.xaml KeyBindings) - NOT PORTED: 15 mappings - Ctrl+T/W/Tab/Shift+Tab/K/Shift+P/Shift+O/F/F5/pipe/Shift+Return/Alt+Left/Right/Ctrl+1-9.
- **Update Check Service** (UpdateService.cs) - NEEDS-VERIFICATION: Velopack background checks; 3-hour cadence; toast + confirm dialog.
- **New Project Dialog** (NewProjectDialog.xaml.cs) - NOT PORTED: Folder picker; appends to projects.json.
- **Multi-Agent Discovery** (4 ISessionDiscovery implementations) - PARTIAL: Claude done; Copilot/OpenCode/Pi missing.

### MEDIUM (1-2 weeks, 3-5 files + integration)

- **Full Telemetry Stack** (*TelemetryService + *SessionDiscovery, 4 of each) - PARTIAL: Claude mostly done; Copilot/OpenCode/Pi missing entirely. ~300 LOC per service.
- **Context Menus** (ContextMenuFactory.cs + SidebarView.xaml.cs) - DONE: Right-click for projects/worktrees; copy/reveal/terminal/new-worktree/remove/PR actions. Ported to sidebar.rs (2462 lines).
- **Process Tree Management** (ProcessTreeKiller.cs Win32 job objects) - NEEDS-VERIFICATION: Kill entire pty tree on app exit; check gpui-terminal integration.
- **PR Detection + Status** (GitHub/GiteaPullRequestService.cs) - NEEDS-VERIFICATION: gh/tea CLI wrappers; PR create; status polling; check Rust session-state wiring.

### BIG (structural blockers, 2-4 weeks each)

- **Session-State Tracking + Hydration** (SessionStore + SessionManager + app startup) - BLOCKER / NOT STARTED: Load projects.json + layout -> spawn pty per tab -> restore working directory + auto_type -> marshal telemetry streams -> surface in status bar. Blocks all UI-facing features (tabs, status bar, session resumption, PR detection). Estimated 3-4 weeks.
- **Themed Color System** (DesignTokens.xaml) - NEEDS-VERIFICATION: 20+ color tokens (accent, text primary/secondary/faint, panel, border); currently dark-only. Check theme.rs wiring to gpui.

---

## Already Ported (Verified)

✓ Window-state persistence (Rust: window_state.rs)
✓ Layout persistence including collapsed-projects (Rust: layout.rs)
✓ Claude telemetry - transcript tail, model discovery, token tracking (Rust: claude_telemetry.rs)
✓ Claude session discovery (Rust: claude_discovery.rs)
✓ Sidebar UI + context menus (Rust: sidebar.rs, 2462 lines)
✓ New-worktree dialog - branch picker + path input (Rust: new_worktree_dialog.rs)
✓ Notifications system - toast surface (Rust: notifications.rs)
✓ Git status / branch tracking (Rust: git.rs)
✓ Settings persistence (Rust: settings.rs)
✓ App paths & LOCALAPPDATA directory structure (Rust: paths.rs)
✓ Status bar - activity state, tokens, model display (done in prior pass)

---

## Next 3 Dispatches (Priority Order)

### 1. Session-State Hydration + Session Manager (3-4 weeks, CRITICAL PATH)
Load projects.json + layout state on startup; spawn one pty per open tab with restored working directory; re-run auto_type command (claude/copilot/etc.) to re-attach agent; marshal telemetry polls to session events; surface in status bar. This unblocks every remaining tab UI feature.

### 2. Keyboard Shortcuts (3-4 days, HIGH USER IMPACT)
Map 15 WPF KeyBindings to gpui key handlers in app.rs. Bind to AppShell methods (focus group, close tab, new tab, command palette). Users expect Ctrl+W/Ctrl+T on day 1.

### 3. Copilot + OpenCode + Pi Telemetry (1 week, PARALLEL-ABLE)
Copy Claude telemetry architecture - 250ms polling + FSWatcher + transcript parser. Add 3 new services to codescope-rs/core/src/. Medium priority (status-bar completeness; less common than Claude but needed for cross-platform usage).

---

## What NOT to Flag (WPF-specific, skipped)

- XAML layout markup (App.xaml, MainWindow.xaml) — UI framework artifact.
- WPF dependency properties/triggers/ControlTemplates — replaced by gpui equivalents.
- MVVM relay-command plumbing in ViewModels — Rust uses direct event handlers.
- Resource dictionary merging — Rust theme system is different architecture.
- Dispatcher marshaling — gpui handles threading internally.

---

## Known Items Tracked Separately

- Collapsed-projects persistence (#111) → DONE in layout.rs
- Copy-PR-URL (#112) → in flight; needs PR detection wired to session state
- Mono/sans split (#110) → DONE in theme.rs
- Session-state tracking → Big-Step-1 in separate tracking; gates all UI

---

Generated: 2026-05-10
