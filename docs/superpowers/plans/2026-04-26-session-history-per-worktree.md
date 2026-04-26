# Session History per Worktree — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface soft-closed sessions per-worktree in the sidebar with explicit reopen, replacing the implicit auto-resume on *New session*.

**Architecture:** UI-only — `Session.ClosedAt` and `SoftCloseSessionAsync` / `RestoreSessionAsync` already exist in `CodeScope.Core`. Plan adds a `History` collection on `WorktreeViewModel`, projects closed sessions into it from the existing `SessionAdded` / `SessionRemoved` event stream, renders a collapsible disclosure in `SidebarView.xaml`, and wires reopen + rename + hard-delete actions. The auto-resume block in `MainViewModel.NewSessionAsync` is removed; reopen becomes explicit. During implementation a `SessionStoreChange.SessionSoftClosed` event was introduced in Core to give `SidebarViewModel.RemoveSession` a race-free signal distinguishing a soft-close demotion from a hard-remove, avoiding a store re-query race window.

**Tech stack:** .NET 10, C# 14, WPF, CommunityToolkit.Mvvm, NSubstitute (tests), FluentAssertions (tests).

**Spec:** `docs/superpowers/specs/2026-04-26-session-history-design.md`.

---

## File map

- **Create:** none
- **Modify:**
  - `src/CodeScope.Ui/ViewModels/WorktreeViewModel.cs` — new `History` collection + `IsHistoryExpanded` + derived `HasHistory` / `HistoryCount`
  - `src/CodeScope.Ui/ViewModels/SidebarViewModel.StoreSync.cs` — project closed sessions to `History`; route `SessionAdded` / `SessionRemoved` to the right collection
  - `src/CodeScope.Ui/ViewModels/MainViewModel.cs` — drop auto-resume block in `NewSessionAsync`; widen `CloseTabAsync` to soft-close shells too; add `ReopenClosedSessionAsync` command
  - `src/CodeScope.Ui/Views/SidebarView.xaml` — convert worktree `DataTemplate` to `HierarchicalDataTemplate` with `ItemsSource={Binding History}`; add history-row template + disclosure
  - `src/CodeScope.Ui/Views/SidebarView.xaml.cs` — extend double-click handler for history rows; add history-row context menu builder; add "Reopen most recent" item to worktree context menu
  - `tests/CodeScope.Core.Tests/SessionStoreTests.cs` — add cascade-clears-closed-sessions regression test
  - `docs/DECISIONS.md` — note the auto-resume removal
  - `docs/HANDOFF.md` — session entry at end

---

## Task 1: Cascade test — `RemoveWorktreeAsync` clears closed sessions

**Files:**
- Modify: `tests/CodeScope.Core.Tests/SessionStoreTests.cs` (add new fact at end)

- [ ] **Step 1: Read the existing soft-close tests for reference**

Read `tests/CodeScope.Core.Tests/SessionStoreTests.cs:606–697` to see the soft-close / restore test setup (helper `BuildStore`, mock signatures for `IGitService.RemoveWorktreeAsync` with `bool force` param, and the standard `Project + Worktree + Session` arrangement).

- [ ] **Step 2: Write the failing test**

Append to `tests/CodeScope.Core.Tests/SessionStoreTests.cs`, just before the final `}` of the class:

```csharp
[Fact]
public async Task RemoveWorktreeAsync_Cascades_To_Closed_Sessions()
{
    var (store, git, _) = BuildStore();
    var p = await store.AddProjectAsync("C:/repo", "repo");
    p.IsSuccess.Should().BeTrue();
    git.AddWorktreeAsync("C:/repo", "C:/repo-feat", "feat", null, default)
        .Returns(Result<bool>.Ok(true));
    var wt = await store.AddWorktreeAsync(p.Value.Id, "C:/repo-feat", "feat");
    wt.IsSuccess.Should().BeTrue();

    var openSession = new Session
    {
        Id = "open-1",
        WorktreePath = "C:/repo-feat",
        WorktreeId = wt.Value.Id,
        AgentId = "claude",
        AgentSessionId = "agent-open",
    };
    var closedSession = new Session
    {
        Id = "closed-1",
        WorktreePath = "C:/repo-feat",
        WorktreeId = wt.Value.Id,
        AgentId = "claude",
        AgentSessionId = "agent-closed",
    };
    (await store.AddSessionAsync(p.Value.Id, openSession)).IsSuccess.Should().BeTrue();
    (await store.AddSessionAsync(p.Value.Id, closedSession)).IsSuccess.Should().BeTrue();
    (await store.SoftCloseSessionAsync(closedSession.Id)).IsSuccess.Should().BeTrue();

    git.RemoveWorktreeAsync("C:/repo", "C:/repo-feat", false, default)
        .Returns(Result<bool>.Ok(true));
    git.ListWorktreesAsync("C:/repo", default)
        .Returns(Result<IReadOnlyList<WorktreeInfo>>.Ok(
            new[] { new WorktreeInfo { Path = "C:/repo", Branch = "main", IsPrimary = true } }));

    var removed = await store.RemoveWorktreeAsync(p.Value.Id, wt.Value.Id);

    removed.IsSuccess.Should().BeTrue();
    var project = store.Projects.Single(x => x.Id == p.Value.Id);
    project.Worktrees.Should().NotContain(w => w.Id == wt.Value.Id);
    project.Sessions.Should().NotContain(s => s.Id == openSession.Id);
    project.Sessions.Should().NotContain(s => s.Id == closedSession.Id,
        because: "soft-closed sessions on a removed worktree must cascade-delete");
}
```

If `BuildStore` returns a different tuple shape, adapt the destructuring to match the existing tests in this file (look at the `Async Task SoftCloseSessionAsync_Marks_Closed_And_Emits_Removed` test at line 606 for the canonical setup).

- [ ] **Step 3: Run the test — expect PASS (regression confirm)**

This test is a regression confirmation, not a TDD failure. The cascade already exists in `SessionStore.RemoveWorktreeAsync` (lines 441–446: `Sessions = p.Sessions.Where(s => s.WorktreeId != worktreeId).ToList()`). The test exists to lock in the behaviour for the history feature.

```pwsh
dotnet test tests/CodeScope.Core.Tests/CodeScope.Core.Tests.csproj `
  --filter "FullyQualifiedName~RemoveWorktreeAsync_Cascades_To_Closed_Sessions" -v normal
```

Expected: **PASS**. If it fails, the cascade is already broken upstream — stop and investigate before continuing.

- [ ] **Step 4: Commit**

```bash
git add tests/CodeScope.Core.Tests/SessionStoreTests.cs
git commit -m "$(cat <<'EOF'
test(session-store): cover cascade-removal of closed sessions

Lock in the existing behaviour where RemoveWorktreeAsync also drops
soft-closed sessions belonging to that worktree. Regression-only —
the cascade is already wired in RemoveWorktreeAsync. Needed before
introducing the history surface so a future change can't silently
strand orphaned closed sessions on a removed worktree.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Widen soft-close — shells also keep history

Today `MainViewModel.CloseTabAsync` only soft-closes resumable agents (Claude/Codex with `ResumeByIdArgs` + persisted `AgentSessionId`); shells get hard-removed via `RemoveSessionAsync`. The history surface is supposed to include shells (reopening = respawn pwsh in `WorktreePath`), so we widen the soft-close gate.

**Files:**
- Modify: `src/CodeScope.Ui/ViewModels/MainViewModel.cs` (around lines 681–695)

- [ ] **Step 1: Locate the resumable gate**

Read `src/CodeScope.Ui/ViewModels/MainViewModel.cs:661–696` to confirm the current shape:

```csharp
private async Task CloseTabAsync(SessionTabViewModel? tab, bool hardRemove)
{
    // ... tab/group lookup elided ...
    var resumable = !hardRemove
        && storedForTab?.AgentSessionId is { Length: > 0 }
        && !string.IsNullOrEmpty(storedForTab.AgentId)
        && !string.Equals(storedForTab.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase)
        && _agents.GetById(storedForTab.AgentId!)?.ResumeByIdArgs.Count > 0;

    if (resumable)
    {
        await _store.SoftCloseSessionAsync(tab.Descriptor.Id).ConfigureAwait(true);
    }
    else
    {
        await _store.RemoveSessionAsync(tab.Descriptor.Id).ConfigureAwait(true);
    }
    // ...
}
```

- [ ] **Step 2: Replace the gate with a soft-close-by-default rule**

Replace the `var resumable = …` block + the `if (resumable)` block with:

```csharp
    // History: every closed session is preserved (soft-close) unless the caller asks for a
    // hard remove (Restart, worktree-cascade rollback, etc.). Shells reopen as a fresh pwsh
    // in the same cwd; resumable agents (Claude/Codex) reopen via ResumeByIdArgs +
    // AgentSessionId. Sessions whose store row vanished mid-flight (storedForTab is null)
    // also fall through to RemoveSessionAsync — there is nothing to soft-close.
    var canSoftClose = !hardRemove && storedForTab is not null;
    if (canSoftClose)
    {
        await _store.SoftCloseSessionAsync(tab.Descriptor.Id).ConfigureAwait(true);
    }
    else
    {
        await _store.RemoveSessionAsync(tab.Descriptor.Id).ConfigureAwait(true);
    }
```

Update the XML doc comment on `CloseTabAsync` (lines 654–660) to match:

```csharp
/// <summary>
/// Closes a tab. When <paramref name="hardRemove"/> is <c>false</c> (default) the session is
/// <em>soft-closed</em> — the row stays in the store with <see cref="Session.ClosedAt"/> set,
/// reachable from the worktree's history surface. Reopen logic in
/// <c>ReopenClosedSessionAsync</c> resumes resumable agents via <c>--resume &lt;id&gt;</c> and
/// respawns shells in the original cwd. Callers that want the row gone for good (Restart,
/// worktree cascade) pass <c>true</c>.
/// </summary>
```

- [ ] **Step 3: Build and run the existing tests**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
```

Expected: build clean, all 148 tests pass (147 pre-existing + 1 from Task 1). No UI tests exist for `CloseTabAsync`, so the change is verified by build + Core suite + Task 6 smoke later.

- [ ] **Step 4: Commit**

```bash
git add src/CodeScope.Ui/ViewModels/MainViewModel.cs
git commit -m "$(cat <<'EOF'
feat(history): soft-close every session, including shells

Previously CloseTabAsync only soft-closed resumable agents (Claude/
Codex). Shells went straight to RemoveSessionAsync, so they had no
history representation. The upcoming per-worktree history surface
shows every closed session — shells respawn fresh in the original
cwd on reopen, agents resume by id. hardRemove (Restart, cascade
rollback) still hard-deletes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Drop auto-resume in `NewSessionAsync`

**Files:**
- Modify: `src/CodeScope.Ui/ViewModels/MainViewModel.cs` (lines 365–385 — the auto-resume block)
- Modify: `docs/DECISIONS.md` (append a new ADR entry)

- [ ] **Step 1: Read the current auto-resume block**

Read `src/CodeScope.Ui/ViewModels/MainViewModel.cs:365–385`. The block looks like:

```csharp
    // Soft-close resume — if this worktree has a closed session that would match the agent
    // ... (full block including the FirstOrDefault lookup and TryRestoreSessionAsync call)
    var resolvedAgentId = ResolveAgentIdForNewSession(project, agentId);
    if (worktree is not null
        && resolvedAgentId is not null
        && !string.Equals(resolvedAgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase))
    {
        var closed = project.Sessions.FirstOrDefault(s =>
            s.ClosedAt is not null
            && string.Equals(s.WorktreeId, worktree.Id, StringComparison.Ordinal)
            && string.Equals(s.AgentId, resolvedAgentId, StringComparison.OrdinalIgnoreCase)
            && s.AgentSessionId is { Length: > 0 });
        if (closed is not null && await TryRestoreSessionAsync(project, worktree, closed).ConfigureAwait(true))
        {
            return;
        }
    }
```

- [ ] **Step 2: Remove the auto-resume block**

Delete those lines. Keep the `var resolvedAgentId = ResolveAgentIdForNewSession(project, agentId);` line — it's still used a few lines later by the `useShell` / `agent` resolution. After the deletion, `ResolveAgentIdForNewSession` is called once and its return value flows directly into the existing fresh-session creation path. The `TryRestoreSessionAsync` method itself stays — it gets reused by Task 7's reopen command.

The function body should now move directly from the `resolvedAgentId` assignment to the `var useShell = …` line (currently at 389) with no intermediate logic.

- [ ] **Step 3: Add a `docs/DECISIONS.md` entry**

Read the last ADR in `docs/DECISIONS.md` to match the style (numbering + heading format). Append a new entry:

```markdown
## ADR-NN — Auto-resume on `New session` removed; per-worktree history is the explicit surface

**Date:** 2026-04-26
**Status:** Accepted

### Context

`MainViewModel.NewSessionAsync` previously matched any soft-closed session for
`(worktree, agent)` and restored it transparently in place of minting a fresh
session. The implicit behaviour shipped because there was no UI to see closed
sessions. Side-effect: a second closed conversation on the same worktree was
shadowed by `FirstOrDefault`.

### Decision

Drop the auto-resume block. *New session* always mints fresh. Reopening a
closed session is an explicit action driven by the per-worktree history
surface (sidebar disclosure under each worktree + a "Reopen most recent
closed session" item in the worktree context menu).

### Consequences

- Returning users learn one new affordance (history disclosure) and the
  one-keystroke worktree-context-menu shortcut for the most-recent-closed
  case.
- The `TryRestoreSessionAsync` helper stays as the implementation of explicit
  reopen.
- The drag-between-groups path (`MoveTabToGroup`) is unaffected — it never
  routed through the auto-resume block.
```

(Replace `ADR-NN` with the next sequential number used in the file.)

- [ ] **Step 4: Build and test**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
```

Expected: build clean, 148/148 pass.

- [ ] **Step 5: Commit**

```bash
git add src/CodeScope.Ui/ViewModels/MainViewModel.cs docs/DECISIONS.md
git commit -m "$(cat <<'EOF'
feat(history): remove implicit auto-resume on New session

New session now always mints fresh. Restoring a closed session is
explicit, driven by the per-worktree history surface coming in the
next commits. TryRestoreSessionAsync stays — it becomes the body of
the explicit Reopen command. Drag-between-groups (MoveTabToGroup) is
on a different code path and unaffected.

ADR added to docs/DECISIONS.md.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `WorktreeViewModel.History` collection + derived members

**Files:**
- Modify: `src/CodeScope.Ui/ViewModels/WorktreeViewModel.cs`

- [ ] **Step 1: Add the `History` collection alongside `Sessions`**

In the constructor (around line 17), after `Sessions = [];`, add:

```csharp
        History = [];
        History.CollectionChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(HasHistory));
            OnPropertyChanged(nameof(HistoryCount));
            OnPropertyChanged(nameof(HistoryHeader));
        };
```

After the `public ObservableCollection<SessionTabViewModel> Sessions { get; }` declaration (line 58), add:

```csharp
    /// <summary>
    /// Soft-closed sessions belonging to this worktree. Sorted by store-projection logic
    /// (most-recent <c>ClosedAt</c> first). Empty for fresh worktrees; the sidebar history
    /// disclosure is hidden when <see cref="HasHistory"/> is false.
    /// </summary>
    public ObservableCollection<SessionTabViewModel> History { get; }

    [ObservableProperty]
    private bool _isHistoryExpanded;

    public bool HasHistory => History.Count > 0;

    public int HistoryCount => History.Count;

    /// <summary>"History (3)" — bound by the sidebar disclosure header.</summary>
    public string HistoryHeader => $"History ({HistoryCount})";
```

- [ ] **Step 2: Build to confirm no compile errors**

```pwsh
dotnet build src/CodeScope.Ui/CodeScope.Ui.csproj -c Debug
```

Expected: build clean.

- [ ] **Step 3: Commit**

```bash
git add src/CodeScope.Ui/ViewModels/WorktreeViewModel.cs
git commit -m "$(cat <<'EOF'
feat(sidebar): WorktreeViewModel.History collection scaffold

New ObservableCollection plus IsHistoryExpanded / HasHistory /
HistoryCount / HistoryHeader. Used by the upcoming sidebar
disclosure. No projection wiring yet — that lands in the next
commit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Project closed sessions into `History` from the store

**Files:**
- Modify: `src/CodeScope.Ui/ViewModels/SidebarViewModel.StoreSync.cs`

- [ ] **Step 1: Update `BuildProjectVM` to seed both collections**

Replace the closed-sessions-skip block at lines 75–82 with a two-pass projection. Replace:

```csharp
            // Soft-closed sessions persist on disk but have no live tab — skip them in the tree
            // so the worktree row reflects "no active session" and offers New session (which
            // will resume the soft-closed conversation transparently).
            foreach (var s in p.Sessions.Where(x => x.WorktreeId == wt.Id && x.ClosedAt is null))
            {
                wvm.Sessions.Add(BuildSessionRow(p.Id, wt.Id, s));
            }
```

With:

```csharp
            // Open sessions → live row; soft-closed sessions → history (sorted most-recent first).
            // Both collections live on the worktree VM; the sidebar template hides History when empty.
            foreach (var s in p.Sessions.Where(x => x.WorktreeId == wt.Id && x.ClosedAt is null))
            {
                wvm.Sessions.Add(BuildSessionRow(p.Id, wt.Id, s));
            }
            foreach (var s in p.Sessions
                .Where(x => x.WorktreeId == wt.Id && x.ClosedAt is not null)
                .OrderByDescending(x => x.ClosedAt))
            {
                wvm.History.Add(BuildSessionRow(p.Id, wt.Id, s));
            }
```

- [ ] **Step 2: Route `SessionAdded` to the right collection**

Replace `AppendSession` (lines 182–193) with:

```csharp
    private void AppendSession(string projectId, Session session)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        if (pvm is null) { return; }
        var wvm = (session.WorktreeId is not null
            ? pvm.Worktrees.FirstOrDefault(w => w.Id == session.WorktreeId)
            : null)
            ?? pvm.Worktrees.FirstOrDefault(w => w.IsPrimary)
            ?? pvm.Worktrees.FirstOrDefault();
        if (wvm is null) { return; }

        // SessionAdded fires for both a brand-new live session AND for a restore (ClosedAt
        // cleared by RestoreSessionAsync). On restore, also clear any stale history row for
        // the same id so the session doesn't appear twice.
        if (session.ClosedAt is null)
        {
            var stale = wvm.History.FirstOrDefault(x => x.Descriptor.Id == session.Id);
            if (stale is not null) { wvm.History.Remove(stale); }
            wvm.Sessions.Add(BuildSessionRow(projectId, wvm.Id, session));
        }
        else
        {
            wvm.History.Insert(0, BuildSessionRow(projectId, wvm.Id, session));
        }
    }
```

- [ ] **Step 3: Route `SessionRemoved` to the right collection**

`SoftCloseSessionAsync` raises `SessionRemoved` *but the row is still in the store with `ClosedAt` set*. `RemoveSessionAsync` raises `SessionRemoved` *and the row is gone*. Distinguish by re-querying the store.

Replace `RemoveSession` (lines 195–205) with:

```csharp
    private void RemoveSession(string sessionId)
    {
        // Determine soft-close vs hard-remove by peeking the store. Soft-closed → move the
        // session row from Sessions to History (insert at top to honour ClosedAt-desc order).
        // Hard-removed → drop from whichever collection it's in.
        var stored = _store.Projects
            .SelectMany(p => p.Sessions.Select(s => (project: p, session: s)))
            .FirstOrDefault(x => x.session.Id == sessionId);

        foreach (var p in Projects)
        {
            foreach (var w in p.Worktrees)
            {
                var live = w.Sessions.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (live is not null)
                {
                    w.Sessions.Remove(live);
                    if (stored.session is { ClosedAt: not null })
                    {
                        // Re-issue a fresh row instead of reusing `live` so its IsActive flag
                        // and any tab-bar bindings reset cleanly to the dead-row state.
                        w.History.Insert(0, BuildSessionRow(p.Id, w.Id, stored.session));
                    }
                    return;
                }
                var dead = w.History.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (dead is not null) { w.History.Remove(dead); return; }
            }
        }
    }
```

- [ ] **Step 4: Update `RenameSession` to look in both collections**

Replace `RenameSession` (lines 207–217) with:

```csharp
    private void RenameSession(string sessionId, string? newName)
    {
        foreach (var p in Projects)
        {
            foreach (var w in p.Worktrees)
            {
                var live = w.Sessions.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (live is not null) { live.DisplayName = newName ?? live.Descriptor.Title; return; }
                var dead = w.History.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (dead is not null) { dead.DisplayName = newName ?? dead.Descriptor.Title; return; }
            }
        }
    }
```

- [ ] **Step 5: Build + run tests**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
```

Expected: build clean, 148/148 pass.

- [ ] **Step 6: Commit**

```bash
git add src/CodeScope.Ui/ViewModels/SidebarViewModel.StoreSync.cs
git commit -m "$(cat <<'EOF'
feat(sidebar): project closed sessions into WorktreeViewModel.History

BuildProjectVM seeds both Sessions (open) and History (closed). The
event-driven path moves rows between the two collections: soft-close
(SessionRemoved while the store row still has ClosedAt set) demotes
to History; restore (SessionAdded with ClosedAt null) promotes back
to Sessions and clears any stale history row. Rename touches
whichever collection the row lives in.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Sidebar XAML — disclosure + history row template

**Files:**
- Modify: `src/CodeScope.Ui/Views/SidebarView.xaml`

- [ ] **Step 1: Convert worktree `DataTemplate` to `HierarchicalDataTemplate`**

The current worktree row at line 256 is `<DataTemplate DataType="{x:Type vm:WorktreeViewModel}">` — a flat template. Convert it to:

```xml
<HierarchicalDataTemplate
    DataType="{x:Type vm:WorktreeViewModel}"
    ItemsSource="{Binding History}">
    <!-- existing 28-px Grid contents stay exactly as they are -->
</HierarchicalDataTemplate>
```

(Open the file, find `<DataTemplate DataType="{x:Type vm:WorktreeViewModel}">` and `</DataTemplate>` that closes it on line 393. Change the open tag and the close tag only — do NOT touch the inner `Grid`.)

- [ ] **Step 2: Add a history-row `DataTemplate`**

Above the worktree HierarchicalDataTemplate (so it's resolvable for the sub-items), add a small `DataTemplate` that draws a closed-session row. Place it in `<TreeView.Resources>` near line 250, just before the worktree template:

```xml
<!-- Closed-session row inside a worktree's History disclosure. Dim opacity, outline dot,
     relative-time suffix. Bound to SessionTabViewModel — same VM type as live rows, but
     parented under WorktreeViewModel.History so the binding resolves to this template. -->
<DataTemplate x:Key="SidebarHistoryRowTemplate" DataType="{x:Type vm:SessionTabViewModel}">
    <Grid Height="24" Opacity="0.55">
        <Grid.ColumnDefinitions>
            <ColumnDefinition Width="36" />     <!-- indent past the dot column -->
            <ColumnDefinition Width="Auto" />   <!-- 6px outline dot -->
            <ColumnDefinition Width="*" />      <!-- session title -->
            <ColumnDefinition Width="Auto" />   <!-- relative timestamp -->
        </Grid.ColumnDefinitions>

        <Ellipse Grid.Column="1"
                 Width="6" Height="6"
                 Margin="0,0,8,0"
                 VerticalAlignment="Center"
                 Stroke="{DynamicResource Text.Faint}"
                 StrokeThickness="1"
                 Fill="Transparent" />

        <TextBlock Grid.Column="2"
                   Text="{Binding DisplayName}"
                   FontFamily="{DynamicResource Fig.Font.Mono}"
                   FontSize="11"
                   VerticalAlignment="Center"
                   Foreground="{DynamicResource Text.Secondary}"
                   TextTrimming="CharacterEllipsis" />

        <TextBlock Grid.Column="3"
                   Margin="8,0,12,0"
                   Text="{Binding ClosedAtRelative}"
                   FontFamily="{DynamicResource Fig.Font.Mono}"
                   FontSize="10"
                   VerticalAlignment="Center"
                   Foreground="{DynamicResource Text.Faint}" />
    </Grid>
</DataTemplate>
```

- [ ] **Step 3: Add `ClosedAtRelative` on `SessionTabViewModel`**

The history row binds to `ClosedAtRelative` on `SessionTabViewModel`, which doesn't exist yet. Open `src/CodeScope.Ui/ViewModels/SessionTabViewModel.cs` and add:

```csharp
    /// <summary>
    /// When this row is a closed-session entry in a worktree's history, holds the
    /// ClosedAt timestamp. Null for live tabs. Set by <see cref="SidebarViewModel"/>
    /// projection (BuildSessionRow) when the source <see cref="Session"/> has a
    /// non-null <c>ClosedAt</c>.
    /// </summary>
    public DateTimeOffset? ClosedAt { get; init; }

    /// <summary>"2h ago" / "yesterday" / "3d ago" / "Mar 14" — empty for live rows.</summary>
    public string ClosedAtRelative
    {
        get
        {
            if (ClosedAt is not { } when_) { return string.Empty; }
            var delta = DateTimeOffset.UtcNow - when_;
            if (delta < TimeSpan.FromMinutes(1)) { return "just now"; }
            if (delta < TimeSpan.FromHours(1))   { return $"{(int)delta.TotalMinutes}m ago"; }
            if (delta < TimeSpan.FromHours(24))  { return $"{(int)delta.TotalHours}h ago"; }
            if (delta < TimeSpan.FromDays(2))    { return "yesterday"; }
            if (delta < TimeSpan.FromDays(7))    { return $"{(int)delta.TotalDays}d ago"; }
            return when_.LocalDateTime.ToString("MMM d");
        }
    }
```

Then update `BuildSessionRow` in `SidebarViewModel.StoreSync.cs` (around line 97) to forward `ClosedAt`:

```csharp
    private static SessionTabViewModel BuildSessionRow(string projectId, string worktreeId, Session s)
    {
        var descriptor = new SessionDescriptor
        {
            Id = s.Id,
            WorkingDirectory = s.WorktreePath,
            Shell = "pwsh.exe",
            ShellArgs = [],
            Title = s.DisplayName ?? s.AgentId ?? System.IO.Path.GetFileName(s.WorktreePath),
        };
        return new SessionTabViewModel(descriptor, projectId, s.AgentId, s.DisplayName)
        {
            IsActive = false,
            ClosedAt = s.ClosedAt,
        };
    }
```

If `SessionTabViewModel`'s constructor doesn't allow init-only properties on the public surface (because of CommunityToolkit `ObservableObject` patterns), make `ClosedAt` a settable `init` property — the surface is already `public sealed partial class SessionTabViewModel : ObservableObject` so plain `init` works.

- [ ] **Step 4: Wire the disclosure header on the worktree row**

Inside the worktree `HierarchicalDataTemplate`'s `Grid` (the existing 28-px row), there's no slot for an inline disclosure — TreeView nests the children under the worktree row automatically. The disclosure visibility is the *existence* of `History` items: when `History` is empty, the TreeViewItem's expand chevron simply has nothing to expand to. To make the header explicit, add a "History (N)" text row as the *first synthetic child* via an alternative — instead, lean on the TreeView default behaviour and add a small `History (N)` label inside the worktree row itself, right-aligned under `StatusLabel`, only when `HasHistory` is true.

Insert into the worktree row Grid, after the `StatusLabel` TextBlock at line 391:

```xml
<TextBlock
    Grid.Column="4"
    Margin="8,0,12,0"
    Text="{Binding HistoryHeader}"
    FontFamily="{DynamicResource Fig.Font.Mono}"
    FontSize="10"
    VerticalAlignment="Center"
    Foreground="{DynamicResource Text.Faint}"
    Visibility="{Binding HasHistory, Converter={StaticResource BoolToVisibility}}" />
```

Wait — Column 4 is already taken by `StatusLabel`. Add another column to the Grid `ColumnDefinitions` (currently 5 columns at lines 258–263) at the right end:

```xml
<ColumnDefinition Width="Auto" />  <!-- history-count badge -->
```

And put the new TextBlock in `Grid.Column="5"`. Keep `StatusLabel` in column 4 unchanged.

(If after build the layout looks crowded, drop the per-row badge and rely solely on the tree's expand chevron — the History rows themselves are the discoverable surface. Document the decision in the commit if you take that fallback.)

- [ ] **Step 5: Tell the TreeViewItem template to use the history row template for child items**

The TreeViewItem item-template selector falls through `DataType="{x:Type vm:SessionTabViewModel}"`. There is no DataTemplate defined for `SessionTabViewModel` in the sidebar (live sessions aren't rendered as TreeView children — they live in the tab strip). So the template defined in Step 2 (`SidebarHistoryRowTemplate`) needs to bind by `DataType`, not `x:Key`.

Change Step 2's template open tag from:

```xml
<DataTemplate x:Key="SidebarHistoryRowTemplate" DataType="{x:Type vm:SessionTabViewModel}">
```

to (keep DataType, drop the key — implicit lookup by DataType):

```xml
<DataTemplate DataType="{x:Type vm:SessionTabViewModel}">
```

Now any `SessionTabViewModel` instance encountered as a TreeView item (i.e. those in `WorktreeViewModel.History` since that's what the HierarchicalDataTemplate exposes) renders with this template.

- [ ] **Step 6: Build, run the app, and verify**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
```

Then run the dev build:

```pwsh
$env:CODESCOPE_DEV = "1"
dotnet run --project src/CodeScope.App
```

Manual checks:
- A worktree with no closed sessions has no history-related visual cruft (no chevron expansion target, no badge).
- Close a Claude tab → the worktree row shows "History (1)", expanding the worktree reveals the dim closed-session row with a relative timestamp.
- Close another → "History (2)" updates, and the rows are sorted with most-recent first.
- Use wpf-cli to capture before/after screenshots if available.

- [ ] **Step 7: Commit**

```bash
git add src/CodeScope.Ui/Views/SidebarView.xaml src/CodeScope.Ui/ViewModels/SessionTabViewModel.cs src/CodeScope.Ui/ViewModels/SidebarViewModel.StoreSync.cs
git commit -m "$(cat <<'EOF'
feat(sidebar): render closed-session history under each worktree

Worktree row becomes HierarchicalDataTemplate with ItemsSource bound
to History. Closed-session rows render with a dim opacity, an outline
dot, and a relative-time suffix ("2h ago" / "yesterday" / "Mar 14").
A History (N) badge sits next to the status slug — empty when there
is no history. SessionTabViewModel grows ClosedAt + ClosedAtRelative
so the same VM type covers both live and dead rows.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `ReopenClosedSessionAsync` command

**Files:**
- Modify: `src/CodeScope.Ui/ViewModels/MainViewModel.cs`

- [ ] **Step 1: Generalise `TryRestoreSessionAsync` for shells**

The current implementation at lines 559–598 short-circuits for shells (lines 561–565). Lift the shell branch into the body:

Replace the entire `TryRestoreSessionAsync` method with:

```csharp
    /// <summary>
    /// Restores a soft-closed session: clears <see cref="Session.ClosedAt"/> in the store and
    /// spawns a tab. Resumable agents (Claude/Codex) get a <c>--resume &lt;id&gt;</c> descriptor;
    /// shells respawn pwsh in the original cwd. Returns <c>false</c> only when the store
    /// rejects the restore.
    /// </summary>
    private async Task<bool> TryRestoreSessionAsync(Project project, Worktree worktree, Session closed)
    {
        var restored = await _store.RestoreSessionAsync(closed.Id).ConfigureAwait(true);
        if (restored.IsFailure)
        {
            _logger.LogWarning("RestoreSession: {Error}", restored.Error);
            return false;
        }

        var useShell = string.IsNullOrEmpty(closed.AgentId)
            || string.Equals(closed.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase);
        var agent = useShell ? null : _agents.GetById(closed.AgentId!);

        SessionDescriptor descriptor;
        if (agent is null)
        {
            descriptor = _sessionManager.CreateShellSession(worktree.Path, closed.Id);
        }
        else
        {
            descriptor = _sessionManager.CreateAgentSession(
                worktree.Path, agent, closed.Id, resume: true, agentSessionId: closed.AgentSessionId);
        }
        if (worktree.Branch is { Length: > 0 } branch)
        {
            descriptor = descriptor with { Title = $"{project.Name} · {branch}" };
        }

        var vm = new SessionTabViewModel(descriptor, project.Id, closed.AgentId, closed.DisplayName, agent?.Icon);
        FocusedGroup.Tabs.Add(vm);
        SelectedTab = vm;

        if (string.Equals(closed.AgentId, "claude", StringComparison.OrdinalIgnoreCase)
            && closed.AgentSessionId is { Length: > 0 } sid)
        {
            _telemetry?.Register(sid, worktree.Path);
        }
        BeginClaudeAdoption(descriptor.Id, closed.AgentId, worktree.Path, DateTimeOffset.UtcNow);
        OnPropertyChanged(nameof(CanCloseGroup));
        CloseGroupCommand.NotifyCanExecuteChanged();
        return true;
    }
```

- [ ] **Step 2: Add the public `ReopenClosedSessionAsync` command**

Add right after `TryRestoreSessionAsync`:

```csharp
    /// <summary>
    /// Public entry-point for reopening a closed session from the sidebar history surface.
    /// Looks up the parent project + worktree by id, then delegates to
    /// <see cref="TryRestoreSessionAsync"/>. No-op when the session is no longer in the
    /// store (e.g. removed from history between right-click and click).
    /// </summary>
    [RelayCommand]
    private async Task ReopenClosedSessionAsync(string? sessionId)
    {
        if (string.IsNullOrEmpty(sessionId)) { return; }
        var hit = _store.Projects
            .SelectMany(p => p.Sessions.Select(s => (project: p, session: s)))
            .FirstOrDefault(x => x.session.Id == sessionId);
        if (hit.session is null || hit.session.ClosedAt is null) { return; }
        var worktree = hit.project.Worktrees.FirstOrDefault(w => w.Id == hit.session.WorktreeId)
                       ?? hit.project.Worktrees.FirstOrDefault(w => w.IsPrimary)
                       ?? hit.project.Worktrees.FirstOrDefault();
        if (worktree is null) { return; }
        await TryRestoreSessionAsync(hit.project, worktree, hit.session).ConfigureAwait(true);
    }
```

- [ ] **Step 3: Build + tests**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
```

Expected: 148/148 pass.

- [ ] **Step 4: Commit**

```bash
git add src/CodeScope.Ui/ViewModels/MainViewModel.cs
git commit -m "$(cat <<'EOF'
feat(history): ReopenClosedSessionAsync command + shell-aware restore

TryRestoreSessionAsync now handles shells too (respawn pwsh in the
original cwd) instead of short-circuiting. ReopenClosedSessionAsync
is the new public command bound to history-row reopen — looks up
project/worktree by session id and delegates. Auto-resume callers
are gone (Task 3); this is the only remaining caller of the restore
path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Sidebar interactions — double-click + history-row context menu

**Files:**
- Modify: `src/CodeScope.Ui/Views/SidebarView.xaml.cs`

- [ ] **Step 1: Extend `OnTreeDoubleClick` to handle history rows**

Read `src/CodeScope.Ui/Views/SidebarView.xaml.cs:101–113`. The current handler only acts on `WorktreeViewModel`. Extend it:

Replace the body with:

```csharp
    private void OnTreeDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is not DependencyObject origin) { return; }
        var item = FindAncestor<TreeViewItem>(origin);
        if (item is null) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }

        switch (item.DataContext)
        {
            case WorktreeViewModel wt:
                if (wt.Sessions.FirstOrDefault() is { } first
                    && main.AllTabs.FirstOrDefault(t => t.Descriptor.Id == first.Descriptor.Id) is { } tab)
                {
                    main.SelectedTab = tab;
                }
                e.Handled = true;
                return;
            case SessionTabViewModel session when session.ClosedAt is not null:
                if (main.ReopenClosedSessionCommand.CanExecute(session.Descriptor.Id))
                {
                    main.ReopenClosedSessionCommand.Execute(session.Descriptor.Id);
                }
                e.Handled = true;
                return;
        }
    }
```

- [ ] **Step 2: Route history rows through `OnTreeSelectionChanged` to a new menu builder**

In `OnTreeSelectionChanged` (lines 115–153), the `case SessionTabViewModel session:` branch on line 149 currently builds the live-session menu. We want history rows (i.e. `ClosedAt is not null`) to get a different menu.

Replace the switch at line 141–152 with:

```csharp
        switch (e.NewValue)
        {
            case ProjectViewModel proj:
                BuildProjectMenu(vm, proj);
                break;
            case WorktreeViewModel wt:
                BuildWorktreeMenu(vm, wt);
                break;
            case SessionTabViewModel session when session.ClosedAt is not null:
                BuildHistorySessionMenu(vm, session);
                break;
            case SessionTabViewModel session:
                BuildSessionMenu(vm, session);
                break;
        }
```

- [ ] **Step 3: Add `BuildHistorySessionMenu`**

Add right after `BuildSessionMenu` (after line 447):

```csharp
    private void BuildHistorySessionMenu(SidebarViewModel vm, SessionTabViewModel session)
    {
        TreeContextMenu.Items.Add(BuildContextHeader(
            "Accent.Primary", session.DisplayName ?? "(closed session)", session.AgentId ?? "shell"));

        TreeContextMenu.Items.Add(BuildGroupLabel("History"));

        var reopen = BuildItem(
            "Reopen", "Ctx.Icon.Terminal", "↵",
            () => WithMainVm(main => main.ReopenClosedSessionCommand.Execute(session.Descriptor.Id)));
        reopen.Tag = "primary";
        TreeContextMenu.Items.Add(reopen);

        TreeContextMenu.Items.Add(BuildItem(
            "Rename…", "Ctx.Icon.Pencil", "F2",
            () => vm.RenameSessionCommand.Execute(session)));

        TreeContextMenu.Items.Add(BuildSeparator());
        var remove = BuildItem(
            "Remove from history", "Ctx.Icon.Trash", "Del",
            () => vm.RemoveSessionFromHistoryCommand.Execute(session));
        remove.Tag = "danger";
        TreeContextMenu.Items.Add(remove);
    }
```

- [ ] **Step 4: Add `RenameSessionCommand` + `RemoveSessionFromHistoryCommand` to `SidebarViewModel`**

Open `src/CodeScope.Ui/ViewModels/SidebarViewModel.Commands.cs`. Find a similar `[RelayCommand]` (e.g. `RenameWorktree`) and add alongside:

```csharp
    [RelayCommand]
    private async Task RenameSessionAsync(SessionTabViewModel? row)
    {
        if (row is null) { return; }
        var dialog = new RenameDialog
        {
            Title = "Rename session",
            Owner = Application.Current?.MainWindow,
            InitialName = row.DisplayName ?? string.Empty,
        };
        if (dialog.ShowDialog() != true) { return; }
        var newName = string.IsNullOrWhiteSpace(dialog.NewName) ? null : dialog.NewName.Trim();
        var result = await _store.RenameSessionAsync(row.Descriptor.Id, newName).ConfigureAwait(true);
        if (result.IsFailure) { Toast("Rename failed", result.Error, ToastSeverity.Err); }
    }

    [RelayCommand]
    private async Task RemoveSessionFromHistoryAsync(SessionTabViewModel? row)
    {
        if (row is null) { return; }
        var result = await _store.RemoveSessionAsync(row.Descriptor.Id).ConfigureAwait(true);
        if (result.IsFailure) { Toast("Remove failed", result.Error, ToastSeverity.Err); }
    }
```

(If `RenameDialog` already takes a different parameter shape in this codebase, mirror the call site used by `RenameWorktreeAsync`. Open `SidebarViewModel.Commands.cs`, search for `RenameWorktreeAsync`, and adapt the dialog construction to match.)

- [ ] **Step 5: Build, run, and verify**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
$env:CODESCOPE_DEV = "1"
dotnet run --project src/CodeScope.App
```

Smoke checks:
- Double-click on a history row reopens the session in the focused group; for Claude that means `claude --resume <id>` and the transcript continues.
- Right-click on a history row shows: header / **Reopen** (default) / Rename… / Remove from history (danger).
- Right-click on a *live* session row still shows the existing live-session menu unchanged.
- Rename a closed session and verify the new name persists across an app restart.
- Remove from history and verify it disappears (cannot be re-reached) and that `projects.json` no longer carries that id.

- [ ] **Step 6: Commit**

```bash
git add src/CodeScope.Ui/Views/SidebarView.xaml.cs src/CodeScope.Ui/ViewModels/SidebarViewModel.Commands.cs
git commit -m "$(cat <<'EOF'
feat(history): double-click + right-click reopen + manage closed sessions

Sidebar double-click on a history row triggers the explicit Reopen
command. Right-click yields a dedicated history-row menu (Reopen as
the primary action, Rename…, Remove from history as a danger item).
Live-session menu is untouched.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Worktree context menu — "Reopen most recent closed session"

**Files:**
- Modify: `src/CodeScope.Ui/Views/SidebarView.xaml.cs`

- [ ] **Step 1: Insert the reopen item in `BuildWorktreeMenu`**

In `BuildWorktreeMenu` (starts at line 157), find the `// ── SESSION ──` group label (line 170). Right after the existing session items (after the `if (!wt.HasActiveSession) { … } else { … }` block ends at line 262), insert:

```csharp
        if (wt.HasHistory)
        {
            var reopenLast = BuildItem(
                "Reopen most recent closed session", "Ctx.Icon.Restart", null,
                () => WithMainVm(main =>
                {
                    if (wt.History.FirstOrDefault() is { } first)
                    {
                        main.ReopenClosedSessionCommand.Execute(first.Descriptor.Id);
                    }
                }));
            TreeContextMenu.Items.Add(reopenLast);
        }
```

`History` is sorted most-recent-first by the projection in Task 5, so `FirstOrDefault()` is the right pick.

- [ ] **Step 2: Build, run, and verify**

```pwsh
dotnet build src/CodeScope.App/CodeScope.App.csproj -c Debug
dotnet test  CodeScope.sln -c Debug
$env:CODESCOPE_DEV = "1"
dotnet run --project src/CodeScope.App
```

Smoke:
- Right-click on a worktree with empty history → the new item is absent.
- Right-click on a worktree with history → "Reopen most recent closed session" is present and reopens the top entry.
- Drag-between-groups for an open Claude tab still works (regression check — should be untouched, but the spec calls for explicit verification).

- [ ] **Step 3: Commit**

```bash
git add src/CodeScope.Ui/Views/SidebarView.xaml.cs
git commit -m "$(cat <<'EOF'
feat(history): worktree menu shortcut for most-recent reopen

Right-click on a worktree with history shows a "Reopen most recent
closed session" item — the keyboard-equivalent ergonomics covered
by a single click. Hidden when history is empty.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Update `docs/HANDOFF.md`

**Files:**
- Modify: `docs/HANDOFF.md`

- [ ] **Step 1: Bump the cursor**

Read `docs/HANDOFF.md`. Add a new session entry above the *Session 20* block, in the same style. Cover:

- Branch name, head SHA after Task 9 commit (use `git rev-parse --short HEAD`)
- The seven commits of this session, with one-liners each
- The DECISIONS.md entry
- Move the "Reopen-most-recent shortcut decision" out of any deferred-list, since it's no longer needed
- New rough-edge entry **only if** something visible deferred (e.g. if the per-row badge was dropped in Task 6 Step 4)

- [ ] **Step 2: Update build/test counters**

Bump the test count to 148 (147 pre-existing + 1 from Task 1). Build status stays green.

- [ ] **Step 3: Final build + test run**

```pwsh
dotnet build CodeScope.sln -c Debug
dotnet test  CodeScope.sln -c Debug
```

Expected: build clean, 148/148 pass.

- [ ] **Step 4: Commit**

```bash
git add docs/HANDOFF.md
git commit -m "$(cat <<'EOF'
docs(handoff): session N — per-worktree session history surface

Documents the soft-close gate widening, auto-resume removal,
WorktreeViewModel.History wiring, sidebar disclosure + dim styling,
ReopenClosedSessionAsync, history-row context menu, and the
worktree-level "Reopen most recent" shortcut.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review notes

- **Spec coverage.** Tasks 4–6 cover the sidebar surface (architecture + visual), Tasks 2–3+7 cover the option-b auto-resume removal, Tasks 7–8 cover shell-session inclusion, Task 8 covers all interactions (double-click / context menu / Rename / Remove from history), Task 9 covers the worktree-level "reopen most recent", Task 1 covers the cascade test, Task 3 + Task 10 cover the documentation requirement, and the unspoken drag-between-groups check sits in Task 9 Step 2.
- **No retention cap and no keyboard shortcut.** Both are explicit non-goals in the spec; no task implements them.
- **Type consistency.** `History` is the property name everywhere; `HasHistory` / `HistoryCount` / `HistoryHeader` are derived; `IsHistoryExpanded` is the CommunityToolkit observable property; `ReopenClosedSessionCommand` is the generated command name from `[RelayCommand] ReopenClosedSessionAsync`. `SessionTabViewModel.ClosedAt` is the new bridge between live and dead rows.
- **Drag-between-groups.** Untouched by every task. Verified explicitly in Task 9 Step 2 smoke list.
