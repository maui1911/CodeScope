using Microsoft.Extensions.Logging.Abstractions;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

/// <summary>
/// Regression coverage for issue #29 part A. Without OldItems handling in the status-bar
/// hook collection-changed handlers, <c>_statusBarHookedTabs</c> grew without bound and
/// retained every closed <see cref="SessionTabViewModel"/> for the lifetime of the
/// MainViewModel singleton. These tests assert eviction on Remove.
/// </summary>
public sealed class MainViewModelStatusBarHookCleanupTests
{
    [Fact]
    public void RemovingTab_EvictsFromHookedTabsSet()
    {
        var vm = MakeVmWithStatusBarHooks();
        var tab = MakeTab("s1");
        vm.Tabs.Add(tab);

        vm.StatusBarHookedTabCountForTests.Should().Be(1);

        vm.Tabs.Remove(tab);

        vm.StatusBarHookedTabCountForTests.Should().Be(0);
    }

    [Fact]
    public void RemovingMultipleTabs_EvictsAll()
    {
        var vm = MakeVmWithStatusBarHooks();
        var t1 = MakeTab("s1");
        var t2 = MakeTab("s2");
        var t3 = MakeTab("s3");
        vm.Tabs.Add(t1);
        vm.Tabs.Add(t2);
        vm.Tabs.Add(t3);

        vm.StatusBarHookedTabCountForTests.Should().Be(3);

        vm.Tabs.Remove(t1);
        vm.Tabs.Remove(t3);

        vm.StatusBarHookedTabCountForTests.Should().Be(1);
    }

    [Fact]
    public void OpenCloseManyTabs_HookedSetReturnsToZero()
    {
        // The leak this guards against was visible only after many open/close cycles —
        // simulate that explicitly so the regression bites if OldItems handling regresses.
        var vm = MakeVmWithStatusBarHooks();
        for (var i = 0; i < 50; i++)
        {
            var tab = MakeTab($"s{i}");
            vm.Tabs.Add(tab);
            vm.Tabs.Remove(tab);
        }

        vm.StatusBarHookedTabCountForTests.Should().Be(0);
    }

    private static MainViewModel MakeVmWithStatusBarHooks()
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([]);
        var manager = Substitute.For<ISessionManager>();
        var agents = Substitute.For<IAgentRegistry>();
        agents.GetAll().Returns([]);
        var vm = new MainViewModel(
            manager, store, agents,
            NullLogger<MainViewModel>.Instance,
            pickFolder: () => null);
        vm.HookStatusBarSources();
        return vm;
    }

    private static SessionTabViewModel MakeTab(string id) =>
        new(
            new SessionDescriptor
            {
                Id = id,
                WorkingDirectory = @"C:\repo",
                Shell = "pwsh.exe",
                ShellArgs = [],
                Title = id,
            },
            projectId: null,
            agentId: "claude",
            displayNameOverride: id);
}
