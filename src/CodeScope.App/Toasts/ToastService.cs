using System.Collections.ObjectModel;
using System.Windows;
using NoScope.CodeScope.Ui.Services;

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

    public ObservableCollection<ToastItemViewModel> Items { get; } = [];

    public void Show(ToastRequest request)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is null) { return; }
        if (!dispatcher.CheckAccess())
        {
            dispatcher.BeginInvoke(() => Show(request));
            return;
        }

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
        // If everything visible is an error and the user keeps stacking errors, we let
        // the stack grow past MaxVisible so the user sees them all rather than silently
        // dropping the oldest critical message on the floor.
        while (Items.Count(i => i.Severity != ToastSeverity.Err) > MaxVisible)
        {
            var victim = Items.FirstOrDefault(i => i.Severity != ToastSeverity.Err);
            if (victim is null) { break; }
            victim.StopTimer();
            Items.Remove(victim);
        }
    }

    public void Dismiss(string id)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is null) { return; }
        if (!dispatcher.CheckAccess())
        {
            dispatcher.BeginInvoke(() => Dismiss(id));
            return;
        }

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
