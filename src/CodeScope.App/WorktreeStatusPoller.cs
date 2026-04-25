using System.Collections.Concurrent;
using System.IO;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.App;

/// <summary>
/// Background timer that walks every worktree in the store and refreshes its
/// <see cref="WorktreeStatus"/>. Ticks every 3 s but skips individual worktrees whose previous
/// polls returned identical results, doubling their effective cadence up to ~48 s. Any change
/// resets the back-off. Non-existent worktree paths are skipped silently.
/// </summary>
public sealed class WorktreeStatusPoller : BackgroundService
{
    private static readonly TimeSpan Cadence = TimeSpan.FromSeconds(3);
    private const int MaxSkipTicks = 15; // 3 s tick * (MaxSkipTicks + 1) ≈ 48 s cap

    /// <summary>
    /// Poll-tick threshold for treating a missing path as "user intentionally deleted
    /// it" rather than a transient filesystem hiccup (network drive going away briefly,
    /// AV moving a file). Two consecutive misses ≈ 6 s before the entry is dropped.
    /// </summary>
    private const int MissingPathTicksBeforePrune = 2;

    private readonly ConcurrentDictionary<string, PollBackoff<WorktreeStatus>> _states = new();
    private readonly ConcurrentDictionary<string, int> _missingTicks = new();

    private readonly ISessionStore _store;
    private readonly IGitService _git;
    private readonly ILogger<WorktreeStatusPoller> _logger;

    public WorktreeStatusPoller(ISessionStore store, IGitService git, ILogger<WorktreeStatusPoller> logger)
    {
        _store = store;
        _git = git;
        _logger = logger;
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        // Initial delay gives the UI + store time to load.
        try { await Task.Delay(TimeSpan.FromMilliseconds(500), stoppingToken).ConfigureAwait(false); }
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
        // Snapshot first so concurrent mutations don't corrupt iteration.
        var snapshot = _store.Projects;

        foreach (var project in snapshot)
        {
            foreach (var worktree in project.Worktrees)
            {
                if (ct.IsCancellationRequested) { return; }
                if (string.IsNullOrWhiteSpace(worktree.Path) || !Directory.Exists(worktree.Path))
                {
                    // Path missing — likely deleted by the user outside the app. Wait
                    // MissingPathTicksBeforePrune consecutive misses before removing
                    // (Directory.Exists is reliable on Windows, but we still want a
                    // small buffer for transient filesystem oddities). Primary worktrees
                    // never auto-prune — their absence means the project itself is broken
                    // and the user has to remove the project.
                    if (worktree.IsPrimary) { continue; }

                    var misses = _missingTicks.AddOrUpdate(worktree.Id, 1, (_, n) => n + 1);
                    if (misses >= MissingPathTicksBeforePrune)
                    {
                        _logger.LogInformation(
                            "Pruning worktree {Worktree} — path {Path} no longer exists ({Misses} misses)",
                            worktree.Id, worktree.Path, misses);
                        try
                        {
                            var pruned = await _store.PruneMissingWorktreeAsync(project.Id, worktree.Id, ct).ConfigureAwait(false);
                            if (pruned.IsFailure)
                            {
                                _logger.LogWarning("Prune failed for {Worktree}: {Error}", worktree.Id, pruned.Error);
                            }
                            else
                            {
                                _missingTicks.TryRemove(worktree.Id, out _);
                                _states.TryRemove(worktree.Id, out _);
                            }
                        }
                        catch (Exception ex)
                        {
                            _logger.LogWarning(ex, "Prune threw for {Worktree}", worktree.Id);
                        }
                    }
                    continue;
                }

                // Reset the miss counter once a poll sees the path again — covers the
                // user re-creating the directory before we decide to prune.
                _missingTicks.TryRemove(worktree.Id, out _);

                var state = _states.GetOrAdd(worktree.Id, _ => new PollBackoff<WorktreeStatus>());
                if (state.TicksUntilNextPoll > 0)
                {
                    state.TicksUntilNextPoll--;
                    continue;
                }

                try
                {
                    var result = await _git.GetStatusAsync(worktree.Path, ct).ConfigureAwait(false);
                    if (result.IsSuccess)
                    {
                        _store.UpdateWorktreeStatus(project.Id, worktree.Id, result.Value);
                        state.Observe(result.Value, MaxSkipTicks);
                    }
                    else
                    {
                        // Errors (detached HEAD, non-repo, etc.) count as "unchanged" — backs off noise.
                        state.Observe(state.Last, MaxSkipTicks);
                    }
                }
                catch (Exception ex)
                {
                    _logger.LogDebug(ex, "Status poll failed for {Path}", worktree.Path);
                    state.Observe(state.Last, MaxSkipTicks);
                }
            }
        }
    }

}
