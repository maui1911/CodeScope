namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Watches <c>~/.claude/projects/&lt;enc-cwd&gt;/</c> for newly-created <c>.jsonl</c> transcripts
/// and reports back whichever UUID Claude Code actually chose for the spawned session.
///
/// <para>Rationale (session 17): Claude Code v2.1.118 rotates / forks session ids on
/// <c>/clear</c> and on <c>--continue</c> / <c>--resume</c>, so CodeScope can no longer mint
/// a UUID up front (<c>--session-id &lt;uuid&gt;</c>) and expect the transcript to live at that
/// path. Instead we let the CLI pick its own id and adopt whatever file appears first.</para>
///
/// <para>The service must be resilient to:
/// <list type="bullet">
///   <item>The jsonl being created <i>before</i> the watch starts (first-line race) —
///     callers pass a <c>since</c> timestamp; any existing file newer than <c>since</c> counts.</item>
///   <item>Multiple jsonl files appearing in the same directory concurrently (two tabs
///     on the same cwd) — each active watch gets its own callback, any file wins.</item>
///   <item>The directory not existing yet — create it on-demand.</item>
/// </list></para>
/// </summary>
public interface IClaudeSessionDiscovery
{
    /// <summary>
    /// Begin watching <paramref name="workingDirectory"/>'s encoded transcript dir for
    /// <c>.jsonl</c> files created after <paramref name="since"/>. <paramref name="onDiscovered"/>
    /// is invoked on the watcher thread exactly once — dispose the returned handle to stop.
    /// Disposing after a successful discovery is a no-op.
    /// </summary>
    /// <param name="workingDirectory">Session cwd — encoded per <see cref="ClaudeTranscriptParser.EncodeCwd"/>.</param>
    /// <param name="since">Only files whose <c>FileInfo.CreationTimeUtc &gt;= since</c> qualify.</param>
    /// <param name="onDiscovered">Callback receiving the detected session id (bare UUID, no extension) and the full transcript path.</param>
    IDisposable Watch(string workingDirectory, DateTimeOffset since, Action<string, string> onDiscovered);
}
