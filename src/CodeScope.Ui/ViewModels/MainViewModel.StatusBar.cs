using System.Collections.Specialized;
using System.ComponentModel;
using System.Linq;
using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Structured status-bar projection — clusters per <c>docs/design/html/CodeScope - Status Bar Spec.html</c>.
/// Left cluster reads from the focused session + its worktree; right cluster rolls up workspace-wide agent state.
/// </summary>
public sealed partial class MainViewModel
{
    /// <summary>Hooks once Sidebar is attached — wires per-tab Status subscriptions and worktree dirty/ahead change propagation.</summary>
    internal void HookStatusBarSources()
    {
        Groups.CollectionChanged += (_, e) =>
        {
            if (e.NewItems is not null)
            {
                foreach (var g in e.NewItems.OfType<EditorGroupViewModel>()) { HookGroupForStatusBar(g); }
            }
            RaiseStatusBarChanged();
        };
        foreach (var g in Groups) { HookGroupForStatusBar(g); }
        foreach (var t in AllTabs) { HookTabForStatusBar(t); }

        if (Sidebar is not null)
        {
            Sidebar.Projects.CollectionChanged += (_, _) => RaiseStatusBarChanged();
            foreach (var p in Sidebar.Projects) { HookProjectForStatusBar(p); }
            Sidebar.Projects.CollectionChanged += (_, e) =>
            {
                if (e.NewItems is not null)
                {
                    foreach (var p in e.NewItems.OfType<ProjectViewModel>()) { HookProjectForStatusBar(p); }
                }
            };
        }
    }

    private void HookProjectForStatusBar(ProjectViewModel p)
    {
        p.Worktrees.CollectionChanged += (_, e) =>
        {
            if (e.NewItems is not null)
            {
                foreach (var w in e.NewItems.OfType<WorktreeViewModel>()) { HookWorktreeForStatusBar(w); }
            }
            RaiseStatusBarChanged();
        };
        foreach (var w in p.Worktrees) { HookWorktreeForStatusBar(w); }
    }

    private readonly HashSet<WorktreeViewModel> _statusBarHookedWts = [];
    private void HookWorktreeForStatusBar(WorktreeViewModel w)
    {
        if (!_statusBarHookedWts.Add(w)) { return; }
        w.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(WorktreeViewModel.IsDirty)
                or nameof(WorktreeViewModel.Ahead)
                or nameof(WorktreeViewModel.Behind)
                or nameof(WorktreeViewModel.Added)
                or nameof(WorktreeViewModel.Removed)
                or nameof(WorktreeViewModel.ChangedFiles)
                or nameof(WorktreeViewModel.DisplayBranch))
            {
                RaiseStatusBarChanged();
            }
        };
    }

    private void HookGroupForStatusBar(EditorGroupViewModel g)
    {
        g.Tabs.CollectionChanged += (_, e) =>
        {
            if (e.NewItems is not null)
            {
                foreach (var t in e.NewItems.OfType<SessionTabViewModel>()) { HookTabForStatusBar(t); }
            }
            RaiseStatusBarChanged();
        };
    }

    private readonly HashSet<SessionTabViewModel> _statusBarHookedTabs = [];
    private void HookTabForStatusBar(SessionTabViewModel t)
    {
        if (!_statusBarHookedTabs.Add(t)) { return; }
        t.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(SessionTabViewModel.Status)
                || e.PropertyName == nameof(SessionTabViewModel.DisplayName)
                || e.PropertyName == nameof(SessionTabViewModel.TokensUsed)
                || e.PropertyName == nameof(SessionTabViewModel.TurnCount)
                || e.PropertyName == nameof(SessionTabViewModel.LastTurnDurationSec)
                || e.PropertyName == nameof(SessionTabViewModel.ContextWindowTokens))
            {
                RaiseStatusBarChanged();
            }
        };
    }

    private void RaiseStatusBarChanged()
    {
        OnPropertyChanged(nameof(StatusHasSession));
        OnPropertyChanged(nameof(StatusBranch));
        OnPropertyChanged(nameof(StatusDotState));
        OnPropertyChanged(nameof(StatusIsDirty));
        OnPropertyChanged(nameof(StatusAdded));
        OnPropertyChanged(nameof(StatusRemoved));
        OnPropertyChanged(nameof(StatusChangedFiles));
        OnPropertyChanged(nameof(StatusAddedText));
        OnPropertyChanged(nameof(StatusRemovedText));
        OnPropertyChanged(nameof(StatusHasAdded));
        OnPropertyChanged(nameof(StatusHasRemoved));
        OnPropertyChanged(nameof(StatusHasDiffStats));
        OnPropertyChanged(nameof(StatusAhead));
        OnPropertyChanged(nameof(StatusBehind));
        OnPropertyChanged(nameof(StatusHasRemoteDelta));
        OnPropertyChanged(nameof(StatusAheadBehindText));
        OnPropertyChanged(nameof(StatusModelName));
        OnPropertyChanged(nameof(StatusAgentBusy));
        OnPropertyChanged(nameof(StatusAgentReady));
        OnPropertyChanged(nameof(StatusAgentSummaryVisible));
        OnPropertyChanged(nameof(StatusGroupCountText));
        OnPropertyChanged(nameof(StatusGroupCountVisible));
        OnPropertyChanged(nameof(StatusWorktreeCount));
        OnPropertyChanged(nameof(StatusWorktreeCountText));
        OnPropertyChanged(nameof(StatusWorkspaceVisible));
        OnPropertyChanged(nameof(StatusDirtyCount));
        OnPropertyChanged(nameof(StatusDirtyCountText));
        OnPropertyChanged(nameof(StatusDirtyVisible));
        OnPropertyChanged(nameof(StatusPrCount));
        OnPropertyChanged(nameof(StatusPrCountText));
        OnPropertyChanged(nameof(StatusPrVisible));
        OnPropertyChanged(nameof(StatusCiFailCount));
        OnPropertyChanged(nameof(StatusCiFailCountText));
        OnPropertyChanged(nameof(StatusCiFailVisible));
        OnPropertyChanged(nameof(StatusEmptyMessage));
        OnPropertyChanged(nameof(StatusEmptyVisible));
        OnPropertyChanged(nameof(StatusTurnCountText));
        OnPropertyChanged(nameof(StatusTurnCountVisible));
        OnPropertyChanged(nameof(StatusTokensText));
        OnPropertyChanged(nameof(StatusTokensVisible));
        OnPropertyChanged(nameof(StatusTokensPercentText));
        OnPropertyChanged(nameof(StatusTokensPercentVisible));
        OnPropertyChanged(nameof(StatusTurnsText));
        OnPropertyChanged(nameof(StatusTurnsVisible));
        OnPropertyChanged(nameof(StatusTurnDurationText));
        OnPropertyChanged(nameof(StatusTurnDurationVisible));
    }

    private WorktreeViewModel? ResolveFocusedWorktree()
    {
        if (SelectedTab is not { } tab) { return null; }
        if (Sidebar is null) { return null; }
        // Sidebar session rows are mirror instances (distinct from tab-strip VMs),
        // so match by descriptor id (= persisted Session.Id) instead of reference.
        var id = tab.Descriptor.Id;
        foreach (var p in Sidebar.Projects)
        {
            if (tab.ProjectId is not null && p.Id != tab.ProjectId) { continue; }
            foreach (var w in p.Worktrees)
            {
                foreach (var s in w.Sessions)
                {
                    if (s.Descriptor.Id == id) { return w; }
                }
            }
        }
        return null;
    }

    public bool StatusHasSession => SelectedTab is not null;

    public bool StatusEmptyVisible => Sidebar is not null && Sidebar.Projects.Count == 0;
    public string StatusEmptyMessage => "CodeScope — add a project to begin.";

    /// <summary>Branch of the focused session's worktree, or the tab's display name as fallback.</summary>
    public string StatusBranch
    {
        get
        {
            var wt = ResolveFocusedWorktree();
            if (wt is not null) { return wt.DisplayBranch; }
            return SelectedTab?.DisplayName ?? string.Empty;
        }
    }

    /// <summary>"busy" / "ready" — mirrors the focused tab's <see cref="TabStatus"/>.</summary>
    public string StatusDotState => SelectedTab?.Status switch
    {
        TabStatus.Busy => "busy",
        _ => "ready",
    };

    public bool StatusIsDirty => ResolveFocusedWorktree()?.IsDirty ?? false;
    public int StatusAdded => ResolveFocusedWorktree()?.Added ?? 0;
    public int StatusRemoved => ResolveFocusedWorktree()?.Removed ?? 0;
    public int StatusChangedFiles => ResolveFocusedWorktree()?.ChangedFiles ?? 0;
    public string StatusAddedText => StatusAdded > 0 ? $"+{StatusAdded}" : string.Empty;
    public string StatusRemovedText => StatusRemoved > 0 ? $"−{StatusRemoved}" : string.Empty;
    public bool StatusHasAdded => StatusAdded > 0;
    public bool StatusHasRemoved => StatusRemoved > 0;
    public bool StatusHasDiffStats => StatusIsDirty && (StatusAdded > 0 || StatusRemoved > 0 || StatusChangedFiles > 0);
    public int StatusAhead => ResolveFocusedWorktree()?.Ahead ?? 0;
    public int StatusBehind => ResolveFocusedWorktree()?.Behind ?? 0;
    public bool StatusHasRemoteDelta => StatusAhead > 0 || StatusBehind > 0;
    public string StatusAheadBehindText => (StatusAhead, StatusBehind) switch
    {
        (0, 0) => string.Empty,
        (> 0, 0) => $"↑{StatusAhead}",
        (0, > 0) => $"↓{StatusBehind}",
        _ => $"↑{StatusAhead} ↓{StatusBehind}",
    };

    /// <summary>Agent display name for the focused session (from the registry). "shell" for shell tabs.</summary>
    public string StatusModelName
    {
        get
        {
            if (SelectedTab is null) { return string.Empty; }
            var agentId = SelectedTab.AgentId;
            if (string.IsNullOrWhiteSpace(agentId)) { return "shell"; }
            var profile = _agents.GetById(agentId);
            return profile?.DisplayName ?? agentId;
        }
    }

    public int StatusAgentBusy => AllTabs.Count(t => t.Status == TabStatus.Busy);
    public int StatusAgentReady => AllTabs.Count(t => t.Status == TabStatus.Ready);
    public bool StatusAgentSummaryVisible => AllTabs.Any();

    public bool StatusGroupCountVisible => Groups.Count > 1;
    public string StatusGroupCountText => $"{Groups.Count} groups";

    /// <summary>Total worktree count across all tracked projects, used for the workspace summary.</summary>
    public int StatusWorktreeCount
        => Sidebar is null ? 0 : Sidebar.Projects.Sum(p => p.Worktrees.Count);
    public bool StatusWorkspaceVisible => StatusWorktreeCount > 0;
    public string StatusWorktreeCountText
        => $"{StatusWorktreeCount} worktree{(StatusWorktreeCount == 1 ? "" : "s")}";

    /// <summary>Number of dirty worktrees (uncommitted changes) across all projects.</summary>
    public int StatusDirtyCount
        => Sidebar is null ? 0 : Sidebar.Projects.Sum(p => p.Worktrees.Count(w => w.IsDirty));
    public bool StatusDirtyVisible => StatusDirtyCount > 0;
    public string StatusDirtyCountText => $"{StatusDirtyCount} dirty";

    /// <summary>Number of worktrees with an open pull request.</summary>
    public int StatusPrCount
        => Sidebar is null ? 0 : Sidebar.Projects.Sum(p => p.Worktrees.Count(w => w.HasPullRequest));
    public bool StatusPrVisible => StatusPrCount > 0;
    public string StatusPrCountText => $"{StatusPrCount} PR{(StatusPrCount == 1 ? "" : "s")}";

    /// <summary>Number of worktrees whose PR has a failing CI status.</summary>
    public int StatusCiFailCount => Sidebar is null
        ? 0
        : Sidebar.Projects.Sum(p => p.Worktrees.Count(w => w.PullRequest?.CiStatus == CiStatus.Failure));
    public bool StatusCiFailVisible => StatusCiFailCount > 0;
    public string StatusCiFailCountText => $"{StatusCiFailCount} CI failing";

    /// <summary>Tab count — simple proxy for "turns" while runtime wall-clock isn't wired.</summary>
    public int StatusTurnCount => AllTabs.Count();
    public bool StatusTurnCountVisible => StatusTurnCount > 0;
    public string StatusTurnCountText => $"{StatusTurnCount} tab{(StatusTurnCount == 1 ? "" : "s")}";

    /// <summary>
    /// Focused session's token total, K/M-formatted — or <c>"&lt;used&gt;/&lt;cap&gt;"</c> when the
    /// agent's <see cref="AgentProfile.ContextWindowTokens"/> is known. Empty until the first
    /// assistant turn lands.
    /// </summary>
    public string StatusTokensText
    {
        get
        {
            if (SelectedTab is not { TokensUsed: > 0 } t) { return string.Empty; }
            var cap = ContextWindowFor(t);
            return cap > 0 ? $"{FormatTokens(t.TokensUsed)}/{FormatTokens(cap)}" : FormatTokens(t.TokensUsed);
        }
    }
    public bool StatusTokensVisible => SelectedTab is { TokensUsed: > 0 };

    /// <summary>Percent of the context window consumed, e.g. "3%" or "87%". Empty when cap is unknown.</summary>
    public string StatusTokensPercentText
    {
        get
        {
            if (SelectedTab is not { TokensUsed: > 0 } t) { return string.Empty; }
            var cap = ContextWindowFor(t);
            if (cap <= 0) { return string.Empty; }
            var pct = Math.Clamp(t.TokensUsed * 100.0 / cap, 0, 999);
            return pct >= 10 ? $"{pct:0}%" : $"{pct:0.#}%";
        }
    }
    public bool StatusTokensPercentVisible => SelectedTab is { TokensUsed: > 0 } && ContextWindowFor(SelectedTab) > 0;

    /// <summary>Focused session's assistant-turn count from the transcript (spec callout #9 proxy).</summary>
    public string StatusTurnsText => SelectedTab is { TurnCount: > 0 } t ? $"{t.TurnCount} turn{(t.TurnCount == 1 ? "" : "s")}" : string.Empty;
    public bool StatusTurnsVisible => SelectedTab is { TurnCount: > 0 };

    /// <summary>Wall-clock of the most recent turn, e.g. "3.1s" / "42s" / "2m 12s".</summary>
    public string StatusTurnDurationText => SelectedTab is { LastTurnDurationSec: > 0 } t ? FormatDuration(t.LastTurnDurationSec) : string.Empty;
    public bool StatusTurnDurationVisible => SelectedTab is { LastTurnDurationSec: > 0 };

    private int ContextWindowFor(SessionTabViewModel t)
    {
        // Transcript-detected cap wins — it reflects the model the agent is actually running,
        // including extended-context (1M) SKUs. Falls back to the agent profile's baked default
        // when the transcript hasn't surfaced a recognisable model id yet.
        if (t.ContextWindowTokens > 0) { return t.ContextWindowTokens; }
        if (string.IsNullOrEmpty(t.AgentId)) { return 0; }
        var profile = _agents.GetById(t.AgentId);
        return profile?.ContextWindowTokens ?? 0;
    }

    private static string FormatTokens(int n) => n switch
    {
        >= 1_000_000 => $"{n / 1_000_000.0:0.0}M",
        >= 1_000 => $"{n / 1_000.0:0.#}k",
        _ => n.ToString(),
    };

    private static string FormatDuration(double seconds) => seconds switch
    {
        < 10 => $"{seconds:0.0}s",
        < 60 => $"{seconds:0}s",
        < 3600 => $"{(int)(seconds / 60)}m {(int)(seconds % 60)}s",
        _ => $"{(int)(seconds / 3600)}h {(int)((seconds % 3600) / 60)}m",
    };
}
