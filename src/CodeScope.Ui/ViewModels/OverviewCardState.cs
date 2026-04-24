namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Three-way status matching the Overview mock's sdot (`active`/`idle`/`wait`).
/// Derived per card from the worktree's dirty flag and session descriptor.
/// </summary>
public enum OverviewCardState
{
    Active,
    Idle,
    Waiting,
}

/// <summary>
/// Segmented filter used by the Overview header (All · Active · Idle · Waiting).
/// </summary>
public enum OverviewFilter
{
    All,
    Active,
    Idle,
    Waiting,
}
