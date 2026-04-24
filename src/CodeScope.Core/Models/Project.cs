namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// A git repository that appears in the project sidebar.
/// </summary>
public sealed record Project
{
    public required string Id { get; init; }

    public required string Name { get; init; }

    /// <summary>Absolute path to the primary working tree.</summary>
    public required string Path { get; init; }

    /// <summary>Default branch name (e.g. "main"). Used as the base for new worktrees.</summary>
    public string DefaultBranch { get; init; } = "main";

    /// <summary>Directory where new worktrees are created. Defaults to "${Path}.worktrees".</summary>
    public string? WorktreeRoot { get; init; }

    /// <summary>
    /// Per-project agent override. When non-null, takes precedence over
    /// <see cref="Services.IAgentRegistry.GetDefault"/> for new sessions in this project.
    /// Null = use the global default.
    /// </summary>
    public string? DefaultAgentId { get; init; }

    public IReadOnlyList<Session> Sessions { get; init; } = [];

    /// <summary>
    /// Worktrees tracked for this project. On first load after Phase 3, a synthetic primary
    /// worktree is injected by <c>ProjectStore.Migrate</c> if this list is empty.
    /// </summary>
    public IReadOnlyList<Worktree> Worktrees { get; init; } = [];
}
