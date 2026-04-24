using System.Collections.Concurrent;
using System.IO;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.App;

/// <summary>
/// Background timer that refreshes each worktree's open-PR state (number + CI rollup).
/// Ticks every 30 s but skips individual worktrees whose previous polls returned identical
/// results, doubling their effective cadence up to a 5-minute cap. Any change resets the
/// back-off. <c>gh</c>/<c>tea</c> are rate-limited, so idle repos shouldn't burn quota.
/// </summary>
public sealed class PullRequestStatusPoller : BackgroundService
{
    private static readonly TimeSpan Cadence = TimeSpan.FromSeconds(30);
    private const int MaxSkipTicks = 9; // 30 s tick * (MaxSkipTicks + 1) = 5 min cap

    /// <summary>Per-worktree poll state; keyed by worktree id.</summary>
    private readonly ConcurrentDictionary<string, PollBackoff<PullRequestInfo>> _states = new();

    private readonly ISessionStore _store;
    private readonly IPullRequestService _pullRequests;
    private readonly IGitService _git;
    private readonly ILogger<PullRequestStatusPoller> _logger;

    public PullRequestStatusPoller(
        ISessionStore store,
        IPullRequestService pullRequests,
        IGitService git,
        ILogger<PullRequestStatusPoller> logger)
    {
        _store = store;
        _pullRequests = pullRequests;
        _git = git;
        _logger = logger;
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        // Longer initial delay than the status poller — gh auth probe is slow on cold start.
        try { await Task.Delay(TimeSpan.FromSeconds(2), stoppingToken).ConfigureAwait(false); }
        catch (OperationCanceledException) { return; }

        using var timer = new PeriodicTimer(Cadence);
        try
        {
            do
            {
                await PollAllAsync(stoppingToken).ConfigureAwait(false);
            }
            while (await timer.WaitForNextTickAsync(stoppingToken).ConfigureAwait(false));
        }
        catch (OperationCanceledException)
        {
            return; // expected on stop
        }
    }

    /// <summary>
    /// Forces an immediate poll of every worktree, bypassing the per-worktree back-off.
    /// Safe to call from the UI thread — work happens on the caller's continuation.
    /// </summary>
    public Task RefreshAsync(CancellationToken ct = default)
    {
        foreach (var state in _states.Values) { state.TicksUntilNextPoll = 0; }
        return PollAllAsync(ct);
    }

    private async Task PollAllAsync(CancellationToken ct)
    {
        var snapshot = _store.Projects;

        foreach (var project in snapshot)
        {
            foreach (var worktree in project.Worktrees)
            {
                if (ct.IsCancellationRequested) { return; }
                if (string.IsNullOrWhiteSpace(worktree.Path) || !Directory.Exists(worktree.Path))
                {
                    continue;
                }

                var state = _states.GetOrAdd(worktree.Id, _ => new PollBackoff<PullRequestInfo>());
                if (state.TicksUntilNextPoll > 0)
                {
                    state.TicksUntilNextPoll--;
                    continue;
                }

                // Branch on the persisted worktree; Phase 4's status poller keeps it synced with reality.
                var branch = worktree.Branch;
                if (string.IsNullOrWhiteSpace(branch))
                {
                    var current = await _git.GetCurrentBranchAsync(worktree.Path, ct).ConfigureAwait(false);
                    branch = current.IsSuccess ? current.Value : null;
                }
                if (string.IsNullOrWhiteSpace(branch))
                {
                    _store.UpdateWorktreePullRequest(project.Id, worktree.Id, null);
                    state.Observe(null, MaxSkipTicks);
                    continue;
                }

                try
                {
                    var result = await _pullRequests.GetOpenPrForBranchAsync(worktree.Path, branch, ct).ConfigureAwait(false);
                    if (result.IsSuccess)
                    {
                        _store.UpdateWorktreePullRequest(project.Id, worktree.Id, result.Value);
                        state.Observe(result.Value, MaxSkipTicks);
                    }
                    else
                    {
                        // Failures here are very noisy when gh/tea aren't installed — keep at debug.
                        _logger.LogDebug("PR poll failed for {Path}: {Error}", worktree.Path, result.Error);
                        // Treat errors like "no change" so we don't hammer a broken gh config.
                        state.Observe(state.Last, MaxSkipTicks);
                    }
                }
                catch (Exception ex)
                {
                    _logger.LogDebug(ex, "PR poll threw for {Path}", worktree.Path);
                    state.Observe(state.Last, MaxSkipTicks);
                }
            }
        }
    }

}
