using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class ClaudeSessionDiscovery : IClaudeSessionDiscovery
{
    // FileSystemWatcher fires Created well before the file is fully populated; a short poll
    // fallback doubles up so we notice files that materialised during the race between
    // "tab launched" and "watcher enabled".
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(350);

    private readonly ILogger<ClaudeSessionDiscovery> _logger;
    private readonly string _projectsRoot;

    public ClaudeSessionDiscovery(ILogger<ClaudeSessionDiscovery> logger)
        : this(logger, DefaultProjectsRoot()) { }

    /// <summary>Test-seam constructor — point at a throwaway projects root.</summary>
    public ClaudeSessionDiscovery(ILogger<ClaudeSessionDiscovery> logger, string projectsRoot)
    {
        _logger = logger;
        _projectsRoot = projectsRoot;
    }

    public IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(workingDirectory);
        ArgumentNullException.ThrowIfNull(onDiscovered);

        var dir = Path.Combine(_projectsRoot, ClaudeTranscriptParser.EncodeCwd(workingDirectory));
        try { Directory.CreateDirectory(dir); }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Claude discovery: cannot ensure {Dir}", dir);
            return NoopDisposable.Instance;
        }

        var handle = new WatchHandle(since.UtcDateTime, onDiscovered, _logger);
        try
        {
            handle.Watcher = new FileSystemWatcher(dir, "*.jsonl")
            {
                NotifyFilter = NotifyFilters.FileName | NotifyFilters.CreationTime | NotifyFilters.LastWrite,
                EnableRaisingEvents = true,
            };
            handle.Watcher.Created += (_, e) => handle.TryConsider(e.FullPath);
            handle.Watcher.Changed += (_, e) => handle.TryConsider(e.FullPath);
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Claude discovery: failed to watch {Dir}", dir);
        }

        // Poll fallback for the handful of cases where FSWatcher misses the Created event
        // (buffer overflow in the watcher, or the jsonl was already there when we attached).
        handle.PollTimer = new Timer(_ =>
        {
            try
            {
                foreach (var path in Directory.EnumerateFiles(dir, "*.jsonl"))
                {
                    handle.TryConsider(path);
                }
            }
            catch (Exception ex) { _logger.LogTrace(ex, "Claude discovery poll failed for {Dir}", dir); }
        }, null, TimeSpan.Zero, PollInterval);

        return handle;
    }

    private static string DefaultProjectsRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".claude", "projects");

    private sealed class WatchHandle(DateTime sinceUtc, Action<string, string> onDiscovered, ILogger logger) : IDisposable
    {
        // Keeps the watch running for the tab's lifetime. Claude Code rotates its session id
        // on `/clear` by writing a brand new jsonl in the same cwd dir — a one-shot watcher
        // would miss every rotation and leave telemetry pinned to the abandoned transcript.
        // Each unique jsonl path fires the callback once; the caller (MainViewModel) decides
        // whether to actually re-adopt by comparing against its current persisted id.
        private readonly HashSet<string> _fired = new(StringComparer.OrdinalIgnoreCase);
        private readonly object _firedLock = new();
        private int _disposed;

        public FileSystemWatcher? Watcher;
        public Timer? PollTimer;

        public void TryConsider(string path)
        {
            if (Volatile.Read(ref _disposed) != 0) { return; }
            try
            {
                var info = new FileInfo(path);
                if (!info.Exists) { return; }
                // CreationTimeUtc is the closest proxy to "when Claude created this session".
                // LastWriteTimeUtc would also work but risks adopting an ancient jsonl that
                // was merely touched. We require the file to be at least as new as the spawn.
                if (info.CreationTimeUtc < sinceUtc && info.LastWriteTimeUtc < sinceUtc) { return; }

                var id = Path.GetFileNameWithoutExtension(path);
                if (!IsValidSessionId(id)) { return; }

                lock (_firedLock)
                {
                    if (!_fired.Add(path)) { return; }
                }

                try { onDiscovered(id, path); }
                catch (Exception ex) { logger.LogWarning(ex, "Claude discovery: subscriber threw"); }
            }
            catch (Exception ex) { logger.LogTrace(ex, "Claude discovery: consider failed for {Path}", path); }
        }

        private static bool IsValidSessionId(string? id)
            => Guid.TryParseExact(id, "D", out _);

        public void Dispose()
        {
            if (Interlocked.Exchange(ref _disposed, 1) != 0) { return; }
            try { Watcher?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "Claude discovery: watcher dispose threw"); }
            try { PollTimer?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "Claude discovery: poll timer dispose threw"); }
            Watcher = null;
            PollTimer = null;
        }
    }

    private sealed class NoopDisposable : IDisposable
    {
        public static readonly NoopDisposable Instance = new();
        public void Dispose() { }
    }
}
