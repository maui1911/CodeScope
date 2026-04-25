using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.App;

/// <summary>
/// Background timer that refreshes each worktree's open-PR state (number + CI rollup).
/// Ticks every 30 s but skips individual worktrees whose previous polls returned identical
/// results, doubling their effective cadence up to a 5-minute cap. Any change resets the
/// back-off. <c>gh</c>/<c>tea</c> are rate-limited, so idle repos shouldn't burn quota.
/// </summary>
public sealed class PullRequestStatusPoller : WorktreePoller<PullRequestInfo>
{
    private readonly IPullRequestService _pullRequests;
    private readonly IGitService _git;

    public PullRequestStatusPoller(
        ISessionStore store,
        IPullRequestService pullRequests,
        IGitService git,
        ILogger<PullRequestStatusPoller> logger)
        : base(store, logger)
    {
        _pullRequests = pullRequests;
        _git = git;
    }

    protected override TimeSpan Cadence => TimeSpan.FromSeconds(30);
    // Longer initial delay than the status poller — gh auth probe is slow on cold start.
    protected override TimeSpan InitialDelay => TimeSpan.FromSeconds(2);
    protected override int MaxSkipTicks => 9; // 30 s tick * (MaxSkipTicks + 1) = 5 min cap

    protected override async Task ProbeAsync(Project project, Worktree worktree, PollBackoff<PullRequestInfo> state, CancellationToken ct)
    {
        // Branch on the persisted worktree; the status poller keeps it synced with reality.
        var branch = worktree.Branch;
        if (string.IsNullOrWhiteSpace(branch))
        {
            var current = await _git.GetCurrentBranchAsync(worktree.Path, ct).ConfigureAwait(false);
            branch = current.IsSuccess ? current.Value : null;
        }
        if (string.IsNullOrWhiteSpace(branch))
        {
            Store.UpdateWorktreePullRequest(project.Id, worktree.Id, null);
            state.Observe(null, MaxSkipTicks);
            return;
        }

        try
        {
            var result = await _pullRequests.GetOpenPrForBranchAsync(worktree.Path, branch, ct).ConfigureAwait(false);
            if (result.IsSuccess)
            {
                Store.UpdateWorktreePullRequest(project.Id, worktree.Id, result.Value);
                state.Observe(result.Value, MaxSkipTicks);
            }
            else
            {
                // Failures here are very noisy when gh/tea aren't installed — keep at debug.
                Logger.LogDebug("PR poll failed for {Path}: {Error}", worktree.Path, result.Error);
                // Treat errors like "no change" so we don't hammer a broken gh config.
                state.Observe(state.Last, MaxSkipTicks);
            }
        }
        catch (Exception ex)
        {
            Logger.LogDebug(ex, "PR poll threw for {Path}", worktree.Path);
            state.Observe(state.Last, MaxSkipTicks);
        }
    }
}
