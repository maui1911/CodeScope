using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class ProjectViewModelTests
{
    private static Project MakeProject(string name = "codescope", string defaultBranch = "main", string? root = null) =>
        new()
        {
            Id = $"p-{name}",
            Name = name,
            Path = @"C:\dev\" + name,
            DefaultBranch = defaultBranch,
            WorktreeRoot = root,
        };

    private static Worktree MakeWorktree(string id) =>
        new()
        {
            Id = id,
            Path = @"C:\dev\codescope.worktrees\" + id,
            Branch = id,
        };

    private static WorktreeViewModel MakeWorktreeVm(string id, bool isDirty = false, bool hasPr = false)
    {
        var vm = new WorktreeViewModel("p", MakeWorktree(id));
        if (isDirty) { vm.IsDirty = true; }
        if (hasPr) { vm.PullRequest = new PullRequestInfo { Number = 1, State = "OPEN", Url = "https://x/y/pull/1", CiStatus = CiStatus.None }; }
        return vm;
    }

    [Fact]
    public void AutomationId_BuildsFromName()
    {
        new ProjectViewModel(MakeProject("CodeScope")).AutomationId.Should().Be("Project_CodeScope");
        new ProjectViewModel(MakeProject("hello world")).AutomationId.Should().Be("Project_hello_world");
    }

    [Fact]
    public void DefaultBranch_FallsBackToMain_WhenBlank()
    {
        new ProjectViewModel(MakeProject(defaultBranch: "")).DefaultBranch.Should().Be("main");
        new ProjectViewModel(MakeProject(defaultBranch: "  ")).DefaultBranch.Should().Be("main");
        new ProjectViewModel(MakeProject(defaultBranch: "develop")).DefaultBranch.Should().Be("develop");
    }

    [Fact]
    public void Summary_EmptyWhenNoWorktrees()
    {
        new ProjectViewModel(MakeProject()).Summary.Should().BeEmpty();
    }

    [Fact]
    public void Summary_RendersCountAndStateSegments()
    {
        var vm = new ProjectViewModel(MakeProject());
        vm.Worktrees.Add(MakeWorktreeVm("a", isDirty: true));
        vm.Worktrees.Add(MakeWorktreeVm("b", hasPr: true));
        vm.Worktrees.Add(MakeWorktreeVm("c"));

        vm.Summary.Should().Be("3 · 1 dirty · 1 PR");
    }

    [Fact]
    public void Summary_PluralisesPRsCorrectly()
    {
        var vm = new ProjectViewModel(MakeProject());
        vm.Worktrees.Add(MakeWorktreeVm("a", hasPr: true));
        vm.Worktrees.Add(MakeWorktreeVm("b", hasPr: true));
        vm.Summary.Should().Be("2 · 2 PRs");
    }

    [Fact]
    public void CountBadge_EmptyWhenNoWorktrees()
    {
        new ProjectViewModel(MakeProject()).CountBadge.Should().BeEmpty();
    }

    [Fact]
    public void CountBadge_ReflectsWorktreeCount()
    {
        var vm = new ProjectViewModel(MakeProject());
        vm.Worktrees.Add(MakeWorktreeVm("a"));
        vm.Worktrees.Add(MakeWorktreeVm("b"));
        vm.CountBadge.Should().Be("2");
    }

    [Fact]
    public void HasNoWorktrees_RaisesPropertyChanged_OnCollectionMutation()
    {
        var vm = new ProjectViewModel(MakeProject());
        var changed = new List<string?>();
        vm.PropertyChanged += (_, e) => changed.Add(e.PropertyName);

        vm.Worktrees.Add(MakeWorktreeVm("a"));

        changed.Should().Contain(nameof(ProjectViewModel.CountBadge));
        changed.Should().Contain(nameof(ProjectViewModel.HasWaitingChild));
    }
}
