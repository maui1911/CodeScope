using System.Collections.ObjectModel;
using System.Diagnostics;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>A worktree node inside a <see cref="ProjectViewModel"/>. Owns its session rows.</summary>
public sealed partial class WorktreeViewModel : ObservableObject
{
    public WorktreeViewModel(string projectId, Worktree worktree)
    {
        ProjectId = projectId;
        Worktree = worktree;
        _isExpanded = true;
        Sessions = [];
        // Sessions.Count drives HasActiveSession and DotState — raise both when the
        // collection mutates so the accent bar + 6px dot update without a full rebuild.
        // Additionally subscribe to per-session Status changes so the pulse lights up
        // when any child session flips to TabStatus.Wait (e.g. Claude paused on a tool
        // call). No unsubscribe needed: sessions live for the lifetime of their row.
        Sessions.CollectionChanged += (_, e) =>
        {
            if (e.NewItems is not null)
            {
                foreach (var s in e.NewItems.OfType<SessionTabViewModel>())
                {
                    s.PropertyChanged += OnChildSessionPropertyChanged;
                }
            }
            OnPropertyChanged(nameof(HasActiveSession));
            OnPropertyChanged(nameof(DotState));
            OnPropertyChanged(nameof(StatusLabel));
        };
    }

    public string ProjectId { get; }

    public Worktree Worktree { get; private set; }

    public string Id => Worktree.Id;

    public string Path => Worktree.Path;

    public bool IsPrimary => Worktree.IsPrimary;

    public string DisplayBranch => Worktree.Branch ?? (Worktree.IsPrimary ? "main" : "(no branch)");

    /// <summary>
    /// Stable, human-readable id for UIA automation. Combines project id with the
    /// branch name so two worktrees in different projects on the same branch don't
    /// collide. Wpf-cli snapshots will show e.g. <c>a:Worktree_codescope__main</c>.
    /// </summary>
    public string AutomationId
        => $"Worktree_{AutomationIds.SafeToken(ProjectId)}__{AutomationIds.SafeToken(DisplayBranch)}";

    public ObservableCollection<SessionTabViewModel> Sessions { get; }

    [ObservableProperty]
    private bool _isExpanded;

    [ObservableProperty]
    private bool _isDirty;

    [ObservableProperty]
    private int _ahead;

    [ObservableProperty]
    private int _behind;

    [ObservableProperty]
    private int _added;

    [ObservableProperty]
    private int _removed;

    [ObservableProperty]
    private int _changedFiles;

    [ObservableProperty]
    private PullRequestInfo? _pullRequest;

    [ObservableProperty]
    private bool _isVisible = true;

    /// <summary>Called by the sidebar filter: hides the row when <paramref name="filter"/> misses.</summary>
    public void ApplyFilter(string filter)
    {
        if (string.IsNullOrEmpty(filter)) { IsVisible = true; return; }
        var hay = $"{DisplayBranch}\n{Path}\n{Worktree.Branch ?? ""}";
        IsVisible = hay.Contains(filter, StringComparison.OrdinalIgnoreCase);
    }

    public bool HasPullRequest => PullRequest is not null;

    /// <summary>"#42" — blank when no open PR.</summary>
    public string PrBadgeText => PullRequest is { Number: var n and > 0 } ? $"#{n}" : string.Empty;

    /// <summary>Compact CI glyph: ✓ success, ◐ pending, ✗ failure, · none. Empty when no PR.</summary>
    public string CiGlyph => PullRequest?.CiStatus switch
    {
        CiStatus.Success => "✓",
        CiStatus.Pending => "◐",
        CiStatus.Failure => "✗",
        CiStatus.None => "·",
        _ => string.Empty,
    };

    [RelayCommand(CanExecute = nameof(HasPullRequest))]
    private void OpenPullRequest()
    {
        if (PullRequest is not { Url: { Length: > 0 } url }) { return; }
        try
        {
            Process.Start(new ProcessStartInfo(url) { UseShellExecute = true });
        }
        catch (Exception ex)
        {
            // Browser launch failures are non-fatal; the user can always copy the URL from the tooltip.
            System.Diagnostics.Debug.WriteLine($"[WorktreeViewModel] OpenPullRequest: {ex.Message}");
        }
    }

    /// <summary>'●' when clean, '✎' when dirty. Bound by the sidebar template.</summary>
    public string DirtyGlyph => IsDirty ? "✎" : "●";

    /// <summary>Has at least one open session — drives the accent left-bar in the sidebar row.</summary>
    public bool HasActiveSession => Sessions.Count > 0;

    /// <summary>
    /// Session-state classifier for the 6px dot on the sidebar worktree row.
    /// Semantics from the sidebar spec (§7): <c>rest</c> = no session, <c>idle</c> = has session
    /// but nothing demanding attention, <c>wait</c> = needs user (red, pulses). The <c>active</c>
    /// (selected) state is owned by the <see cref="System.Windows.Controls.TreeViewItem"/> itself
    /// and resolved in XAML via IsSelected triggers.
    /// </summary>
    public string DotState
    {
        get
        {
            if (PullRequest?.CiStatus == CiStatus.Failure) { return "wait"; }
            if (HasWaitingSession) { return "wait"; }
            if (HasActiveSession) { return "idle"; }
            return "rest";
        }
    }

    /// <summary>True when any of this worktree's sessions is paused on a tool call / waiting for the user.</summary>
    public bool HasWaitingSession => Sessions.Any(s => s.Status == TabStatus.Wait);

    private void OnChildSessionPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(SessionTabViewModel.Status))
        {
            OnPropertyChanged(nameof(HasWaitingSession));
            OnPropertyChanged(nameof(DotState));
            OnPropertyChanged(nameof(StatusLabel));
        }
    }

    /// <summary>
    /// Short right-aligned status slug used by the sidebar worktree row.
    ///   "wait" — PR open with failing CI (user review needed)
    ///   "chg"  — working tree is dirty
    ///   "↑N"/"↓N"/"↑N ↓N" — clean but out of sync with upstream
    ///   "idle" — clean + in sync
    /// Matches the "idle"/"wait"/"3 chg" labels in the Overview reference mock.
    /// </summary>
    public string StatusLabel
    {
        get
        {
            if (HasWaitingSession) { return "wait"; }
            if (PullRequest?.CiStatus == CiStatus.Failure) { return "wait"; }
            if (IsDirty) { return "chg"; }
            if (Ahead > 0 || Behind > 0) { return AheadBehindText; }
            return "idle";
        }
    }

    /// <summary>`↑3 ↓2` compact form, empty when neither ahead nor behind.</summary>
    public string AheadBehindText => (Ahead, Behind) switch
    {
        (0, 0) => string.Empty,
        (> 0, 0) => $"↑{Ahead}",
        (0, > 0) => $"↓{Behind}",
        _ => $"↑{Ahead} ↓{Behind}",
    };

    public void Replace(Worktree worktree)
    {
        Worktree = worktree;
        OnPropertyChanged(nameof(DisplayBranch));
        OnPropertyChanged(nameof(Path));
        // AutomationId is derived from DisplayBranch — a rename invalidates any wpf-cli
        // ref that resolved to the old token, so notify or the test layer keeps pointing
        // at a stale slug until something else triggers a snapshot.
        OnPropertyChanged(nameof(AutomationId));
    }

    /// <summary>Applies a refreshed <see cref="NoScope.CodeScope.Core.Models.WorktreeStatus"/> from the poller.</summary>
    public void ApplyStatus(NoScope.CodeScope.Core.Models.WorktreeStatus status)
    {
        IsDirty = status.IsDirty;
        Ahead = status.Ahead;
        Behind = status.Behind;
        Added = status.Added;
        Removed = status.Removed;
        ChangedFiles = status.ChangedFiles;
        if (status.Branch is { Length: > 0 } b && Worktree.Branch != b)
        {
            Worktree = Worktree with { Branch = b };
            OnPropertyChanged(nameof(DisplayBranch));
            OnPropertyChanged(nameof(AutomationId));
        }
        OnPropertyChanged(nameof(DirtyGlyph));
        OnPropertyChanged(nameof(AheadBehindText));
        OnPropertyChanged(nameof(AddedRemovedText));
        OnPropertyChanged(nameof(HasAddedRemoved));
    }

    /// <summary>"+3 −1" style display; empty when nothing changed. Binary-only changes show "~N files".</summary>
    public string AddedRemovedText
    {
        get
        {
            if (Added == 0 && Removed == 0)
            {
                return ChangedFiles > 0 ? $"~{ChangedFiles}" : string.Empty;
            }
            if (Added > 0 && Removed > 0) { return $"+{Added} −{Removed}"; }
            if (Added > 0) { return $"+{Added}"; }
            return $"−{Removed}";
        }
    }

    public bool HasAddedRemoved => Added > 0 || Removed > 0 || ChangedFiles > 0;

    partial void OnIsDirtyChanged(bool value)
    {
        OnPropertyChanged(nameof(DirtyGlyph));
        OnPropertyChanged(nameof(StatusLabel));
        OnPropertyChanged(nameof(DotState));
    }

    partial void OnAheadChanged(int value)
    {
        OnPropertyChanged(nameof(AheadBehindText));
        OnPropertyChanged(nameof(StatusLabel));
    }

    partial void OnBehindChanged(int value)
    {
        OnPropertyChanged(nameof(AheadBehindText));
        OnPropertyChanged(nameof(StatusLabel));
    }

    partial void OnPullRequestChanged(PullRequestInfo? value)
    {
        OnPropertyChanged(nameof(HasPullRequest));
        OnPropertyChanged(nameof(PrBadgeText));
        OnPropertyChanged(nameof(CiGlyph));
        OnPropertyChanged(nameof(StatusLabel));
        OnPropertyChanged(nameof(DotState));
        OpenPullRequestCommand.NotifyCanExecuteChanged();
    }
}
