using System.Collections.Concurrent;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class PiTelemetryService : IPiTelemetryService
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(250);

    private readonly ILogger<PiTelemetryService> _logger;
    private readonly string _sessionsRoot;
    private readonly ConcurrentDictionary<string, Watch> _watches = new();
    private readonly Timer? _pollTimer;
    private readonly object _timerLock = new();
    private bool _timerArmed;
    private FileSystemWatcher? _rootWatcher;

    public PiTelemetryService(ILogger<PiTelemetryService> logger)
        : this(logger, DefaultSessionsRoot(), enablePolling: true) { }

    /// <summary>Test-seam: point at a throwaway sessions root, no polling unless requested.</summary>
    public PiTelemetryService(ILogger<PiTelemetryService> logger, string sessionsRoot)
        : this(logger, sessionsRoot, enablePolling: false) { }

    /// <summary>Full test-seam constructor.</summary>
    public PiTelemetryService(ILogger<PiTelemetryService> logger, string sessionsRoot, bool enablePolling)
    {
        _logger = logger;
        _sessionsRoot = sessionsRoot;
        if (enablePolling)
        {
            // Start paused — armed on first Register, disarmed on last Unregister. Issue #36.
            _pollTimer = new Timer(_ => PollAll(), null, Timeout.Infinite, Timeout.Infinite);
        }

        // Single recursive watcher across the whole sessions root: cheaper than one per
        // registration AND covers files that don't exist yet at Register time (fresh launch
        // where pi hasn't written its header line).
        try
        {
            Directory.CreateDirectory(_sessionsRoot);
            _rootWatcher = new FileSystemWatcher(_sessionsRoot, "*.jsonl")
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
            _logger.LogWarning(ex, "Pi telemetry: failed to attach root watcher at {Root}", _sessionsRoot);
        }
    }

    public event EventHandler<ClaudeSessionTelemetry>? Updated;

    public void Register(string sessionId, string workingDirectory)
    {
        if (string.IsNullOrWhiteSpace(sessionId)) { return; }

        Unregister(sessionId);

        // workingDirectory hint is ignored: Pi's cwd→dir encoding isn't reliably recoverable
        // on Windows, so we resolve the file path by scanning for "*_<sessionId>.jsonl" anywhere
        // under the sessions root. When the file doesn't exist yet (fresh launch race), the
        // root watcher will route the Created event back through OnFileEvent → Adopt.
        var watch = new Watch(sessionId);
        _watches[sessionId] = watch;

        var existing = TryLocate(sessionId);
        if (existing is not null)
        {
            watch.FilePath = existing;
            TryRead(watch);
        }

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
        catch (Exception ex) { _logger.LogTrace(ex, "Pi telemetry: poll timer dispose threw"); }
        try { _rootWatcher?.Dispose(); }
        catch (Exception ex) { _logger.LogTrace(ex, "Pi telemetry: root watcher dispose threw"); }
        foreach (var w in _watches.Values) { w.Dispose(); }
        _watches.Clear();
    }

    private void OnFileEvent(object? _, FileSystemEventArgs e)
    {
        // The path matches "*_<sid>.jsonl" — extract the trailing UUID and route to the
        // registered watch (if any).
        var sid = PiTranscriptParser.ExtractSessionIdFromFileName(e.Name ?? string.Empty);
        if (sid is null) { return; }
        if (!_watches.TryGetValue(sid, out var watch)) { return; }
        if (watch.FilePath is null) { watch.FilePath = e.FullPath; }
        TryRead(watch);
    }

    private void PollAll()
    {
        foreach (var watch in _watches.Values)
        {
            try
            {
                if (watch.FilePath is null)
                {
                    var found = TryLocate(watch.SessionId);
                    if (found is null) { continue; }
                    watch.FilePath = found;
                }
                var info = new FileInfo(watch.FilePath);
                if (!info.Exists) { continue; }
                if (info.Length == watch.Offset) { continue; }
                TryRead(watch);
            }
            catch (Exception ex)
            {
                _logger.LogTrace(ex, "Pi telemetry poll failed for {Sid}", watch.SessionId);
            }
        }
    }

    private string? TryLocate(string sessionId)
    {
        // Pi pads session-files as "<timestamp>_<uuid>.jsonl"; the suffix pattern uniquely
        // identifies the file across the entire sessions root (UUIDs are unique).
        if (!Directory.Exists(_sessionsRoot)) { return null; }
        try
        {
            return Directory.EnumerateFiles(_sessionsRoot, $"*_{sessionId}.jsonl",
                SearchOption.AllDirectories).FirstOrDefault();
        }
        catch (Exception ex)
        {
            _logger.LogTrace(ex, "Pi telemetry locate failed for {Sid}", sessionId);
            return null;
        }
    }

    private void TryRead(Watch watch)
    {
        if (watch.FilePath is null) { return; }
        lock (watch.ReadLock)
        {
            if (!File.Exists(watch.FilePath)) { return; }
            try
            {
                using var fs = new FileStream(watch.FilePath, FileMode.Open, FileAccess.Read,
                    FileShare.ReadWrite | FileShare.Delete);
                if (watch.Offset > fs.Length) { watch.Offset = 0; }
                fs.Seek(watch.Offset, SeekOrigin.Begin);
                using var reader = new StreamReader(fs);

                var contextTokens = watch.Snapshot?.ContextTokens ?? 0;
                var turns = watch.Snapshot?.TurnCount ?? 0;
                var lastAt = watch.Snapshot?.LastTurnAt;
                var lastDuration = watch.Snapshot?.LastTurnDuration;
                var activity = watch.Snapshot?.Activity ?? ClaudeActivityState.Unknown;
                var modelId = watch.Snapshot?.ModelId;
                var contextCap = watch.Snapshot?.ContextWindowTokens ?? 0;
                var changed = false;

                while (reader.ReadLine() is { } line)
                {
                    var entry = PiTranscriptParser.ParseLine(line);
                    if (entry is null) { continue; }

                    // Pi has dedicated `model_change` events; latch the model + cap as they arrive.
                    if (entry.Type == "model_change" && !string.IsNullOrWhiteSpace(entry.Model))
                    {
                        modelId = entry.Model;
                        var cap = ClaudeModelCatalog.GetContextWindow(entry.Model);
                        if (cap > 0) { contextCap = cap; }
                        changed = true;
                        continue;
                    }

                    if (entry.Type != "message") { continue; }

                    // Activity FSM:
                    //   user / toolResult        → Composing (agent is now responding)
                    //   assistant + stop         → Idle
                    //   assistant + tool_use     → PendingToolUse
                    if (entry.Role is "user" or "toolResult")
                    {
                        activity = ClaudeActivityState.Composing;
                        // Only a fresh user turn anchors LastUserTurnAt — toolResult marks the
                        // mid-turn return of a tool call, so resetting here would cut the
                        // measured turn-duration short. Mirrors ClaudeTelemetryService's
                        // `!entry.UserCarriesToolResult` guard.
                        if (entry.Role == "user" && entry.Timestamp is { } userTs)
                        {
                            watch.LastUserTurnAt = userTs;
                        }
                        changed = true;
                        continue;
                    }

                    if (entry.Role == "assistant")
                    {
                        activity = entry.StopReason switch
                        {
                            "tool_use" => ClaudeActivityState.PendingToolUse,
                            "stop" => ClaudeActivityState.Idle,
                            "end_turn" => ClaudeActivityState.Idle, // anthropic-style alias
                            _ => activity,
                        };
                        changed = true;
                    }

                    if (!entry.HasUsage) { continue; }

                    if (!string.IsNullOrWhiteSpace(entry.Model) && entry.Model != modelId)
                    {
                        modelId = entry.Model;
                        var detected = ClaudeModelCatalog.GetContextWindow(entry.Model);
                        if (detected > 0) { contextCap = detected; }
                    }

                    contextTokens = entry.InputTokens + entry.CacheReadTokens + entry.CacheCreationTokens + entry.OutputTokens;
                    turns += 1;
                    if (entry.Timestamp is { } ts)
                    {
                        if (watch.LastUserTurnAt is { } userAt && ts > userAt)
                        {
                            lastDuration = ts - userAt;
                        }
                        lastAt = ts;
                    }
                }

                watch.Offset = fs.Position;

                if (changed)
                {
                    var snap = new ClaudeSessionTelemetry(
                        watch.SessionId, contextTokens, turns, lastAt, lastDuration, activity, modelId, contextCap);
                    watch.Snapshot = snap;
                    try { Updated?.Invoke(this, snap); }
                    catch (Exception ex) { _logger.LogWarning(ex, "Pi telemetry subscriber threw"); }
                }
            }
            catch (IOException ex) { _logger.LogTrace(ex, "Pi telemetry: mid-flush race for {Path}", watch.FilePath); }
            catch (Exception ex) { _logger.LogWarning(ex, "Pi telemetry: read failed for {Path}", watch.FilePath); }
        }
    }

    private static string DefaultSessionsRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".pi", "agent", "sessions");

    private sealed class Watch(string sessionId) : IDisposable
    {
        public string SessionId { get; } = sessionId;
        public string? FilePath { get; set; }
        public long Offset;
        public ClaudeSessionTelemetry? Snapshot;
        public DateTimeOffset? LastUserTurnAt;
        public readonly object ReadLock = new();

        public void Dispose() { }
    }
}
