namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Surfaces a native Windows toast (Action Center) when an agent's turn completes —
/// but only when the main app window is minimized. Click activation routes back via
/// <see cref="Activated"/> with the agent session id so the host can restore the
/// window and focus the matching tab.
/// </summary>
/// <remarks>
/// The implementation owns the gate: callers fire-and-forget on every transition to
/// Idle and the notifier itself decides whether the user actually sees a toast. That
/// keeps the activity-FSM call site (<c>MainViewModel.PushActivityNotification</c>)
/// free of window-state plumbing.
/// </remarks>
public interface IIdleToastNotifier
{
    /// <summary>
    /// Shows a "turn complete" toast for the given session if the host window is
    /// minimized; otherwise no-op. Must be called on the WPF dispatcher — the
    /// implementation reads <c>Application.Current.MainWindow.WindowState</c>, which
    /// is dispatcher-affined. Existing call sites (<c>MainViewModel.PushActivityNotification</c>
    /// → <c>ApplyTelemetry</c>) already marshal onto the dispatcher before invoking.
    /// </summary>
    /// <param name="agentSessionId">
    /// The agent-side session id (Claude UUID etc.) — embedded in the toast as the
    /// activation argument so click-routing can match the right tab.
    /// </param>
    /// <param name="sessionTitle">Display name shown on the toast.</param>
    /// <param name="detail">Body line, e.g. "Turn complete · 14s".</param>
    void NotifyTurnComplete(string agentSessionId, string sessionTitle, string detail);

    /// <summary>
    /// Raised when the user clicks a toast emitted by this notifier. Argument is the
    /// agent session id originally passed to <see cref="NotifyTurnComplete"/>. Marshalled
    /// onto the WPF dispatcher by the implementation.
    /// </summary>
    event EventHandler<string>? Activated;
}
