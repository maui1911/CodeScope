namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// Describes a coding agent CLI that CodeScope can spawn in a session.
/// BYOA: the agent binary must be on PATH.
/// </summary>
public sealed record AgentProfile
{
    /// <summary>Stable id used in config and cross-references.</summary>
    public required string Id { get; init; }

    /// <summary>Human-readable name (e.g. "Claude Code").</summary>
    public required string DisplayName { get; init; }

    /// <summary>Executable name resolved via PATH (e.g. "claude").</summary>
    public required string Command { get; init; }

    /// <summary>Args passed when resuming an existing session (e.g. ["--continue"]).</summary>
    public IReadOnlyList<string> ResumeArgs { get; init; } = [];

    /// <summary>Args passed when starting a brand-new session.</summary>
    public IReadOnlyList<string> NewSessionArgs { get; init; } = [];

    /// <summary>
    /// Optional CLI flag that accepts a caller-supplied session id on launch
    /// (e.g. <c>--session-id</c> for Claude Code). When non-null, CodeScope
    /// generates a UUID per new session, passes it to the CLI with this flag,
    /// and persists it so later resumes can target that specific conversation.
    /// Null for CLIs that don't support deterministic session ids.
    /// </summary>
    public string? SessionIdFlag { get; init; }

    /// <summary>
    /// Args used to resume a specific session id (e.g. <c>["--resume"]</c> for
    /// Claude Code — the stored UUID is appended as the next token). Empty/null
    /// falls back to <see cref="ResumeArgs"/>.
    /// </summary>
    public IReadOnlyList<string> ResumeByIdArgs { get; init; } = [];

    /// <summary>True for the default agent picked when creating a tab without explicit selection.</summary>
    public bool IsDefault { get; init; }

    /// <summary>Optional single-glyph icon (emoji or symbol) used by the UI to decorate sessions.</summary>
    public string? Icon { get; init; }

    /// <summary>
    /// Nominal context-window capacity (in tokens) for the model this agent runs, or 0 when
    /// unknown. Drives the status-bar token progress read-out (<c>&lt;used&gt;/&lt;cap&gt; · &lt;pct&gt;%</c>).
    /// Claude Code defaults to Opus 4.7 with the 1M-context variant in this setup, so the
    /// baked default there is 1_000_000. Users can override via <c>projects.json</c>.
    /// </summary>
    public int ContextWindowTokens { get; init; }
}
