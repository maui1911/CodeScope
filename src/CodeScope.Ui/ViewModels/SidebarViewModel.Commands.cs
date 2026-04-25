using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Ui.Services;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Sidebar commands — project/worktree actions exposed to the tree's context menus.
/// Kept separate from the core VM so the observable state + store-sync read cleanly.
/// </summary>
public sealed partial class SidebarViewModel
{
    [RelayCommand]
    private async Task AddProjectAsync()
    {
        var folder = _pickFolder();
        if (string.IsNullOrWhiteSpace(folder)) { return; }
        var r = await _store.AddProjectAsync(folder, displayName: null).ConfigureAwait(true);
        if (r.IsFailure) { _logger.LogWarning("AddProject failed: {Error}", r.Error); }
    }

    /// <summary>
    /// Adds a project at <paramref name="folder"/> directly, skipping the folder picker.
    /// Used by the drag-drop handler on the sidebar — duplicate paths are rejected by the store.
    /// </summary>
    public async Task AddProjectByPathAsync(string folder)
    {
        if (string.IsNullOrWhiteSpace(folder)) { return; }
        var r = await _store.AddProjectAsync(folder, displayName: null).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Project added", r.Value.Name, ToastSeverity.Ok);
        }
        else
        {
            _logger.LogDebug("Drop AddProject: {Error}", r.Error);
        }
    }

    [RelayCommand]
    private async Task RemoveProjectAsync(ProjectViewModel? project)
    {
        if (project is null) { return; }
        var r = await _store.RemoveProjectAsync(project.Id).ConfigureAwait(true);
        if (r.IsFailure) { _logger.LogWarning("RemoveProject failed: {Error}", r.Error); }
    }

    [RelayCommand]
    private async Task AddWorktreeAsync(ProjectViewModel? project)
    {
        if (project is null) { return; }

        // Pre-fetch branches so the dialog renders the base-branch dropdown synchronously.
        // Failures fall through with an empty list — the dialog still opens and forks from HEAD.
        IReadOnlyList<BranchInfo> branches = [];
        if (_git is not null && !string.IsNullOrWhiteSpace(project.Path))
        {
            var br = await _git.ListBranchesAsync(project.Path).ConfigureAwait(true);
            if (br.IsSuccess) { branches = br.Value; }
            else { _logger.LogDebug("ListBranches failed: {Error}", br.Error); }
        }

        var request = new NoScope.CodeScope.Ui.Dialogs.NewWorktreeRequest(
            ProjectName: project.Name,
            ProjectPath: project.Path,
            WorktreeRoot: project.WorktreeRoot,
            Branches: branches,
            DefaultBase: project.DefaultBranch);

        var picked = _pickNewWorktree(request);
        if (picked is null) { return; }

        var r = await _store.AddWorktreeAsync(project.Id, picked.Path, picked.Branch, picked.BaseBranch).ConfigureAwait(true);
        if (r.IsFailure)
        {
            _logger.LogWarning("AddWorktree failed: {Error}", r.Error);
            return;
        }

        if (picked.SpawnSession)
        {
            // The store's Changed event has already rebuilt Projects/Worktrees synchronously
            // (see SidebarViewModel.StoreSync.cs), so the new row is addressable now.
            var freshProject = Projects.FirstOrDefault(p => p.Id == project.Id);
            var newWorktree = freshProject?.Worktrees.FirstOrDefault(w => w.Id == r.Value.Id);
            if (newWorktree is not null)
            {
                SelectedWorktree = newWorktree;
                RaiseSpawnSessionRequested(newWorktree);
            }
        }
    }

    [RelayCommand]
    private async Task SetProjectDefaultAgentAsync((ProjectViewModel Project, string? AgentId) request)
    {
        var r = await _store.SetProjectDefaultAgentAsync(request.Project.Id, request.AgentId).ConfigureAwait(true);
        if (r.IsFailure)
        {
            _logger.LogWarning("SetProjectDefaultAgent failed: {Error}", r.Error);
            return;
        }

        var label = string.IsNullOrEmpty(request.AgentId)
            ? "(global default)"
            : request.AgentId;
        Toast("Default agent updated", $"{request.Project.Name} → {label}", ToastSeverity.Info);
    }

    [RelayCommand]
    private async Task FetchAllAsync(ProjectViewModel? project)
    {
        if (project is null || _git is null || string.IsNullOrWhiteSpace(project.Path)) { return; }
        var r = await _git.FetchAllAsync(project.Path).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Fetch complete", project.Name, ToastSeverity.Ok);
        }
        else
        {
            _logger.LogDebug("Fetch failed: {Error}", r.Error);
            ErrToast("Fetch failed", r.Error, retry: () => FetchAllAsync(project));
        }
    }

    [RelayCommand]
    private async Task PullAsync(WorktreeViewModel? worktree)
    {
        if (worktree is null || _git is null || string.IsNullOrWhiteSpace(worktree.Path)) { return; }
        var r = await _git.PullAsync(worktree.Path).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Pull complete", $"{worktree.DisplayBranch} fast-forwarded", ToastSeverity.Ok);
        }
        else
        {
            _logger.LogDebug("Pull failed: {Error}", r.Error);
            ErrToast("Pull failed", r.Error, retry: () => PullAsync(worktree));
        }
    }

    [RelayCommand]
    private async Task RebaseOntoDefaultAsync(WorktreeViewModel? worktree)
    {
        if (worktree is null || _git is null || string.IsNullOrWhiteSpace(worktree.Path)) { return; }
        var project = _store.Projects.FirstOrDefault(p => p.Id == worktree.ProjectId);
        var defaultBranch = string.IsNullOrWhiteSpace(project?.DefaultBranch) ? "main" : project!.DefaultBranch;
        var baseRef = $"origin/{defaultBranch}";

        var confirm = Dialogs.ConfirmDialog.Confirm(
            title: $"Rebase '{worktree.DisplayBranch}' onto {baseRef}?",
            body: $"Conflicts (if any) will leave the rebase in progress — resolve them in the worktree and run `git rebase --continue` / `--abort` manually.\n\nPath: {worktree.Path}",
            confirmLabel: "Rebase");
        if (!confirm) { return; }

        var r = await _git.RebaseOntoAsync(worktree.Path, baseRef).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Rebase complete", $"{worktree.DisplayBranch} ↻ {baseRef}", ToastSeverity.Ok);
        }
        else
        {
            _logger.LogWarning("Rebase failed: {Error}", r.Error);
            // Rebase failure usually means conflicts the user has to resolve manually,
            // so a blind retry would re-fail. Copy-only is the right affordance here.
            ErrToast("Rebase failed or has conflicts", r.Error);
        }
    }

    [RelayCommand]
    private async Task DiscardChangesAsync(WorktreeViewModel? worktree)
    {
        if (worktree is null || _git is null || string.IsNullOrWhiteSpace(worktree.Path)) { return; }
        var confirm = Dialogs.ConfirmDialog.Destructive(
            title: $"Discard ALL local changes in '{worktree.DisplayBranch}'?",
            body: $"This resets the worktree to HEAD and removes untracked files/dirs. Unsaved work cannot be recovered.\n\nPath: {worktree.Path}",
            confirmLabel: "Discard");
        if (!confirm) { return; }

        var r = await _git.DiscardChangesAsync(worktree.Path).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Changes discarded", worktree.DisplayBranch, ToastSeverity.Ok);
        }
        else
        {
            _logger.LogWarning("DiscardChanges failed: {Error}", r.Error);
            ErrToast("Discard failed", r.Error);
        }
    }

    [RelayCommand]
    private void RevealInExplorer(object? target)
    {
        var path = ResolvePath(target);
        if (string.IsNullOrWhiteSpace(path)) { return; }
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", $"\"{path}\"")
            {
                UseShellExecute = true,
            });
        }
        catch (Exception ex)
        {
            _logger.LogWarning(ex, "RevealInExplorer failed for {Path}", path);
        }
    }

    [RelayCommand]
    private void CopyPath(object? target)
    {
        var path = ResolvePath(target);
        if (string.IsNullOrWhiteSpace(path)) { return; }
        try
        {
            System.Windows.Clipboard.SetText(path);
            Toast("Path copied", path, ToastSeverity.Info);
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "Clipboard write failed");
        }
    }

    [RelayCommand]
    private void OpenInWindowsTerminal(object? target)
    {
        var path = ResolvePath(target);
        if (string.IsNullOrWhiteSpace(path)) { return; }

        // Prefer wt.exe (Windows Terminal) for a tabbed experience; fall back to pwsh in a new window.
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("wt.exe", $"-d \"{path}\"")
            {
                UseShellExecute = true,
            });
        }
        catch (Exception wtEx)
        {
            _logger.LogDebug(wtEx, "wt.exe launch failed for {Path}, falling back to pwsh", path);
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("pwsh.exe")
                {
                    UseShellExecute = true,
                    WorkingDirectory = path,
                });
            }
            catch (Exception ex)
            {
                _logger.LogWarning(ex, "OpenInWindowsTerminal failed for {Path}", path);
            }
        }
    }

    [RelayCommand]
    private void CopyBranch(WorktreeViewModel? worktree)
    {
        var branch = worktree?.Worktree.Branch;
        if (string.IsNullOrWhiteSpace(branch)) { return; }
        try
        {
            System.Windows.Clipboard.SetText(branch);
            Toast("Branch copied", branch, ToastSeverity.Info);
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "Clipboard write failed");
        }
    }

    [RelayCommand]
    private void CopyPullRequestUrl(WorktreeViewModel? worktree)
    {
        var url = worktree?.PullRequest?.Url;
        if (string.IsNullOrWhiteSpace(url)) { return; }
        try
        {
            System.Windows.Clipboard.SetText(url);
            Toast("PR URL copied", url, ToastSeverity.Info);
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "Clipboard write failed");
        }
    }

    [RelayCommand]
    private async Task CreatePullRequestAsync(WorktreeViewModel? worktree)
    {
        if (worktree is null || _pullRequests is null) { return; }

        // Find the project's repo path — PR commands run from the project root, not the worktree,
        // because gh/tea resolve the remote from .git, which is shared across worktrees.
        var project = Projects.FirstOrDefault(p => p.Id == worktree.ProjectId);
        if (project is null || string.IsNullOrWhiteSpace(project.Path))
        {
            _logger.LogWarning("CreatePR: project path not found for worktree {Id}", worktree.Id);
            return;
        }
        if (string.IsNullOrWhiteSpace(worktree.Worktree.Branch))
        {
            _logger.LogWarning("CreatePR: worktree {Id} has no branch", worktree.Id);
            return;
        }

        var result = await _pullRequests.CreateForBranchAsync(
            project.Path, worktree.Worktree.Branch!, title: null, body: null).ConfigureAwait(true);

        if (result.IsFailure)
        {
            _logger.LogWarning("CreatePR failed: {Error}", result.Error);
            ErrToast("Create pull request failed", result.Error,
                retry: () => CreatePullRequestAsync(worktree));
            return;
        }

        _store.UpdateWorktreePullRequest(worktree.ProjectId, worktree.Id, result.Value);
        var pr = result.Value;
        Toast(
            pr.Number > 0 ? $"Pull request #{pr.Number} created" : "Pull request created",
            $"{project.Name} · {worktree.DisplayBranch}",
            ToastSeverity.Ok);
    }

    [RelayCommand]
    private async Task RenameWorktreeAsync(WorktreeViewModel? worktree)
    {
        if (worktree is null || worktree.IsPrimary) { return; }

        var currentLeaf = System.IO.Path.GetFileName(worktree.Path.TrimEnd('\\', '/'));
        var newLeaf = Dialogs.RenameDialog.Prompt(currentLeaf);
        if (string.IsNullOrWhiteSpace(newLeaf) || string.Equals(newLeaf, currentLeaf, StringComparison.Ordinal))
        {
            return;
        }

        var parent = System.IO.Path.GetDirectoryName(worktree.Path.TrimEnd('\\', '/'));
        if (string.IsNullOrWhiteSpace(parent))
        {
            ErrToast("Rename failed", "Could not determine parent folder");
            return;
        }
        var newPath = System.IO.Path.Combine(parent, newLeaf);

        var r = await _store.RenameWorktreeAsync(worktree.ProjectId, worktree.Id, newPath).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Worktree renamed", $"{currentLeaf} → {newLeaf}", ToastSeverity.Ok);
        }
        else
        {
            _logger.LogWarning("RenameWorktree failed: {Error}", r.Error);
            ErrToast("Rename failed", r.Error);
        }
    }

    [RelayCommand]
    private async Task RemoveWorktreeAsync(WorktreeViewModel? worktree)
    {
        if (worktree is null || worktree.IsPrimary) { return; }
        var confirm = Dialogs.ConfirmDialog.Destructive(
            title: $"Delete worktree '{worktree.DisplayBranch}'?",
            body: $"Path: {worktree.Path}\n\nOpen sessions will be closed first. Unpushed commits stay on the branch.",
            confirmLabel: "Delete");
        if (!confirm) { return; }

        // Close any tabs pinned to this worktree first so pwsh releases the cwd lock on
        // the directory. Without this `git worktree remove` fails on Windows with a file-
        // in-use error and the sidebar silently stays stale. The close callback returns a
        // rollback lambda we invoke if the whole remove flow ultimately fails — without it,
        // a failed delete would leave the worktree in place but with its tabs vanished.
        Func<Task>? rollback = null;
        if (CloseWorktreeSessionsAsync is { } closeSessions)
        {
            try { rollback = await closeSessions(worktree.ProjectId, worktree.Id).ConfigureAwait(true); }
            catch (Exception ex) { _logger.LogWarning(ex, "Closing sessions for worktree {Id} failed", worktree.Id); }

            // Give WPF a beat to run the SessionTabView Unloaded teardown and let ConPTY
            // kill the pwsh child — otherwise we race the file lock into `git worktree remove`.
            await Task.Delay(250).ConfigureAwait(true);
        }

        var r = await _store.RemoveWorktreeAsync(worktree.ProjectId, worktree.Id).ConfigureAwait(true);
        if (r.IsSuccess)
        {
            Toast("Worktree removed", worktree.DisplayBranch, ToastSeverity.Ok);
            return;
        }

        _logger.LogWarning("RemoveWorktree failed: {Error}", r.Error);

        // Offer --force when the normal remove is rejected — typically dirty worktree or
        // a lingering lock. Force still can't beat a live Windows file lock, but it covers
        // the common "you have uncommitted changes" case cleanly.
        var retry = Dialogs.ConfirmDialog.Destructive(
            title: "Couldn't remove worktree — force?",
            body: $"{r.Error}\n\nForce remove will discard uncommitted changes and untracked files in the worktree.",
            confirmLabel: "Force remove");
        if (!retry)
        {
            await InvokeRollbackAsync(rollback).ConfigureAwait(true);
            ErrToast("Remove failed", r.Error);
            return;
        }

        var forced = await _store.RemoveWorktreeAsync(worktree.ProjectId, worktree.Id, force: true).ConfigureAwait(true);
        if (forced.IsSuccess)
        {
            Toast("Worktree force-removed", worktree.DisplayBranch, ToastSeverity.Ok);
        }
        else
        {
            _logger.LogWarning("Force RemoveWorktree failed: {Error}", forced.Error);
            await InvokeRollbackAsync(rollback).ConfigureAwait(true);
            ErrToast("Remove failed", forced.Error);
        }
    }

    private async Task InvokeRollbackAsync(Func<Task>? rollback)
    {
        if (rollback is null) { return; }
        try { await rollback().ConfigureAwait(true); }
        catch (Exception ex) { _logger.LogWarning(ex, "Worktree close rollback threw"); }
    }

    /// <summary>Unwraps the ProjectViewModel / WorktreeViewModel / string alternatives passed by menus.</summary>
    private static string? ResolvePath(object? target) => target switch
    {
        ProjectViewModel p => p.Path,
        WorktreeViewModel w => w.Path,
        string s => s,
        _ => null,
    };
}
