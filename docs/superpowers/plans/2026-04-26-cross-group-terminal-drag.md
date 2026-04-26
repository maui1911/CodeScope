# Cross-group terminal drag — Implementation Plan

Spec: `docs/superpowers/specs/2026-04-26-cross-group-terminal-drag-design.md`
Branch: continue on `feature/session-history-per-worktree` (still local).
Commit cadence: one commit per logical step below.

## Steps

### 1. `ISessionViewHostPool` + `SessionViewHostPool`
- New files in `src/CodeScope.Ui/Services/`.
- Pool is a `Dictionary<string, SessionTabView>` keyed by `SessionDescriptor.Id`.
- `Acquire(id, factory)`: returns existing or creates via factory, stores, returns.
- `TryGet(id)`: dictionary lookup.
- `Release(id)`: detach from current parent, run the same teardown the
  `SessionTabView.Unloaded` handler runs today, remove from dict.
- Dispatcher-affined; no locking — WPF UI thread only.
- Register in DI as singleton in `App.xaml.cs`.

### 2. Off-screen parking host in `MainWindow`
- Add a `Border x:Name="SessionParking" Visibility="Hidden" IsHitTestVisible="False" Width="0" Height="0"` to the root grid (zero footprint, but parented).
- Expose via a static `MainWindow.ParkSessionView(SessionTabView v)` that the pool can call when `Acquire` materialises a new view.
- Alternative: pool just holds the view by reference and parks it under an internal `Decorator` it owns; that avoids reaching into MainWindow. Pick the alternative — keeps the pool self-contained.

### 3. `EditorGroupView` rewrite
- XAML: replace the `ItemsControl` with a single `ContentControl x:Name="ActiveSlot"`.
- Code-behind: subscribe to `EditorGroupViewModel.PropertyChanged` for `SelectedTab`. On change:
  1. If `Content` is currently a `SessionTabView`, return it to the pool's parking decorator (i.e. just clear `Content` — pool will re-acquire on next request from any group; we add a `Park(view)` method on the pool to make the intent explicit and re-attach it to the parking decorator).
  2. Resolve the new selected tab: `pool.Acquire(SelectedTab.Id, () => new SessionTabView { DataContext = SelectedTab })`. Set as `ActiveSlot.Content`.
- Drag/drop handlers unchanged.
- Keep the `IsFocused` accent rail and `OnGroupGotKeyboardFocus` logic.

### 4. `SessionTabView.Unloaded` teardown removal
- Drop the `Unloaded += TeardownShell` line.
- Keep `TeardownShell` itself but make it `internal` so the pool can call it from `Release`. Rename to `Teardown()`.

### 5. `MainViewModel.MoveTabToGroup` simplification
- Drop the entire agent-resume / telemetry-rebind block (lines ~865–889).
  Add a comment noting *why* (HWND survives now, no respawn).
- Same-group reorder path: unchanged.
- Cross-group: just the collection moves + selection / focus shuffle.

### 6. Pool.Release on close paths
- `MainViewModel.CloseTabAsync` → after collection removal, `pool.Release(id)`.
- `MainViewModel.RestartSessionAsync` → release before duplicate.
- `SessionStore.RemoveWorktreeAsync` cascade → release each closed session id.
  The cascade is currently triggered through `CloseWorktreeSessionsAsync`
  callback in `MainViewModel`; release sits there.

### 7. DI wiring
- `App.xaml.cs` registers `ISessionViewHostPool` as singleton.
- Pass to `MainViewModel` constructor (extends ctor — bump the call site too).
- `EditorGroupView` resolves via `Application.Current.MainWindow.DataContext` →
  `MainViewModel.SessionViewPool` (mirror existing access pattern in
  `OnGroupPreviewMouseDown`).

### 8. Tests
- `tests/CodeScope.Ui.Tests/Services/SessionViewHostPoolTests.cs`:
  Acquire returns same instance for same id; different ids → different;
  Release removes; TryGet null after release; double-release is no-op.
  These tests need a STA-thread test fixture since they touch
  `SessionTabView` (UserControl). The Ui.Tests project already has STA setup.
- `tests/CodeScope.Ui.Tests/ViewModels/MoveTabAcrossGroupsTests.cs`: build
  two groups, move a tab, assert the same `SessionTabViewModel` reference
  ends up in the target.
- Existing tests that mocked the agent-resume path in `MoveTabToGroup` need
  trimming — assertions that observed `Rebind` being called are removed.

### 9. Build, test, smoke
- `dotnet build CodeScope.sln -c Debug`
- `dotnet test CodeScope.sln -c Debug`
- Dev-build smoke per spec § Tests with `wpf-cli`.

### 10. Update `HANDOFF.md` + `DECISIONS.md`
- Move "Drag tab between groups loses scroll buffer" from rough-edges to a
  new shipped session entry.
- Add ADR-0014.

### 11. Commit
- One commit per step where it makes sense (1+2 together, 3 alone, 4+5 together,
  6 alone, 7+8 together, 9 verifies, 10 alone).
- Co-authored-by trailer per repo convention.
