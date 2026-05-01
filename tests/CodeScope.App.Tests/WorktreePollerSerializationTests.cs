using System.Threading.Channels;
using Microsoft.Extensions.Logging.Abstractions;
using NoScope.CodeScope.App.Polling;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.App.Tests;

/// <summary>
/// Verifies that <see cref="WorktreePoller{TState}.RefreshAsync"/> and the background
/// timer's <c>PollAllAsync</c> are serialised — concurrent F5 + tick must not run two
/// probes for the same worktree, nor race the <c>TicksUntilNextPoll</c> read/write.
/// Issue #39.
/// </summary>
public sealed class WorktreePollerSerializationTests
{
    [Fact]
    public async Task ConcurrentRefresh_DoesNotInterleaveProbes()
    {
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([Project("p1", Worktree("a"))]);

        var poller = new GatedProbePoller(store);

        var first = poller.RefreshAsync();
        await poller.WaitForProbeStart();

        var second = poller.RefreshAsync();

        // Give the second refresh a window to mistakenly enter the probe.
        await Task.Delay(50);

        poller.MaxConcurrentProbes.Should().Be(1);
        second.IsCompleted.Should().BeFalse("second refresh must wait for the first to release the gate");

        poller.ReleaseProbe();
        await first;

        await poller.WaitForProbeStart();
        poller.ReleaseProbe();
        await second;

        poller.MaxConcurrentProbes.Should().Be(1);
        poller.ProbeCount.Should().Be(2);
    }

    [Fact]
    public async Task RefreshDuringInflightTimerTick_WaitsForCompletion()
    {
        // Drives the gate with an unblocked first refresh that simulates a timer tick
        // already in flight, then asserts the second refresh blocks until it releases.
        var store = Substitute.For<ISessionStore>();
        store.Projects.Returns([Project("p1", Worktree("a"), Worktree("b"))]);

        var poller = new GatedProbePoller(store);

        var inflight = poller.RefreshAsync();
        await poller.WaitForProbeStart();

        var refresh = poller.RefreshAsync();
        await Task.Delay(50);
        refresh.IsCompleted.Should().BeFalse();

        // Drain the in-flight probes (two worktrees → two probe entries).
        poller.ReleaseProbe();
        await poller.WaitForProbeStart();
        poller.ReleaseProbe();
        await inflight;

        // Second refresh now drains.
        await poller.WaitForProbeStart();
        poller.ReleaseProbe();
        await poller.WaitForProbeStart();
        poller.ReleaseProbe();
        await refresh;

        poller.MaxConcurrentProbes.Should().Be(1);
    }

    private static Project Project(string id, params Worktree[] worktrees) =>
        new() { Id = id, Name = id, Path = $@"C:\repo\{id}", Worktrees = worktrees };

    private static Worktree Worktree(string id) =>
        new() { Id = id, Path = $@"C:\repo\{id}", IsPrimary = false };

    private sealed class GatedProbePoller(ISessionStore store)
        : WorktreePoller<string>(store, NullLogger<GatedProbePoller>.Instance)
    {
        private readonly SemaphoreSlim _probeStarted = new(0, int.MaxValue);
        private readonly Channel<TaskCompletionSource> _releases =
            Channel.CreateUnbounded<TaskCompletionSource>();
        private int _concurrent;

        public int ProbeCount;
        public int MaxConcurrentProbes;

        protected override TimeSpan Cadence => TimeSpan.FromSeconds(1);
        protected override TimeSpan InitialDelay => TimeSpan.Zero;
        protected override int MaxSkipTicks => 5;

        protected override ValueTask<bool> TryAcceptWorktreeAsync(Project project, Worktree worktree, CancellationToken ct)
            => ValueTask.FromResult(true);

        protected override async Task ProbeAsync(Project project, Worktree worktree, PollBackoff<string> state, CancellationToken ct)
        {
            var current = Interlocked.Increment(ref _concurrent);
            InterlockedExtensions.Max(ref MaxConcurrentProbes, current);
            Interlocked.Increment(ref ProbeCount);

            var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            await _releases.Writer.WriteAsync(release, ct).ConfigureAwait(false);
            _probeStarted.Release();

            try { await release.Task.ConfigureAwait(false); }
            finally { Interlocked.Decrement(ref _concurrent); }
        }

        public async Task WaitForProbeStart()
        {
            var ok = await _probeStarted.WaitAsync(TimeSpan.FromSeconds(2)).ConfigureAwait(false);
            if (!ok) { throw new TimeoutException("Probe did not start within 2s."); }
        }

        public void ReleaseProbe()
        {
            if (!_releases.Reader.TryRead(out var tcs))
            {
                throw new InvalidOperationException("No probe is waiting to be released.");
            }
            tcs.SetResult();
        }
    }

    private static class InterlockedExtensions
    {
        public static void Max(ref int target, int value)
        {
            int snapshot;
            do
            {
                snapshot = Volatile.Read(ref target);
                if (value <= snapshot) { return; }
            }
            while (Interlocked.CompareExchange(ref target, value, snapshot) != snapshot);
        }
    }
}
