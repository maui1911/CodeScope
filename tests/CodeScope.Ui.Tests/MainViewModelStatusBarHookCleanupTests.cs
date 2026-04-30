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

    [Fact]
    public void MovingTab_DoesNotEvict()
    {
        // ObservableCollection.Move fires CollectionChanged with OldItems set.
        // Eviction must NOT fire on Move — only on Remove/Replace — otherwise
        // reordering tabs within a group would drop the entry and re-hook a
        // duplicate PropertyChanged handler.
        var vm = MakeVmWithStatusBarHooks();
        vm.Tabs.Add(MakeTab("s1"));
        vm.Tabs.Add(MakeTab("s2"));
        vm.Tabs.Add(MakeTab("s3"));

        vm.StatusBarHookedTabCountForTests.Should().Be(3);

        vm.Tabs.Move(0, 2);

        vm.StatusBarHookedTabCountForTests.Should().Be(3);
    }

    [Fact]
    public void ClearingTabs_RemovesAllEntries()
    {
        // ObservableCollection.Clear raises CollectionChanged with action=Reset and
        // OldItems=null, so the reconcile path (not the OldItems path) must handle it.
        var vm = MakeVmWithStatusBarHooks();
        vm.Tabs.Add(MakeTab("s1"));
        vm.Tabs.Add(MakeTab("s2"));
        vm.Tabs.Add(MakeTab("s3"));

        vm.StatusBarHookedTabCountForTests.Should().Be(3);

        vm.Tabs.Clear();

        vm.StatusBarHookedTabCountForTests.Should().Be(0);
    }

    [Fact]
    public void CrossGroupMove_KeepsExactlyOneHandler()
    {
        // MoveTabToGroup transfers the same SessionTabViewModel between groups via
        // Remove + Add. A naive HashSet<VM> would Remove on source, then Add returns
        // true on target → a second PropertyChanged handler is attached, doubling
        // every status-bar recompute for that tab. The dict-based hook stores the
        // handler instance and unsubscribes precisely on eviction — but the eviction
        // must skip the Remove because the tab is still alive in another group.
        var vm = MakeVmWithStatusBarHooks();
        var sourceGroup = vm.FocusedGroup;
        vm.SplitRightCommand.Execute(null);
        var targetGroup = vm.Groups.Last(g => !ReferenceEquals(g, sourceGroup));

        var tab = MakeTab("s1");
        sourceGroup.Tabs.Add(tab);
        vm.StatusBarHookedTabCountForTests.Should().Be(1);

        // Cross-group move: same VM removed from source, re-added to target.
        sourceGroup.Tabs.Remove(tab);
        targetGroup.Tabs.Add(tab);

        vm.StatusBarHookedTabCountForTests.Should().Be(1);

        // And only ONE handler should be attached — drive a single property change
        // and assert the recompute count via badge service hooks.
        var badge = Substitute.For<NoScope.CodeScope.Ui.Services.ITaskbarBadgeService>();
        var vm2 = MakeVmWithBadge(badge);
        var srcG = vm2.FocusedGroup;
        vm2.SplitRightCommand.Execute(null);
        var dstG = vm2.Groups.Last(g => !ReferenceEquals(g, srcG));
        var tab2 = MakeTab("s2");
        srcG.Tabs.Add(tab2);
        srcG.Tabs.Remove(tab2);
        dstG.Tabs.Add(tab2);
        badge.ClearReceivedCalls();

        tab2.Status = TabStatus.Busy;

        // Exactly one badge.Apply call would fire from one PropertyChanged handler.
        badge.Received(1).Apply(Arg.Any<int>(), Arg.Any<int>());
    }

    private static MainViewModel MakeVmWithBadge(NoScope.CodeScope.Ui.Services.ITaskbarBadgeService badge)
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([]);
        var manager = Substitute.For<ISessionManager>();
        var agents = Substitute.For<IAgentRegistry>();
        agents.GetAll().Returns([]);
        var vm = new MainViewModel(
            manager, store, agents,
            NullLogger<MainViewModel>.Instance,
            pickFolder: () => null,
            taskbarBadge: badge);
        vm.HookStatusBarSources();
        return vm;
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
