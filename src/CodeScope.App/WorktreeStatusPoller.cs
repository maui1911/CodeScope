using System.Collections.Concurrent;
using System.IO;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Services;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.App;

/// <summary>
/// Background timer that walks every worktree in the store and refreshes its
/// <see cref="WorktreeStatus"/>. Ticks every 3 s but skips individual worktrees whose previous
/// polls returned identical results, doubling their effective cadence up to ~48 s. Any change
/// resets the back-off. Non-existent worktree paths are pruned after two consecutive misses.
/// </summary>
public sealed class WorktreeStatusPoller : WorktreePoller<WorktreeStatus>
{
    /// <summary>
    /// Poll-tick threshold for treating a missing path as "user intentionally deleted
    /// it" rather than a transient filesystem hiccup (network drive going away briefly,
    /// AV moving a file). Two consecutive misses ≈ 6 s before the entry is dropped.
    /// </summary>
    private const int MissingPathTicksBeforePrune = 2;

    private readonly ConcurrentDictionary<string, int> _missingTicks = new();

    private readonly IGitService _git;
    private readonly IToastService? _toasts;

    public WorktreeStatusPoller(
        ISessionStore store,
        IGitService git,
        ILogger<WorktreeStatusPoller> logger,
        IToastService? toasts = null)
        : base(store, logger)
    {
        _git = git;
        _toasts = toasts;
    }

    protected override TimeSpan Cadence => TimeSpan.FromSeconds(3);
    protected override TimeSpan InitialDelay => TimeSpan.FromMilliseconds(500);
    protected override int MaxSkipTicks => 15; // 3 s tick * (MaxSkipTicks + 1) ≈ 48 s cap

    protected override async ValueTask<bool> TryAcceptWorktreeAsync(Project project, Worktree worktree, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(worktree.Path) || !Directory.Exists(worktree.Path))
        {
            // Path missing — likely deleted by the user outside the app. Wait
            // MissingPathTicksBeforePrune consecutive misses before removing
            // (Directory.Exists is reliable on Windows, but we still want a
            // small buffer for transient filesystem oddities). Primary worktrees
            // never auto-prune — their absence means the project itself is broken
            // and the user has to remove the project.
            if (worktree.IsPrimary) { return false; }

            var misses = _missingTicks.AddOrUpdate(worktree.Id, 1, (_, n) => n + 1);
            if (misses >= MissingPathTicksBeforePrune)
            {
                Logger.LogInformation(
                    "Pruning worktree {Worktree} — path {Path} no longer exists ({Misses} misses)",
                    worktree.Id, worktree.Path, misses);
                try
                {
                    var pruned = await Store.PruneMissingWorktreeAsync(project.Id, worktree.Id, ct).ConfigureAwait(false);
                    if (pruned.IsFailure)
                    {
                        Logger.LogWarning("Prune failed for {Worktree}: {Error}", worktree.Id, pruned.Error);
                    }
                    else
                    {
                        _missingTicks.TryRemove(worktree.Id, out _);
                        States.TryRemove(worktree.Id, out _);
                        // User-visible heads-up that the entry vanished — silent removal
                        // is confusing when the sidebar count just changes by one without
                        // any acknowledgment that "the folder you deleted is also gone here".
                        var branchLabel = worktree.Branch ?? Path.GetFileName(worktree.Path.TrimEnd('\\', '/'));
                        _toasts?.Show(new ToastRequest(
                            ToastSeverity.Warn,
                            "Worktree removed",
                            $"'{branchLabel}' was pruned because its folder no longer exists.",
                            Id: $"prune-{worktree.Id}"));
                    }
                }
                catch (Exception ex)
                {
                    Logger.LogWarning(ex, "Prune threw for {Worktree}", worktree.Id);
                }
            }
            return false;
        }

        // Reset the miss counter once a poll sees the path again — covers the
        // user re-creating the directory before we decide to prune.
        _missingTicks.TryRemove(worktree.Id, out _);
        return true;
    }

    protected override async Task ProbeAsync(Project project, Worktree worktree, PollBackoff<WorktreeStatus> state, CancellationToken ct)
    {
        try
        {
            var result = await _git.GetStatusAsync(worktree.Path, ct).ConfigureAwait(false);
            if (result.IsSuccess)
            {
                Store.UpdateWorktreeStatus(project.Id, worktree.Id, result.Value);
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
            Logger.LogDebug(ex, "Status poll failed for {Path}", worktree.Path);
            state.Observe(state.Last, MaxSkipTicks);
        }
    }
}
