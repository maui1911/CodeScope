namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Tails OpenCode message-JSON directories and projects per-session token totals + activity for
/// the status bar. Mirrors <see cref="IClaudeTelemetryService"/> / <see cref="IPiTelemetryService"/>;
/// emits <see cref="ClaudeSessionTelemetry"/> records so the UI consumer stays agent-agnostic.
///
/// <para>OpenCode persists each message as its own JSON file under
/// <c>%USERPROFILE%\.local\share\opencode\project\&lt;slug&gt;\storage\message\&lt;sessionId&gt;\msg_*.json</c>.
/// The slug is derived from the project path (git slug or <c>global</c> for non-git), so the
/// service searches recursively for the matching <c>message/&lt;sessionId&gt;</c> directory rather
/// than predicting the slug.</para>
/// </summary>
public interface IOpenCodeTelemetryService : IDisposable
{
    /// <summary>Raised on the service's own thread — consumers must marshal to the UI dispatcher.</summary>
    event EventHandler<ClaudeSessionTelemetry>? Updated;

    /// <summary>
    /// Begin watching the message directory for <paramref name="sessionId"/>. <paramref name="workingDirectory"/>
    /// is currently a hint and unused — we resolve the directory by recursive scan from the
    /// OpenCode data root. Safe to call repeatedly.
    /// </summary>
    void Register(string sessionId, string workingDirectory);

    /// <summary>Stop watching the session.</summary>
    void Unregister(string sessionId);

    /// <summary>Latest snapshot, or <c>null</c> if the session was never registered or has no entries yet.</summary>
    ClaudeSessionTelemetry? GetSnapshot(string sessionId);
}
