using Microsoft.Extensions.Logging.Abstractions;
using NoScope.CodeScope.App.Polling;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.App.Tests;

/// <summary>
/// Verifies that <see cref="WorktreePoller{TState}.States"/> evicts entries for worktrees
/// and projects that have been removed between ticks. Without this reconciliation, every
/// poller (notably <c>PullRequestStatusPoller</c>) accumulated stale <c>PollBackoff</c>
/// entries for the lifetime of the process — see issue #29.
/// </summary>
public sealed class WorktreePollerStateReconciliationTests
{
    [Fact]
    public async Task Refresh_DropsStateForRemovedWorktrees()
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([Project("p1", Worktree("a"), Worktree("b"))]);

        var poller = new TestPoller(store);

        await poller.RefreshAsync();
        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a", "b"]);

        // Simulate a worktree removal mid-process: the store now reports only "a".
        store.Projects.Returns([Project("p1", Worktree("a"))]);

        await poller.RefreshAsync();
        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a"]);
    }

    [Fact]
    public async Task Refresh_DropsStateForRemovedProjects()
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([
            Project("p1", Worktree("a")),
            Project("p2", Worktree("b"), Worktree("c")),
        ]);

        var poller = new TestPoller(store);

        await poller.RefreshAsync();
        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a", "b", "c"]);

        // Drop p2 entirely — both b and c should be reclaimed.
        store.Projects.Returns([Project("p1", Worktree("a"))]);

        await poller.RefreshAsync();
        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a"]);
    }

    [Fact]
    public async Task Refresh_KeepsStateWhenNothingRemoved()
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([Project("p1", Worktree("a"), Worktree("b"))]);

        var poller = new TestPoller(store);

        await poller.RefreshAsync();
        await poller.RefreshAsync();

        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a", "b"]);
    }

    [Fact]
    public async Task Refresh_DropsStaleEntryWhenSwappedWithNew()
    {
        // Regression: remove "b" and add "c" between ticks — States.Count stays at 2
        // so a count-based guard would skip reconciliation entirely.
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([Project("p1", Worktree("a"), Worktree("b"))]);

        var poller = new TestPoller(store);

        await poller.RefreshAsync();
        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a", "b"]);

        store.Projects.Returns([Project("p1", Worktree("a"), Worktree("c"))]);

        await poller.RefreshAsync();
        poller.StateKeysSnapshot().Should().BeEquivalentTo(["a", "c"]);
    }

    private static Project Project(string id, params Worktree[] worktrees) =>
        new() { Id = id, Name = id, Path = $@"C:\repo\{id}", Worktrees = worktrees };

    private static Worktree Worktree(string id) =>
        new() { Id = id, Path = $@"C:\repo\{id}", IsPrimary = false };

    private sealed class TestPoller(ISessionStore store)
        : WorktreePoller<string>(store, NullLogger<TestPoller>.Instance)
    {
        protected override TimeSpan Cadence => TimeSpan.FromSeconds(1);
        protected override TimeSpan InitialDelay => TimeSpan.Zero;
        protected override int MaxSkipTicks => 5;

        // Skip the on-disk path check so tests don't need real worktree directories;
        // reconciliation behavior is independent of probe outcomes anyway.
        protected override ValueTask<bool> TryAcceptWorktreeAsync(Project project, Worktree worktree, CancellationToken ct)
            => ValueTask.FromResult(true);

        protected override Task ProbeAsync(Project project, Worktree worktree, PollBackoff<string> state, CancellationToken ct)
        {
            // GetOrAdd in the base PollAllAsync has already added the entry — nothing else to do.
            return Task.CompletedTask;
        }

        public IReadOnlyCollection<string> StateKeysSnapshot() => States.Keys.ToArray();
    }
}
