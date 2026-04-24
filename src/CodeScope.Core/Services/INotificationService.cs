namespace NoScope.CodeScope.Core.Services;

/// <summary>Semantic class of a notification — drives glyph + tone in the popover.</summary>
public enum NotificationKind
{
    /// <summary>Agent finished a turn after being in Wait/Composing — user can reply.</summary>
    SessionReady = 0,
    /// <summary>Agent paused for a permission prompt (manual-mode tool_use).</summary>
    SessionWaiting = 1,
    /// <summary>Catch-all for future event sources.</summary>
    Generic = 2,
}

/// <summary>
/// One entry in the notifications queue. Immutable — <see cref="INotificationService.MarkRead"/>
/// replaces the record in place rather than mutating.
/// </summary>
public sealed record NotificationEntry(
    string Id,
    string? SessionId,
    string? SessionTitle,
    NotificationKind Kind,
    string Title,
    string Detail,
    DateTimeOffset Timestamp,
    bool IsRead);

/// <summary>
/// In-memory ring buffer of recent agent events, surfaced by the status-bar bell cluster
/// (spec §11 of <c>docs/design/html/CodeScope - Status Bar Spec.html</c>).
/// Persists for the lifetime of the process only — old entries fall off once
/// <see cref="MaxEntries"/> is reached.
/// </summary>
public interface INotificationService
{
    /// <summary>Most-recent-first snapshot. Safe to enumerate off the UI thread.</summary>
    IReadOnlyList<NotificationEntry> Entries { get; }

    int UnreadCount { get; }

    /// <summary>Raised after every mutation (push, mark-read, clear). Fires on the caller's thread.</summary>
    event EventHandler? Changed;

    /// <summary>Ring-buffer cap. New entries evict the oldest when full.</summary>
    int MaxEntries { get; }

    void Push(NotificationEntry entry);

    void MarkAllRead();
    void MarkRead(string id);
    /// <summary>Marks every entry tied to <paramref name="sessionId"/> as read — used when the user focuses that tab.</summary>
    void MarkSessionRead(string sessionId);

    void Clear();
}
