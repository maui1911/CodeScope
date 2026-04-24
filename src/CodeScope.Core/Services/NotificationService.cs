namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Simple thread-safe in-memory implementation of <see cref="INotificationService"/>.
/// One instance per process; registered as a singleton in DI.
/// </summary>
public sealed class NotificationService : INotificationService
{
    private readonly object _lock = new();
    private readonly LinkedList<NotificationEntry> _entries = new();

    public NotificationService(int maxEntries = 50)
    {
        if (maxEntries < 1) { throw new ArgumentOutOfRangeException(nameof(maxEntries)); }
        MaxEntries = maxEntries;
    }

    public int MaxEntries { get; }

    public IReadOnlyList<NotificationEntry> Entries
    {
        get { lock (_lock) { return _entries.ToArray(); } }
    }

    public int UnreadCount
    {
        get { lock (_lock) { return _entries.Count(e => !e.IsRead); } }
    }

    public event EventHandler? Changed;

    public void Push(NotificationEntry entry)
    {
        ArgumentNullException.ThrowIfNull(entry);
        lock (_lock)
        {
            _entries.AddFirst(entry);
            while (_entries.Count > MaxEntries) { _entries.RemoveLast(); }
        }
        Changed?.Invoke(this, EventArgs.Empty);
    }

    public void MarkAllRead()
    {
        var touched = false;
        lock (_lock)
        {
            var node = _entries.First;
            while (node is not null)
            {
                if (!node.Value.IsRead)
                {
                    node.Value = node.Value with { IsRead = true };
                    touched = true;
                }
                node = node.Next;
            }
        }
        if (touched) { Changed?.Invoke(this, EventArgs.Empty); }
    }

    public void MarkRead(string id)
    {
        ArgumentNullException.ThrowIfNull(id);
        var touched = false;
        lock (_lock)
        {
            var node = _entries.First;
            while (node is not null)
            {
                if (node.Value.Id == id && !node.Value.IsRead)
                {
                    node.Value = node.Value with { IsRead = true };
                    touched = true;
                    break;
                }
                node = node.Next;
            }
        }
        if (touched) { Changed?.Invoke(this, EventArgs.Empty); }
    }

    public void MarkSessionRead(string sessionId)
    {
        ArgumentNullException.ThrowIfNull(sessionId);
        var touched = false;
        lock (_lock)
        {
            var node = _entries.First;
            while (node is not null)
            {
                if (node.Value.SessionId == sessionId && !node.Value.IsRead)
                {
                    node.Value = node.Value with { IsRead = true };
                    touched = true;
                }
                node = node.Next;
            }
        }
        if (touched) { Changed?.Invoke(this, EventArgs.Empty); }
    }

    public void Clear()
    {
        var hadAny = false;
        lock (_lock)
        {
            hadAny = _entries.Count > 0;
            _entries.Clear();
        }
        if (hadAny) { Changed?.Invoke(this, EventArgs.Empty); }
    }
}
