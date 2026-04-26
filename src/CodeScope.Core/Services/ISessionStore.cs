using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Single source of truth for the in-memory Projects/Sessions graph.
/// Every mutation persists atomically via <see cref="IProjectStore"/> and fires <see cref="Changed"/>.
/// </summary>
public interface ISessionStore
{
    IReadOnlyList<Project> Projects { get; }

    event EventHandler<SessionStoreChange>? Changed;

    Task LoadAsync(CancellationToken ct = default);

    Task<Result<Project>> AddProjectAsync(string path, string? displayName, CancellationToken ct = default);

    Task<Result<bool>> RemoveProjectAsync(string projectId, CancellationToken ct = default);

    Task<Result<Session>> AddSessionAsync(string projectId, Session session, CancellationToken ct = default);

    Task<Result<bool>> RemoveSessionAsync(string sessionId, CancellationToken ct = default);

    /// <summary>
    /// Marks a session as closed without discarding it — persists a <see cref="Session.ClosedAt"/>
    /// timestamp so the row is preserved in the store and appears in the per-worktree history
    /// surface for explicit reopen via <see cref="RestoreSessionAsync"/>. Raises
    /// <see cref="SessionStoreChange.SessionSoftClosed"/> (carrying the closed Session) so
    /// listeners can route it to a history surface without re-querying store state.
    /// </summary>
    Task<Result<bool>> SoftCloseSessionAsync(string sessionId, CancellationToken ct = default);

    /// <summary>
    /// Clears <see cref="Session.ClosedAt"/> on a previously soft-closed session and re-publishes it
    /// via <see cref="SessionStoreChange.SessionAdded"/>. Returns the restored <see cref="Session"/>.
    /// </summary>
    Task<Result<Session>> RestoreSessionAsync(string sessionId, CancellationToken ct = default);

    Task<Result<bool>> RenameSessionAsync(string sessionId, string? newName, CancellationToken ct = default);

    /// <summary>
    /// Updates the persisted <see cref="Session.AgentSessionId"/> — typically after
    /// <see cref="IClaudeSessionDiscovery"/> adopts whichever UUID Claude Code actually chose.
    /// No-op success when the value is unchanged.
    /// </summary>
    Task<Result<bool>> UpdateAgentSessionIdAsync(string sessionId, string? agentSessionId, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git worktree add</c> and records the worktree in the project.
    /// <paramref name="baseBranch"/> forwards to <c>-b &lt;new&gt; &lt;base&gt;</c>; null forks from HEAD.
    /// </summary>
    Task<Result<Worktree>> AddWorktreeAsync(string projectId, string newWorktreePath, string newBranch, string? baseBranch = null, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git worktree remove</c> and removes the worktree from the project. Primary worktrees are rejected.
    /// <paramref name="force"/> passes <c>--force</c> through to git, which discards uncommitted changes /
    /// untracked files in the worktree. Callers must confirm with the user first.
    /// </summary>
    Task<Result<bool>> RemoveWorktreeAsync(string projectId, string worktreeId, bool force = false, CancellationToken ct = default);

    /// <summary>
    /// Drops a worktree entry whose working directory is gone (user ran <c>rm -rf</c>
    /// outside the app). Skips git's <c>worktree remove</c> entirely — git can't operate
    /// on a path that no longer exists, and the .git/worktrees/&lt;name&gt; metadata is
    /// harmless ghost data that <c>git worktree prune</c> sweeps the next time the user
    /// runs anything under the project. Primary worktrees are rejected (their absence
    /// means the whole project is broken; the user has to remove the project itself).
    /// Cascades through <see cref="Session.WorktreeId"/> so orphaned sessions disappear too.
    /// </summary>
    Task<Result<bool>> PruneMissingWorktreeAsync(string projectId, string worktreeId, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git worktree move</c> to relocate the worktree folder, then cascades the new path
    /// into every <see cref="Session.WorktreePath"/> referencing the moved worktree and persists.
    /// The worktree id is preserved. Primary worktrees are rejected.
    /// </summary>
    Task<Result<Worktree>> RenameWorktreeAsync(string projectId, string worktreeId, string newWorktreePath, CancellationToken ct = default);

    /// <summary>
    /// In-memory update of a worktree's transient status (branch/dirty/ahead/behind). Not persisted.
    /// Triggered by the status poller. Returns <c>true</c> when the update was applied (worktree
    /// still exists and the change was raised), <c>false</c> when the worktree has been removed
    /// between polling and applying. Synchronous by design so pollers don't interleave with
    /// async mutators that return <see cref="Task{Result}"/>.
    /// </summary>
    bool UpdateWorktreeStatus(string projectId, string worktreeId, Models.WorktreeStatus status);

    /// <summary>
    /// Broadcasts the current open PR (or <c>null</c> if none) for a worktree.
    /// <para>
    /// The <see cref="Models.Worktree"/> record itself does NOT carry PR state — the projection
    /// lives in <c>NoScope.CodeScope.Ui.ViewModels.WorktreeViewModel.PullRequest</c> via the
    /// <see cref="SessionStoreChange.WorktreePullRequestUpdated"/> event. Triggered by the PR
    /// poller and by <c>CreatePullRequest</c>. Returns <c>true</c> when the event was raised,
    /// <c>false</c> when the worktree has been removed. Synchronous for the same reason as
    /// <see cref="UpdateWorktreeStatus"/>.
    /// </para>
    /// </summary>
    bool UpdateWorktreePullRequest(string projectId, string worktreeId, Models.PullRequestInfo? pullRequest);

    /// <summary>
    /// Sets a project's default agent id (persisted). Passing null clears the override
    /// and falls back to the global default. Raises <see cref="SessionStoreChange.ProjectDefaultAgentChanged"/>.
    /// </summary>
    Task<Result<bool>> SetProjectDefaultAgentAsync(string projectId, string? agentId, CancellationToken ct = default);
}

/// <summary>Change events emitted by <see cref="ISessionStore.Changed"/>.</summary>
public abstract record SessionStoreChange
{
    public sealed record ProjectAdded(Project Project) : SessionStoreChange;
    public sealed record ProjectRemoved(string ProjectId) : SessionStoreChange;
    public sealed record SessionAdded(string ProjectId, Session Session) : SessionStoreChange;
    public sealed record SessionRemoved(string SessionId) : SessionStoreChange;
    /// <summary>
    /// A session was soft-closed (kept on disk with ClosedAt set). The full
    /// closed Session is included so consumers can route to a history surface
    /// without needing to re-query store state.
    /// </summary>
    public sealed record SessionSoftClosed(string ProjectId, Session Session) : SessionStoreChange;
    public sealed record SessionRenamed(string SessionId, string? NewName) : SessionStoreChange;
    public sealed record Loaded(IReadOnlyList<Project> Projects) : SessionStoreChange;
    public sealed record WorktreeAdded(string ProjectId, Worktree Worktree) : SessionStoreChange;
    public sealed record WorktreeRemoved(string ProjectId, string WorktreeId) : SessionStoreChange;
    public sealed record WorktreeRenamed(string ProjectId, string WorktreeId, string OldPath, string NewPath) : SessionStoreChange;
    public sealed record WorktreeStatusUpdated(string ProjectId, string WorktreeId, WorktreeStatus Status) : SessionStoreChange;
    public sealed record WorktreePullRequestUpdated(string ProjectId, string WorktreeId, PullRequestInfo? PullRequest) : SessionStoreChange;
    public sealed record ProjectDefaultAgentChanged(string ProjectId, string? AgentId) : SessionStoreChange;
}
