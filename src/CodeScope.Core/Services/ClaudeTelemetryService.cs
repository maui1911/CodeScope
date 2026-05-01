using System.Collections.Concurrent;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class ClaudeTelemetryService : IClaudeTelemetryService
{
    // FSWatcher latency on the JSONL tail is 100–500 ms in the wild; a low-rate poll
    // closes the gap so the Wait pulse feels instant. Poll is cheap: we stat the file
    // and short-circuit when Length == last-seen Offset.
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(250);

    private readonly ILogger<ClaudeTelemetryService> _logger;
    private readonly string _projectsRoot;
    private readonly ConcurrentDictionary<string, Watch> _watches = new();
    private readonly Timer? _pollTimer;
    private readonly object _timerLock = new();
    private bool _timerArmed;

    public ClaudeTelemetryService(ILogger<ClaudeTelemetryService> logger)
        : this(logger, DefaultProjectsRoot(), enablePolling: true) { }

    /// <summary>Test-seam constructor — allows pointing at a throwaway projects-root during unit tests.</summary>
    public ClaudeTelemetryService(ILogger<ClaudeTelemetryService> logger, string projectsRoot)
        : this(logger, projectsRoot, enablePolling: false) { }

    /// <summary>Full test-seam constructor — opts into the 250 ms poll fallback.</summary>
    public ClaudeTelemetryService(ILogger<ClaudeTelemetryService> logger, string projectsRoot, bool enablePolling)
    {
        _logger = logger;
        _projectsRoot = projectsRoot;
        if (enablePolling)
        {
            // Start paused — RefreshTimerArmed() arms it on the first Register and disarms
            // it on the last Unregister, so an idle CodeScope (no agent sessions) doesn't
            // burn 4×/sec across the four telemetry services for nothing. Issue #36.
            _pollTimer = new Timer(_ => PollAll(), null, Timeout.Infinite, Timeout.Infinite);
        }
    }

    public event EventHandler<ClaudeSessionTelemetry>? Updated;

    public void Register(string sessionId, string workingDirectory)
    {
        if (string.IsNullOrWhiteSpace(sessionId) || string.IsNullOrWhiteSpace(workingDirectory)) { return; }

        var dir = Path.Combine(_projectsRoot, ClaudeTranscriptParser.EncodeCwd(workingDirectory));
        var file = Path.Combine(dir, sessionId + ".jsonl");

        Unregister(sessionId);

        var watch = new Watch(sessionId, file);
        _watches[sessionId] = watch;

        try
        {
            Directory.CreateDirectory(dir);
            watch.Watcher = new FileSystemWatcher(dir, "*.jsonl")
            {
                NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.Size | NotifyFilters.FileName,
                EnableRaisingEvents = true,
            };
            watch.Watcher.Changed += (_, e) => OnFileEvent(watch, e.FullPath);
            watch.Watcher.Created += (_, e) => OnFileEvent(watch, e.FullPath);
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Claude telemetry: failed to watch {Dir}", dir);
        }

        // Replay anything already on disk.
        TryRead(watch);

        RefreshTimerArmed();
    }

    public void Unregister(string sessionId)
    {
        if (_watches.TryRemove(sessionId, out var watch)) { watch.Dispose(); }
        RefreshTimerArmed();
    }

    /// <summary>
    /// Arms the poll timer when at least one watch is registered, disarms it otherwise.
    /// Prevents the 250 ms callback from firing continuously on an idle CodeScope.
    /// </summary>
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
        catch (Exception ex) { _logger.LogTrace(ex, "Claude telemetry: poll timer dispose threw"); }
        foreach (var w in _watches.Values) { w.Dispose(); }
        _watches.Clear();
    }

    private void OnFileEvent(Watch watch, string path)
    {
        if (!string.Equals(path, watch.FilePath, StringComparison.OrdinalIgnoreCase)) { return; }
        TryRead(watch);
    }

    private void PollAll()
    {
        foreach (var watch in _watches.Values)
        {
            try
            {
                // Cheap stat — skip the open+seek when nothing new has been appended.
                var info = new FileInfo(watch.FilePath);
                if (!info.Exists) { continue; }
                if (info.Length == watch.Offset) { continue; }
                TryRead(watch);
            }
            catch (Exception ex)
            {
                _logger.LogTrace(ex, "Claude telemetry poll stat failed for {Path}", watch.FilePath);
            }
        }
    }

    private void TryRead(Watch watch)
    {
        lock (watch.ReadLock)
        {
            if (!File.Exists(watch.FilePath)) { return; }
            try
            {
                using var fs = new FileStream(watch.FilePath, FileMode.Open, FileAccess.Read,
                    FileShare.ReadWrite | FileShare.Delete);
                if (watch.Offset > fs.Length) { watch.Offset = 0; } // file truncated/rewritten
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
                    var entry = ClaudeTranscriptParser.ParseLine(line);
                    if (entry is null) { continue; }

                    // Activity state machine — walks the transcript in order:
                    //   user (plain)          → Composing (agent now responding)
                    //   user (tool_result)    → Composing (clears any pending tool_use)
                    //   assistant end_turn    → Idle
                    //   assistant tool_use    → PendingToolUse (permission prompt in manual mode)
                    // Other types (file-history-snapshot, attachments, hooks) don't move the state.
                    if (entry.Type == "user")
                    {
                        activity = ClaudeActivityState.Composing;
                        if (!entry.UserCarriesToolResult && entry.Timestamp is { } userTs)
                        {
                            watch.LastUserTurnAt = userTs;
                        }
                        changed = true;
                        continue;
                    }

                    if (entry.Type == "assistant")
                    {
                        activity = entry.StopReason switch
                        {
                            "tool_use" => ClaudeActivityState.PendingToolUse,
                            "end_turn" => ClaudeActivityState.Idle,
                            _ => activity,
                        };
                        changed = true;
                    }

                    if (!entry.HasUsage) { continue; }

                    // Assistant turns carry the effective model id — latch the most recent one
                    // so a mid-session model switch updates the cap.
                    if (!string.IsNullOrWhiteSpace(entry.Model) && entry.Model != modelId)
                    {
                        modelId = entry.Model;
                        var detected = ClaudeModelCatalog.GetContextWindow(entry.Model);
                        if (detected > 0) { contextCap = detected; }
                    }

                    // Overwrite, not accumulate — see the ClaudeSessionTelemetry docstring for why.
                    // Cache-read IS included here (it's in-context even if prefilled), unlike
                    // BillableTokens which excludes it to reflect Anthropic's metered pricing.
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
                    catch (Exception ex) { _logger.LogWarning(ex, "Claude telemetry subscriber threw"); }
                }
            }
            catch (IOException ex) { _logger.LogTrace(ex, "Claude telemetry: mid-flush race for {Path} — next event will catch up", watch.FilePath); }
            catch (Exception ex)
            {
                _logger.LogWarning(ex, "Claude telemetry: read failed for {Path}", watch.FilePath);
            }
        }
    }

    private static string DefaultProjectsRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".claude", "projects");

    private sealed class Watch(string sessionId, string filePath) : IDisposable
    {
        public string SessionId { get; } = sessionId;
        public string FilePath { get; } = filePath;
        public FileSystemWatcher? Watcher { get; set; }
        public long Offset;
        public ClaudeSessionTelemetry? Snapshot;
        public DateTimeOffset? LastUserTurnAt;
        public readonly object ReadLock = new();

        public void Dispose()
        {
            try { Watcher?.Dispose(); }
            catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"Claude telemetry: watcher dispose threw: {ex}"); }
        }
    }
}
