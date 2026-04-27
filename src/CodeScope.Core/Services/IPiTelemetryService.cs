namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Tails pi-coding-agent <c>session.jsonl</c> transcripts and projects per-session token totals
/// + activity state for the status bar. Mirrors <see cref="IClaudeTelemetryService"/>; emits
/// <see cref="ClaudeSessionTelemetry"/> records so the UI consumer can stay agent-agnostic.
///
/// <para>Pi names files <c>&lt;timestamp&gt;_&lt;uuid&gt;.jsonl</c> under
/// <c>~/.pi/agent/sessions/--&lt;encoded-cwd&gt;--/</c>. Because the timestamp prefix isn't
/// recoverable from the session id alone, <see cref="Register"/> scans the configured root for
/// any subdirectory containing a file whose stem ends with the requested id.</para>
/// </summary>
public interface IPiTelemetryService : IDisposable
{
    /// <summary>Raised on the service's own thread — consumers must marshal to the UI dispatcher.</summary>
    event EventHandler<ClaudeSessionTelemetry>? Updated;

    /// <summary>
    /// Begin watching the transcript for <paramref name="sessionId"/>. <paramref name="workingDirectory"/>
    /// is currently a hint and unused — Pi's encoding scheme isn't documented for Windows, so
    /// the service searches every subdirectory of the sessions root for a matching file.
    /// Safe to call repeatedly with the same id (replaces the prior watch).
    /// </summary>
    void Register(string sessionId, string workingDirectory);

    /// <summary>Stop tailing the transcript for <paramref name="sessionId"/>.</summary>
    void Unregister(string sessionId);

    /// <summary>Latest snapshot, or <c>null</c> if the session was never registered or has no entries yet.</summary>
    ClaudeSessionTelemetry? GetSnapshot(string sessionId);
}
