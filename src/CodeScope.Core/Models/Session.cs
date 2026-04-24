namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// A persisted session: a tab that should be restored at app launch.
/// Runtime state (the live pty, process handle, etc.) lives in the session manager, not here.
/// </summary>
public sealed record Session
{
    /// <summary>Stable id used in config and cross-references.</summary>
    public required string Id { get; init; }

    /// <summary>Absolute working directory the terminal is pinned to.</summary>
    public required string WorktreePath { get; init; }

    /// <summary>Branch associated with the worktree, if known.</summary>
    public string? Branch { get; init; }

    /// <summary>Agent id (see <see cref="AgentProfile.Id"/>), or null for a bare shell.</summary>
    public string? AgentId { get; init; }

    /// <summary>Optional user override for the display name. Null means auto-derive.</summary>
    public string? DisplayName { get; init; }

    /// <summary>Id of the owning <see cref="Worktree"/>. Null for pre-Phase 3 sessions.</summary>
    public string? WorktreeId { get; init; }

    /// <summary>Last time this session was activated. UTC.</summary>
    public DateTimeOffset? LastOpened { get; init; }

    /// <summary>
    /// Optional agent-side session identifier — the UUID CodeScope passed via
    /// <c>--session-id</c> when the CLI supports it. Used to resume the exact
    /// conversation with <c>--resume &lt;id&gt;</c> on app restart or tab move,
    /// instead of relying on <c>--continue</c>'s "most recent in cwd" heuristic.
    /// Null for pre-upgrade sessions, for shells, and for agents without
    /// <see cref="AgentProfile.SessionIdFlag"/>.
    /// </summary>
    public string? AgentSessionId { get; init; }

    /// <summary>
    /// Soft-close marker. Non-null means the tab was closed but the conversation
    /// is kept on disk so the next <c>New session</c> on the same worktree/agent
    /// can resume it via <c>--resume &lt;AgentSessionId&gt;</c>. Null = live.
    /// </summary>
    public DateTimeOffset? ClosedAt { get; init; }
}
