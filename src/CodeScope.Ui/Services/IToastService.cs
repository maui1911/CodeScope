namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Severity drives the toast's accent rail, icon, tinted backgrounds and
/// auto-dismiss policy. Mirrors the four flavors in
/// <c>docs/design/html/CodeScope - Errors and Toasts.html</c> (§03).
/// </summary>
public enum ToastSeverity
{
    Info = 0,
    Ok = 1,
    Warn = 2,
    Err = 3,
}

/// <summary>
/// One inline action button on a toast. Maximum two per toast (spec §02 callout 5).
/// Click resolves before the toast auto-dismisses, so an async handler can finish its
/// work before the row disappears.
/// </summary>
public sealed record ToastAction(string Label, Action Invoke, bool IsPrimary = false);

/// <summary>
/// One toast request. <see cref="Id"/> drives de-dupe — re-pushing the same id within
/// the lifetime of a visible toast replaces it in place (spec §04 stack rules).
/// <see cref="Duration"/> overrides the severity default (info/ok 4s · warn 8s ·
/// err persistent); pass <see cref="System.Threading.Timeout.InfiniteTimeSpan"/> for
/// "never auto-dismiss".
/// </summary>
public sealed record ToastRequest(
    ToastSeverity Severity,
    string Title,
    string? Message = null,
    IReadOnlyList<ToastAction>? Actions = null,
    string? Id = null,
    TimeSpan? Duration = null);

/// <summary>
/// App-wide toast surface. Implementation lives in CodeScope.App and is hosted inside
/// a <c>Popup</c> so it gets its own top-level HWND — that's the only way to render
/// above the <c>Microsoft.Terminal.Wpf</c> HwndHost children that fill the workspace.
/// Without the popup, toasts that overlap a terminal silently disappear behind it
/// (classic WPF airspace).
/// </summary>
public interface IToastService
{
    /// <summary>Show a toast (or replace the existing one with the same id).</summary>
    void Show(ToastRequest request);

    /// <summary>Dismiss a toast by id. No-op if no toast with that id is visible.</summary>
    void Dismiss(string id);
}

/// <summary>Convenience helpers so callers don't carry severity in every call.</summary>
public static class ToastServiceExtensions
{
    public static void Info(this IToastService svc, string title, string? message = null, string? id = null)
        => svc.Show(new ToastRequest(ToastSeverity.Info, title, message, Id: id));

    public static void Ok(this IToastService svc, string title, string? message = null, string? id = null)
        => svc.Show(new ToastRequest(ToastSeverity.Ok, title, message, Id: id));

    public static void Warn(this IToastService svc, string title, string? message = null, string? id = null)
        => svc.Show(new ToastRequest(ToastSeverity.Warn, title, message, Id: id));

    public static void Err(this IToastService svc, string title, string? message = null, string? id = null)
        => svc.Show(new ToastRequest(ToastSeverity.Err, title, message, Id: id));
}
