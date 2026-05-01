using System.Collections.Concurrent;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class OpenCodeTelemetryService : IOpenCodeTelemetryService
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(350);

    /// <summary>
    /// How long to skip the recursive <see cref="TryLocateMessageDir"/> scan after a
    /// "not found" result. Without this, every poll (350 ms) re-runs a recursive
    /// directory walk over the entire opencode data root for sessions whose message dir
    /// hasn't materialised yet — visible CPU on warm sessions. Issue #36.
    /// </summary>
    private static readonly TimeSpan LocateNotFoundTtl = TimeSpan.FromSeconds(2);

    private readonly ILogger<OpenCodeTelemetryService> _logger;
    private readonly string _dataRoot;
    private readonly ConcurrentDictionary<string, Watch> _watches = new();
    private readonly Timer? _pollTimer;
    private readonly object _timerLock = new();
    private bool _timerArmed;
    private FileSystemWatcher? _rootWatcher;

    public OpenCodeTelemetryService(ILogger<OpenCodeTelemetryService> logger)
        : this(logger, DefaultDataRoot(), enablePolling: true) { }

    /// <summary>Test-seam: point at a throwaway opencode data root, no polling unless requested.</summary>
    public OpenCodeTelemetryService(ILogger<OpenCodeTelemetryService> logger, string dataRoot)
        : this(logger, dataRoot, enablePolling: false) { }

    /// <summary>Full test-seam constructor.</summary>
    public OpenCodeTelemetryService(ILogger<OpenCodeTelemetryService> logger, string dataRoot, bool enablePolling)
    {
        _logger = logger;
        _dataRoot = dataRoot;
        if (enablePolling)
        {
            // Start paused — armed on first Register, disarmed on last Unregister. Issue #36.
            _pollTimer = new Timer(_ => PollAll(), null, Timeout.Infinite, Timeout.Infinite);
        }

        try
        {
            Directory.CreateDirectory(_dataRoot);
            _rootWatcher = new FileSystemWatcher(_dataRoot, "msg_*.json")
            {
                IncludeSubdirectories = true,
                NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.Size | NotifyFilters.FileName | NotifyFilters.CreationTime,
                EnableRaisingEvents = true,
            };
            _rootWatcher.Created += OnFileEvent;
            _rootWatcher.Changed += OnFileEvent;
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "OpenCode telemetry: failed to attach root watcher at {Root}", _dataRoot);
        }
    }

    public event EventHandler<ClaudeSessionTelemetry>? Updated;

    public void Register(string sessionId, string workingDirectory)
    {
        if (string.IsNullOrWhiteSpace(sessionId)) { return; }

        Unregister(sessionId);

        var watch = new Watch(sessionId);
        _watches[sessionId] = watch;

        watch.MessageDir = TryLocateMessageDir(sessionId);
        if (watch.MessageDir is null) { watch.LastLocateMissAt = DateTimeOffset.UtcNow; }
        Recompute(watch);

        RefreshTimerArmed();
    }

    public void Unregister(string sessionId)
    {
        if (_watches.TryRemove(sessionId, out var watch)) { watch.Dispose(); }
        RefreshTimerArmed();
    }

    private void RefreshTimerArmed()
    {
        if (_pollTimer is null) { return; }
        lock (_timerLock)
        {
            var shouldBeArmed = !_watches.IsEmpty;
            if (shouldBeArmed == _timerArmed) { return; }
            _pollTimer.Change(
                shouldBeArmed ? PollInterval : Timeout.InfiniteTimeSpan,
                shouldBeArmed ? PollInterval : Timeout.InfiniteTimeSpan);
            _timerArmed = shouldBeArmed;
        }
    }

    internal bool IsPollTimerArmedForTest
    {
        get { lock (_timerLock) { return _timerArmed; } }
    }

    public ClaudeSessionTelemetry? GetSnapshot(string sessionId) =>
        _watches.TryGetValue(sessionId, out var w) ? w.Snapshot : null;

    public void Dispose()
    {
        try { _pollTimer?.Dispose(); }
        catch (Exception ex) { _logger.LogTrace(ex, "OpenCode telemetry: poll timer dispose threw"); }
        try { _rootWatcher?.Dispose(); }
        catch (Exception ex) { _logger.LogTrace(ex, "OpenCode telemetry: root watcher dispose threw"); }
        foreach (var w in _watches.Values) { w.Dispose(); }
        _watches.Clear();
    }

    private void OnFileEvent(object? _, FileSystemEventArgs e)
    {
        // Path layout: <root>/project/<slug>/storage/message/<sessionId>/<file>.json
        // Validate the grandparent dir is `message` so a stray `msg_*.json` outside the
        // canonical layout (manual test file, future extensions) can't accidentally bind
        // to a registered session id and corrupt its telemetry.
        var messageDir = Path.GetDirectoryName(e.FullPath);
        if (string.IsNullOrEmpty(messageDir)) { return; }
        var grandparent = Path.GetFileName(Path.GetDirectoryName(messageDir) ?? string.Empty);
        if (!string.Equals(grandparent, "message", StringComparison.OrdinalIgnoreCase)) { return; }

        var sid = Path.GetFileName(messageDir);
        if (string.IsNullOrEmpty(sid)) { return; }
        if (!_watches.TryGetValue(sid, out var watch)) { return; }
        if (watch.MessageDir is null) { watch.MessageDir = messageDir; }
        Recompute(watch);
    }

    private void PollAll()
    {
        var now = DateTimeOffset.UtcNow;
        foreach (var watch in _watches.Values)
        {
            try
            {
                if (watch.MessageDir is null)
                {
                    // Throttle the recursive scan — the directory only appears once opencode
                    // writes its first message file, and a 350 ms recursive walk over the
                    // entire data root every tick is wasteful. Issue #36.
                    if (watch.LastLocateMissAt is { } missedAt && now - missedAt < LocateNotFoundTtl)
                    {
                        continue;
                    }
                    watch.MessageDir = TryLocateMessageDir(watch.SessionId);
                    if (watch.MessageDir is null)
                    {
                        watch.LastLocateMissAt = now;
                        continue;
                    }
                    watch.LastLocateMissAt = null;
                }
                Recompute(watch);
            }
            catch (Exception ex) { _logger.LogTrace(ex, "OpenCode telemetry poll failed for {Sid}", watch.SessionId); }
        }
    }

    private string? TryLocateMessageDir(string sessionId)
    {
        if (!Directory.Exists(_dataRoot)) { return null; }
        try
        {
            // Match the trailing path segment so we don't pick up a sibling project's directory
            // that happens to embed the session id.
            return Directory.EnumerateDirectories(_dataRoot, sessionId, SearchOption.AllDirectories)
                .FirstOrDefault(d =>
                {
                    var parent = Path.GetFileName(Path.GetDirectoryName(d) ?? string.Empty);
                    return string.Equals(parent, "message", StringComparison.OrdinalIgnoreCase);
                });
        }
        catch (Exception ex)
        {
            _logger.LogTrace(ex, "OpenCode telemetry locate failed for {Sid}", sessionId);
            return null;
        }
    }

    private void Recompute(Watch watch)
    {
        if (watch.MessageDir is null || !Directory.Exists(watch.MessageDir)) { return; }
        lock (watch.ReadLock)
        {
            try
            {
                // OpenCode never modifies a message file once written — files are appended to the
                // directory only. So we only parse files we haven't seen yet, then re-aggregate
                // the in-memory entry list to derive the snapshot.
                foreach (var file in Directory.EnumerateFiles(watch.MessageDir, "msg_*.json"))
                {
                    if (watch.SeenFiles.Contains(file)) { continue; }
                    string content;
                    try { content = File.ReadAllText(file); }
                    catch (IOException) { continue; } // mid-write race — pick it up next poll
                    var entry = OpenCodeMessageParser.ParseContent(content);
                    if (entry is null) { continue; }
                    watch.SeenFiles.Add(file);
                    watch.Entries.Add(entry);
                }

                if (watch.Entries.Count == 0) { return; }

                // Order canonically by metadata.time.created so file-system enumeration order
                // doesn't skew the activity FSM.
                watch.Entries.Sort((a, b) =>
                    Nullable.Compare(a.CreatedAt, b.CreatedAt));

                var lastAssistantWithUsage = watch.Entries.LastOrDefault(e => e.HasUsage);
                var lastEntry = watch.Entries[^1];
                var lastUser = watch.Entries.LastOrDefault(e => e.Role == "user");

                var contextTokens = lastAssistantWithUsage?.ContextTokens ?? 0;
                var turns = watch.Entries.Count(e => e.HasUsage);
                var lastTurnAt = lastAssistantWithUsage?.CompletedAt ?? lastAssistantWithUsage?.CreatedAt;
                TimeSpan? lastDuration = null;
                if (lastAssistantWithUsage is not null && lastUser?.CreatedAt is { } userCreated
                    && lastAssistantWithUsage.CreatedAt is { } assistantCreated
                    && assistantCreated > userCreated)
                {
                    lastDuration = (lastAssistantWithUsage.CompletedAt ?? assistantCreated) - userCreated;
                }

                // Activity FSM:
                //   user is most recent                      → Composing
                //   assistant + pending tool call            → PendingToolUse
                //   assistant + completed (no pending tools) → Idle
                //   assistant + not yet completed            → Composing (streaming)
                ClaudeActivityState activity;
                if (lastEntry.Role == "user")
                {
                    activity = ClaudeActivityState.Composing;
                }
                else if (lastEntry.Role == "assistant" && lastEntry.HasPendingToolCall)
                {
                    activity = ClaudeActivityState.PendingToolUse;
                }
                else if (lastEntry.Role == "assistant" && lastEntry.CompletedAt is not null)
                {
                    activity = ClaudeActivityState.Idle;
                }
                else
                {
                    activity = ClaudeActivityState.Composing;
                }

                var modelId = lastAssistantWithUsage?.ModelId;
                var contextCap = string.IsNullOrEmpty(modelId)
                    ? watch.Snapshot?.ContextWindowTokens ?? 0
                    : Math.Max(ClaudeModelCatalog.GetContextWindow(modelId!), watch.Snapshot?.ContextWindowTokens ?? 0);

                var snap = new ClaudeSessionTelemetry(
                    watch.SessionId, contextTokens, turns, lastTurnAt, lastDuration, activity, modelId, contextCap);

                if (snap.Equals(watch.Snapshot)) { return; }
                watch.Snapshot = snap;
                try { Updated?.Invoke(this, snap); }
                catch (Exception ex) { _logger.LogWarning(ex, "OpenCode telemetry subscriber threw"); }
            }
            catch (Exception ex)
            {
                _logger.LogWarning(ex, "OpenCode telemetry: recompute failed for {Sid}", watch.SessionId);
            }
        }
    }

    private static string DefaultDataRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".local", "share", "opencode");

    private sealed class Watch(string sessionId) : IDisposable
    {
        public string SessionId { get; } = sessionId;
        public string? MessageDir { get; set; }
        public DateTimeOffset? LastLocateMissAt { get; set; }
        public readonly HashSet<string> SeenFiles = new(StringComparer.OrdinalIgnoreCase);
        public readonly List<OpenCodeMessageEntry> Entries = [];
        public ClaudeSessionTelemetry? Snapshot;
        public readonly object ReadLock = new();

        public void Dispose() { }
    }
}
