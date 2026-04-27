namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Watches <c>~/.pi/agent/sessions/</c> for new session-jsonl files whose <c>session</c> header
/// references the launched session's working directory, and reports back the UUID Pi adopted.
///
/// <para>Why peek the header instead of trusting the directory name: Pi normalizes the cwd into
/// its directory name with platform-dependent rules that we don't model exactly (and that change
/// across Pi versions). The <c>session</c> header's <c>cwd</c> field is the canonical record of
/// "what cwd did Pi launch in", so matching by header content is encoding-agnostic.</para>
///
/// <para>Mirrors <see cref="IClaudeSessionDiscovery"/>'s contract: the <c>since</c> argument
/// filters out pre-existing files, the callback fires once per matching file, and disposing
/// the handle stops the watch.</para>
/// </summary>
public interface IPiSessionDiscovery
{
    /// <summary>
    /// Watch for new Pi session files in <paramref name="workingDirectory"/>. <paramref name="onDiscovered"/>
    /// is invoked with <c>(sessionId, filePath)</c> on the watcher thread; dispose the returned
    /// handle to stop.
    /// </summary>
    IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered);
}
