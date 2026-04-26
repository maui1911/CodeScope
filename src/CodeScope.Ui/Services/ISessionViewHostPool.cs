using NoScope.CodeScope.Ui.Views;

namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Owns the lifecycle of <see cref="SessionTabView"/> instances keyed by session id.
///
/// <para>The pool exists so a single <see cref="SessionTabView"/> instance — and therefore the
/// inner <c>EasyTerminalControl</c> <c>HwndHost</c>, the inner <c>Microsoft.Terminal.Wpf</c>
/// renderer HWND, and the ConPTY child process and scrollback buffer underneath — survives
/// being moved between WPF parents (e.g. dragging a tab from one editor group to another).</para>
///
/// <para>Without the pool, each <see cref="Views.EditorGroupView"/> instantiates its own
/// <see cref="SessionTabView"/> from a <c>DataTemplate</c>; moving the VM between groups
/// destroys the source-group <c>ContentPresenter</c>, which unloads the view, which destroys
/// the HwndHost, which kills the ConPTY child. With the pool, the same view is reparented
/// instead — WPF preserves the underlying HWND across reparent
/// (<see href="https://learn.microsoft.com/en-us/dotnet/desktop/wpf/advanced/hosting-win32-content-in-wpf"/>).</para>
///
/// <para>Pool releases (close / restart / cascade) explicitly tear the ConPTY down via
/// <see cref="SessionTabView.Teardown"/> — the view itself no longer hooks <c>Unloaded</c>
/// because <c>Unloaded</c> fires on every reparent.</para>
/// </summary>
public interface ISessionViewHostPool
{
    /// <summary>
    /// Returns the <see cref="SessionTabView"/> for <paramref name="sessionId"/>, materialising
    /// it via <paramref name="factory"/> on first request. Subsequent calls for the same id
    /// return the same instance.
    /// </summary>
    SessionTabView Acquire(string sessionId, Func<SessionTabView> factory);

    /// <summary>Returns the cached view if present, else null. Does not create.</summary>
    SessionTabView? TryGet(string sessionId);

    /// <summary>
    /// Detaches the view from any visual parent, runs ConPTY teardown via
    /// <see cref="SessionTabView.Teardown"/>, and removes the entry from the pool. Idempotent.
    /// </summary>
    void Release(string sessionId);
}
