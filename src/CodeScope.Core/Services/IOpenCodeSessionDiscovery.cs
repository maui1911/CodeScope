namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Watches the OpenCode data root for new <c>message/&lt;sessionId&gt;/msg_*.json</c> files whose
/// embedded <c>metadata.assistant.path.cwd</c> matches the launched session's working directory,
/// and reports back the session id OpenCode chose.
///
/// <para>Adoption requires at least one assistant message because that's where the cwd is
/// recorded — sessions created but never run won't fire the callback. In practice the first
/// assistant turn lands within seconds of launch.</para>
/// </summary>
public interface IOpenCodeSessionDiscovery
{
    /// <summary>
    /// Watch for new OpenCode session files in <paramref name="workingDirectory"/>.
    /// <paramref name="onDiscovered"/> is invoked with <c>(sessionId, messageFilePath)</c>
    /// on the watcher thread; dispose the returned handle to stop.
    /// </summary>
    IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered);
}
