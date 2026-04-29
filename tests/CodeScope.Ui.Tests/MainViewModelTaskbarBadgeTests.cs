using Microsoft.Extensions.Logging.Abstractions;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

/// <summary>
/// Unit tests for the <see cref="MainViewModel"/> taskbar-badge aggregation logic.
/// Most cases drive recompute directly via <c>RecomputeTaskbarBadgeForTests</c> to keep
/// the count assertions tight; <see cref="StatusFlip_TriggersRecompute_ViaProductionHook"/>
/// exercises the real <see cref="MainViewModel.HookStatusBarSources"/> →
/// <c>RaiseStatusBarChanged</c> → <c>RecomputeTaskbarBadge</c> wiring end-to-end.
/// </summary>
public sealed class MainViewModelTaskbarBadgeTests
{
    // ──── Helpers ────────────────────────────────────────────────────────────

    private static SessionDescriptor MakeDescriptor(string id = "s1") =>
        new()
        {
            Id = id,
            WorkingDirectory = @"C:\repo",
            Shell = "pwsh.exe",
            ShellArgs = [],
            Title = id,
        };

    /// <summary>
    /// Creates an agent tab (non-empty, non-"shell" AgentId).
    /// </summary>
    private static SessionTabViewModel AgentTab(
        string agentId = "claude",
        string id = "s1",
        TabStatus status = TabStatus.Idle) =>
        new(MakeDescriptor(id), projectId: null, agentId: agentId, displayNameOverride: agentId)
        {
            Status = status,
        };

    /// <summary>
    /// Creates a shell tab (null AgentId — same as a plain shell session).
    /// </summary>
    private static SessionTabViewModel ShellTab(string id = "shell1", TabStatus status = TabStatus.Idle) =>
        new(MakeDescriptor(id), projectId: null, agentId: null, displayNameOverride: "shell")
        {
            Status = status,
        };

    private static MainViewModel MakeVm(ITaskbarBadgeService badge, params SessionTabViewModel[] tabs)
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([]);

        var manager = Substitute.For<ISessionManager>();
        var agents = Substitute.For<IAgentRegistry>();
        agents.GetAll().Returns([]);

        var vm = new MainViewModel(
            manager,
            store,
            agents,
            NullLogger<MainViewModel>.Instance,
            pickFolder: () => null,
            taskbarBadge: badge);

        foreach (var t in tabs)
        {
            vm.Tabs.Add(t);
        }

        return vm;
    }

    // ──── Tests ──────────────────────────────────────────────────────────────

    [Fact]
    public void EmptyWorkspace_TriggersClear()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = MakeVm(badge);

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received(1).Apply(0, 0);
    }

    [Fact]
    public void IdleAgentTab_GreenSignal()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = MakeVm(badge, AgentTab(status: TabStatus.Idle));

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received(1).Apply(0, 1);
    }

    [Fact]
    public void TwoBusyOfThree_RedTwo()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = MakeVm(badge,
            AgentTab("claude", "s1", TabStatus.Busy),
            AgentTab("codex",  "s2", TabStatus.Busy),
            AgentTab("pi",     "s3", TabStatus.Idle));

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received(1).Apply(2, 3);
    }

    [Fact]
    public void TwelveBusy_RawCountPassedThrough()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var tabs = Enumerable.Range(1, 12)
                             .Select(i => AgentTab("claude", $"s{i}", TabStatus.Busy))
                             .ToArray();
        var vm = MakeVm(badge, tabs);

        vm.RecomputeTaskbarBadgeForTests();

        // Cap is the service's responsibility, not the VM's — raw counts are forwarded.
        badge.Received(1).Apply(12, 12);
    }

    [Fact]
    public void ShellTab_NotCounted()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        // A shell tab that is Busy must not increment either count.
        var vm = MakeVm(badge,
            ShellTab("shell1", TabStatus.Busy),
            AgentTab("claude", "s1", TabStatus.Idle));

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received(1).Apply(0, 1);
    }

    [Fact]
    public void StatusFlip_TriggersRecompute_ViaProductionHook()
    {
        // Drives the same path as production: HookStatusBarSources subscribes to per-tab
        // Status PropertyChanged; flipping Status fires RaiseStatusBarChanged which calls
        // RecomputeTaskbarBadge. Sidebar isn't needed — that branch in HookStatusBarSources
        // is gated by `Sidebar is not null`.
        var badge = Substitute.For<ITaskbarBadgeService>();
        var tab = AgentTab("claude", "s1", TabStatus.Idle);
        var vm = MakeVm(badge, tab);
        vm.HookStatusBarSources();
        badge.ClearReceivedCalls();

        tab.Status = TabStatus.Busy;

        badge.Received().Apply(1, 1);
    }
}
