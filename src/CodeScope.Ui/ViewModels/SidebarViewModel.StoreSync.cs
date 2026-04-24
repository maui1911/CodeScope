using System.Windows;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Wpf.Ui.Controls;

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
            // Soft-closed sessions persist on disk but have no live tab — skip them in the tree
            // so the worktree row reflects "no active session" and offers New session (which
            // will resume the soft-closed conversation transparently).
            foreach (var s in p.Sessions.Where(x => x.WorktreeId == wt.Id && x.ClosedAt is null))
            {
                wvm.Sessions.Add(BuildSessionRow(p.Id, wt.Id, s));
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
                Toast($"CI passed on #{pr.Number}", label, ControlAppearance.Success);
                break;
            case CiStatus.Failure:
                Toast($"CI failed on #{pr.Number}", label, ControlAppearance.Danger);
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
        wvm.Sessions.Add(BuildSessionRow(projectId, wvm.Id, session));
    }

    private void RemoveSession(string sessionId)
    {
        foreach (var p in Projects)
        {
            foreach (var w in p.Worktrees)
            {
                var s = w.Sessions.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (s is not null) { w.Sessions.Remove(s); return; }
            }
        }
    }

    private void RenameSession(string sessionId, string? newName)
    {
        foreach (var p in Projects)
        {
            foreach (var w in p.Worktrees)
            {
                var s = w.Sessions.FirstOrDefault(x => x.Descriptor.Id == sessionId);
                if (s is not null) { s.DisplayName = newName ?? s.Descriptor.Title; return; }
            }
        }
    }
}
