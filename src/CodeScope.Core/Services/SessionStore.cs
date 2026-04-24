using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class SessionStore : ISessionStore
{
    private readonly IProjectStore _persistence;
    private readonly IGitService _git;
    private readonly ILogger<SessionStore> _logger;
    private readonly List<Project> _projects = [];
    private readonly object _lock = new();

    public SessionStore(IProjectStore persistence, IGitService git, ILogger<SessionStore> logger)
    {
        _persistence = persistence;
        _git = git;
        _logger = logger;
    }

    public IReadOnlyList<Project> Projects
    {
        get
        {
            lock (_lock) { return _projects.ToArray(); }
        }
    }

    public event EventHandler<SessionStoreChange>? Changed;

    public async Task LoadAsync(CancellationToken ct = default)
    {
        var result = await _persistence.LoadAsync(ct).ConfigureAwait(false);
        if (result.IsFailure)
        {
            _logger.LogWarning("SessionStore: failed to load config: {Error}", result.Error);
            return;
        }

        lock (_lock)
        {
            _projects.Clear();
            _projects.AddRange(result.Value.Projects);
        }

        Raise(new SessionStoreChange.Loaded(Projects));
    }

    public async Task<Result<Project>> AddProjectAsync(string path, string? displayName, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return Result<Project>.Fail("Project path must be non-empty");
        }

        var normalized = Path.GetFullPath(path);
        lock (_lock)
        {
            if (_projects.Any(p =>
                    p.Path.Length > 0
                    && string.Equals(Path.GetFullPath(p.Path), normalized, StringComparison.OrdinalIgnoreCase)))
            {
                return Result<Project>.Fail($"Project for path '{normalized}' already exists");
            }
        }

        var name = string.IsNullOrWhiteSpace(displayName)
            ? Path.GetFileName(normalized.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar))
            : displayName;
        if (string.IsNullOrWhiteSpace(name)) { name = normalized; }

        // Synthesize a primary worktree at creation so the sidebar has something to bind
        // to immediately. Mirrors the migration rule in ProjectStore.Migrate that runs on
        // load — without this, newly-added projects render as empty shells in-memory until
        // the next save→load round-trip. Branch is left null; WorktreeStatusPoller fills
        // it in on the first tick.
        var primary = new Worktree
        {
            Id = "primary",
            Path = normalized,
            IsPrimary = true,
            Branch = null,
        };
        var project = new Project
        {
            Id = Guid.NewGuid().ToString("n"),
            Name = name,
            Path = normalized,
            DefaultBranch = "main",
            Worktrees = [primary],
        };

        lock (_lock) { _projects.Add(project); }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure)
        {
            lock (_lock) { _projects.RemoveAll(p => p.Id == project.Id); }
            return Result<Project>.Fail(saved.Error);
        }

        Raise(new SessionStoreChange.ProjectAdded(project));
        return Result<Project>.Ok(project);
    }

    public async Task<Result<bool>> RemoveProjectAsync(string projectId, CancellationToken ct = default)
    {
        bool removed;
        lock (_lock) { removed = _projects.RemoveAll(p => p.Id == projectId) > 0; }
        if (!removed)
        {
            return Result<bool>.Fail($"Project '{projectId}' not found");
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return saved; }

        Raise(new SessionStoreChange.ProjectRemoved(projectId));
        return Result<bool>.Ok(true);
    }

    public async Task<Result<Session>> AddSessionAsync(string projectId, Session session, CancellationToken ct = default)
    {
        Project? target;
        lock (_lock) { target = _projects.FirstOrDefault(p => p.Id == projectId); }
        if (target is null)
        {
            return Result<Session>.Fail($"Project '{projectId}' not found");
        }

        var stamped = session with { LastOpened = DateTimeOffset.UtcNow };

        lock (_lock)
        {
            var index = _projects.FindIndex(p => p.Id == projectId);
            var updatedSessions = target.Sessions.Append(stamped).ToList();
            _projects[index] = target with { Sessions = updatedSessions };
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return Result<Session>.Fail(saved.Error); }

        Raise(new SessionStoreChange.SessionAdded(projectId, stamped));
        return Result<Session>.Ok(stamped);
    }

    public async Task<Result<bool>> RemoveSessionAsync(string sessionId, CancellationToken ct = default)
    {
        var removed = false;
        lock (_lock)
        {
            for (var i = 0; i < _projects.Count; i++)
            {
                var p = _projects[i];
                if (p.Sessions.Any(s => s.Id == sessionId))
                {
                    _projects[i] = p with { Sessions = p.Sessions.Where(s => s.Id != sessionId).ToList() };
                    removed = true;
                    break;
                }
            }
        }
        if (!removed)
        {
            return Result<bool>.Fail($"Session '{sessionId}' not found");
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return saved; }

        Raise(new SessionStoreChange.SessionRemoved(sessionId));
        return Result<bool>.Ok(true);
    }

    public async Task<Result<bool>> SoftCloseSessionAsync(string sessionId, CancellationToken ct = default)
    {
        var changed = false;
        lock (_lock)
        {
            for (var i = 0; i < _projects.Count; i++)
            {
                var p = _projects[i];
                var sessions = p.Sessions.ToList();
                var idx = sessions.FindIndex(s => s.Id == sessionId);
                if (idx < 0) { continue; }
                // Idempotent — re-closing an already-closed session is a no-op, not an error.
                if (sessions[idx].ClosedAt is not null) { return Result<bool>.Ok(true); }
                sessions[idx] = sessions[idx] with { ClosedAt = DateTimeOffset.UtcNow };
                _projects[i] = p with { Sessions = sessions };
                changed = true;
                break;
            }
        }
        if (!changed) { return Result<bool>.Fail($"Session '{sessionId}' not found"); }
        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure)
        {
            // Rollback the in-memory mutation so a retry doesn't hit the idempotency guard
            // and silently succeed without ever persisting.
            lock (_lock)
            {
                for (var i = 0; i < _projects.Count; i++)
                {
                    var p = _projects[i];
                    var sessions = p.Sessions.ToList();
                    var idx = sessions.FindIndex(s => s.Id == sessionId);
                    if (idx < 0) { continue; }
                    sessions[idx] = sessions[idx] with { ClosedAt = null };
                    _projects[i] = p with { Sessions = sessions };
                    break;
                }
            }
            return saved;
        }
        // Fire SessionRemoved so the sidebar/tab strip drop the row — restore emits SessionAdded.
        Raise(new SessionStoreChange.SessionRemoved(sessionId));
        return Result<bool>.Ok(true);
    }

    public async Task<Result<Session>> RestoreSessionAsync(string sessionId, CancellationToken ct = default)
    {
        Session? restored = null;
        string? projectId = null;
        lock (_lock)
        {
            for (var i = 0; i < _projects.Count; i++)
            {
                var p = _projects[i];
                var sessions = p.Sessions.ToList();
                var idx = sessions.FindIndex(s => s.Id == sessionId);
                if (idx < 0) { continue; }
                restored = sessions[idx] with { ClosedAt = null, LastOpened = DateTimeOffset.UtcNow };
                sessions[idx] = restored;
                _projects[i] = p with { Sessions = sessions };
                projectId = p.Id;
                break;
            }
        }
        if (restored is null || projectId is null)
        {
            return Result<Session>.Fail($"Session '{sessionId}' not found");
        }
        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return Result<Session>.Fail(saved.Error); }
        Raise(new SessionStoreChange.SessionAdded(projectId, restored));
        return Result<Session>.Ok(restored);
    }

    public async Task<Result<bool>> RenameSessionAsync(string sessionId, string? newName, CancellationToken ct = default)
    {
        var renamed = false;
        lock (_lock)
        {
            for (var i = 0; i < _projects.Count; i++)
            {
                var p = _projects[i];
                var sessions = p.Sessions.ToList();
                var idx = sessions.FindIndex(s => s.Id == sessionId);
                if (idx < 0) { continue; }

                sessions[idx] = sessions[idx] with
                {
                    DisplayName = string.IsNullOrWhiteSpace(newName) ? null : newName,
                };
                _projects[i] = p with { Sessions = sessions };
                renamed = true;
                break;
            }
        }
        if (!renamed)
        {
            return Result<bool>.Fail($"Session '{sessionId}' not found");
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return saved; }

        Raise(new SessionStoreChange.SessionRenamed(sessionId,
            string.IsNullOrWhiteSpace(newName) ? null : newName));
        return Result<bool>.Ok(true);
    }

    public async Task<Result<bool>> UpdateAgentSessionIdAsync(string sessionId, string? agentSessionId, CancellationToken ct = default)
    {
        var changed = false;
        var found = false;
        lock (_lock)
        {
            for (var i = 0; i < _projects.Count; i++)
            {
                var p = _projects[i];
                var sessions = p.Sessions.ToList();
                var idx = sessions.FindIndex(s => s.Id == sessionId);
                if (idx < 0) { continue; }
                found = true;
                var existing = sessions[idx].AgentSessionId;
                if (string.Equals(existing, agentSessionId, StringComparison.Ordinal)) { break; }
                sessions[idx] = sessions[idx] with { AgentSessionId = agentSessionId };
                _projects[i] = p with { Sessions = sessions };
                changed = true;
                break;
            }
        }
        if (!found) { return Result<bool>.Fail($"Session '{sessionId}' not found"); }
        if (!changed) { return Result<bool>.Ok(true); }
        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        return saved.IsFailure ? saved : Result<bool>.Ok(true);
    }

    public async Task<Result<Worktree>> AddWorktreeAsync(string projectId, string newWorktreePath, string newBranch, string? baseBranch = null, CancellationToken ct = default)
    {
        Project? project;
        lock (_lock) { project = _projects.FirstOrDefault(p => p.Id == projectId); }
        if (project is null)
        {
            return Result<Worktree>.Fail($"Project '{projectId}' not found");
        }
        if (string.IsNullOrWhiteSpace(project.Path))
        {
            return Result<Worktree>.Fail("Project has no path — cannot add a worktree");
        }
        if (string.IsNullOrWhiteSpace(newBranch))
        {
            return Result<Worktree>.Fail("Branch name is required");
        }
        if (string.IsNullOrWhiteSpace(newWorktreePath))
        {
            return Result<Worktree>.Fail("Worktree path is required");
        }

        var gitResult = await _git.AddWorktreeAsync(project.Path, newWorktreePath, newBranch, baseBranch, ct).ConfigureAwait(false);
        if (gitResult.IsFailure)
        {
            return Result<Worktree>.Fail(gitResult.Error);
        }

        var worktree = new Worktree
        {
            Id = Guid.NewGuid().ToString("n"),
            Path = newWorktreePath,
            Branch = newBranch,
            IsPrimary = false,
        };

        lock (_lock)
        {
            var idx = _projects.FindIndex(p => p.Id == projectId);
            var p = _projects[idx];
            _projects[idx] = p with { Worktrees = p.Worktrees.Append(worktree).ToList() };
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return Result<Worktree>.Fail(saved.Error); }

        Raise(new SessionStoreChange.WorktreeAdded(projectId, worktree));
        return Result<Worktree>.Ok(worktree);
    }

    public async Task<Result<bool>> RemoveWorktreeAsync(string projectId, string worktreeId, bool force = false, CancellationToken ct = default)
    {
        Project? project;
        Worktree? worktree;
        lock (_lock)
        {
            project = _projects.FirstOrDefault(p => p.Id == projectId);
            worktree = project?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        }

        if (project is null || worktree is null)
        {
            return Result<bool>.Fail("Project or worktree not found");
        }
        if (worktree.IsPrimary)
        {
            return Result<bool>.Fail("Primary worktrees cannot be removed");
        }

        var gitResult = await _git.RemoveWorktreeAsync(project.Path, worktree.Path, force, ct).ConfigureAwait(false);
        if (gitResult.IsFailure)
        {
            return Result<bool>.Fail(gitResult.Error);
        }

        lock (_lock)
        {
            var idx = _projects.FindIndex(p => p.Id == projectId);
            var p = _projects[idx];
            _projects[idx] = p with
            {
                Worktrees = p.Worktrees.Where(w => w.Id != worktreeId).ToList(),
                Sessions = p.Sessions.Where(s => s.WorktreeId != worktreeId).ToList(),
            };
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return saved; }

        Raise(new SessionStoreChange.WorktreeRemoved(projectId, worktreeId));
        return Result<bool>.Ok(true);
    }

    public async Task<Result<Worktree>> RenameWorktreeAsync(string projectId, string worktreeId, string newWorktreePath, CancellationToken ct = default)
    {
        Project? project;
        Worktree? worktree;
        lock (_lock)
        {
            project = _projects.FirstOrDefault(p => p.Id == projectId);
            worktree = project?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        }

        if (project is null || worktree is null)
        {
            return Result<Worktree>.Fail("Project or worktree not found");
        }
        if (worktree.IsPrimary)
        {
            return Result<Worktree>.Fail("Primary worktrees cannot be renamed");
        }
        if (string.IsNullOrWhiteSpace(newWorktreePath))
        {
            return Result<Worktree>.Fail("New worktree path is required");
        }
        if (string.Equals(worktree.Path, newWorktreePath, StringComparison.OrdinalIgnoreCase))
        {
            return Result<Worktree>.Ok(worktree);
        }

        var oldPath = worktree.Path;
        var gitResult = await _git.MoveWorktreeAsync(project.Path, oldPath, newWorktreePath, ct).ConfigureAwait(false);
        if (gitResult.IsFailure)
        {
            return Result<Worktree>.Fail(gitResult.Error);
        }

        Worktree updated;
        lock (_lock)
        {
            var pidx = _projects.FindIndex(p => p.Id == projectId);
            var p = _projects[pidx];
            var wts = p.Worktrees.ToList();
            var widx = wts.FindIndex(w => w.Id == worktreeId);
            updated = wts[widx] with { Path = newWorktreePath };
            wts[widx] = updated;
            var sessions = p.Sessions
                .Select(s => s.WorktreeId == worktreeId ? s with { WorktreePath = newWorktreePath } : s)
                .ToList();
            _projects[pidx] = p with { Worktrees = wts, Sessions = sessions };
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return Result<Worktree>.Fail(saved.Error); }

        Raise(new SessionStoreChange.WorktreeRenamed(projectId, worktreeId, oldPath, newWorktreePath));
        return Result<Worktree>.Ok(updated);
    }

    public bool UpdateWorktreeStatus(string projectId, string worktreeId, WorktreeStatus status)
    {
        lock (_lock)
        {
            var pidx = _projects.FindIndex(p => p.Id == projectId);
            if (pidx < 0) { return false; }
            var p = _projects[pidx];
            var wts = p.Worktrees.ToList();
            var widx = wts.FindIndex(w => w.Id == worktreeId);
            if (widx < 0) { return false; }

            // Keep the Branch on the persisted Worktree synced with observed reality — next save
            // will roundtrip the current branch even across restarts.
            if (status.Branch is not null && wts[widx].Branch != status.Branch)
            {
                wts[widx] = wts[widx] with { Branch = status.Branch };
                _projects[pidx] = p with { Worktrees = wts };
            }
        }

        Raise(new SessionStoreChange.WorktreeStatusUpdated(projectId, worktreeId, status));
        return true;
    }

    public async Task<Result<bool>> SetProjectDefaultAgentAsync(string projectId, string? agentId, CancellationToken ct = default)
    {
        var normalized = string.IsNullOrWhiteSpace(agentId) ? null : agentId;

        bool found;
        lock (_lock)
        {
            var idx = _projects.FindIndex(p => p.Id == projectId);
            found = idx >= 0;
            if (found)
            {
                _projects[idx] = _projects[idx] with { DefaultAgentId = normalized };
            }
        }
        if (!found)
        {
            return Result<bool>.Fail($"Project '{projectId}' not found");
        }

        var saved = await SaveSnapshotAsync(ct).ConfigureAwait(false);
        if (saved.IsFailure) { return saved; }

        Raise(new SessionStoreChange.ProjectDefaultAgentChanged(projectId, normalized));
        return Result<bool>.Ok(true);
    }

    public bool UpdateWorktreePullRequest(string projectId, string worktreeId, PullRequestInfo? pullRequest)
    {
        bool found;
        lock (_lock)
        {
            var pidx = _projects.FindIndex(p => p.Id == projectId);
            if (pidx < 0) { return false; }
            found = _projects[pidx].Worktrees.Any(w => w.Id == worktreeId);
        }

        if (!found) { return false; }
        Raise(new SessionStoreChange.WorktreePullRequestUpdated(projectId, worktreeId, pullRequest));
        return true;
    }

    private Task<Result<bool>> SaveSnapshotAsync(CancellationToken ct)
    {
        ProjectsConfig snapshot;
        lock (_lock)
        {
            snapshot = new ProjectsConfig { Projects = _projects.ToArray() };
        }
        return _persistence.SaveAsync(snapshot, ct);
    }

    private void Raise(SessionStoreChange change) => Changed?.Invoke(this, change);
}
