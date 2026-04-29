namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Tails Copilot CLI <c>events.jsonl</c> transcripts (<c>~/.copilot/session-state/&lt;uuid&gt;/events.jsonl</c>)
/// and projects per-session token totals + activity state for the status bar. Mirrors
/// <see cref="IClaudeTelemetryService"/>; emits <see cref="ClaudeSessionTelemetry"/> records so the
/// UI consumer stays agent-agnostic.
///
/// <para>Copilot stores each session in its own directory under <c>~/.copilot/session-state/</c>,
/// named by session UUID. The <c>events.jsonl</c> file inside carries structured events
/// (session.start, user.message, assistant.message, tool.*, assistant.turn_end, session.shutdown).
/// Token usage per-turn is limited to <c>outputTokens</c> on <c>assistant.message</c> events;
/// full input/cache breakdowns only appear in <c>session.shutdown</c>.</para>
/// </summary>
public interface ICopilotTelemetryService : IDisposable
{
    /// <summary>Raised on the service's own thread — consumers must marshal to the UI dispatcher.</summary>
    event EventHandler<ClaudeSessionTelemetry>? Updated;

    /// <summary>
    /// Begin watching the transcript for <paramref name="sessionId"/>. The service looks for
    /// <c>~/.copilot/session-state/&lt;sessionId&gt;/events.jsonl</c>.
    /// Safe to call repeatedly with the same id (replaces the prior watch).
    /// </summary>
    void Register(string sessionId, string workingDirectory);

    /// <summary>Stop tailing the transcript for <paramref name="sessionId"/>.</summary>
    void Unregister(string sessionId);

    /// <summary>Latest snapshot, or <c>null</c> if the session was never registered or has no entries yet.</summary>
    ClaudeSessionTelemetry? GetSnapshot(string sessionId);
}
