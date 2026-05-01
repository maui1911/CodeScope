using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.App.Diagnostics;

/// <summary>
/// Dev-mode-only memory watchdog. Logs <see cref="Process.WorkingSet64"/> every cadence
/// and surfaces a warning when the working set has grown by a non-trivial amount since
/// the last log. Helps catch retention regressions during long dev sessions where
/// per-session terminal scrollback (the dominant working-set contributor at scale, see
/// issue #35) could otherwise creep up unnoticed.
/// <para>
/// Only registered when <c>CODESCOPE_DEV=1</c> at process start — the cadence is loose
/// enough that it'd be harmless in production, but there's no value in surfacing internal
/// memory chatter to end users.
/// </para>
/// <para>
/// Why not also cap scrollback per session: <c>Microsoft.Terminal.Wpf</c> 1.22 doesn't
/// expose the underlying renderer's history-line cap on its public API
/// (<c>ITerminalConnection</c> / <c>TerminalContainer</c>). Capping would require either
/// a fork that surfaces the upstream <c>HwndTerminal</c> setting or a periodic synthetic
/// "clear scrollback" sequence written into the pty — both are out of scope for a
/// telemetry tweak. This watchdog is the observability half; the cap is parked until
/// upstream surfaces an API.
/// </para>
/// </summary>
public sealed class MemoryWatchdog(ILogger<MemoryWatchdog> logger, ISessionStore store) : BackgroundService
{
    /// <summary>
    /// Working-set growth threshold beyond which the periodic log line gets bumped from
    /// Trace to Warning. 50 MB chosen as roughly the cost of one fat ConPTY scrollback
    /// at maximum default size — anything larger per cadence is worth surfacing.
    /// </summary>
    private const long GrowthThresholdBytes = 50L * 1024 * 1024;

    private static readonly TimeSpan Cadence = TimeSpan.FromMinutes(5);
    private static readonly TimeSpan InitialDelay = TimeSpan.FromMinutes(1);

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        try { await Task.Delay(InitialDelay, stoppingToken).ConfigureAwait(false); }
        catch (OperationCanceledException) { return; }

        var lastWorkingSet = 0L;
        using var timer = new PeriodicTimer(Cadence);
        try
        {
            do
            {
                Snapshot(ref lastWorkingSet);
            }
            while (await timer.WaitForNextTickAsync(stoppingToken).ConfigureAwait(false));
        }
        catch (OperationCanceledException) { /* shutdown */ }
    }

    private void Snapshot(ref long lastWorkingSet)
    {
        try
        {
            // Environment.WorkingSet returns the same value as Process.WorkingSet64 without
            // allocating a Process instance (which holds an unmanaged handle that would leak
            // every tick if not disposed).
            var ws = Environment.WorkingSet;
            var sessionCount = store.Projects.Sum(p => p.Sessions.Count(s => s.ClosedAt is null));
            var deltaBytes = lastWorkingSet == 0 ? 0 : ws - lastWorkingSet;
            var wsMb = ws / (1024.0 * 1024.0);
            var deltaMb = deltaBytes / (1024.0 * 1024.0);

            if (lastWorkingSet > 0 && deltaBytes >= GrowthThresholdBytes)
            {
                logger.LogWarning(
                    "MemoryWatchdog: working set grew {DeltaMb:F0} MB since last tick (now {WsMb:F0} MB across {Sessions} live session(s))",
                    deltaMb, wsMb, sessionCount);
            }
            else
            {
                logger.LogTrace(
                    "MemoryWatchdog: working set {WsMb:F0} MB, delta {DeltaMb:+0;-0;0} MB, {Sessions} live session(s)",
                    wsMb, deltaMb, sessionCount);
            }

            lastWorkingSet = ws;
        }
        catch (Exception ex)
        {
            logger.LogTrace(ex, "MemoryWatchdog: snapshot failed");
        }
    }
}
