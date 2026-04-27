using System.Windows;
using Microsoft.Extensions.Logging;
using Microsoft.Toolkit.Uwp.Notifications;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.App.Notifications;

/// <summary>
/// Windows Action-Center toast implementation of <see cref="IIdleToastNotifier"/>. Uses
/// the <c>Microsoft.Toolkit.Uwp.Notifications</c> compat layer so the unpackaged WPF host
/// gets a real WinRT toast (visible while the window is minimized) without us hand-rolling
/// AUMID + COM-activator registration.
///
/// <para>The minimize gate lives here, not at the call site, so the activity-FSM hook in
/// <c>MainViewModel</c> stays a one-liner. We also de-dupe per session id within a short
/// window to absorb the FSWatcher-then-poll re-fires that the telemetry layer is known
/// to emit (~100–500 ms apart).</para>
/// </summary>
public sealed class WindowsIdleToastNotifier : IIdleToastNotifier, IDisposable
{
    private const string ArgKey = "agentSessionId";
    private static readonly TimeSpan DedupeWindow = TimeSpan.FromSeconds(2);

    private readonly ILogger<WindowsIdleToastNotifier> _logger;
    private readonly Dictionary<string, DateTimeOffset> _lastFiredBySession = [];
    private readonly Lock _gate = new();
    private bool _activatedHookInstalled;
    private bool _disposed;

    public WindowsIdleToastNotifier(ILogger<WindowsIdleToastNotifier> logger)
    {
        _logger = logger;
        // The compat layer's OnActivated subscription is what triggers AUMID + COM activator
        // registration on first use. Subscribing eagerly so a click that arrives before the
        // first NotifyTurnComplete (unlikely, but possible after a future API addition)
        // still routes correctly.
        TryInstallActivatedHook();
    }

    public event EventHandler<string>? Activated;

    public void NotifyTurnComplete(string agentSessionId, string sessionTitle, string detail)
    {
        if (string.IsNullOrWhiteSpace(agentSessionId)) { return; }
        if (!IsMainWindowMinimized()) { return; }

        // De-dupe: telemetry transitions fire from FSWatcher *and* the poll fallback
        // (~100–500 ms apart) and would otherwise stack two identical toasts. Anything
        // older than the de-dupe window can no longer suppress a future toast, so we
        // prune in-place — the dict stays bounded to "sessions seen in the last 2 s",
        // a handful of entries at most.
        lock (_gate)
        {
            var now = DateTimeOffset.UtcNow;
            var cutoff = now - DedupeWindow;
            foreach (var staleKey in _lastFiredBySession
                         .Where(kv => kv.Value < cutoff)
                         .Select(kv => kv.Key)
                         .ToList())
            {
                _lastFiredBySession.Remove(staleKey);
            }
            if (_lastFiredBySession.ContainsKey(agentSessionId)) { return; }
            _lastFiredBySession[agentSessionId] = now;
        }

        try
        {
            new ToastContentBuilder()
                .AddArgument(ArgKey, agentSessionId)
                .AddText(sessionTitle)
                .AddText(detail)
                .Show(toast =>
                {
                    // ExpiresOnReboot keeps Action Center clean: a click after a reboot wouldn't
                    // reach our (now-dead) COM activator anyway, so the entry would be a dead end.
                    toast.ExpiresOnReboot = true;
                });
        }
        catch (Exception ex)
        {
            // Toast surface is non-essential; never let it bring the app down. Common failures
            // are sandbox / missing Start-menu shortcut on first run before Velopack registered.
            _logger.LogDebug(ex, "Toast Show failed for session {SessionId}", agentSessionId);
        }
    }

    private void TryInstallActivatedHook()
    {
        if (_activatedHookInstalled) { return; }
        try
        {
            ToastNotificationManagerCompat.OnActivated += OnToastActivated;
            _activatedHookInstalled = true;
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "Failed to install toast Activated hook");
        }
    }

    private void OnToastActivated(ToastNotificationActivatedEventArgsCompat e)
    {
        try
        {
            var args = ToastArguments.Parse(e.Argument);
            if (!args.Contains(ArgKey)) { return; }
            var sid = args[ArgKey];
            if (string.IsNullOrEmpty(sid)) { return; }

            // The compat layer fires this on a background thread; marshal back to the WPF
            // dispatcher before raising so subscribers (MainWindow) can touch UI freely.
            var app = Application.Current;
            if (app?.Dispatcher is { } d)
            {
                d.BeginInvoke(() => Activated?.Invoke(this, sid));
            }
            else
            {
                Activated?.Invoke(this, sid);
            }
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "Toast activation parse failed: {Argument}", e.Argument);
        }
    }

    private static bool IsMainWindowMinimized()
    {
        var win = Application.Current?.MainWindow;
        return win is not null && win.WindowState == WindowState.Minimized;
    }

    public void Dispose()
    {
        if (_disposed) { return; }
        _disposed = true;
        if (_activatedHookInstalled)
        {
            try { ToastNotificationManagerCompat.OnActivated -= OnToastActivated; }
            catch (Exception ex) { _logger.LogTrace(ex, "Detach toast hook"); }
        }
    }
}
