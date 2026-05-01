using NoScope.CodeScope.Core;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

/// <summary>
/// Verifies the closed-session retention sweep applied on <c>LoadAsync</c> and on every
/// <c>SoftCloseSessionAsync</c>. Issue #33: cap = 100 closed/worktree, TTL = 90 days.
/// Live sessions and out-of-policy worktrees are untouched.
/// </summary>
public sealed class SessionRetentionPolicyTests
{
    private static (SessionStore store, IProjectStore persistence) Make(ProjectsConfig initial)
    {
        var persistence = Substitute.For<IProjectStore>();
        persistence.LoadAsync(Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<ProjectsConfig>.Ok(initial)));
        persistence.SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>())
            .Returns(Task.FromResult(Result<bool>.Ok(true)));
        var git = Substitute.For<IGitService>();
        var store = new SessionStore(persistence, git, NullLogger<SessionStore>.Instance);
        return (store, persistence);
    }

    private static Session ClosedAt(string id, string? worktreeId, DateTimeOffset closedAt) =>
        new() { Id = id, WorktreePath = @"C:\repo", WorktreeId = worktreeId, ClosedAt = closedAt };

    private static Session Live(string id, string? worktreeId = null) =>
        new() { Id = id, WorktreePath = @"C:\repo", WorktreeId = worktreeId, ClosedAt = null };

    [Fact]
    public async Task LoadAsync_Drops_Sessions_Older_Than_MaxAge()
    {
        var now = DateTimeOffset.UtcNow;
        var ancient = now - SessionRetentionPolicy.MaxAge - TimeSpan.FromDays(1);
        var fresh = now - TimeSpan.FromDays(1);

        var cfg = new ProjectsConfig
        {
            Projects = [new Project
            {
                Id = "p", Name = "P", Path = @"C:\p",
                Sessions =
                [
                    ClosedAt("ancient", "w", ancient),
                    ClosedAt("fresh", "w", fresh),
                    Live("live", "w"),
                ],
            }],
        };

        var (store, persistence) = Make(cfg);
        await store.LoadAsync();

        var sessionIds = store.Projects.Single().Sessions.Select(s => s.Id).ToList();
        sessionIds.Should().Contain(["fresh", "live"]);
        sessionIds.Should().NotContain("ancient");
        // Persisted because the retention sweep mutated state.
        await persistence.Received(1).SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>());
    }

    [Fact]
    public async Task LoadAsync_Keeps_Newest_N_When_Cap_Exceeded()
    {
        var now = DateTimeOffset.UtcNow;
        var sessions = new List<Session>();
        for (var i = 0; i < SessionRetentionPolicy.MaxPerWorktree + 25; i++)
        {
            // Older indices = older ClosedAt — i=0 is the oldest.
            sessions.Add(ClosedAt($"s{i:D3}", "w", now - TimeSpan.FromHours(i)));
        }

        var cfg = new ProjectsConfig
        {
            Projects = [new Project { Id = "p", Name = "P", Path = @"C:\p", Sessions = sessions }],
        };

        var (store, _) = Make(cfg);
        await store.LoadAsync();

        var keptIds = store.Projects.Single().Sessions.Select(s => s.Id).ToHashSet();
        keptIds.Count.Should().Be(SessionRetentionPolicy.MaxPerWorktree);
        // Newest 100 ids (s000..s099) survive; the older 25 (s100..s124) drop.
        for (var i = 0; i < SessionRetentionPolicy.MaxPerWorktree; i++)
        {
            keptIds.Should().Contain($"s{i:D3}", "newest sessions must be retained");
        }
    }

    [Fact]
    public async Task LoadAsync_NoMutation_When_Within_Policy_Skips_Save()
    {
        var now = DateTimeOffset.UtcNow;
        var cfg = new ProjectsConfig
        {
            Projects = [new Project
            {
                Id = "p", Name = "P", Path = @"C:\p",
                Sessions =
                [
                    ClosedAt("a", "w", now - TimeSpan.FromDays(1)),
                    ClosedAt("b", "w", now - TimeSpan.FromDays(2)),
                    Live("c", "w"),
                ],
            }],
        };

        var (store, persistence) = Make(cfg);
        await store.LoadAsync();

        // No prune happened → no save call (Load itself doesn't persist).
        await persistence.DidNotReceive().SaveAsync(Arg.Any<ProjectsConfig>(), Arg.Any<CancellationToken>());
    }

    [Fact]
    public async Task SoftCloseSessionAsync_Drops_Oldest_When_Cap_Hit()
    {
        var now = DateTimeOffset.UtcNow;
        // Pre-seed exactly the cap with closed sessions, then add one live one and close it.
        var sessions = new List<Session>();
        for (var i = 0; i < SessionRetentionPolicy.MaxPerWorktree; i++)
        {
            sessions.Add(ClosedAt($"old{i:D3}", "w", now - TimeSpan.FromHours(i + 1)));
        }
        sessions.Add(Live("victim", "w"));

        var cfg = new ProjectsConfig
        {
            Projects = [new Project { Id = "p", Name = "P", Path = @"C:\p", Sessions = sessions }],
        };

        var (store, _) = Make(cfg);
        await store.LoadAsync();

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        // Closing one more pushes count to MaxPerWorktree+1 → oldest drops.
        var result = await store.SoftCloseSessionAsync("victim");
        result.IsSuccess.Should().BeTrue();

        store.Projects.Single().Sessions
            .Count(s => s.ClosedAt is not null && s.WorktreeId == "w")
            .Should().Be(SessionRetentionPolicy.MaxPerWorktree);

        // Oldest (highest index) is the one dropped — was at +99h.
        store.Projects.Single().Sessions.Should().NotContain(s => s.Id == $"old{SessionRetentionPolicy.MaxPerWorktree - 1:D3}");

        // SessionSoftClosed for the victim AND a SessionRemoved for the pruned old row.
        events.OfType<SessionStoreChange.SessionSoftClosed>().Should().ContainSingle(e => e.Session.Id == "victim");
        events.OfType<SessionStoreChange.SessionRemoved>().Should().ContainSingle();
    }

    [Fact]
    public async Task SoftCloseSessionAsync_NoPrune_When_Within_Policy()
    {
        var (store, persistence) = Make(new ProjectsConfig
        {
            Projects = [new Project { Id = "p", Name = "P", Path = @"C:\p" }],
        });
        await store.LoadAsync();
        await store.AddSessionAsync("p", new Session { Id = "s", WorktreeId = "w", WorktreePath = @"C:\p" });
        persistence.ClearReceivedCalls();

        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.SoftCloseSessionAsync("s");
        result.IsSuccess.Should().BeTrue();
        events.OfType<SessionStoreChange.SessionRemoved>().Should().BeEmpty(
            "no retention sweep should fire when the worktree is well below cap");
    }

    [Fact]
    public async Task SoftCloseSessionAsync_Sweep_Stays_Scoped_To_Affected_Project()
    {
        // Regression guard: SoftClose on project A must NOT prune anything from project B,
        // even when both projects are independently over-cap. The sweep is project-scoped
        // for perf (avoid walking unrelated state); confirm the filter is honoured.
        var now = DateTimeOffset.UtcNow;

        // Project A — at cap, with one live session about to be closed (the trigger).
        var sessionsA = new List<Session>();
        for (var i = 0; i < SessionRetentionPolicy.MaxPerWorktree; i++)
        {
            sessionsA.Add(ClosedAt($"a{i:D3}", "wA", now - TimeSpan.FromHours(i + 1)));
        }
        sessionsA.Add(Live("victim-a", "wA"));

        var cfg = new ProjectsConfig
        {
            Projects =
            [
                new Project { Id = "pA", Name = "A", Path = @"C:\a", Sessions = sessionsA },
                new Project { Id = "pB", Name = "B", Path = @"C:\b" },
            ],
        };

        var (store, _) = Make(cfg);
        await store.LoadAsync();

        // After Load, project B is empty. Add an over-cap closed row directly via
        // AddSessionAsync (bypasses the SoftClose path so no migration sweep is triggered).
        await store.AddSessionAsync("pB", new Session
        {
            Id = "b-overflow",
            WorktreeId = "wB",
            WorktreePath = @"C:\b",
            ClosedAt = now,
        });
        store.Projects.Single(p => p.Id == "pB").Sessions.Should().HaveCount(1);

        // SoftClose on project A — A's bucket goes from cap to cap+1, triggering A's prune.
        // The scoped sweep must NOT visit project B even though B has a closed session
        // (which would survive in B anyway, but the contract is that scoped means scoped).
        var events = new List<SessionStoreChange>();
        store.Changed += (_, c) => events.Add(c);

        var result = await store.SoftCloseSessionAsync("victim-a");
        result.IsSuccess.Should().BeTrue();

        store.Projects.Single(p => p.Id == "pA").Sessions
            .Count(s => s.ClosedAt is not null).Should().Be(SessionRetentionPolicy.MaxPerWorktree);
        store.Projects.Single(p => p.Id == "pB").Sessions.Should().ContainSingle(s => s.Id == "b-overflow",
            "scoped sweep must not touch the unrelated project");
    }

    [Fact]
    public async Task Retention_Buckets_Are_PerWorktree_Not_PerProject()
    {
        var now = DateTimeOffset.UtcNow;
        // Worktree A: way over cap. Worktree B: well under cap.
        var sessions = new List<Session>();
        for (var i = 0; i < SessionRetentionPolicy.MaxPerWorktree + 5; i++)
        {
            sessions.Add(ClosedAt($"a{i:D3}", "wA", now - TimeSpan.FromHours(i)));
        }
        for (var i = 0; i < 3; i++)
        {
            sessions.Add(ClosedAt($"b{i:D3}", "wB", now - TimeSpan.FromHours(i)));
        }

        var cfg = new ProjectsConfig
        {
            Projects = [new Project { Id = "p", Name = "P", Path = @"C:\p", Sessions = sessions }],
        };

        var (store, _) = Make(cfg);
        await store.LoadAsync();

        var kept = store.Projects.Single().Sessions;
        kept.Count(s => s.WorktreeId == "wA").Should().Be(SessionRetentionPolicy.MaxPerWorktree);
        kept.Count(s => s.WorktreeId == "wB").Should().Be(3, "wB was nowhere near the cap");
    }
}
