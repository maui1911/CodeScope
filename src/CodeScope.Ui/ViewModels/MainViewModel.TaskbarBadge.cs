using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Window-level aggregation that drives <see cref="ITaskbarBadgeService"/>. The recompute
/// fires from <see cref="RaiseStatusBarChanged"/>, which already runs on every per-tab
/// <see cref="SessionTabViewModel.Status"/> change and every <see cref="EditorGroupViewModel.Tabs"/>
/// mutation — no separate subscription needed.
/// </summary>
public sealed partial class MainViewModel
{
    private int _busyAgentCount = -1;
    private int _agentTabCount = -1;

    public int BusyAgentCount => _busyAgentCount;
    public int AgentTabCount => _agentTabCount;

    private static bool IsAgentTab(SessionTabViewModel t)
        => !string.IsNullOrEmpty(t.AgentId)
           && !string.Equals(t.AgentId, ShellSentinel, System.StringComparison.OrdinalIgnoreCase);

    private void RecomputeTaskbarBadge()
    {
        var busy = 0;
        var agents = 0;
        foreach (var tab in AllTabs)
        {
            if (!IsAgentTab(tab)) { continue; }
            agents++;
            if (tab.Status == TabStatus.Busy) { busy++; }
        }

        var changed = false;
        if (agents != _agentTabCount)
        {
            _agentTabCount = agents;
            OnPropertyChanged(nameof(AgentTabCount));
            changed = true;
        }
        if (busy != _busyAgentCount)
        {
            _busyAgentCount = busy;
            OnPropertyChanged(nameof(BusyAgentCount));
            changed = true;
        }

        if (changed) { _taskbarBadge?.Apply(busy, agents); }
    }

    /// <summary>Test-only entry point — production path runs through <c>RaiseStatusBarChanged</c>.</summary>
    internal void RecomputeTaskbarBadgeForTests() => RecomputeTaskbarBadge();
}
