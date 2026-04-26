using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using NoScope.CodeScope.Ui.Views;

namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Default <see cref="ISessionViewHostPool"/> implementation. Holds a single
/// <see cref="SessionTabView"/> per session id. WPF's HwndHost reparent-without-destroy
/// behaviour does the rest: when a <see cref="ContentControl"/> drops its current
/// <see cref="SessionTabView"/> as content (because another group's <c>ContentControl</c>
/// just adopted it, or because selection changed), WPF parents the orphaned view under
/// its internal SystemResources hidden window and the inner native HWND survives. As long
/// as someone holds a managed reference (the pool's dictionary) the view isn't disposed
/// and the HwndHost isn't destroyed.
///
/// <para>Single-threaded: WPF UI thread only. The dictionary is mutated from
/// <c>EditorGroupView</c> code-behind handlers and from <c>MainViewModel</c> command paths,
/// all of which run on the dispatcher.</para>
/// </summary>
public sealed class SessionViewHostPool : ISessionViewHostPool
{
    private readonly Dictionary<string, SessionTabView> _views = [];

    public SessionTabView Acquire(string sessionId, Func<SessionTabView> factory)
    {
        ArgumentException.ThrowIfNullOrEmpty(sessionId);
        ArgumentNullException.ThrowIfNull(factory);
        EnsureUiThread();

        if (_views.TryGetValue(sessionId, out var existing))
        {
            // Belt-and-braces: if the view still has a logical parent (e.g. the source
            // group of a cross-group drag didn't get its SelectedTab change in before
            // the target tried to attach), unparent it here so the caller can safely
            // assign it to its own ContentControl.Content without WPF throwing
            // "element already has a logical parent".
            DetachFromParent(existing);
            return existing;
        }
        var created = factory();
        _views[sessionId] = created;
        return created;
    }

    public SessionTabView? TryGet(string sessionId)
        => _views.TryGetValue(sessionId, out var v) ? v : null;

    public void Release(string sessionId)
    {
        if (string.IsNullOrEmpty(sessionId)) { return; }
        EnsureUiThread();
        if (!_views.Remove(sessionId, out var view)) { return; }

        // Detach from any visual parent first so the parent doesn't keep a stale reference.
        // The host is either a ContentControl in an EditorGroupView or WPF's internal
        // SystemResources hidden window (orphaned). For the ContentControl case clearing
        // Content here keeps the WPF visual tree consistent; for the orphaned case there
        // is no logical parent to clear.
        DetachFromParent(view);

        // Run ConPTY teardown explicitly. SessionTabView.Unloaded no longer carries this
        // because Unloaded fires on every reparent — we'd kill the terminal on a drag.
        view.Teardown();
    }

    private static void EnsureUiThread()
    {
        var dispatcher = System.Windows.Application.Current?.Dispatcher;
        if (dispatcher is null) { return; } // unit tests / design-time — accept anything
        if (!dispatcher.CheckAccess())
        {
            throw new InvalidOperationException(
                "SessionViewHostPool must be called on the WPF UI dispatcher; cross-thread mutation " +
                "would race with WPF visual-tree state.");
        }
    }

    private static void DetachFromParent(SessionTabView view)
    {
        switch (view.Parent)
        {
            case ContentControl cc when ReferenceEquals(cc.Content, view):
                cc.Content = null;
                break;
            case ContentPresenter cp when ReferenceEquals(cp.Content, view):
                cp.Content = null;
                break;
            case Decorator dec when ReferenceEquals(dec.Child, view):
                dec.Child = null;
                break;
            case Panel panel when panel.Children.Contains(view):
                panel.Children.Remove(view);
                break;
            case Popup popup when ReferenceEquals(popup.Child, view):
                popup.Child = null;
                break;
            // Other parent types (or null parent) — nothing to do; WPF will GC the view
            // once the pool's dictionary entry is gone.
        }
    }
}
