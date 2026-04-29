using System.Linq;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Window-level aggregation that drives <see cref="ITaskbarBadgeService"/>. Reuses the
/// existing tab-status hooks in <see cref="MainViewModel"/>.<see cref="HookStatusBarSources"/>;
/// the call site lives inside <see cref="RaiseStatusBarChanged"/> so we don't subscribe twice
/// to the same <see cref="SessionTabViewModel.Status"/> events.
/// </summary>
public sealed partial class MainViewModel
{
    public int BusyAgentCount => AllTabs.Count(IsAgentBusy);
    public int AgentTabCount => AllTabs.Count(IsAgentTab);

    private static bool IsAgentTab(SessionTabViewModel t)
        => !string.IsNullOrEmpty(t.AgentId)
           && !string.Equals(t.AgentId, ShellSentinel, System.StringComparison.OrdinalIgnoreCase);

    private static bool IsAgentBusy(SessionTabViewModel t)
        => IsAgentTab(t) && t.Status == TabStatus.Busy;

    private void RecomputeTaskbarBadge()
    {
        _taskbarBadge?.Apply(BusyAgentCount, AgentTabCount);
        OnPropertyChanged(nameof(BusyAgentCount));
        OnPropertyChanged(nameof(AgentTabCount));
    }

    /// <summary>Test-only entry point — production path runs through <c>RaiseStatusBarChanged</c>.</summary>
    internal void RecomputeTaskbarBadgeForTests() => RecomputeTaskbarBadge();
}
