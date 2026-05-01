using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.ComponentModel;
using System.Windows;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// The Overview grid's backing VM. Mirrors <see cref="SidebarViewModel"/> + the running Tabs
/// into one card per session, filterable by state. This is the VM behind the screen shown when
/// the user hits <c>Ctrl+Shift+O</c> or clicks the sidebar's <i>Overview</i> button.
/// </summary>
public sealed partial class OverviewViewModel : ObservableObject
{
    private readonly SidebarViewModel _sidebar;
    private readonly ObservableCollection<EditorGroupViewModel> _groups;
    private readonly IAgentRegistry? _agents;

    /// <summary>
    /// Tracks whether the Overview screen is currently visible. While hidden every status
    /// tick (worktree poller every 3s, PR poller every 30s) and every Tabs/Projects mutation
    /// would otherwise rebuild <see cref="Cards"/> + <see cref="FilteredCards"/> from
    /// scratch — meaningful continuous GC churn behind a screen the user isn't even looking
    /// at. We instead set <see cref="_dirty"/> when an event fires while hidden, and flush
    /// a single rebuild on the hidden→visible transition. Subscriptions stay armed so the
    /// dirty flag remains honest across the gap. Issue #30.
    /// </summary>
    // Default: hidden. Most sessions start with the workspace on stage; MainViewModel flips
    // IsActive on when the user toggles IsOverviewVisible. The constructor's unconditional
    // initial Rebuild() seeds Cards once; gated rebuilds take over from there.
    private bool _isActive;
    private bool _dirty;

    public OverviewViewModel(
        SidebarViewModel sidebar,
        ObservableCollection<EditorGroupViewModel> groups,
        IAgentRegistry? agents = null)
    {
        _sidebar = sidebar;
        _groups = groups;
        _agents = agents;

        Cards = [];
        FilteredCards = [];

        _groups.CollectionChanged += OnGroupsChanged;
        foreach (var g in _groups) { HookGroup(g); }

        _sidebar.Projects.CollectionChanged += OnProjectsChanged;
        foreach (var p in _sidebar.Projects) { HookProject(p); }

        Rebuild();
    }

    /// <summary>
    /// Set to <c>true</c> when the Overview is visible, <c>false</c> when hidden. Drives
    /// the gate that suppresses rebuilds while the screen isn't on stage. The host
    /// (<see cref="MainViewModel"/>) flips this in lockstep with <c>IsOverviewVisible</c>.
    /// </summary>
    public bool IsActive
    {
        get => _isActive;
        set
        {
            if (_isActive == value) { return; }
            _isActive = value;
            if (value && _dirty)
            {
                _dirty = false;
                RebuildOnUi();
            }
        }
    }

    private IEnumerable<SessionTabViewModel> AllTabs => _groups.SelectMany(g => g.Tabs);

    private void OnGroupsChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (e.OldItems is not null)
        {
            foreach (var g in e.OldItems.OfType<EditorGroupViewModel>()) { UnhookGroup(g); }
        }
        if (e.NewItems is not null)
        {
            foreach (var g in e.NewItems.OfType<EditorGroupViewModel>()) { HookGroup(g); }
        }
        RebuildOnUi();
    }

    private void HookGroup(EditorGroupViewModel g) => g.Tabs.CollectionChanged += OnTabsChanged;
    private void UnhookGroup(EditorGroupViewModel g) => g.Tabs.CollectionChanged -= OnTabsChanged;
    private void OnTabsChanged(object? sender, NotifyCollectionChangedEventArgs e) => RebuildOnUi();

    public ObservableCollection<OverviewCardViewModel> Cards { get; }

    public ObservableCollection<OverviewCardViewModel> FilteredCards { get; }

    [ObservableProperty]
    private OverviewFilter _filter = OverviewFilter.All;

    partial void OnFilterChanged(OverviewFilter value) => ApplyFilter();

    public int CountAll => Cards.Count;
    public int CountActive => Cards.Count(c => c.State == OverviewCardState.Active);
    public int CountIdle => Cards.Count(c => c.State == OverviewCardState.Idle);
    public int CountWaiting => Cards.Count(c => c.State == OverviewCardState.Waiting);

    public int ProjectCount => _sidebar.Projects.Count(p => p.Worktrees.Any(HasAnyVisibleSession));

    /// <summary>"6 sessions across 3 projects" — drives the Overview header subtitle.</summary>
    public string SubtitleBody
    {
        get
        {
            var sessions = Cards.Count;
            var projects = ProjectCount;
            if (sessions == 0) { return "no active sessions yet"; }
            var sessionPart = sessions == 1 ? "1 session" : $"{sessions} sessions";
            var projectPart = projects == 1 ? "1 project" : $"{projects} projects";
            return $"{sessionPart} across {projectPart}";
        }
    }

    public bool IsEmpty => Cards.Count == 0;

    public bool FilterIsAll => Filter == OverviewFilter.All;
    public bool FilterIsActive => Filter == OverviewFilter.Active;
    public bool FilterIsIdle => Filter == OverviewFilter.Idle;
    public bool FilterIsWaiting => Filter == OverviewFilter.Waiting;

    /// <summary>Raised when the user clicks a card. The host swaps to the workspace with this session focused.</summary>
    public event EventHandler<SessionTabViewModel>? FocusSessionRequested;

    /// <summary>Raised when the user hits the back-link. Host flips <c>IsOverviewVisible</c> off.</summary>
    public event EventHandler? BackRequested;

    [RelayCommand]
    private void SetFilter(string? name)
    {
        if (!Enum.TryParse<OverviewFilter>(name, ignoreCase: true, out var value)) { return; }
        Filter = value;
    }

    [RelayCommand]
    private void Back() => BackRequested?.Invoke(this, EventArgs.Empty);

    [RelayCommand]
    private void OpenCard(OverviewCardViewModel? card)
    {
        if (card is null) { return; }
        FocusSessionRequested?.Invoke(this, card.Session);
    }

    /// <summary>
    /// Marshal the rebuild to the UI thread — store events fire from background pollers and we
    /// touch ObservableCollections here. Gated on <see cref="IsActive"/> so hidden Overview
    /// only sets the dirty flag and defers the rebuild to the next show.
    /// </summary>
    private void RebuildOnUi()
    {
        if (!_isActive)
        {
            _dirty = true;
            return;
        }
        if (Application.Current?.Dispatcher is { } d && !d.CheckAccess())
        {
            d.BeginInvoke(Rebuild);
        }
        else
        {
            Rebuild();
        }
    }

    private void Rebuild()
    {
        foreach (var existing in Cards)
        {
            existing.OpenRequested -= OnCardOpenRequested;
        }
        Cards.Clear();

        foreach (var project in _sidebar.Projects)
        {
            foreach (var wt in project.Worktrees)
            {
                var sessions = AllTabs.Where(t =>
                        string.Equals(t.ProjectId, project.Id, StringComparison.OrdinalIgnoreCase)
                        && string.Equals(t.Descriptor.WorkingDirectory, wt.Path, StringComparison.OrdinalIgnoreCase))
                    .ToList();
                if (sessions.Count == 0) { continue; }

                foreach (var session in sessions)
                {
                    var state = DeriveState(wt, session);
                    var agentDisplay = ResolveAgentDisplay(session);
                    var card = new OverviewCardViewModel(
                        project.Name, wt, session, state, sessions.Count, agentDisplay);
                    card.OpenRequested += OnCardOpenRequested;
                    Cards.Add(card);
                }
            }
        }

        OnPropertyChanged(nameof(CountAll));
        OnPropertyChanged(nameof(CountActive));
        OnPropertyChanged(nameof(CountIdle));
        OnPropertyChanged(nameof(CountWaiting));
        OnPropertyChanged(nameof(ProjectCount));
        OnPropertyChanged(nameof(SubtitleBody));
        OnPropertyChanged(nameof(IsEmpty));

        ApplyFilter();
    }

    private void ApplyFilter()
    {
        FilteredCards.Clear();
        var target = Filter;
        foreach (var card in Cards)
        {
            var pass = target switch
            {
                OverviewFilter.All => true,
                OverviewFilter.Active => card.State == OverviewCardState.Active,
                OverviewFilter.Idle => card.State == OverviewCardState.Idle,
                OverviewFilter.Waiting => card.State == OverviewCardState.Waiting,
                _ => true,
            };
            if (pass) { FilteredCards.Add(card); }
        }

        OnPropertyChanged(nameof(FilterIsAll));
        OnPropertyChanged(nameof(FilterIsActive));
        OnPropertyChanged(nameof(FilterIsIdle));
        OnPropertyChanged(nameof(FilterIsWaiting));
    }

    private void OnCardOpenRequested(object? sender, SessionTabViewModel session)
        => FocusSessionRequested?.Invoke(this, session);

    private void OnProjectsChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (e.OldItems is not null)
        {
            foreach (var item in e.OldItems.OfType<ProjectViewModel>()) { UnhookProject(item); }
        }
        if (e.NewItems is not null)
        {
            foreach (var item in e.NewItems.OfType<ProjectViewModel>()) { HookProject(item); }
        }
        RebuildOnUi();
    }

    private void HookProject(ProjectViewModel p)
    {
        p.Worktrees.CollectionChanged += OnWorktreesChanged;
        foreach (var w in p.Worktrees) { HookWorktree(w); }
    }

    private void UnhookProject(ProjectViewModel p)
    {
        p.Worktrees.CollectionChanged -= OnWorktreesChanged;
        foreach (var w in p.Worktrees) { UnhookWorktree(w); }
    }

    private void OnWorktreesChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (e.OldItems is not null)
        {
            foreach (var w in e.OldItems.OfType<WorktreeViewModel>()) { UnhookWorktree(w); }
        }
        if (e.NewItems is not null)
        {
            foreach (var w in e.NewItems.OfType<WorktreeViewModel>()) { HookWorktree(w); }
        }
        RebuildOnUi();
    }

    private void HookWorktree(WorktreeViewModel w) => w.PropertyChanged += OnWorktreePropertyChanged;
    private void UnhookWorktree(WorktreeViewModel w) => w.PropertyChanged -= OnWorktreePropertyChanged;

    private void OnWorktreePropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        // Status / PR changes mutate the card preview + state; rebuild on the UI thread so
        // the dot color and "changes: N" chip stay honest.
        switch (e.PropertyName)
        {
            case nameof(WorktreeViewModel.IsDirty):
            case nameof(WorktreeViewModel.Ahead):
            case nameof(WorktreeViewModel.Behind):
            case nameof(WorktreeViewModel.PullRequest):
            case nameof(WorktreeViewModel.DisplayBranch):
                RebuildOnUi();
                break;
        }
    }

    private static bool HasAnyVisibleSession(WorktreeViewModel w) => w.Sessions.Count > 0;

    private static OverviewCardState DeriveState(WorktreeViewModel wt, SessionTabViewModel session)
    {
        // Heuristic until we track agent stdin/stdout state:
        //   - dirty working tree implies the agent is currently editing → Active
        //   - PR with failing CI implies the agent is waiting on the user to review → Waiting
        //   - otherwise Idle
        if (wt.PullRequest is { CiStatus: Core.Models.CiStatus.Failure }) { return OverviewCardState.Waiting; }
        if (wt.IsDirty) { return OverviewCardState.Active; }
        return OverviewCardState.Idle;
    }

    private string ResolveAgentDisplay(SessionTabViewModel session)
    {
        if (_agents is null || string.IsNullOrEmpty(session.AgentId)) { return "shell"; }
        if (string.Equals(session.AgentId, MainViewModel.ShellSentinel, StringComparison.OrdinalIgnoreCase))
        {
            return "shell";
        }
        var agent = _agents.GetById(session.AgentId);
        return agent?.DisplayName ?? session.AgentId;
    }
}
