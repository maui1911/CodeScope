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
        if (watch.MessageDir is null)
        {
            // Atomic 8-byte write — Register and PollAll both touch this field, so a plain
            // DateTimeOffset? assignment would race (the struct is >8 bytes, not atomic).
            Interlocked.Exchange(ref watch.LastLocateMissAtTicks, DateTimeOffset.UtcNow.UtcTicks);
        }
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
                    var missTicks = Interlocked.Read(ref watch.LastLocateMissAtTicks);
                    if (missTicks != 0 && now.UtcTicks - missTicks < LocateNotFoundTtl.Ticks)
                    {
                        continue;
                    }
                    watch.MessageDir = TryLocateMessageDir(watch.SessionId);
                    if (watch.MessageDir is null)
                    {
                        Interlocked.Exchange(ref watch.LastLocateMissAtTicks, now.UtcTicks);
                        continue;
                    }
                    Interlocked.Exchange(ref watch.LastLocateMissAtTicks, 0);
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
        if (watch.MessageDir is null) { return; }
        DirectoryInfo dirInfo;
        try { dirInfo = new DirectoryInfo(watch.MessageDir); }
        catch (Exception ex)
        {
            _logger.LogTrace(ex, "OpenCode telemetry: dir stat failed for {Path}", watch.MessageDir);
            return;
        }
        if (!dirInfo.Exists) { return; }
        var dirMtime = dirInfo.LastWriteTimeUtc;

        lock (watch.ReadLock)
        {
            try
            {
                // Quiet-tick short-circuit: OpenCode bumps the directory's mtime whenever a new
                // message file is created. If the directory's mtime hasn't moved since our last
                // walk AND we already produced a snapshot, there can't be anything new to parse.
                // Skips the per-tick `EnumerateFiles` + per-file `FileInfo` syscalls on idle
                // sessions (the dominant case). Capture the mtime BEFORE the walk so a file
                // that lands between this check and our enumerate is still picked up next tick.
                if (dirMtime <= watch.LastWalkedDirMtime && watch.Snapshot is not null)
                {
                    return;
                }

                // OpenCode never modifies a message file once written, so each file is parsed at
                // most once. Files are filtered by mtime against a watermark snapshotted at loop
                // entry; same-tick siblings are disambiguated by a tiny set holding only files
                // at the watermark instant. The watermark is advanced AFTER the walk because
                // EnumerateFiles is not mtime-ordered: if we advanced mid-loop, an older sibling
                // returned later in the iteration would be filtered against the freshly-bumped
                // watermark and skipped permanently (its CreatedAt entry never reaching
                // LastUser/LastEntry/TurnCount). No per-message retention — three running
                // aggregates cover the whole snapshot: last-by-CreatedAt overall (activity FSM),
                // last user (turn duration), last assistant-with-usage (tokens + model +
                // lastTurnAt). Issue #31.
                var anyNewParsed = false;
                var entryWatermark = watch.MtimeWatermark;
                var maxMtimeSeen = entryWatermark;
                HashSet<string>? newSeenAtMax = null;
                foreach (var file in Directory.EnumerateFiles(watch.MessageDir, "msg_*.json"))
                {
                    FileInfo info;
                    try { info = new FileInfo(file); }
                    catch (Exception ex)
                    {
                        _logger.LogTrace(ex, "OpenCode telemetry: stat failed for {Path}", file);
                        continue;
                    }
                    if (!info.Exists) { continue; }

                    var lwt = info.LastWriteTimeUtc;
                    if (lwt < entryWatermark) { continue; }
                    if (lwt == entryWatermark && watch.SeenAtWatermark.Contains(file)) { continue; }

                    string content;
                    try { content = File.ReadAllText(file); }
                    catch (IOException) { continue; } // mid-write race — pick it up next poll

                    var entry = OpenCodeMessageParser.ParseContent(content);
                    if (entry is null) { continue; }

                    // Track the post-loop watermark + the set of files exactly at it. Files
                    // strictly between entryWatermark and maxMtimeSeen don't need to be tracked
                    // — next tick will filter them out by the new watermark anyway.
                    if (lwt > maxMtimeSeen)
                    {
                        maxMtimeSeen = lwt;
                        newSeenAtMax = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { file };
                    }
                    else if (lwt == maxMtimeSeen)
                    {
                        (newSeenAtMax ??= new HashSet<string>(StringComparer.OrdinalIgnoreCase)).Add(file);
                    }

                    // Update the three CreatedAt-keyed aggregates. Files can land out of disk-order
                    // (filesystem semantics, not OpenCode's fault), so each candidate is the entry
                    // with the largest CreatedAt for its slice — not just "the last one we saw."
                    var ec = entry.CreatedAt ?? DateTimeOffset.MinValue;
                    if (watch.LastEntry is null || ec > (watch.LastEntry.CreatedAt ?? DateTimeOffset.MinValue))
                    {
                        watch.LastEntry = entry;
                    }
                    if (entry.Role == "user" &&
                        (watch.LastUser is null || ec > (watch.LastUser.CreatedAt ?? DateTimeOffset.MinValue)))
                    {
                        watch.LastUser = entry;
                    }
                    if (entry.HasUsage)
                    {
                        if (watch.LastAssistantWithUsage is null ||
                            ec > (watch.LastAssistantWithUsage.CreatedAt ?? DateTimeOffset.MinValue))
                        {
                            watch.LastAssistantWithUsage = entry;
                        }
                        watch.TurnCount += 1;
                    }
                    anyNewParsed = true;
                }

                // Commit the watermark only after the walk so mid-loop advances can't filter
                // out older siblings returned later by EnumerateFiles.
                if (newSeenAtMax is not null)
                {
                    if (maxMtimeSeen > entryWatermark)
                    {
                        watch.MtimeWatermark = maxMtimeSeen;
                        watch.SeenAtWatermark.Clear();
                    }
                    foreach (var f in newSeenAtMax) { watch.SeenAtWatermark.Add(f); }
                }

                // Record the dir mtime captured before the walk — next tick can short-circuit
                // unless the dir has changed again. Done unconditionally (even when the walk
                // produced nothing new) so a "no-change" tick still updates the bar.
                watch.LastWalkedDirMtime = dirMtime;

                if (watch.LastEntry is null) { return; }
                if (!anyNewParsed && watch.Snapshot is not null) { return; }

                var lastAssistantWithUsage = watch.LastAssistantWithUsage;
                var lastEntry = watch.LastEntry;
                var lastUser = watch.LastUser;

                var contextTokens = lastAssistantWithUsage?.ContextTokens ?? 0;
                var turns = watch.TurnCount;
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

        // UTC ticks of the last failed TryLocateMessageDir attempt; 0 = never missed.
        // Stored as a long so Register and PollAll can write/read atomically via
        // Interlocked operations (DateTimeOffset? is >8 bytes and not torn-read safe).
        public long LastLocateMissAtTicks;

        // mtime-based "have we seen this file" gate — replaces the unbounded SeenFiles HashSet.
        // OpenCode never modifies a written file, so file mtime is stable and a watermark is
        // sufficient. SeenAtWatermark holds only files at exactly the watermark instant
        // (same-tick siblings on coarse-resolution clocks); it resets whenever the watermark
        // advances, so its size is bounded by the number of messages written within one mtime
        // tick — vanishingly small in practice.
        public DateTime MtimeWatermark = DateTime.MinValue;
        public readonly HashSet<string> SeenAtWatermark = new(StringComparer.OrdinalIgnoreCase);

        // Directory mtime captured at the start of the most recent successful Recompute walk.
        // Used to short-circuit subsequent quiet-tick walks when no new file has appeared.
        public DateTime LastWalkedDirMtime = DateTime.MinValue;

        // Running aggregates — replaces the per-file Entries list. Each candidate is the entry
        // with the largest metadata.time.created within its slice (overall / role=user /
        // assistant-with-usage). TurnCount is incremented on every newly-parsed HasUsage entry.
        public OpenCodeMessageEntry? LastEntry;
        public OpenCodeMessageEntry? LastUser;
        public OpenCodeMessageEntry? LastAssistantWithUsage;
        public int TurnCount;

        public ClaudeSessionTelemetry? Snapshot;
        public readonly object ReadLock = new();

        public void Dispose() { }
    }
}
