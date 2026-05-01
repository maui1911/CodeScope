using CommunityToolkit.Mvvm.Input;

namespace NoScope.CodeScope.Ui.ViewModels;

public sealed partial class MainViewModel
{
    /// <summary>
    /// Ctrl+K action: assembles the palette from current state and dispatches the pick.
    /// Built each time so per-worktree "Create PR" entries stay fresh against the latest sidebar tree.
    /// </summary>
    [RelayCommand]
    private async Task OpenCommandPaletteAsync()
    {
        var actions = BuildPaletteActions();
        var picked = Dialogs.CommandPaletteDialog.Prompt(actions);
        if (picked is null) { return; }
        await picked.Execute().ConfigureAwait(true);
    }

    internal IReadOnlyList<PaletteAction> BuildPaletteActions()
    {
        var list = new List<PaletteAction>
        {
            new("New session",       "Ctrl+T",         () => { NewSessionCommand.Execute(null); return Task.CompletedTask; }, Icon: "▶"),
            new("Close current tab", "Ctrl+W",         () => { CloseTabCommand.Execute(null); return Task.CompletedTask; }, Icon: "×"),
            new("Next tab",          "Ctrl+Tab",       () => { NextTabCommand.Execute(null); return Task.CompletedTask; }, Icon: "→"),
            new("Previous tab",      "Ctrl+Shift+Tab", () => { PrevTabCommand.Execute(null); return Task.CompletedTask; }, Icon: "←"),
            new("Focus sidebar filter", "Ctrl+F",      () => { FocusSidebarFilterCommand.Execute(null); return Task.CompletedTask; }, Icon: "⌕"),
            new("Refresh all",          "F5",          () => { RefreshAllCommand.Execute(null); return Task.CompletedTask; }, Icon: "↻"),
            new("Overview · all sessions", "Ctrl+Shift+O", () => { ToggleOverviewCommand.Execute(null); return Task.CompletedTask; }, Icon: "▦"),
        };

        // Open tab rows: quick-switch without reaching for the mouse.
        foreach (var tab in Tabs)
        {
            var local = tab;
            list.Add(new PaletteAction(
                $"Switch to: {local.DisplayName}",
                local.Descriptor.WorkingDirectory,
                () => { SelectedTab = local; return Task.CompletedTask; },
                Icon: "◉"));
        }

        if (Sidebar is not null)
        {
            list.Add(new PaletteAction("Add project", "pick a folder",
                () => { Sidebar.AddProjectCommand.Execute(null); return Task.CompletedTask; }, Icon: "+"));

            foreach (var project in Sidebar.Projects)
            {
                foreach (var wt in project.Worktrees)
                {
                    var branch = wt.DisplayBranch;

                    list.Add(new PaletteAction(
                        $"Reveal: {project.Name} · {branch}",
                        wt.Path,
                        () => { Sidebar.RevealInExplorerCommand.Execute(wt); return Task.CompletedTask; },
                        Icon: "📁"));

                    list.Add(new PaletteAction(
                        $"Open in new group: {project.Name} · {branch}",
                        "Ctrl+Shift+↵",
                        () => { OpenInNewGroupCommand.Execute(wt); return Task.CompletedTask; },
                        Icon: "⫎"));

                    if (wt.HasPullRequest)
                    {
                        list.Add(new PaletteAction(
                            $"Open pull request {wt.PrBadgeText}",
                            $"{project.Name} · {branch}",
                            () => { wt.OpenPullRequestCommand.Execute(null); return Task.CompletedTask; },
                            Icon: wt.CiGlyph));
                    }
                    else if (!string.IsNullOrWhiteSpace(wt.Worktree.Branch))
                    {
                        list.Add(new PaletteAction(
                            "Create pull request",
                            $"{project.Name} · {branch}",
                            () => { Sidebar.CreatePullRequestCommand.Execute(wt); return Task.CompletedTask; },
                            Icon: "◎"));
                    }
                }
            }
        }

        return list;
    }
}
