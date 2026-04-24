using System.Collections.ObjectModel;
using System.Windows;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// UI projection of <see cref="INotificationService"/> — marshals service events onto the
/// dispatcher, exposes an <see cref="ObservableCollection{T}"/> for the status-bar popover,
/// and tracks open/closed state for the bell cluster.
///
/// Lives as a property of <see cref="MainViewModel"/> so the status-bar XAML can bind via
/// <c>Notifications.*</c>.
/// </summary>
public sealed partial class NotificationsViewModel : ObservableObject
{
    private readonly INotificationService _service;

    public NotificationsViewModel(INotificationService service)
    {
        _service = service;
        Entries = [];
        _service.Changed += OnServiceChanged;
        RefreshFromService();
    }

    public ObservableCollection<NotificationEntry> Entries { get; }

    /// <summary>Unread count — drives the 4 px blue dot over the bell glyph (spec §11).</summary>
    [ObservableProperty]
    private int _unreadCount;

    public bool HasUnread => UnreadCount > 0;
    public bool HasAny => Entries.Count > 0;

    partial void OnUnreadCountChanged(int value) => OnPropertyChanged(nameof(HasUnread));

    /// <summary>Popover visibility — flipped by the bell button + <c>StaysOpen=False</c> dismiss.</summary>
    [ObservableProperty]
    private bool _isOpen;

    public event EventHandler<NotificationEntry>? ActivateRequested;

    private void OnServiceChanged(object? sender, EventArgs e)
    {
        var app = Application.Current;
        if (app?.Dispatcher is { } d && !d.CheckAccess())
        {
            d.BeginInvoke(RefreshFromService);
            return;
        }
        RefreshFromService();
    }

    private void RefreshFromService()
    {
        // Full-replace — the buffer is small (<=50) so diffing isn't worth the complexity.
        Entries.Clear();
        foreach (var e in _service.Entries) { Entries.Add(e); }
        UnreadCount = _service.UnreadCount;
        OnPropertyChanged(nameof(HasAny));
    }

    [RelayCommand]
    private void Toggle() => IsOpen = !IsOpen;

    [RelayCommand]
    private void Open()
    {
        IsOpen = true;
        // Opening the popover is the user acknowledging the queue.
        _service.MarkAllRead();
    }

    [RelayCommand]
    private void ClearAll() => _service.Clear();

    [RelayCommand]
    private void Activate(NotificationEntry? entry)
    {
        if (entry is null) { return; }
        _service.MarkRead(entry.Id);
        ActivateRequested?.Invoke(this, entry);
        IsOpen = false;
    }
}
