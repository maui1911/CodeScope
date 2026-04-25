using System.Collections.ObjectModel;
using System.Windows.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.App.Toasts;

/// <summary>
/// One row in <see cref="ToastService"/>'s observable collection. Owns the auto-dismiss
/// <see cref="DispatcherTimer"/> and the meter progress (0..1, drives the 2px strip
/// at the bottom of the toast). Hover pauses the timer for the entire stack — the
/// host wires <see cref="Pause"/> / <see cref="Resume"/> from a single MouseEnter /
/// MouseLeave on the popup, so this VM doesn't need to know about its siblings.
/// </summary>
public sealed partial class ToastItemViewModel : ObservableObject
{
    private readonly Action<ToastItemViewModel> _onDismiss;
    private readonly DispatcherTimer? _ticker;
    private readonly TimeSpan _duration;
    // Mutable start anchor: shifts forward by the paused span on each Resume so OnTick
    // can keep the simple `elapsed = now - start` formula without tracking a separate
    // accumulator. Pausing 2s on a 4s toast → start moves +2s → meter resumes at the
    // exact same Progress it paused at.
    private DateTime _start;
    private DateTime _pausedAt;
    private bool _paused;

    public ToastItemViewModel(
        ToastRequest request,
        TimeSpan duration,
        Action<ToastItemViewModel> onDismiss)
    {
        _onDismiss = onDismiss;
        _duration = duration;
        _start = DateTime.UtcNow;

        Id = request.Id ?? Guid.NewGuid().ToString("N");
        Severity = request.Severity;
        Title = request.Title;
        Message = request.Message;
        Actions = new ObservableCollection<ToastActionViewModel>(
            (request.Actions ?? []).Select(a => new ToastActionViewModel(a, this)));
        Progress = 1.0;

        // Persistent toasts (errors by default) draw no meter and don't tick.
        if (duration == Timeout.InfiniteTimeSpan || duration == TimeSpan.Zero)
        {
            HasMeter = false;
            Progress = 1.0;
            return;
        }

        HasMeter = true;
        // 60ms ≈ 16fps — smooth enough for a 4–8s drain without burning the dispatcher.
        _ticker = new DispatcherTimer(DispatcherPriority.Background) { Interval = TimeSpan.FromMilliseconds(60) };
        _ticker.Tick += OnTick;
        _ticker.Start();
    }

    public string Id { get; }
    public ToastSeverity Severity { get; }
    public string Title { get; }
    public string? Message { get; }
    public ObservableCollection<ToastActionViewModel> Actions { get; }

    /// <summary>
    /// 1.0 → 0.0 over <see cref="_duration"/>. The meter strip uses this via a width
    /// converter so the bar drains right-to-left in unison with the auto-dismiss.
    /// </summary>
    [ObservableProperty]
    private double _progress;

    public bool HasMeter { get; }
    public bool HasMessage => !string.IsNullOrEmpty(Message);
    public bool HasActions => Actions.Count > 0;

    private void OnTick(object? sender, EventArgs e)
    {
        if (_paused) { return; }
        var elapsed = DateTime.UtcNow - _start;
        var p = 1.0 - (elapsed.TotalMilliseconds / _duration.TotalMilliseconds);
        if (p <= 0)
        {
            Progress = 0;
            DismissCommand.Execute(null);
            return;
        }
        Progress = p;
    }

    public void Pause()
    {
        if (!HasMeter || _paused) { return; }
        _paused = true;
        _pausedAt = DateTime.UtcNow;
    }

    public void Resume()
    {
        if (!HasMeter || !_paused) { return; }
        // Shift start forward by however long we were paused — the elapsed-since-start
        // formula in OnTick then naturally resumes at the exact pre-pause Progress.
        _start = _start + (DateTime.UtcNow - _pausedAt);
        _paused = false;
    }

    /// <summary>
    /// Stops the timer without invoking <see cref="DismissCommand"/>. Used by
    /// <see cref="ToastService.Dismiss(string)"/> and the cap eviction so externally-
    /// removed toasts don't keep ticking and re-firing dismissal on a detached VM.
    /// </summary>
    internal void StopTimer() => _ticker?.Stop();

    [RelayCommand]
    private void Dismiss()
    {
        _ticker?.Stop();
        _onDismiss(this);
    }
}

/// <summary>
/// Per-action wrapper so the toast template can bind to a button command. Click
/// invokes the user-supplied <see cref="ToastAction.Invoke"/> handler then dismisses
/// the toast — actions are noun-verbs of the toast (spec §09 "Actions are noun-verbs"),
/// they're never neutral "OK" so leaving the toast up after a click would be redundant.
/// </summary>
public sealed partial class ToastActionViewModel : ObservableObject
{
    private readonly ToastAction _model;
    private readonly ToastItemViewModel _owner;

    public ToastActionViewModel(ToastAction model, ToastItemViewModel owner)
    {
        _model = model;
        _owner = owner;
    }

    public string Label => _model.Label;
    public bool IsPrimary => _model.IsPrimary;
    /// <summary>Forwarded so the action button's accent can follow the toast severity.</summary>
    public ToastSeverity Severity => _owner.Severity;

    [RelayCommand]
    private void Invoke()
    {
        try { _model.Invoke(); }
        finally { _owner.DismissCommand.Execute(null); }
    }
}
