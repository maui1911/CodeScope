using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class PiSessionDiscovery : IPiSessionDiscovery
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(350);

    private readonly ILogger<PiSessionDiscovery> _logger;
    private readonly string _sessionsRoot;

    public PiSessionDiscovery(ILogger<PiSessionDiscovery> logger)
        : this(logger, DefaultSessionsRoot()) { }

    /// <summary>Test-seam: point at a throwaway sessions root.</summary>
    public PiSessionDiscovery(ILogger<PiSessionDiscovery> logger, string sessionsRoot)
    {
        _logger = logger;
        _sessionsRoot = sessionsRoot;
    }

    public IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(workingDirectory);
        ArgumentNullException.ThrowIfNull(onDiscovered);

        try { Directory.CreateDirectory(_sessionsRoot); }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Pi discovery: cannot ensure {Dir}", _sessionsRoot);
            return NoopDisposable.Instance;
        }

        var canonCwd = CanonicalizePath(workingDirectory);
        var handle = new WatchHandle(since.UtcDateTime, canonCwd, onDiscovered, _logger);

        try
        {
            handle.Watcher = new FileSystemWatcher(_sessionsRoot, "*.jsonl")
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
            _logger.LogWarning(ex, "Pi discovery: failed to watch {Dir}", _sessionsRoot);
        }

        // Poll fallback handles the same races Claude's discovery does: file pre-existed,
        // FSWatcher buffer overflow, header line not yet flushed at Created event time.
        handle.PollTimer = new Timer(_ =>
        {
            try
            {
                if (!Directory.Exists(_sessionsRoot)) { return; }
                foreach (var path in Directory.EnumerateFiles(_sessionsRoot, "*.jsonl", SearchOption.AllDirectories))
                {
                    handle.TryConsider(path);
                }
            }
            catch (Exception ex) { _logger.LogTrace(ex, "Pi discovery poll failed"); }
        }, null, TimeSpan.Zero, PollInterval);

        return handle;
    }

    /// <summary>
    /// Canonicalize a path for cross-platform comparison: lowercase, forward-slashes, drive
    /// colon stripped, trimmed leading slashes. So <c>C:\dev\codescope</c> and <c>/c/dev/codescope</c>
    /// and <c>c:/dev/codescope</c> all collapse to <c>c/dev/codescope</c>.
    /// </summary>
    internal static string CanonicalizePath(string path)
    {
        if (string.IsNullOrEmpty(path)) { return string.Empty; }
        return path
            .Replace('\\', '/')
            .Replace(":", string.Empty)
            .TrimStart('/')
            .TrimEnd('/')
            .ToLowerInvariant();
    }

    private static string DefaultSessionsRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".pi", "agent", "sessions");

    private sealed class WatchHandle(
        DateTime sinceUtc,
        string canonCwd,
        Action<string, string> onDiscovered,
        ILogger logger) : IDisposable
    {
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
                if (info.CreationTimeUtc < sinceUtc && info.LastWriteTimeUtc < sinceUtc) { return; }

                var sid = PiTranscriptParser.ExtractSessionIdFromFileName(info.Name);
                if (sid is null) { return; }

                lock (_firedLock)
                {
                    if (_fired.Contains(path)) { return; }
                }

                // Peek the header (first non-empty line) — match by cwd to avoid adopting an
                // unrelated session that landed in the watch root from another workspace.
                if (!HeaderMatches(path)) { return; }

                lock (_firedLock)
                {
                    if (!_fired.Add(path)) { return; }
                }

                try { onDiscovered(sid, path); }
                catch (Exception ex) { logger.LogWarning(ex, "Pi discovery: subscriber threw"); }
            }
            catch (Exception ex) { logger.LogTrace(ex, "Pi discovery: consider failed for {Path}", path); }
        }

        private bool HeaderMatches(string path)
        {
            try
            {
                using var fs = new FileStream(path, FileMode.Open, FileAccess.Read,
                    FileShare.ReadWrite | FileShare.Delete);
                using var reader = new StreamReader(fs);
                while (reader.ReadLine() is { } line)
                {
                    if (string.IsNullOrWhiteSpace(line)) { continue; }
                    var entry = PiTranscriptParser.ParseLine(line);
                    if (entry?.Type != "session") { return false; }
                    var theirs = CanonicalizePath(entry.Cwd ?? string.Empty);
                    return string.Equals(theirs, canonCwd, StringComparison.Ordinal);
                }
                return false;
            }
            catch (IOException) { return false; }
            catch (Exception ex)
            {
                logger.LogTrace(ex, "Pi discovery: header peek failed for {Path}", path);
                return false;
            }
        }

        public void Dispose()
        {
            if (Interlocked.Exchange(ref _disposed, 1) != 0) { return; }
            try { Watcher?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "Pi discovery: watcher dispose threw"); }
            try { PollTimer?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "Pi discovery: poll timer dispose threw"); }
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
