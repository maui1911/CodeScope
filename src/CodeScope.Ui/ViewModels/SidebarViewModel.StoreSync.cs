using System.Windows;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Sidebar store-sync — projects the <see cref="ISessionStore"/> change stream into the
/// observable VM tree. Dispatcher-safe; all mutation happens on the UI thread.
/// </summary>
public sealed partial class SidebarViewModel
{
    private void OnStoreChanged(object? sender, SessionStoreChange change)
    {
        void Apply()
        {
            switch (change)
            {
                case SessionStoreChange.Loaded loaded:
                    RebuildTree(loaded.Projects);
                    break;
                case SessionStoreChange.ProjectAdded added:
                    Projects.Add(BuildProjectVM(added.Project));
                    break;
                case SessionStoreChange.ProjectRemoved removed:
                    RemoveProjectVM(removed.ProjectId);
                    break;
                case SessionStoreChange.WorktreeAdded wtAdded:
                    AppendWorktree(wtAdded.ProjectId, wtAdded.Worktree);
                    break;
                case SessionStoreChange.WorktreeRemoved wtRemoved:
                    RemoveWorktreeVM(wtRemoved.ProjectId, wtRemoved.WorktreeId);
                    break;
                case SessionStoreChange.WorktreeRenamed wtRenamed:
                    ReplaceWorktree(wtRenamed.ProjectId, wtRenamed.WorktreeId);
                    break;
                case SessionStoreChange.WorktreeStatusUpdated wtStatus:
                    ApplyStatus(wtStatus.ProjectId, wtStatus.WorktreeId, wtStatus.Status);
                    break;
                case SessionStoreChange.WorktreePullRequestUpdated wtPr:
                    ApplyPullRequest(wtPr.ProjectId, wtPr.WorktreeId, wtPr.PullRequest);
                    break;
                case SessionStoreChange.ProjectDefaultAgentChanged projAgent:
                    ApplyProjectDefaultAgent(projAgent.ProjectId);
                    break;
                case SessionStoreChange.SessionAdded sAdded:
                    AppendSession(sAdded.ProjectId, sAdded.Session);
                    break;
                case SessionStoreChange.SessionRemoved sRemoved:
                    RemoveSession(sRemoved.SessionId);
                    break;
                case SessionStoreChange.SessionRenamed sRenamed:
                    RenameSession(sRenamed.SessionId, sRenamed.NewName);
                    break;
            }
        }

        if (Application.Current?.Dispatcher is { } d && !d.CheckAccess()) { d.Invoke(Apply); }
        else { Apply(); }
    }

    private void RebuildTree(IReadOnlyList<Project> projects)
    {
        Projects.Clear();
        foreach (var p in projects) { Projects.Add(BuildProjectVM(p)); }
    }

    private ProjectViewModel BuildProjectVM(Project p)
    {
        var pvm = new ProjectViewModel(p);
        foreach (var wt in p.Worktrees)
        {
            var wvm = new WorktreeViewModel(p.Id, wt);
            // Open sessions → live row; soft-closed sessions → history (sorted most-recent first).
            // Both collections live on the worktree VM; the sidebar template hides History when empty.
            foreach (var s in p.Sessions.Where(x => x.WorktreeId == wt.Id && x.ClosedAt is null))
            {
                wvm.Sessions.Add(BuildSessionRow(p.Id, wt.Id, s));
            }
            foreach (var s in p.Sessions
                .Where(x => x.WorktreeId == wt.Id && x.ClosedAt is not null)
                .OrderByDescending(x => x.ClosedAt))
            {
                wvm.History.Add(BuildSessionRow(p.Id, wt.Id, s));
            }
            pvm.Worktrees.Add(wvm);
        }
        // Sessions without an explicit worktreeId attach to primary (or first) worktree.
        var primary = pvm.Worktrees.FirstOrDefault(w => w.IsPrimary) ?? pvm.Worktrees.FirstOrDefault();
        if (primary is not null)
        {
            foreach (var s in p.Sessions.Where(x => x.WorktreeId is null
                || pvm.Worktrees.All(w => w.Id != x.WorktreeId)))
            {
                primary.Sessions.Add(BuildSessionRow(p.Id, primary.Id, s));
            }
        }
        return pvm;
    }

    private static SessionTabViewModel BuildSessionRow(string projectId, string worktreeId, Session s)
    {
        // Lightweight mirror — MainViewModel owns the running descriptor.
        var descriptor = new SessionDescriptor
        {
            Id = s.Id,
            WorkingDirectory = s.WorktreePath,
            Shell = "pwsh.exe",
            ShellArgs = [],
            Title = s.DisplayName ?? s.AgentId ?? System.IO.Path.GetFileName(s.WorktreePath),
        };
        return new SessionTabViewModel(descriptor, projectId, s.AgentId, s.DisplayName) { IsActive = false };
    }

    private void RemoveProjectVM(string projectId)
    {
        var vm = Projects.FirstOrDefault(p => p.Id == projectId);
        if (vm is not null) { Projects.Remove(vm); }
    }

    private void AppendWorktree(string projectId, Worktree wt)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        if (pvm is null) { return; }
        pvm.Worktrees.Add(new WorktreeViewModel(projectId, wt));
    }

    private void RemoveWorktreeVM(string projectId, string worktreeId)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        var wvm = pvm?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        if (pvm is not null && wvm is not null) { pvm.Worktrees.Remove(wvm); }
    }

    private void ReplaceWorktree(string projectId, string worktreeId)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        var wvm = pvm?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        var fresh = _store.Projects.FirstOrDefault(p => p.Id == projectId)
            ?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        if (wvm is not null && fresh is not null) { wvm.Replace(fresh); }
    }

    private void ApplyStatus(string projectId, string worktreeId, WorktreeStatus status)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        var wvm = pvm?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        wvm?.ApplyStatus(status);
        pvm?.NotifySummaryChanged();
    }

    private void ApplyProjectDefaultAgent(string projectId)
    {
        // The store has already persisted the new value. Reseat the VM's Project snapshot
        // so DefaultAgentId reflects reality.
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        var fresh = _store.Projects.FirstOrDefault(p => p.Id == projectId);
        if (pvm is not null && fresh is not null) { pvm.Replace(fresh); }
    }

    private void ApplyPullRequest(string projectId, string worktreeId, PullRequestInfo? pr)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        var wvm = pvm?.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        if (wvm is null) { return; }

        var previous = wvm.PullRequest;
        wvm.PullRequest = pr;
        pvm?.NotifySummaryChanged();

        // Only toast on a real CI transition with a known prior state — silences startup storm.
        if (previous is null || pr is null || previous.CiStatus == pr.CiStatus) { return; }

        var label = pvm is not null ? $"{pvm.Name} · {wvm.DisplayBranch}" : wvm.DisplayBranch;
        switch (pr.CiStatus)
        {
            case CiStatus.Success:
                Toast($"CI passed on #{pr.Number}", label, ToastSeverity.Ok);
                break;
            case CiStatus.Failure:
                Toast($"CI failed on #{pr.Number}", label, ToastSeverity.Err);
                break;
        }
    }

    private void AppendSession(string projectId, Session session)
    {
        var pvm = Projects.FirstOrDefault(p => p.Id == projectId);
        if (pvm is null) { return; }
        var wvm = (session.WorktreeId is not null
            ? pvm.Worktrees.FirstOrDefault(w => w.Id == session.WorktreeId)
            : null)
            ?? pvm.Worktrees.FirstOrDefault(w => w.IsPrimary)
            ?? pvm.Worktrees.FirstOrDefault();
        if (wvm is null) { return; }

        // SessionAdded fires for both a brand-new live session AND for a restore (ClosedAt
        // cleared by RestoreSessionAsync). On restore, also clear any stale history row for
        // the same id so the session doesn't appear twice.
        if (session.ClosedAt is null)
        {
            var stale = wvm.History.FirstOrDefault(x => x.Descriptor.Id == session.Id);
            if (stale is not null) { wvm.History.Remove(stale); }
            wvm.Sessions.Add(BuildSessionRow(projectId, wvm.Id, session));
        }
        else
        {
            wvm.History.Insert(0, BuildSessionRow(projectId, wvm.Id, session));
        }
    }

    private void RemoveSession(string sessionId)
    {
        // Determine soft-close vs hard-remove by peeking the store. Soft-closed → move the
        // session row from Sessions to History (insert at top to honour ClosedAt-desc order).
        // Hard-removed → drop from whichever collection it's in.
        var stored = _store.Projects
            .SelectMany(p => p.Sessions.Select(s => (project: p, session: s)))
            .FirstOrDefault(x => x.session.Id == sessionId);

        foreach (var p in Projects)
        {
            foreach (var w in p.Worktrees)
            {
                var live = w.Sessions.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (live is not null)
                {
                    w.Sessions.Remove(live);
                    if (stored.session is { ClosedAt: not null })
                    {
                        // Re-issue a fresh row instead of reusing `live` so its IsActive flag
                        // and any tab-bar bindings reset cleanly to the dead-row state.
                        w.History.Insert(0, BuildSessionRow(p.Id, w.Id, stored.session));
                    }
                    return;
                }
                var dead = w.History.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (dead is not null) { w.History.Remove(dead); return; }
            }
        }
    }

    private void RenameSession(string sessionId, string? newName)
    {
        foreach (var p in Projects)
        {
            foreach (var w in p.Worktrees)
            {
                var live = w.Sessions.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (live is not null) { live.DisplayName = newName ?? live.Descriptor.Title; return; }
                var dead = w.History.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (dead is not null) { dead.DisplayName = newName ?? dead.Descriptor.Title; return; }
            }
        }
    }
}
