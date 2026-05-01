using System.Collections.ObjectModel;
using System.Windows;
using NoScope.CodeScope.Ui.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.App.Toasts;

/// <summary>
/// WPF implementation of <see cref="IToastService"/>. Owns the
/// <see cref="ObservableCollection{T}"/> the popup binds to. Marshals every mutation
/// onto the dispatcher because <see cref="UpdateService"/> calls in from the threadpool
/// and SidebarViewModel calls in from RelayCommand handlers — both paths need to
/// resolve safely without the caller knowing the threading model.
/// </summary>
public sealed class ToastService : IToastService
{
    /// <summary>Bottom-up, newest-first stack. Cap matches spec §04 "max 3 visible".</summary>
    private const int MaxVisible = 3;

    /// <summary>
    /// Hard cap on visible error toasts. Errors are persistent (no auto-dismiss) so a
    /// recurring poller-driven failure (e.g. <c>gh</c> not on PATH, FS-watcher race) can
    /// stack VMs + their associated commands until the user dismisses each one manually.
    /// Beyond this cap we drop the oldest visible error to keep the visible stack and
    /// command-list bounded — the user still sees the latest <c>MaxVisibleErr</c> errors.
    /// Issue #34. The first defence remains stable <c>Id</c>-dedupe at every poller call
    /// site; this is the safety net for unstable-id stragglers.
    /// </summary>
    private const int MaxVisibleErr = 20;

    public ObservableCollection<ToastItemViewModel> Items { get; } = [];

    public void Show(ToastRequest request)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is not null && !dispatcher.CheckAccess())
        {
            dispatcher.BeginInvoke(() => Show(request));
            return;
        }

        ShowCore(request);
    }

    public void Dismiss(string id)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is not null && !dispatcher.CheckAccess())
        {
            dispatcher.BeginInvoke(() => Dismiss(id));
            return;
        }

        DismissCore(id);
    }

    /// <summary>
    /// Dispatcher-bound mutation of <see cref="Items"/>. Split out from <see cref="Show"/>
    /// so tests can drive the cap/dedupe logic without spinning up a WPF dispatcher.
    /// </summary>
    internal void ShowCore(ToastRequest request)
    {
        var duration = request.Duration ?? DefaultDuration(request.Severity);

        // De-dupe by id — same id within visible lifetime replaces in place. Kills the
        // "4× Saved." stack flicker that's noted as a bug, not a feature in spec §09.
        if (request.Id is { } id)
        {
            var existing = Items.FirstOrDefault(i => i.Id == id);
            if (existing is not null)
            {
                existing.StopTimer();
                Items.Remove(existing);
            }
        }

        var vm = new ToastItemViewModel(request, duration, OnDismiss);
        Items.Add(vm);

        // Cap at MaxVisible BUT errors never auto-fold (spec §04 "errors never fold")
        // — count only non-error toasts against the cap and only evict from that pool.
        while (Items.Count(i => i.Severity != ToastSeverity.Err) > MaxVisible)
        {
            var victim = Items.FirstOrDefault(i => i.Severity != ToastSeverity.Err);
            if (victim is null) { break; }
            victim.StopTimer();
            Items.Remove(victim);
        }

        // Hard cap on persistent error toasts (issue #34) — drop oldest beyond MaxVisibleErr.
        while (Items.Count(i => i.Severity == ToastSeverity.Err) > MaxVisibleErr)
        {
            var victim = Items.FirstOrDefault(i => i.Severity == ToastSeverity.Err);
            if (victim is null) { break; }
            victim.StopTimer();
            Items.Remove(victim);
        }
    }

    internal void DismissCore(string id)
    {
        var match = Items.FirstOrDefault(i => i.Id == id);
        if (match is null) { return; }
        match.StopTimer();
        Items.Remove(match);
    }

    private void OnDismiss(ToastItemViewModel item)
    {
        if (Items.Contains(item)) { Items.Remove(item); }
    }

    /// <summary>
    /// Spec §08 timing table: info/ok 4s · warn 8s · err persistent. Errors are
    /// non-negotiable persistent — failures the user needs to see; meter would imply
    /// "safe to ignore," which is the opposite of what an error means.
    /// </summary>
    private static TimeSpan DefaultDuration(ToastSeverity severity) => severity switch
    {
        ToastSeverity.Info => TimeSpan.FromSeconds(4),
        ToastSeverity.Ok => TimeSpan.FromSeconds(4),
        ToastSeverity.Warn => TimeSpan.FromSeconds(8),
        ToastSeverity.Err => Timeout.InfiniteTimeSpan,
        _ => TimeSpan.FromSeconds(4),
    };
}
