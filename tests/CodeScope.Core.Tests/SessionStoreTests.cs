using NoScope.CodeScope.Core;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class SessionStoreTests
{
    private static (SessionStore store, IProjectStore persistence, IGitService git) Make(ProjectsConfig? initial = null)
    {
        var persistence = Substitute.For<IProjectStore>();
        persistence.LoadAsync(Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<ProjectsConfig>.Ok(initial ?? new ProjectsConfig())));
        persistence.SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Ok(true)));

        var git = Substitute.For<IGitService>();
        git.AddWorktreeAsync(Arg.Any<string>(), Arg.Any<string>(), Arg.Any<string>(), Arg.Any<string?>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Ok(true)));
        git.RemoveWorktreeAsync(Arg.Any<string>(), Arg.Any<string>(), Arg.Any<bool>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Ok(true)));
        git.MoveWorktreeAsync(Arg.Any<string>(), Arg.Any<string>(), Arg.Any<string>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Ok(true)));

        var store = new SessionStore(persistence, git, NullLogger<SessionStore>.Instance);
        return (store, persistence, git);
    }

    [Fact]
    public async Task LoadAsync_Populates_Projects_And_Emits_Loaded_Event()
    {
        var cfg = new ProjectsConfig
        {
            Projects = [new Project { Id = "p", Name = "P", Path = @"C:\p" }],
        };
        var (store, _, _) = Make(cfg);
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        await store.LoadAsync();

        store.Projects.Should().HaveCount(1);
        events.OfType<SessionStoreChange.Loaded>().Should().ContainSingle();
    }

    [Fact]
    public async Task AddProjectAsync_Adds_Persists_And_Emits_Event()
    {
        var (store, persistence, _) = Make();
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var added = await store.AddProjectAsync(@"C:\demo", "Demo");

        added.IsSuccess.Should().BeTrue();
        added.Value.Name.Should().Be("Demo");
        store.Projects.Should().ContainSingle(p => string.Equals(p.Path, @"C:\demo", StringComparison.OrdinalIgnoreCase));
        await persistence.Received(1).SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>());
        events.OfType<SessionStoreChange.ProjectAdded>().Should().ContainSingle();
    }

    [Fact]
    public async Task AddProjectAsync_Rejects_Duplicate_Path()
    {
        var (store, _, _) = Make(new ProjectsConfig
        {
            Projects = [new Project { Id = "x", Name = "X", Path = @"C:\demo" }],
        });
        await store.LoadAsync();

        var added = await store.AddProjectAsync(@"C:\demo", "Dup");

        added.IsFailure.Should().BeTrue();
        added.Error.Should().Contain("already");
    }

    [Fact]
    public async Task AddProjectAsync_Defaults_Name_To_Folder_Name()
    {
        var (store, _, _) = Make();

        var added = await store.AddProjectAsync(@"C:\PestScope", displayName: null);

        added.Value.Name.Should().Be("PestScope");
    }

    [Fact]
    public async Task RemoveProjectAsync_Removes_And_Emits_Event()
    {
        var (store, persistence, _) = Make();
        var added = await store.AddProjectAsync(@"C:\demo", "D");
        persistence.ClearReceivedCalls();
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.RemoveProjectAsync(added.Value.Id);

        result.IsSuccess.Should().BeTrue();
        store.Projects.Should().BeEmpty();
        await persistence.Received(1).SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>());
        events.OfType<SessionStoreChange.ProjectRemoved>()
            .Should().ContainSingle(e => e.ProjectId == added.Value.Id);
    }

    [Fact]
    public async Task RemoveProjectAsync_Returns_Fail_For_Unknown_Id()
    {
        var (store, _, _) = Make();
        var r = await store.RemoveProjectAsync("does-not-exist");
        r.IsFailure.Should().BeTrue();
    }

    [Fact]
    public async Task AddSessionAsync_Attaches_Session_To_Project_And_Emits_Event()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.AddSessionAsync(project.Id,
            new Session { Id = "s1", WorktreePath = project.Path });

        result.IsSuccess.Should().BeTrue();
        store.Projects.Single().Sessions.Should().ContainSingle(s => s.Id == "s1");
        events.OfType<SessionStoreChange.SessionAdded>()
            .Should().ContainSingle(e => e.ProjectId == project.Id && e.Session.Id == "s1");
    }

    [Fact]
    public async Task AddSessionAsync_Unknown_Project_Returns_Fail()
    {
        var (store, _, _) = Make();
        var r = await store.AddSessionAsync("nope",
            new Session { Id = "s", WorktreePath = @"C:\a" });
        r.IsFailure.Should().BeTrue();
    }

    [Fact]
    public async Task RemoveSessionAsync_Removes_Session_And_Emits_Event()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        await store.AddSessionAsync(project.Id, new Session { Id = "s", WorktreePath = project.Path });
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.RemoveSessionAsync("s");

        result.IsSuccess.Should().BeTrue();
        store.Projects.Single().Sessions.Should().BeEmpty();
        events.OfType<SessionStoreChange.SessionRemoved>()
            .Should().ContainSingle(e => e.SessionId == "s");
    }

    [Fact]
    public async Task RemoveSessionAsync_Unknown_Id_Returns_Fail()
    {
        var (store, _, _) = Make();
        var r = await store.RemoveSessionAsync("nope");
        r.IsFailure.Should().BeTrue();
    }

    [Fact]
    public async Task RenameSessionAsync_Updates_DisplayName_And_Emits_Event()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        await store.AddSessionAsync(project.Id, new Session { Id = "s", WorktreePath = project.Path });
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.RenameSessionAsync("s", "hot-fix");

        result.IsSuccess.Should().BeTrue();
        store.Projects.Single().Sessions.Single().DisplayName.Should().Be("hot-fix");
        events.OfType<SessionStoreChange.SessionRenamed>()
            .Should().ContainSingle(e => e.SessionId == "s" && e.NewName == "hot-fix");
    }

    [Fact]
    public async Task RenameSessionAsync_Null_Clears_Override()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        await store.AddSessionAsync(project.Id,
            new Session { Id = "s", WorktreePath = project.Path, DisplayName = "was-set" });

        await store.RenameSessionAsync("s", newName: null);

        store.Projects.Single().Sessions.Single().DisplayName.Should().BeNull();
    }

    [Fact]
    public async Task AddWorktreeAsync_Shells_Git_Then_Stores_Worktree()
    {
        var (store, _, git) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x");

        result.IsSuccess.Should().BeTrue();
        result.Value.Branch.Should().Be("feat/x");
        result.Value.IsPrimary.Should().BeFalse();
        await git.Received(1).AddWorktreeAsync(@"C:\demo", @"C:\demo.wt\feat-x", "feat/x", Arg.Any<string?>(), Arg.Any<CancellationToken>());
        store.Projects.Single().Worktrees.Should().ContainSingle(w => w.Branch == "feat/x");
        events.OfType<SessionStoreChange.WorktreeAdded>().Should().ContainSingle();
    }

    [Fact]
    public async Task AddWorktreeAsync_Returns_Fail_When_Git_Fails()
    {
        var (store, _, git) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        git.AddWorktreeAsync(Arg.Any<string>(), Arg.Any<string>(), Arg.Any<string>(), Arg.Any<string?>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Fail("fatal: branch exists")));

        var result = await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\x", "x");

        result.IsFailure.Should().BeTrue();
        result.Error.Should().Contain("fatal");
        // AddProjectAsync now synthesises a primary worktree, so a failed AddWorktree leaves
        // exactly that one behind.
        store.Projects.Single().Worktrees.Should().ContainSingle(w => w.IsPrimary);
    }

    [Fact]
    public async Task RemoveWorktreeAsync_Rejects_Primary()
    {
        var (store, _, _) = Make(new ProjectsConfig
        {
            Projects =
            [
                new Project
                {
                    Id = "p", Name = "P", Path = @"C:\p",
                    Worktrees = [new Worktree { Id = "primary", Path = @"C:\p", IsPrimary = true }],
                },
            ],
        });
        await store.LoadAsync();

        var result = await store.RemoveWorktreeAsync("p", "primary");

        result.IsFailure.Should().BeTrue();
        result.Error.Should().Contain("Primary");
    }

    [Fact]
    public async Task RemoveWorktreeAsync_Removes_Worktree_And_Its_Sessions()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;
        await store.AddSessionAsync(project.Id,
            new Session { Id = "s", WorktreePath = wt.Path, WorktreeId = wt.Id });

        var result = await store.RemoveWorktreeAsync(project.Id, wt.Id);

        result.IsSuccess.Should().BeTrue();
        var proj = store.Projects.Single();
        // The synthesised primary remains; only the secondary worktree (and its session) got removed.
        proj.Worktrees.Should().ContainSingle(w => w.IsPrimary);
        proj.Sessions.Should().BeEmpty();
    }

    [Fact]
    public async Task RenameWorktreeAsync_Shells_Git_Move_And_Updates_Path_And_Sessions()
    {
        var (store, _, git) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;
        await store.AddSessionAsync(project.Id,
            new Session { Id = "s", WorktreePath = wt.Path, WorktreeId = wt.Id });

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.RenameWorktreeAsync(project.Id, wt.Id, @"C:\demo.wt\feat-y");

        result.IsSuccess.Should().BeTrue();
        result.Value.Path.Should().Be(@"C:\demo.wt\feat-y");
        result.Value.Id.Should().Be(wt.Id);

        await git.Received(1).MoveWorktreeAsync(@"C:\demo", @"C:\demo.wt\feat-x", @"C:\demo.wt\feat-y", Arg.Any<CancellationToken>());

        var proj = store.Projects.Single();
        proj.Worktrees.Single(w => !w.IsPrimary).Path.Should().Be(@"C:\demo.wt\feat-y");
        proj.Sessions.Single().WorktreePath.Should().Be(@"C:\demo.wt\feat-y");

        events.OfType<SessionStoreChange.WorktreeRenamed>()
            .Should().ContainSingle()
            .Which.NewPath.Should().Be(@"C:\demo.wt\feat-y");
    }

    [Fact]
    public async Task RenameWorktreeAsync_Rejects_Primary()
    {
        var (store, _, _) = Make(new ProjectsConfig
        {
            Projects =
            [
                new Project
                {
                    Id = "p", Name = "P", Path = @"C:\demo",
                    Worktrees = [new Worktree { Id = "primary", Path = @"C:\demo", IsPrimary = true }],
                }
            ],
        });
        await store.LoadAsync();

        var result = await store.RenameWorktreeAsync("p", "primary", @"C:\demo-renamed");

        result.IsSuccess.Should().BeFalse();
    }

    [Fact]
    public async Task RenameWorktreeAsync_Returns_Fail_When_Git_Fails()
    {
        var (store, _, git) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;
        git.MoveWorktreeAsync(Arg.Any<string>(), Arg.Any<string>(), Arg.Any<string>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Fail("worktree is dirty")));

        var result = await store.RenameWorktreeAsync(project.Id, wt.Id, @"C:\demo.wt\feat-y");

        result.IsSuccess.Should().BeFalse();
        store.Projects.Single().Worktrees.Single(w => !w.IsPrimary).Path.Should().Be(@"C:\demo.wt\feat-x");
    }

    [Fact]
    public async Task UpdateWorktreePullRequest_Raises_Event_With_Payload()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var pr = new PullRequestInfo { Number = 7, State = "OPEN", Url = "https://x", CiStatus = CiStatus.Success };
        store.UpdateWorktreePullRequest(project.Id, wt.Id, pr);

        events.OfType<SessionStoreChange.WorktreePullRequestUpdated>()
            .Should().ContainSingle()
            .Which.PullRequest.Should().BeSameAs(pr);
    }

    [Fact]
    public async Task UpdateWorktreePullRequest_Silently_Noops_For_Unknown_Worktree()
    {
        var (store, _, _) = Make();
        await store.AddProjectAsync(@"C:\demo", "D");

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        store.UpdateWorktreePullRequest("ghost-project", "ghost-wt", pullRequest: null);
        store.UpdateWorktreePullRequest(store.Projects.Single().Id, "ghost-wt", pullRequest: null);

        events.OfType<SessionStoreChange.WorktreePullRequestUpdated>().Should().BeEmpty();
    }

    [Fact]
    public async Task UpdateWorktreeStatus_Unknown_Project_Returns_False_And_Raises_No_Event()
    {
        var (store, _, _) = Make();
        await store.AddProjectAsync(@"C:\demo", "D");

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var applied = store.UpdateWorktreeStatus(
            "ghost-project", "ghost-wt",
            new WorktreeStatus { Branch = "main", IsDirty = false, Ahead = 0, Behind = 0 });

        applied.Should().BeFalse();
        events.OfType<SessionStoreChange.WorktreeStatusUpdated>().Should().BeEmpty();
    }

    [Fact]
    public async Task UpdateWorktreeStatus_Unknown_Worktree_Returns_False_And_Raises_No_Event()
    {
        var (store, _, _) = Make();
        await store.AddProjectAsync(@"C:\demo", "D");

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var applied = store.UpdateWorktreeStatus(
            store.Projects.Single().Id, "ghost-wt",
            new WorktreeStatus { Branch = "main", IsDirty = false, Ahead = 0, Behind = 0 });

        applied.Should().BeFalse();
        events.OfType<SessionStoreChange.WorktreeStatusUpdated>().Should().BeEmpty();
    }

    [Fact]
    public async Task UpdateWorktreeStatus_New_Branch_Syncs_Worktree_And_Raises_Event()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var status = new WorktreeStatus { Branch = "feat/y", IsDirty = false, Ahead = 0, Behind = 0 };
        var applied = store.UpdateWorktreeStatus(project.Id, wt.Id, status);

        applied.Should().BeTrue();
        // Worktree.Branch mirrors the observed branch for future reload persistence.
        store.Projects.Single().Worktrees.Single(w => !w.IsPrimary).Branch.Should().Be("feat/y");
        events.OfType<SessionStoreChange.WorktreeStatusUpdated>()
            .Should().ContainSingle()
            .Which.Status.Should().Be(status);
    }

    [Fact]
    public async Task UpdateWorktreeStatus_Same_Branch_Raises_Event_Without_Mutating_Worktree()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;
        var beforeWt = store.Projects.Single().Worktrees.Single(w => !w.IsPrimary);

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var status = new WorktreeStatus { Branch = wt.Branch, IsDirty = true, Ahead = 0, Behind = 0 };
        var applied = store.UpdateWorktreeStatus(project.Id, wt.Id, status);

        applied.Should().BeTrue();
        // Branch unchanged ⇒ no record replacement (reference equality on the Worktree record).
        store.Projects.Single().Worktrees.Single(w => !w.IsPrimary).Should().BeSameAs(beforeWt);
        events.OfType<SessionStoreChange.WorktreeStatusUpdated>()
            .Should().ContainSingle();
    }

    [Fact]
    public async Task SetProjectDefaultAgent_Persists_And_Raises_Event()
    {
        var (store, persistence, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.SetProjectDefaultAgentAsync(project.Id, "claude");

        result.IsSuccess.Should().BeTrue();
        store.Projects.Single().DefaultAgentId.Should().Be("claude");
        events.OfType<SessionStoreChange.ProjectDefaultAgentChanged>()
            .Should().ContainSingle()
            .Which.AgentId.Should().Be("claude");
        await persistence.Received().SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>());
    }

    [Fact]
    public async Task SetProjectDefaultAgent_Null_Clears_Override()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        await store.SetProjectDefaultAgentAsync(project.Id, "claude");

        var result = await store.SetProjectDefaultAgentAsync(project.Id, null);

        result.IsSuccess.Should().BeTrue();
        store.Projects.Single().DefaultAgentId.Should().BeNull();
    }

    [Fact]
    public async Task SetProjectDefaultAgent_Unknown_Project_Fails()
    {
        var (store, _, _) = Make();
        var result = await store.SetProjectDefaultAgentAsync("ghost", "claude");
        result.IsSuccess.Should().BeFalse();
    }

    [Fact]
    public async Task UpdateWorktreePullRequest_Null_Is_Propagated()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        store.UpdateWorktreePullRequest(project.Id, wt.Id, pullRequest: null);

        events.OfType<SessionStoreChange.WorktreePullRequestUpdated>()
            .Should().ContainSingle()
            .Which.PullRequest.Should().BeNull();
    }

    [Fact]
    public async Task SoftCloseSessionAsync_Marks_Closed_And_Emits_Removed()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var sid = Guid.NewGuid().ToString("n");
        await store.AddSessionAsync(project.Id, new Session
        {
            Id = sid, WorktreePath = @"C:\demo", AgentId = "claude",
            AgentSessionId = "abc",
        });

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.SoftCloseSessionAsync(sid);

        result.IsSuccess.Should().BeTrue();
        var after = store.Projects.SelectMany(p => p.Sessions).Single(s => s.Id == sid);
        after.ClosedAt.Should().NotBeNull();
        after.AgentSessionId.Should().Be("abc", "the resume id must survive soft-close");
        events.OfType<SessionStoreChange.SessionRemoved>().Should().ContainSingle();
    }

    [Fact]
    public async Task RestoreSessionAsync_Clears_ClosedAt_And_Emits_Added()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var sid = Guid.NewGuid().ToString("n");
        await store.AddSessionAsync(project.Id, new Session
        {
            Id = sid, WorktreePath = @"C:\demo", AgentId = "claude",
            AgentSessionId = "abc",
        });
        await store.SoftCloseSessionAsync(sid);

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var restored = await store.RestoreSessionAsync(sid);

        restored.IsSuccess.Should().BeTrue();
        restored.Value.ClosedAt.Should().BeNull();
        restored.Value.AgentSessionId.Should().Be("abc");
        events.OfType<SessionStoreChange.SessionAdded>().Should().ContainSingle();
    }

    [Fact]
    public async Task SoftClose_Is_Idempotent()
    {
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var sid = Guid.NewGuid().ToString("n");
        await store.AddSessionAsync(project.Id, new Session
        {
            Id = sid, WorktreePath = @"C:\demo", AgentId = "claude",
            AgentSessionId = "abc",
        });

        (await store.SoftCloseSessionAsync(sid)).IsSuccess.Should().BeTrue();

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);
        var second = await store.SoftCloseSessionAsync(sid);

        second.IsSuccess.Should().BeTrue();
        events.OfType<SessionStoreChange.SessionRemoved>().Should().BeEmpty("re-close should not emit a second event");
    }

    [Fact]
    public async Task RemoveWorktree_Cascades_Over_Soft_Closed_Sessions()
    {
        // Worktree delete should wipe both live and soft-closed sessions — a resurrected tree
        // shouldn't resurrect a ghost conversation that no longer has a working directory.
        var (store, _, _) = Make();
        var project = (await store.AddProjectAsync(@"C:\demo", "D")).Value;
        var wt = (await store.AddWorktreeAsync(project.Id, @"C:\demo.wt\feat-x", "feat/x")).Value;
        var sid = Guid.NewGuid().ToString("n");
        await store.AddSessionAsync(project.Id, new Session
        {
            Id = sid, WorktreePath = wt.Path, WorktreeId = wt.Id,
            AgentId = "claude", AgentSessionId = "abc",
        });
        await store.SoftCloseSessionAsync(sid);

        var removed = await store.RemoveWorktreeAsync(project.Id, wt.Id);
        removed.IsSuccess.Should().BeTrue();

        store.Projects.Single(p => p.Id == project.Id)
            .Sessions.Should().BeEmpty();
    }
}
