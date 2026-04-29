using System.Collections.Concurrent;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class CopilotTelemetryService : ICopilotTelemetryService
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(250);

    private readonly ILogger<CopilotTelemetryService> _logger;
    private readonly string _sessionStateRoot;
    private readonly ConcurrentDictionary<string, Watch> _watches = new();
    private readonly Timer? _pollTimer;
    private FileSystemWatcher? _rootWatcher;

    public CopilotTelemetryService(ILogger<CopilotTelemetryService> logger)
        : this(logger, DefaultSessionStateRoot(), enablePolling: true) { }

    /// <summary>Test-seam: point at a throwaway session-state root, no polling unless requested.</summary>
    public CopilotTelemetryService(ILogger<CopilotTelemetryService> logger, string sessionStateRoot)
        : this(logger, sessionStateRoot, enablePolling: false) { }

    /// <summary>Full test-seam constructor.</summary>
    public CopilotTelemetryService(ILogger<CopilotTelemetryService> logger, string sessionStateRoot, bool enablePolling)
    {
        _logger = logger;
        _sessionStateRoot = sessionStateRoot;
        if (enablePolling)
        {
            _pollTimer = new Timer(_ => PollAll(), null, PollInterval, PollInterval);
        }

        // Single recursive watcher across the whole session-state root. Copilot creates
        // <uuid>/events.jsonl directories, so we watch for *.jsonl recursively.
        try
        {
            Directory.CreateDirectory(_sessionStateRoot);
            _rootWatcher = new FileSystemWatcher(_sessionStateRoot, "events.jsonl")
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
            _logger.LogWarning(ex, "Copilot telemetry: failed to attach root watcher at {Root}", _sessionStateRoot);
        }
    }

    public event EventHandler<ClaudeSessionTelemetry>? Updated;

    public void Register(string sessionId, string workingDirectory)
    {
        if (string.IsNullOrWhiteSpace(sessionId)) { return; }

        Unregister(sessionId);

        // Copilot stores sessions at ~/.copilot/session-state/<sessionId>/events.jsonl
        var eventsPath = Path.Combine(_sessionStateRoot, sessionId, "events.jsonl");
        var watch = new Watch(sessionId, eventsPath);
        _watches[sessionId] = watch;

        if (File.Exists(eventsPath))
        {
            TryRead(watch);
        }
    }

    public void Unregister(string sessionId)
    {
        if (_watches.TryRemove(sessionId, out var watch)) { watch.Dispose(); }
    }

    public ClaudeSessionTelemetry? GetSnapshot(string sessionId) =>
        _watches.TryGetValue(sessionId, out var w) ? w.Snapshot : null;

    public void Dispose()
    {
        try { _pollTimer?.Dispose(); }
        catch (Exception ex) { _logger.LogTrace(ex, "Copilot telemetry: poll timer dispose threw"); }
        try { _rootWatcher?.Dispose(); }
        catch (Exception ex) { _logger.LogTrace(ex, "Copilot telemetry: root watcher dispose threw"); }
        foreach (var w in _watches.Values) { w.Dispose(); }
        _watches.Clear();
    }

    private void OnFileEvent(object? _, FileSystemEventArgs e)
    {
        // The parent directory name is the session UUID.
        var dirName = Path.GetFileName(Path.GetDirectoryName(e.FullPath) ?? string.Empty);
        if (string.IsNullOrEmpty(dirName)) { return; }
        if (!_watches.TryGetValue(dirName, out var watch)) { return; }
        TryRead(watch);
    }

    private void PollAll()
    {
        foreach (var watch in _watches.Values)
        {
            try
            {
                if (!File.Exists(watch.FilePath)) { continue; }
                var info = new FileInfo(watch.FilePath);
                if (!info.Exists) { continue; }
                if (info.Length == watch.Offset) { continue; }
                TryRead(watch);
            }
            catch (Exception ex)
            {
                _logger.LogTrace(ex, "Copilot telemetry poll failed for {Sid}", watch.SessionId);
            }
        }
    }

    private void TryRead(Watch watch)
    {
        if (!File.Exists(watch.FilePath)) { return; }
        lock (watch.ReadLock)
        {
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
                    var entry = CopilotTranscriptParser.ParseLine(line);
                    if (entry is null) { continue; }

                    switch (entry.EventType)
                    {
                        case "session.start":
                            if (!string.IsNullOrWhiteSpace(entry.Model))
                            {
                                modelId = entry.Model;
                                var cap = ClaudeModelCatalog.GetContextWindow(entry.Model);
                                if (cap > 0) { contextCap = cap; }
                            }
                            changed = true;
                            break;

                        case "user.message":
                            activity = ClaudeActivityState.Composing;
                            if (entry.Timestamp is { } userTs)
                            {
                                watch.LastUserTurnAt = userTs;
                            }
                            changed = true;
                            break;

                        case "assistant.turn_start":
                            // Agent is composing after a tool result or the initial user message.
                            if (activity != ClaudeActivityState.Composing)
                            {
                                activity = ClaudeActivityState.Composing;
                                changed = true;
                            }
                            break;

                        case "assistant.message":
                            if (entry.HasToolRequests)
                            {
                                activity = ClaudeActivityState.PendingToolUse;
                            }
                            // Don't set Idle here — assistant.turn_end does that.
                            changed = true;

                            if (entry.OutputTokens > 0)
                            {
                                // Copilot only provides outputTokens per assistant.message.
                                // Accumulate across the session for the status bar.
                                contextTokens += entry.OutputTokens;
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
                            break;

                        case "tool.execution_start":
                            if (activity == ClaudeActivityState.PendingToolUse)
                            {
                                // Tool started executing — still pending more tools or composing.
                                changed = true;
                            }
                            break;

                        case "tool.execution_complete":
                            // After all tools complete, the next assistant.turn_start marks Composing.
                            activity = ClaudeActivityState.Composing;
                            changed = true;
                            break;

                        case "assistant.turn_end":
                            activity = ClaudeActivityState.Idle;
                            changed = true;
                            break;

                        case "session.shutdown":
                            // Extract full usage from shutdown event for accurate final token counts.
                            var shutdown = CopilotTranscriptParser.ParseShutdownUsage(line);
                            if (shutdown is not null && shutdown.CurrentTokens > 0)
                            {
                                contextTokens = shutdown.CurrentTokens;
                            }
                            activity = ClaudeActivityState.Idle;
                            changed = true;
                            break;
                    }
                }

                watch.Offset = fs.Position;

                if (changed)
                {
                    var snap = new ClaudeSessionTelemetry(
                        watch.SessionId, contextTokens, turns, lastAt, lastDuration, activity, modelId, contextCap);
                    watch.Snapshot = snap;
                    try { Updated?.Invoke(this, snap); }
                    catch (Exception ex) { _logger.LogWarning(ex, "Copilot telemetry subscriber threw"); }
                }
            }
            catch (IOException ex) { _logger.LogTrace(ex, "Copilot telemetry: mid-flush race for {Path}", watch.FilePath); }
            catch (Exception ex) { _logger.LogWarning(ex, "Copilot telemetry: read failed for {Path}", watch.FilePath); }
        }
    }

    private static string DefaultSessionStateRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".copilot", "session-state");

    private sealed class Watch(string sessionId, string filePath) : IDisposable
    {
        public string SessionId { get; } = sessionId;
        public string FilePath { get; } = filePath;
        public long Offset;
        public ClaudeSessionTelemetry? Snapshot;
        public DateTimeOffset? LastUserTurnAt;
        public readonly object ReadLock = new();

        public void Dispose() { }
    }
}
