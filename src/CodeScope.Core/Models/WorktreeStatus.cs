namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// Runtime-only status of a <see cref="Worktree"/>. Populated by the poller, NOT persisted
/// (stale branch/dirty info would confuse users).
/// </summary>
public sealed record WorktreeStatus
{
    public required string? Branch { get; init; }

    /// <summary>True when any file is modified, added, deleted or untracked.</summary>
    public required bool IsDirty { get; init; }

    /// <summary>Commits on local HEAD not yet on upstream. 0 when no upstream or in sync.</summary>
    public required int Ahead { get; init; }

    /// <summary>Commits on upstream not yet on local HEAD.</summary>
    public required int Behind { get; init; }

    /// <summary>Total lines added across modified + deleted + renamed files vs HEAD. 0 when clean or unknown.</summary>
    public int Added { get; init; }

    /// <summary>Total lines removed vs HEAD. 0 when clean or unknown.</summary>
    public int Removed { get; init; }

    /// <summary>File count behind the numstat — informational, shown in tooltips.</summary>
    public int ChangedFiles { get; init; }

    public static WorktreeStatus Unknown { get; } = new()
    {
        Branch = null,
        IsDirty = false,
        Ahead = 0,
        Behind = 0,
    };
}
