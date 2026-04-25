using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace NoScope.CodeScope.Ui.ViewModels;

public partial class MainViewModel
{
    /// <summary>Editor groups; every group owns a tab strip and a workspace region.</summary>
    public ObservableCollection<EditorGroupViewModel> Groups { get; }

    [ObservableProperty]
    private EditorGroupViewModel _focusedGroup;

    partial void OnFocusedGroupChanged(EditorGroupViewModel? oldValue, EditorGroupViewModel newValue)
    {
        if (oldValue is not null) { oldValue.IsFocused = false; }
        newValue.IsFocused = true;
        // Pull the newly-focused group's selection up to the window-global SelectedTab
        // so the title bar + diff panel react to the group switch.
        SelectedTab = newValue.SelectedTab;
        OnPropertyChanged(nameof(CanCloseGroup));
        CloseGroupCommand.NotifyCanExecuteChanged();
    }

    /// <summary>Flattened view of tabs across every group — used by sidebar lookups and Overview.</summary>
    public IEnumerable<SessionTabViewModel> AllTabs => Groups.SelectMany(g => g.Tabs);

    // Populated by LayoutStore before InitializeAsync runs. HydrateFromLoaded consults this
    // to route each restored session to its saved group, so no cross-group re-parent /
    // terminal respawn happens on cold start.
    private IReadOnlyDictionary<string, int>? _pendingSessionToGroup;
    private int _pendingFocusedIndex;

    /// <summary>
    /// Star-weight per group, parallel to <see cref="Groups"/>. Drives the workspace
    /// ColumnDefinitions in MainWindow; persisted across restarts via LayoutStore.
    /// The view captures drag ratios back into this list on splitter DragCompleted.
    /// </summary>
    public List<double> GroupWidths { get; } = [1.0];

    /// <summary>
    /// Raised after <see cref="GroupWidths"/> changes in bulk (hydrate / group add / remove)
    /// so the view can rebuild ColumnDefinitions. Splitter-drag updates write into the list
    /// directly and do NOT raise this — the view already owns the authoritative column widths
    /// at that point.
    /// </summary>
    public event EventHandler? GroupWidthsReset;

    private void RaiseGroupWidthsReset() => GroupWidthsReset?.Invoke(this, EventArgs.Empty);

    /// <summary>
    /// Keeps <see cref="GroupWidths"/> parallel to <see cref="Groups"/> whenever the
    /// collection mutates (split, close-group, auto-collapse, hydrate, drag-between).
    /// Centralising here means individual mutation sites don't need to remember to
    /// touch the widths list — they just drive <see cref="Groups"/> and the shape follows.
    /// </summary>
    private void OnGroupsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        switch (e.Action)
        {
            case System.Collections.Specialized.NotifyCollectionChangedAction.Add:
                {
                    var start = e.NewStartingIndex;
                    if (start < 0) { start = GroupWidths.Count; }
                    for (var i = 0; i < (e.NewItems?.Count ?? 0); i++)
                    {
                        GroupWidths.Insert(Math.Min(start + i, GroupWidths.Count), 1.0);
                    }
                    break;
                }
            case System.Collections.Specialized.NotifyCollectionChangedAction.Remove:
                {
                    var idx = e.OldStartingIndex;
                    var count = e.OldItems?.Count ?? 0;
                    for (var i = 0; i < count; i++)
                    {
                        if (idx >= 0 && idx < GroupWidths.Count) { GroupWidths.RemoveAt(idx); }
                    }
                    break;
                }
            case System.Collections.Specialized.NotifyCollectionChangedAction.Move:
                {
                    var from = e.OldStartingIndex;
                    var to = e.NewStartingIndex;
                    if (from >= 0 && to >= 0 && from < GroupWidths.Count && to < GroupWidths.Count)
                    {
                        var v = GroupWidths[from];
                        GroupWidths.RemoveAt(from);
                        GroupWidths.Insert(to, v);
                    }
                    break;
                }
            case System.Collections.Specialized.NotifyCollectionChangedAction.Reset:
                GroupWidths.Clear();
                for (var i = 0; i < Groups.Count; i++) { GroupWidths.Add(1.0); }
                break;
        }
        RaiseGroupWidthsReset();
    }

    /// <summary>
    /// Forwards a group's SelectedTab changes into MainViewModel.SelectedTab when the
    /// group is focused — keeps WindowTitle, per-tab IsActive bookkeeping, and the
    /// diff panel in sync regardless of which group the user clicked.
    /// </summary>
    private void HookGroupSelection(EditorGroupViewModel group)
    {
        group.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName != nameof(EditorGroupViewModel.SelectedTab)) { return; }
            if (!ReferenceEquals(group, FocusedGroup)) { return; }
            if (ReferenceEquals(SelectedTab, group.SelectedTab)) { return; }
            SelectedTab = group.SelectedTab;
        };
    }

    /// <summary>
    /// Pre-allocates enough groups to hold a persisted layout and stashes the session→group
    /// mapping used during the next hydration. Call on window load, before
    /// <see cref="InitializeAsync"/>. Does nothing if <paramref name="groupCount"/> is invalid.
    /// </summary>
    public void PrepareLayoutFromPersistence(
        int groupCount,
        int focusedIndex,
        IReadOnlyDictionary<string, int> sessionToGroup,
        double[]? groupWidths = null)
    {
        if (groupCount < 1) { return; }
        while (Groups.Count < groupCount)
        {
            var g = new EditorGroupViewModel();
            HookGroupSelection(g);
            Groups.Add(g);
        }
        _pendingSessionToGroup = sessionToGroup;
        _pendingFocusedIndex = Math.Clamp(focusedIndex, 0, Groups.Count - 1);

        GroupWidths.Clear();
        for (var i = 0; i < Groups.Count; i++)
        {
            var w = groupWidths is not null && i < groupWidths.Length && groupWidths[i] > 0
                ? groupWidths[i]
                : 1.0;
            GroupWidths.Add(w);
        }
        RaiseGroupWidthsReset();
    }

    /// <summary>
    /// Snapshots the current group layout — how many groups exist, which one has focus,
    /// which group each session belongs to, and the per-group star widths.
    /// Used by <c>LayoutStore.Save</c> at shutdown.
    /// </summary>
    public (int GroupCount, int FocusedIndex, Dictionary<string, int> SessionToGroup, double[] GroupWidths) CaptureLayout()
    {
        var map = new Dictionary<string, int>();
        for (var i = 0; i < Groups.Count; i++)
        {
            foreach (var tab in Groups[i].Tabs)
            {
                map[tab.Descriptor.Id] = i;
            }
        }
        var focused = Math.Max(0, Groups.IndexOf(FocusedGroup));
        var widths = new double[Groups.Count];
        for (var i = 0; i < widths.Length; i++)
        {
            widths[i] = i < GroupWidths.Count && GroupWidths[i] > 0 ? GroupWidths[i] : 1.0;
        }
        return (Groups.Count, focused, map, widths);
    }

    public bool CanCloseGroup => Groups.Count > 1;

    /// <summary>
    /// Adds a fresh empty group to the right of the focused group and shifts focus to it,
    /// so the next <c>NewSession</c> lands there. Bound to <c>Ctrl+\</c>.
    /// </summary>
    [RelayCommand]
    private void SplitRight()
    {
        var insertAt = Math.Max(0, Groups.IndexOf(FocusedGroup) + 1);
        var group = new EditorGroupViewModel();
        HookGroupSelection(group);
        Groups.Insert(insertAt, group);
        FocusedGroup = group;
        OnPropertyChanged(nameof(CanCloseGroup));
        OnPropertyChanged(nameof(AllTabs));
        CloseGroupCommand.NotifyCanExecuteChanged();
    }

    /// <summary>
    /// Closes the focused group. No-op when only one group exists. For v1 the group must
    /// be empty; session evacuation to a neighbour ships in a follow-up so we don't
    /// disturb long-running pwsh processes mid-flight.
    /// </summary>
    [RelayCommand(CanExecute = nameof(CanCloseGroupAndEmpty))]
    private void CloseGroup()
    {
        if (Groups.Count < 2) { return; }
        if (FocusedGroup.Tabs.Count > 0) { return; }
        var idx = Groups.IndexOf(FocusedGroup);
        var doomed = FocusedGroup;
        var neighbour = Groups[idx == 0 ? 1 : idx - 1];
        Groups.Remove(doomed);
        FocusedGroup = neighbour;
        OnPropertyChanged(nameof(CanCloseGroup));
        OnPropertyChanged(nameof(AllTabs));
    }

    private bool CanCloseGroupAndEmpty() => Groups.Count > 1 && FocusedGroup.Tabs.Count == 0;

    /// <summary>Transfers focus to <paramref name="group"/> (typically called from the view on click).</summary>
    [RelayCommand]
    private void FocusGroup(EditorGroupViewModel? group)
    {
        if (group is null || !Groups.Contains(group)) { return; }
        FocusedGroup = group;
    }

    /// <summary>Alt+Right: cycle focus to the next group (wraps around).</summary>
    [RelayCommand]
    private void FocusNextGroup() => CycleFocus(+1);

    /// <summary>Alt+Left: cycle focus to the previous group (wraps around).</summary>
    [RelayCommand]
    private void FocusPrevGroup() => CycleFocus(-1);

    private void CycleFocus(int delta)
    {
        if (Groups.Count < 2) { return; }
        var idx = Groups.IndexOf(FocusedGroup);
        if (idx < 0) { return; }
        var next = (idx + delta + Groups.Count) % Groups.Count;
        FocusedGroup = Groups[next];
    }

    /// <summary>
    /// Splits a fresh empty group to the right of the focused one and spawns a new
    /// session for the sidebar's currently-selected worktree in that group. Wired to
    /// the worktree context menu's <i>Open in new group</i> entry and to Ctrl+Shift+Enter.
    /// </summary>
    [RelayCommand]
    private async Task OpenInNewGroupAsync(WorktreeViewModel? worktree)
    {
        await OpenInNewGroupWithAgentAsync(worktree, null).ConfigureAwait(true);
    }

    /// <summary>
    /// Overload: split right + spawn a session of a specific agent. Invoked from the
    /// sidebar's "Open in new group ▸ &lt;agent&gt;" submenu.
    /// </summary>
    [RelayCommand]
    private async Task OpenInNewGroupWithAgentAsync((WorktreeViewModel? Worktree, string? AgentId) args)
    {
        await OpenInNewGroupWithAgentAsync(args.Worktree, args.AgentId).ConfigureAwait(true);
    }

    private async Task OpenInNewGroupWithAgentAsync(WorktreeViewModel? worktree, string? agentId)
    {
        if (worktree is not null && Sidebar is not null && !ReferenceEquals(Sidebar.SelectedWorktree, worktree))
        {
            Sidebar.SelectedWorktree = worktree;
        }
        SplitRight();
        await NewSessionAsync(agentId).ConfigureAwait(true);
    }
}
