using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>A Project node in the sidebar tree. Phase 3 nests worktrees as direct children.</summary>
public sealed partial class ProjectViewModel : ObservableObject
{
    public ProjectViewModel(Project project)
    {
        Project = project;
        _isExpanded = true;
        Worktrees = [];
        // Propagate wait-state upward: a collapsed project row needs to show the red
        // dot when any child worktree flips to "wait" (§9 "attention propagates"). Hook
        // child DotState changes — covers PR-CI failure, dirty-with-nothing-else, and
        // now live session Wait too.
        Worktrees.CollectionChanged += (_, e) =>
        {
            if (e.NewItems is not null)
            {
                foreach (var w in e.NewItems.OfType<WorktreeViewModel>())
                {
                    w.PropertyChanged += OnChildWorktreePropertyChanged;
                }
            }
            OnPropertyChanged(nameof(HasWaitingChild));
            OnPropertyChanged(nameof(CountBadge));
            OnPropertyChanged(nameof(HasNoWorktrees));
        };
    }

    private void OnChildWorktreePropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(WorktreeViewModel.DotState))
        {
            OnPropertyChanged(nameof(HasWaitingChild));
        }
    }

    public Project Project { get; private set; }

    public string Id => Project.Id;

    public string Name => Project.Name;

    public string Path => Project.Path;

    /// <summary>
    /// Stable, human-readable id for UIA automation. Composed off <see cref="Name"/>
    /// rather than the project guid so wpf-cli snapshots show e.g. <c>a:Project_codescope</c>
    /// instead of an opaque hash. Whitespace/punctuation collapse to underscores so
    /// the value is safe to use as a ref token.
    /// </summary>
    public string AutomationId => $"Project_{SafeToken(Name)}";

    private static string SafeToken(string? s)
    {
        if (string.IsNullOrWhiteSpace(s)) { return "unknown"; }
        var token = new string([.. s.Select(c => char.IsLetterOrDigit(c) ? c : '_')]).Trim('_');
        return string.IsNullOrEmpty(token) ? "unknown" : token;
    }

    public string? DefaultAgentId => Project.DefaultAgentId;

    public string DefaultBranch => string.IsNullOrWhiteSpace(Project.DefaultBranch) ? "main" : Project.DefaultBranch;

    public string? WorktreeRoot => Project.WorktreeRoot;

    public ObservableCollection<WorktreeViewModel> Worktrees { get; }

    /// <summary>
    /// Compact subtitle rendered next to the project name in the sidebar:
    /// e.g. "3 · 1 dirty · 1 PR". Empty when the project has no worktrees or no signal.
    /// Callers ping <see cref="NotifySummaryChanged"/> whenever a child state changes.
    /// </summary>
    public string Summary
    {
        get
        {
            if (Worktrees.Count == 0) { return string.Empty; }
            var dirty = Worktrees.Count(w => w.IsDirty);
            var prs = Worktrees.Count(w => w.HasPullRequest);
            var parts = new List<string> { $"{Worktrees.Count}" };
            if (dirty > 0) { parts.Add($"{dirty} dirty"); }
            if (prs > 0) { parts.Add($"{prs} PR{(prs == 1 ? "" : "s")}"); }
            return string.Join(" · ", parts);
        }
    }

    /// <summary>
    /// Accent-coloured count rendered on the right of the project row in the sidebar.
    /// The design shows just the worktree count — no dirty/PR suffix.
    /// </summary>
    public string CountBadge => Worktrees.Count > 0 ? Worktrees.Count.ToString() : string.Empty;

    /// <summary>
    /// True when any child worktree is in <c>wait</c> state. Surfaces as a small red dot
    /// next to the count on the project row (spec §9 "attention propagates"), so a
    /// collapsed project still tells you it needs attention.
    /// </summary>
    public bool HasWaitingChild => Worktrees.Any(w => w.DotState == "wait");

    /// <summary>
    /// True when this project has zero worktrees — drives the "(no worktrees)" placeholder row
    /// rendered beneath the project header when the tree item is expanded, so an empty project
    /// doesn't look broken (spec polish item from session 15).
    /// </summary>
    public bool HasNoWorktrees => Worktrees.Count == 0;

    public void NotifySummaryChanged()
    {
        OnPropertyChanged(nameof(Summary));
        OnPropertyChanged(nameof(CountBadge));
        OnPropertyChanged(nameof(HasWaitingChild));
    }

    [ObservableProperty]
    private bool _isExpanded;

    [ObservableProperty]
    private bool _isVisible = true;

    /// <summary>Cascades the filter to children; keeps this project visible if any child matches.</summary>
    public void ApplyFilter(string filter)
    {
        foreach (var w in Worktrees) { w.ApplyFilter(filter); }

        if (string.IsNullOrEmpty(filter))
        {
            IsVisible = true;
            return;
        }

        var ownHit = Name.Contains(filter, StringComparison.OrdinalIgnoreCase)
                     || (Path?.Contains(filter, StringComparison.OrdinalIgnoreCase) ?? false);
        var anyChildVisible = Worktrees.Any(w => w.IsVisible);
        IsVisible = ownHit || anyChildVisible;
    }

    public void Replace(Project project)
    {
        Project = project;
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(Path));
        OnPropertyChanged(nameof(DefaultAgentId));
    }
}
