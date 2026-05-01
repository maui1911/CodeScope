using System.Collections.ObjectModel;
using Microsoft.Extensions.Logging.Abstractions;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

/// <summary>
/// Verifies the rebuild gate added in #30 — while <c>IsActive</c> is false, store / tab /
/// worktree-property events flip a dirty flag instead of rebuilding the cards. The first
/// IsActive=true after a dirty interval triggers a single rebuild.
/// </summary>
public sealed class OverviewViewModelGatingTests
{
    private static OverviewViewModel BuildVM(
        out SidebarViewModel sidebar,
        out ObservableCollection<EditorGroupViewModel> groups)
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([]);
        sidebar = new SidebarViewModel(store, NullLogger<SidebarViewModel>.Instance);
        groups = [];
        return new OverviewViewModel(sidebar, groups);
    }

    private static EditorGroupViewModel MakeGroup() =>
        new();

    private static (string projectId, WorktreeViewModel wt, SessionTabViewModel tab) MakeTab(
        SidebarViewModel sidebar, string projectId = "p1", string wtId = "w1")
    {
        var pvm = new ProjectViewModel(new Project { Id = projectId, Name = projectId, Path = $@"C:\repo\{projectId}" });
        var wvm = new WorktreeViewModel(projectId, new Worktree { Id = wtId, Path = $@"C:\repo\{projectId}\{wtId}", Branch = wtId });
        pvm.Worktrees.Add(wvm);
        sidebar.Projects.Add(pvm);
        var tab = new SessionTabViewModel(
            new SessionDescriptor { Id = "s1", WorkingDirectory = wvm.Path, Shell = "pwsh.exe", Title = "s1" },
            projectId,
            agentId: null,
            displayNameOverride: "s1");
        return (projectId, wvm, tab);
    }

    [Fact]
    public void Hidden_DefersRebuilds_UntilIsActive_FlipsTrue()
    {
        var vm = BuildVM(out var sidebar, out var groups);

        // Default: IsActive=false. Constructor's initial Rebuild has run once, Cards is empty.
        vm.IsActive.Should().BeFalse();
        vm.Cards.Should().BeEmpty();

        // Stage a tab in a group while hidden — should NOT rebuild Cards.
        var group = MakeGroup();
        groups.Add(group);
        var made = MakeTab(sidebar);
        group.Tabs.Add(made.tab);

        vm.Cards.Should().BeEmpty("rebuild was deferred while IsActive=false");

        // Become active → single rebuild flushes the dirty interval.
        vm.IsActive = true;
        vm.Cards.Should().HaveCount(1);
        vm.Cards[0].Session.Should().BeSameAs(made.tab);
    }

    [Fact]
    public void Active_RebuildsImmediately_OnEvent()
    {
        var vm = BuildVM(out var sidebar, out var groups);

        vm.IsActive = true; // simulate user has opened the Overview

        var group = MakeGroup();
        groups.Add(group);
        var made = MakeTab(sidebar);
        group.Tabs.Add(made.tab);

        vm.Cards.Should().HaveCount(1);
    }

    [Fact]
    public void HiddenAgain_StopsRebuilds_AndResumesOnNextActivation()
    {
        var vm = BuildVM(out var sidebar, out var groups);

        // First activation rebuild + add tab.
        vm.IsActive = true;
        var group = MakeGroup();
        groups.Add(group);
        var made1 = MakeTab(sidebar, projectId: "p1", wtId: "w1");
        group.Tabs.Add(made1.tab);
        vm.Cards.Should().HaveCount(1);

        // Hide → subsequent additions stay deferred.
        vm.IsActive = false;
        var made2 = MakeTab(sidebar, projectId: "p2", wtId: "w2");
        group.Tabs.Add(made2.tab);
        vm.Cards.Should().HaveCount(1, "Cards still reflects the pre-hide rebuild");

        // Re-activate → single rebuild reflects the new tab too.
        vm.IsActive = true;
        vm.Cards.Should().HaveCount(2);
    }

    [Fact]
    public void IsActive_NoOp_WhenAssignedSameValue()
    {
        var vm = BuildVM(out var sidebar, out var groups);

        var rebuildCount = 0;
        vm.Cards.CollectionChanged += (_, _) => rebuildCount++;

        // No-op assignments don't rebuild.
        vm.IsActive = false;
        vm.IsActive = false;
        rebuildCount.Should().Be(0);
    }
}
