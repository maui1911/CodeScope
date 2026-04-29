using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class CopilotSessionDiscovery : ICopilotSessionDiscovery
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(350);

    private readonly ILogger<CopilotSessionDiscovery> _logger;
    private readonly string _sessionStateRoot;

    public CopilotSessionDiscovery(ILogger<CopilotSessionDiscovery> logger)
        : this(logger, DefaultSessionStateRoot()) { }

    /// <summary>Test-seam: point at a throwaway session-state root.</summary>
    public CopilotSessionDiscovery(ILogger<CopilotSessionDiscovery> logger, string sessionStateRoot)
    {
        _logger = logger;
        _sessionStateRoot = sessionStateRoot;
    }

    public IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(workingDirectory);
        ArgumentNullException.ThrowIfNull(onDiscovered);

        try { Directory.CreateDirectory(_sessionStateRoot); }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Copilot discovery: cannot ensure {Dir}", _sessionStateRoot);
            return NoopDisposable.Instance;
        }

        var canonCwd = PiSessionDiscovery.CanonicalizePath(workingDirectory);
        var handle = new WatchHandle(since.UtcDateTime, canonCwd, onDiscovered, _logger, _sessionStateRoot);

        // Watch for new directories and changes to workspace.yaml / events.jsonl files.
        try
        {
            handle.Watcher = new FileSystemWatcher(_sessionStateRoot)
            {
                IncludeSubdirectories = true,
                NotifyFilter = NotifyFilters.DirectoryName | NotifyFilters.FileName
                    | NotifyFilters.CreationTime | NotifyFilters.LastWrite | NotifyFilters.Size,
                EnableRaisingEvents = true,
            };
            handle.Watcher.Created += (_, e) => handle.TryConsider(e.FullPath);
            handle.Watcher.Changed += (_, e) => handle.TryConsider(e.FullPath);
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "Copilot discovery: failed to watch {Dir}", _sessionStateRoot);
        }

        // Poll fallback: catches races where the directory pre-existed or the watcher buffer overflowed.
        handle.PollTimer = new Timer(_ =>
        {
            try
            {
                if (!Directory.Exists(_sessionStateRoot)) { return; }
                foreach (var dir in Directory.EnumerateDirectories(_sessionStateRoot))
                {
                    // Try workspace.yaml first, fall back to events.jsonl — TryConsider
                    // resolves the session dir from either file path.
                    var yamlPath = Path.Combine(dir, "workspace.yaml");
                    if (File.Exists(yamlPath))
                    {
                        handle.TryConsider(yamlPath);
                    }
                    else
                    {
                        var eventsPath = Path.Combine(dir, "events.jsonl");
                        if (File.Exists(eventsPath))
                        {
                            handle.TryConsider(eventsPath);
                        }
                    }
                }
            }
            catch (Exception ex) { _logger.LogTrace(ex, "Copilot discovery poll failed"); }
        }, null, TimeSpan.Zero, PollInterval);

        return handle;
    }

    private static string DefaultSessionStateRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".copilot", "session-state");

    private sealed class WatchHandle(
        DateTime sinceUtc,
        string canonCwd,
        Action<string, string> onDiscovered,
        ILogger logger,
        string sessionStateRoot) : IDisposable
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
                // Resolve the session directory — path might be the dir itself, workspace.yaml,
                // events.jsonl, or any other file inside.
                string sessionDir;
                if (Directory.Exists(path))
                {
                    sessionDir = path;
                }
                else
                {
                    sessionDir = Path.GetDirectoryName(path) ?? string.Empty;
                }

                // The session dir must be a direct child of the session-state root.
                var parent = Path.GetDirectoryName(sessionDir);
                if (!string.Equals(parent, sessionStateRoot, StringComparison.OrdinalIgnoreCase))
                {
                    return;
                }

                var dirName = Path.GetFileName(sessionDir);
                if (string.IsNullOrEmpty(dirName)) { return; }

                // Validate that the directory name is a UUID (Copilot session id format).
                if (!Guid.TryParse(dirName, out _)) { return; }

                var info = new DirectoryInfo(sessionDir);
                if (!info.Exists) { return; }
                if (info.CreationTimeUtc < sinceUtc && info.LastWriteTimeUtc < sinceUtc) { return; }

                lock (_firedLock)
                {
                    if (_fired.Contains(dirName)) { return; }
                }

                // Read cwd from workspace.yaml to verify this session belongs to our working directory.
                var yamlPath = Path.Combine(sessionDir, "workspace.yaml");
                if (!CwdMatches(yamlPath))
                {
                    // Fallback: try session.start event in events.jsonl.
                    var eventsPath = Path.Combine(sessionDir, "events.jsonl");
                    if (!CwdMatchesFromEvents(eventsPath)) { return; }
                }

                lock (_firedLock)
                {
                    if (!_fired.Add(dirName)) { return; }
                }

                try { onDiscovered(dirName, sessionDir); }
                catch (Exception ex) { logger.LogWarning(ex, "Copilot discovery: subscriber threw"); }
            }
            catch (Exception ex) { logger.LogTrace(ex, "Copilot discovery: consider failed for {Path}", path); }
        }

        private bool CwdMatches(string yamlPath)
        {
            var cwd = CopilotTranscriptParser.ReadCwdFromWorkspaceYaml(yamlPath);
            if (string.IsNullOrEmpty(cwd)) { return false; }
            var theirs = PiSessionDiscovery.CanonicalizePath(cwd);
            return string.Equals(theirs, canonCwd, StringComparison.Ordinal);
        }

        private bool CwdMatchesFromEvents(string eventsPath)
        {
            if (!File.Exists(eventsPath)) { return false; }
            try
            {
                using var fs = new FileStream(eventsPath, FileMode.Open, FileAccess.Read,
                    FileShare.ReadWrite | FileShare.Delete);
                using var reader = new StreamReader(fs);
                // Only peek the first few lines for session.start.
                for (var i = 0; i < 5 && reader.ReadLine() is { } line; i++)
                {
                    var entry = CopilotTranscriptParser.ParseLine(line);
                    if (entry?.EventType != "session.start") { continue; }
                    if (string.IsNullOrEmpty(entry.Cwd)) { return false; }
                    var theirs = PiSessionDiscovery.CanonicalizePath(entry.Cwd);
                    return string.Equals(theirs, canonCwd, StringComparison.Ordinal);
                }
                return false;
            }
            catch (IOException) { return false; }
            catch (Exception ex)
            {
                logger.LogTrace(ex, "Copilot discovery: events peek failed for {Path}", eventsPath);
                return false;
            }
        }

        public void Dispose()
        {
            if (Interlocked.Exchange(ref _disposed, 1) != 0) { return; }
            try { Watcher?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "Copilot discovery: watcher dispose threw"); }
            try { PollTimer?.Dispose(); }
            catch (Exception ex) { logger.LogTrace(ex, "Copilot discovery: poll timer dispose threw"); }
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
