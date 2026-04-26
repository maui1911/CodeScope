# Cross-group terminal drag without restart — Design

**Date:** 2026-04-26
**Status:** Approved (autonomous mode)
**Author:** Session 22

## Problem

Dragging a session tab between editor groups currently restarts the underlying
ConPTY child. Symptoms:

- pwsh respawns → all in-progress shell state, scrollback, env-var changes are gone.
- Claude / codex / OpenCode resumes via `--resume <id>` (added in session 20) so
  the conversation continues, but the terminal scrollback is wiped and the agent
  re-reads its initial banner.
- Telemetry tail keeps tracking when resume-by-id is available, but the user sees
  a visible flash and the cursor jumps.

Root cause is in WPF lifecycle, not in the terminal control:
`EditorGroupView` binds an `ItemsControl` to `EditorGroupViewModel.Tabs`. When
`MainViewModel.MoveTabToGroup` removes the `SessionTabViewModel` from the source
group's collection, WPF disposes that group's `ContentPresenter`, which unloads
`SessionTabView`, which destroys the inner `EasyTerminalControl` HwndHost, which
kills the ConPTY child. The target group then materialises a fresh
`SessionTabView` from the `DataTemplate` — a brand-new HWND with no buffer.

## Goal

Move a tab between groups with **zero restart**: same process, same scrollback,
same agent state, no telemetry hiccup.

## Approach: shared `SessionTabView` host pool

Decouple `SessionTabView` lifecycle from the WPF visual tree of any single
`EditorGroupView`. One `SessionTabView` instance per session id, owned by a
process-wide pool. Each `EditorGroupView` renders the **selected** tab's view by
asking the pool for the instance and parking the others elsewhere in the visual
tree. Moving a tab between groups becomes a pure VM operation; the visual
underneath the moved tab is the same WPF object, so the inner HwndHost is the
same object, so the HWND survives, so ConPTY survives, so scrollback survives.

WPF's documented HwndHost reparent-without-destroy behaviour applies because we
keep the same `HwndHost` instance across the move. (See:
<https://learn.microsoft.com/en-us/dotnet/desktop/wpf/advanced/hosting-win32-content-in-wpf>.)

### Why not other paths

- **TermPTY-only transfer** (`DisconnectConPTYTerm` → `ConPTYTerm` setter on a
  fresh control): preserves the process and agent state but the renderer's
  scrollback lives in the `Microsoft.Terminal.Wpf` HWND, not in TermPTY. The new
  HWND boots empty. A replay buffer would be approximate at best for full-screen
  TUIs (claude / codex / vim). Kept as a documented fallback if approach A turns
  up airspace gremlins around `GridSplitter` we can't tame.
- **Win32 `SetParent` on the inner ConPTY HWND**: HwndHost has no supported way
  to swap its child HWND between two host instances. Workarounds exist but are
  fragile across `Microsoft.Terminal.Wpf` versions. Rejected.

## Architecture

### Components

```
                 ┌──────────────────────────────┐
MainViewModel ─► │  ISessionViewHostPool        │ ◄──── owned by App DI (singleton, UI only)
                 │  ──────────────────────────  │
                 │  Acquire(sessionId, factory) │
                 │      → SessionTabView        │
                 │  Release(sessionId)          │
                 │  TryGet(sessionId)           │
                 └──────────────┬───────────────┘
                                │ same instance
                ┌───────────────┴────────────────┐
                ▼                                ▼
   EditorGroupView (group A)         EditorGroupView (group B)
   ContentControl pulled              ContentControl pulled
   from pool by SelectedTab.Id        from pool by SelectedTab.Id
```

- **`SessionViewHostPool`** (new, in `CodeScope.Ui/Services`): plain in-memory
  dictionary keyed by `SessionDescriptor.Id`. Returns a singleton
  `SessionTabView` per id, factoried on first request. `Release` runs the
  view's teardown (the same code that lives in `SessionTabView.Unloaded` today)
  and removes it. Thread-affined to the WPF dispatcher.
- **`EditorGroupView` rework**: drop the `ItemsControl<DataTemplate>` for
  per-tab `SessionTabView`s. Replace with a single `ContentControl` whose
  `Content` is the pool-resolved `SessionTabView` for `SelectedTab`. Background:
  the previous design used per-tab visibility triggers so each in-group tab kept
  its own HWND; the pool now owns that "keep-alive" responsibility for *all*
  non-active tabs across the whole window.
- **No explicit parking host needed.** WPF's documented behaviour is that an
  HwndHost removed from the visual tree is reparented under an internal
  SystemResources hidden window rather than destroyed. As long as the pool's
  managed reference keeps the `SessionTabView` from being garbage-collected,
  the inner HwndHost (and its child ConPTY HWND) stays alive. The
  pool-as-reference-keeper *is* the parking strategy — no `Border` placeholder
  is added to `MainWindow`. (An earlier draft of this spec called for an
  explicit parking `Border`; it turned out to be unnecessary belt-and-braces.)
- **`MainViewModel.MoveTabToGroup`**: keeps its VM-level work
  (Tabs.Remove → Tabs.Add, focus shuffle, group auto-collapse). Drops the
  agent-resume / telemetry-rebind hack at lines 869–889 — no longer needed
  because the process never dies.
- **`SessionTabView.Unloaded` teardown**: removed. The pool calls teardown via
  `Release`. `Unloaded` fires on every reparent and would tear ConPTY down on a
  drag if left in place.
- **`MainViewModel.CloseTabAsync` + cascade paths**: call `pool.Release(id)`
  after their existing soft-close logic. `RestartSessionAsync` releases the old
  view before duplicating.

### Data flow — drag between groups

```
user drops tab T from group A onto group B
    │
    ▼
EditorGroupView (B).OnStripDrop
    │
    ▼
MainViewModel.MoveTabToGroup(T, B)
    ├─ A.Tabs.Remove(T)        ◄── no Unloaded teardown anymore
    ├─ B.Tabs.Add(T)
    ├─ A.SelectedTab = A.Tabs[^1] (if any)
    ├─ B.SelectedTab = T
    └─ FocusedGroup = B
    │
    ▼
EditorGroupView (A) sees SelectedTab change
    └─ ContentControl.Content = pool.Acquire(A.SelectedTab.Id)  ◄── A's old leaf parks
EditorGroupView (B) sees SelectedTab change
    └─ ContentControl.Content = pool.Acquire(T.Id)              ◄── same instance, HWND intact
```

WPF's reparent-without-destroy guarantee covers the visual move. The
`Microsoft.Terminal.Wpf` HwndHost stays attached to the same `HwndSource`
(MainWindow's hwnd source) the whole time — only its WPF parent changes.

### Edge cases

- **Same-group drop (reorder)**: unchanged. The active selection doesn't change,
  the pool returns the same instance.
- **Close tab**: `MainViewModel.CloseTabAsync` calls
  `pool.Release(tab.Descriptor.Id)` after the existing soft-close + collection
  removal. Release runs the ConPTY teardown that used to live in `Unloaded`.
- **Restart session**: `RestartSessionAsync` duplicates first (the new tab
  spawns alongside the old one for a beat) then closes the old via
  `CloseTabAsync(hardRemove: true)`, which routes through `SessionViewPool.Release`.
  The brief overlap is intentional — UX is "zero-downtime restart" from the
  user's perspective. Two pwsh in the same cwd is harmless.
- **Cascade on worktree remove**: each cascaded session id triggers
  `pool.Release` so the ConPTY children actually die before
  `git worktree remove` runs (the original reason `Unloaded` had teardown).
- **Reopen from history**: `ReopenClosedSessionAsync` materialises a new
  `SessionTabViewModel`; the pool sees a fresh id and creates a fresh view.
- **App shutdown**: pool doesn't need explicit dispose — process exit kills the
  ConPTY children via the existing `ProcessTreeKiller` job object.
- **Group collapse**: when the source group becomes empty after a move, it's
  removed from `MainViewModel.Groups`. No pool churn (the moved tab is now in
  the target group; nothing else to do).

### Airspace caveat

WPF airspace rules apply: the moved `SessionTabView` is an HwndHost, so
WPF-level overlays drawn on top of it (toasts, command palette dropdown, drag
adorners) must be hosted in a top-level Popup or HwndSource. We already do this
for toasts; nothing changes there. The `GridSplitter` between groups continues
to draw correctly — splitters are siblings of the workspace columns, not
overlays.

## Public surface

```csharp
public interface ISessionViewHostPool
{
    /// Returns the SessionTabView for this session id, creating it via factory
    /// on first request. Subsequent calls return the same instance.
    /// Caller is responsible for parenting the returned view into a visual host.
    SessionTabView Acquire(string sessionId, Func<SessionTabView> factory);

    /// Returns the existing view if present, else null. Does not create.
    SessionTabView? TryGet(string sessionId);

    /// Detaches the view from any visual parent, runs ConPTY teardown,
    /// and removes it from the pool. Idempotent.
    void Release(string sessionId);
}
```

## Tests

- **`Ui.Tests` — pool unit tests** (no WPF needed beyond the existing test rig):
  `Acquire` returns same instance for same id; different ids → different
  instances; `Release` removes; `TryGet` returns null after release.
- **`Ui.Tests` — `MainViewModel.MoveTabToGroup` integration**: existing
  same-group reorder test plus a new cross-group move test that asserts the
  moved tab's `SessionTabViewModel` reference is unchanged in the target group.
- **Hand-tested smoke (wpf-cli, dev build)**:
  1. Start two groups via `Ctrl+\`.
  2. Run `for ($i=0;$i -lt 50;$i++) { Write-Host "line $i" }` in group A's
     pwsh; scroll up.
  3. Drag the tab to group B.
  4. Verify scrollback remains, no flicker, no respawn, prompt unchanged.
  5. Repeat with `claude` mid-tool-call: drag should not interrupt.
  6. Drag back to group A.

## Risks

- **WPF reparent-without-destroy is documented but version-sensitive**. If a
  future WPF update tightens HwndHost teardown on visual-tree removal, we'd
  regress. Mitigation: the parking host keeps the view continuously parented;
  WPF only reparents under the temporary SystemResources window when an
  HwndHost is *fully* removed. As long as the pool always re-parents in the
  same dispatcher tick, we never hit that path.
- **Z-order on first paint after a move**: HwndSource z-order can desync after
  reparent. Mitigation: call `HwndSource.UpdateZOrder()` post-attach if we see
  it. Not pre-emptive — only if smoke surfaces it.
- **Drag perf with 50+ sessions parked off-screen**: every parked
  `EasyTerminalControl` has a live HWND and a live ConPTY child. This is no
  worse than today's behaviour where every open tab keeps a live ConPTY (each
  `SessionTabView` is materialised at construction even when collapsed). No
  regression expected.

## Out of scope

- Floating tab windows (drag a tab out into a top-level window). Future work.
- Drag-tab-out-of-app to other windows. Future work.
- Visual drag adorner polish (the floating-chip motion spec) — separate item
  in HANDOFF backlog.

## Files touched (estimated)

- `src/CodeScope.Ui/Services/ISessionViewHostPool.cs` (new)
- `src/CodeScope.Ui/Services/SessionViewHostPool.cs` (new)
- `src/CodeScope.Ui/Views/EditorGroupView.xaml` — swap ItemsControl for
  ContentControl
- `src/CodeScope.Ui/Views/EditorGroupView.xaml.cs` — wire pool lookup on
  `SelectedTab` change
- `src/CodeScope.Ui/Views/SessionTabView.xaml.cs` — drop `Unloaded` teardown
- `src/CodeScope.Ui/ViewModels/MainViewModel.cs` — drop the agent-resume hack
  in `MoveTabToGroup`; call pool.Release on close paths
- `src/CodeScope.App/App.xaml.cs` — register `ISessionViewHostPool` in DI
- `tests/CodeScope.Ui.Tests/Services/SessionViewHostPoolTests.cs` (new)
- `tests/CodeScope.Ui.Tests/ViewModels/MainViewModelMoveTabTests.cs`
  (new or extend existing)
- `docs/HANDOFF.md` — close the rough edge entry
- `docs/DECISIONS.md` — new ADR-0014 documenting the pool pattern
