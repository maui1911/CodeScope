using System.Collections.Concurrent;
using System.IO;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.App.Polling;

/// <summary>
/// Base for hosted services that walk every worktree in <see cref="ISessionStore"/> on a
/// fixed cadence, with per-worktree exponential back-off (<see cref="PollBackoff{T}"/>).
/// Subclasses provide the cadence, initial delay, and the per-worktree probe — the timer
/// loop, snapshot iteration, missing-path skip, and back-off bookkeeping live here.
/// </summary>
public abstract class WorktreePoller<TState> : BackgroundService
    where TState : class
{
    protected ConcurrentDictionary<string, PollBackoff<TState>> States { get; } = new();
    protected ISessionStore Store { get; }
    protected ILogger Logger { get; }
    protected abstract TimeSpan Cadence { get; }
    protected abstract TimeSpan InitialDelay { get; }
    protected abstract int MaxSkipTicks { get; }

    protected WorktreePoller(ISessionStore store, ILogger logger)
    {
        Store = store;
        Logger = logger;
    }

    protected sealed override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        try { await Task.Delay(InitialDelay, stoppingToken).ConfigureAwait(false); }
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
        foreach (var state in States.Values) { state.TicksUntilNextPoll = 0; }
        return PollAllAsync(ct);
    }

    /// <summary>
    /// Hook for subclasses to short-circuit the probe (e.g. prune missing paths). Returning
    /// <c>false</c> skips the worktree for this tick; returning <c>true</c> proceeds to
    /// <see cref="ProbeAsync"/>. Default: skip blank/missing paths.
    /// </summary>
    protected virtual ValueTask<bool> TryAcceptWorktreeAsync(Project project, Worktree worktree, CancellationToken ct)
        => ValueTask.FromResult(!string.IsNullOrWhiteSpace(worktree.Path) && Directory.Exists(worktree.Path));

    /// <summary>Per-worktree probe — implemented by each poller's specific data fetch.</summary>
    protected abstract Task ProbeAsync(Project project, Worktree worktree, PollBackoff<TState> state, CancellationToken ct);

    private async Task PollAllAsync(CancellationToken ct)
    {
        var snapshot = Store.Projects;
        foreach (var project in snapshot)
        {
            foreach (var worktree in project.Worktrees)
            {
                if (ct.IsCancellationRequested) { return; }
                if (!await TryAcceptWorktreeAsync(project, worktree, ct).ConfigureAwait(false)) { continue; }

                var state = States.GetOrAdd(worktree.Id, _ => new PollBackoff<TState>());
                if (state.TicksUntilNextPoll > 0)
                {
                    state.TicksUntilNextPoll--;
                    continue;
                }

                await ProbeAsync(project, worktree, state, ct).ConfigureAwait(false);
            }
        }
    }
}
