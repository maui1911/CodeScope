namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Snapshot of live telemetry for a single Claude Code session.
/// <para>
/// <see cref="ContextTokens"/> is the most recent assistant turn's total context size
/// (<c>input + cache_read + cache_creation + output</c>) — a close proxy for "how full is
/// the context window right now". It overwrites on each assistant turn, it does NOT
/// accumulate across turns. Summing would double-count: each turn's <c>input_tokens</c>
/// already covers the full prior conversation (Claude is stateless per request), so a
/// running sum explodes past the cap within a handful of turns and the percentage goes
/// nonsensical.
/// </para>
/// </summary>
public sealed record ClaudeSessionTelemetry(
    string SessionId,
    int ContextTokens,
    int TurnCount,
    DateTimeOffset? LastTurnAt,
    TimeSpan? LastTurnDuration,
    ClaudeActivityState Activity,
    string? ModelId = null,
    int ContextWindowTokens = 0);

/// <summary>
/// Semantic activity of a Claude session, derived from the transcript tail. Distinct from the
/// UI's <c>TabStatus</c>: reflects *what the agent is doing* (per its own JSONL) rather than
/// *what the user focused*.
/// </summary>
public enum ClaudeActivityState
{
    /// <summary>No information yet — no assistant entries parsed.</summary>
    Unknown = 0,
    /// <summary>Last assistant turn ended with <c>stop_reason: end_turn</c> — waiting for next user prompt.</summary>
    Idle,
    /// <summary>Last assistant turn ended with <c>stop_reason: tool_use</c> — pending tool call (permission prompt in manual mode).</summary>
    PendingToolUse,
    /// <summary>Most recent entry is a user turn — agent is composing its response.</summary>
    Composing,
}

/// <summary>
/// Tails Claude Code JSONL transcripts (<c>~/.claude/projects/&lt;encoded-cwd&gt;/&lt;session_id&gt;.jsonl</c>)
/// and projects per-session token totals + turn counts for the status bar.
///
/// Registration flow:
/// <list type="number">
///   <item>Call <see cref="Register"/> when a Claude session tab is created — the service
///     starts watching the expected transcript path and replays any existing entries.</item>
///   <item><see cref="Updated"/> fires whenever a new assistant turn lands with usage.</item>
///   <item>Call <see cref="Unregister"/> when the tab is closed.</item>
/// </list>
/// </summary>
public interface IClaudeTelemetryService : IDisposable
{
    /// <summary>Raised on the service's own thread — consumers must marshal to the UI dispatcher.</summary>
    event EventHandler<ClaudeSessionTelemetry>? Updated;

    /// <summary>
    /// Begin watching the transcript for <paramref name="sessionId"/> under <paramref name="workingDirectory"/>.
    /// Safe to call repeatedly with the same id — a second call replaces the prior watch
    /// (e.g. after a resume that kept the same session_id).
    /// </summary>
    void Register(string sessionId, string workingDirectory);

    void Unregister(string sessionId);

    /// <summary>Latest snapshot, or <c>null</c> if the session was never registered or has no entries yet.</summary>
    ClaudeSessionTelemetry? GetSnapshot(string sessionId);
}
