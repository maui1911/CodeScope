using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Ui.Dialogs;

/// <summary>
/// Input envelope for <see cref="NewWorktreeDialog.Prompt(NewWorktreeRequest)"/>.
/// Carries the owning-project context plus a pre-fetched branch list so the dialog
/// renders the base-branch dropdown without doing its own I/O.
/// </summary>
public sealed record NewWorktreeRequest(
    string ProjectName,
    string ProjectPath,
    string? WorktreeRoot,
    IReadOnlyList<BranchInfo> Branches,
    string? DefaultBase);

/// <summary>
/// Result of a successful dialog submission. <see cref="BaseBranch"/> is null when the
/// user picks "(HEAD)" — callers forward a null to <c>git worktree add -b &lt;new&gt;</c>.
/// <see cref="SpawnSession"/> is true when the user wants the sidebar to start a session
/// (using the project's default agent) immediately after the worktree is created.
/// </summary>
public sealed record NewWorktreeResult(string Branch, string Path, string? BaseBranch, bool SpawnSession);
