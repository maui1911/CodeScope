namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Watches <c>~/.copilot/session-state/</c> for new session directories whose <c>workspace.yaml</c>
/// references the launched session's working directory, and reports back the UUID Copilot adopted.
///
/// <para>Copilot stores each session in its own UUID-named directory with a <c>workspace.yaml</c>
/// carrying the canonical <c>cwd</c>. Discovery matches by comparing that <c>cwd</c> against the
/// launched working directory (using the same cross-platform path canonicalisation as Pi).</para>
///
/// <para>Mirrors <see cref="IClaudeSessionDiscovery"/>'s contract.</para>
/// </summary>
public interface ICopilotSessionDiscovery
{
    /// <summary>
    /// Watch for new Copilot session directories in <paramref name="workingDirectory"/>.
    /// <paramref name="onDiscovered"/> is invoked with <c>(sessionId, directoryPath)</c> on the
    /// watcher thread; dispose the returned handle to stop.
    /// </summary>
    IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered);
}
