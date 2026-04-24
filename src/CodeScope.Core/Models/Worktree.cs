namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// A git worktree under a <see cref="Project"/>. Every Project has at least one — the primary —
/// which corresponds to the repo's main working tree at <see cref="Project.Path"/>.
/// </summary>
public sealed record Worktree
{
    /// <summary>Stable id used for sessions to refer to this worktree.</summary>
    public required string Id { get; init; }

    /// <summary>Absolute path of the worktree on disk.</summary>
    public required string Path { get; init; }

    /// <summary>Branch checked out in this worktree, if known. Null until first resolution.</summary>
    public string? Branch { get; init; }

    /// <summary>True when this is the repo's primary working tree.</summary>
    public bool IsPrimary { get; init; }
}
