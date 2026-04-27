using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class OpenCodeSessionDiscovery : IOpenCodeSessionDiscovery
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(400);

    private readonly ILogger<OpenCodeSessionDiscovery> _logger;
    private readonly string _dataRoot;

    public OpenCodeSessionDiscovery(ILogger<OpenCodeSessionDiscovery> logger)
        : this(logger, DefaultDataRoot()) { }

    /// <summary>Test-seam: point at a throwaway opencode data root.</summary>
    public OpenCodeSessionDiscovery(ILogger<OpenCodeSessionDiscovery> logger, string dataRoot)
    {
        _logger = logger;
        _dataRoot = dataRoot;
    }

    public IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(workingDirectory);
        ArgumentNullException.ThrowIfNull(onDiscovered);

        try { Directory.CreateDirectory(_dataRoot); }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "OpenCode discovery: cannot ensure {Dir}", _dataRoot);
            return NoopDisposable.Instance;
        }

        // Reuse the canonical-path helper from the Pi service — same cross-platform comparison
        // problem (slash direction, drive-letter colon, case) and same fix.
        var canonCwd = PiSessionDiscovery.CanonicalizePath(workingDirectory);
        var handle = new WatchHandle(since.UtcDateTime, canonCwd, onDiscovered, _logger);

        try
        {
            handle.Watcher = new FileSystemWatcher(_dataRoot, "msg_*.json")
            {
                IncludeSubdirectories = true,
                NotifyFilter = NotifyFilters.FileName | NotifyFilters.CreationTime | NotifyFilters.LastWrite | NotifyFilters.Size,
                EnableRaisingEvents = true,
            };
            handle.Watcher.Created += (_, e) => handle.TryConsider(e.FullPath);
            handle.Watcher.Changed += (_, e) => handle.TryConsider(e.FullPath);
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "OpenCode discovery: failed to watch {Dir}", _dataRoot);
        }

        handle.PollTimer = new Timer(_ =>
        {
            try
            {
                if (!Directory.Exists(_dataRoot)) { return; }
                foreach (var path in Directory.EnumerateFiles(_dataRoot, "msg_*.json", SearchOption.AllDirectories))
                {
                    handle.TryConsider(path);
                }
            }
            catch (Exception ex) { _logger.LogTrace(ex, "OpenCode discovery poll failed"); }
        }, null, TimeSpan.Zero, PollInterval);

        return handle;
    }

    private static string DefaultDataRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".local", "share", "opencode");

    private sealed class WatchHandle(
        DateTime sinceUtc,
        string canonCwd,
        Action<string, string> onDiscovered,
        ILogger logger) : IDisposable
    {
        private readonly HashSet<string> _firedSessions = new(StringComparer.OrdinalIgnoreCase);
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
                if (info.CreationTimeUtc < sinceUtc && info.LastWriteTimeUtc < sinceUtc) { return; }

                // Path layout: <root>/project/<slug>/storage/message/<sessionId>/msg_*.json
                // The sessionId is the parent directory name; verify by checking the
                // grandparent is "message" so we don't fire on stray msg_*.json files.
                var dir = Path.GetDirectoryName(path);
                if (dir is null) { return; }
                var sessionId = Path.GetFileName(dir);
                if (string.IsNullOrEmpty(sessionId)) { return; }
                var grandparent = Path.GetFileName(Path.GetDirectoryName(dir) ?? string.Empty);
                if (!string.Equals(grandparent, "message", StringComparison.OrdinalIgnoreCase)) { return; }

                lock (_firedLock)
                {
                    if (_firedSessions.Contains(sessionId)) { return; }
                }

                if (!HeaderMatches(path)) { return; }

                lock (_firedLock)
                {
                    if (!_firedSessions.Add(sessionId)) { return; }
                }

                try { onDiscovered(sessionId, path); }
                catch (Exception ex) { logger.LogWarning(ex, "OpenCode discovery: subscriber threw"); }
            }
            catch (Exception ex) { logger.LogTrace(ex, "OpenCode discovery: consider failed for {Path}", path); }
        }

        private bool HeaderMatches(string path)
        {
            try
            {
                using var fs = new FileStream(path, FileMode.Open, FileAccess.Read,
                    FileShare.ReadWrite | FileShare.Delete);
                using var reader = new StreamReader(fs);
                var content = reader.ReadToEnd();
                var entry = OpenCodeMessageParser.ParseContent(content);
                if (entry?.Cwd is null) { return false; }
                var theirs = PiSessionDiscovery.CanonicalizePath(entry.Cwd);
                return string.Equals(theirs, canonCwd, StringComparison.Ordinal);
            }
            catch (IOException) { return false; }
            catch (Exception ex)
            {
                logger.LogTrace(ex, "OpenCode discovery: header peek failed for {Path}", path);
                return false;
            }
        }

        public void Dispose()
        {
            if (Interlocked.Exchange(ref _disposed, 1) != 0) { return; }
            try { Watcher?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "OpenCode discovery: watcher dispose threw"); }
            try { PollTimer?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "OpenCode discovery: poll timer dispose threw"); }
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
