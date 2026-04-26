using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Shapes;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Views;

public partial class SidebarView : UserControl
{
    public SidebarView()
    {
        InitializeComponent();
    }

    /// <summary>No-op now that the inline filter is gone — kept so MainViewModel's Ctrl+F
    /// handler still binds. We may re-surface filtering via the command palette instead.</summary>
    public void FocusFilter() { }

    /// <summary>
    /// Chevron click toggles the owning TreeViewItem's expansion. Without this, the chevron
    /// is a decorative Path and only a double-click on the row expands/collapses — which is
    /// what users report as "clicking the chevron doesn't do anything."
    /// </summary>
    private void OnChevronClick(object sender, MouseButtonEventArgs e)
    {
        if (sender is not DependencyObject origin) { return; }
        var item = FindAncestor<TreeViewItem>(origin);
        if (item is null) { return; }
        item.IsExpanded = !item.IsExpanded;
        e.Handled = true;
    }

    /// <summary>Accept drops only when the payload contains at least one directory.</summary>
    private void OnDragOver(object sender, DragEventArgs e)
    {
        e.Effects = PayloadFolders(e.Data).Count > 0
            ? DragDropEffects.Copy
            : DragDropEffects.None;
        e.Handled = true;
    }

    /// <summary>
    /// Right-click on a tree row should select it first — WPF's TreeViewItem doesn't do this by
    /// default, so the ContextMenu would open over an item that isn't SelectedItem and our
    /// dynamic menu builder (OnTreeSelectionChanged) would never fire for that row.
    /// </summary>
    private void OnTreePreviewRightClick(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is not DependencyObject origin) { return; }
        var item = FindAncestor<TreeViewItem>(origin);
        if (item is not null && !item.IsSelected) { item.IsSelected = true; }
    }

    private static T? FindAncestor<T>(DependencyObject origin) where T : DependencyObject
    {
        var node = origin;
        while (node is not null)
        {
            if (node is T hit) { return hit; }
            node = System.Windows.Media.VisualTreeHelper.GetParent(node);
        }
        return null;
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        if (DataContext is not SidebarViewModel vm) { return; }
        foreach (var folder in PayloadFolders(e.Data))
        {
            await vm.AddProjectByPathAsync(folder);
        }
        e.Handled = true;
    }

    private static IReadOnlyList<string> PayloadFolders(IDataObject data)
    {
        if (!data.GetDataPresent(DataFormats.FileDrop)) { return []; }
        if (data.GetData(DataFormats.FileDrop) is not string[] paths) { return []; }
        return paths.Where(System.IO.Directory.Exists).ToArray();
    }

    private void OnTreeKeyDown(object sender, KeyEventArgs e)
    {
        if (DataContext is not SidebarViewModel vm) { return; }
        if (e.Key == Key.F2 && Tree.SelectedItem is WorktreeViewModel wt && !wt.IsPrimary)
        {
            vm.RenameWorktreeCommand.Execute(wt);
            e.Handled = true;
        }
    }

    /// <summary>
    /// Double-clicking a worktree row with at least one open session focuses its first
    /// session tab — mirrors the ctx-menu "Open session" primary entry. No-op on a
    /// worktree with no sessions (single-click + Ctrl+T / + button is the creation path).
    /// </summary>
    private void OnTreeDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is not DependencyObject origin) { return; }
        var item = FindAncestor<TreeViewItem>(origin);
        if (item is null) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }

        switch (item.DataContext)
        {
            case WorktreeViewModel wt:
                if (wt.Sessions.FirstOrDefault() is { } first
                    && main.AllTabs.FirstOrDefault(t => t.Descriptor.Id == first.Descriptor.Id) is { } tab)
                {
                    main.SelectedTab = tab;
                }
                e.Handled = true;
                return;
            case SessionTabViewModel session when session.ClosedAt is not null:
                if (main.ReopenClosedSessionCommand.CanExecute(session.Descriptor.Id))
                {
                    main.ReopenClosedSessionCommand.Execute(session.Descriptor.Id);
                }
                e.Handled = true;
                return;
        }
    }

    private void OnTreeSelectionChanged(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (DataContext is not SidebarViewModel vm) { return; }

        switch (e.NewValue)
        {
            case ProjectViewModel p:
                vm.SelectedProject = p;
                vm.SelectedWorktree = null;
                break;
            case WorktreeViewModel w:
                vm.SelectedWorktree = w;
                vm.SelectedProject = null;
                break;
            case SessionTabViewModel:
                // Session selection is handled by the tabs strip; sidebar just reveals.
                break;
            default:
                vm.SelectedProject = null;
                vm.SelectedWorktree = null;
                break;
        }

        if (TreeContextMenu is null) { return; }
        TreeContextMenu.Items.Clear();

        switch (e.NewValue)
        {
            case ProjectViewModel proj:
                BuildProjectMenu(vm, proj);
                break;
            case WorktreeViewModel wt:
                BuildWorktreeMenu(vm, wt);
                break;
            case SessionTabViewModel session when session.ClosedAt is not null:
                BuildHistorySessionMenu(vm, session);
                break;
            case SessionTabViewModel session:
                BuildSessionMenu(vm, session);
                break;
        }
    }

    // ─────────────────────────── menu builders ───────────────────────────

    private void BuildWorktreeMenu(SidebarViewModel vm, WorktreeViewModel wt)
    {
        // Project is needed to label the contextual header with the owning repo.
        var owner = vm.Projects.FirstOrDefault(p => p.Id == wt.ProjectId);
        var repoLabel = owner?.Name ?? "";

        // Contextual header: status dot + branch + repo (right-aligned, muted).
        TreeContextMenu.Items.Add(BuildContextHeader(
            WorktreeDotKey(wt),
            wt.DisplayBranch,
            repoLabel));

        // ── SESSION ──
        TreeContextMenu.Items.Add(BuildGroupLabel("Session"));
        if (!wt.HasActiveSession)
        {
            // Submenu: agent picker. Top entry = project/global default (keeps muscle memory
            // of single-click New session; Enter still fires the default), rest list Shell +
            // each registered agent explicitly so the user can pick Claude / Codex / Shell
            // without setting a default first.
            // Note: no Tag="primary" on the submenu root — the PrimaryTemplate style
            // overrides the SubmenuHeader template and strips the chevron. The "Default"
            // entry inside BuildAgentChoices gets the primary accent instead.
            var newSessionRoot = new MenuItem { Header = "New session", Icon = IconFor("Ctx.Icon.Terminal") };
            BuildAgentChoices(newSessionRoot, vm,
                agentId => WithMainVm(main => main.NewSessionCommand.Execute(agentId)),
                defaultShortcut: "↵");
            TreeContextMenu.Items.Add(newSessionRoot);

            var openGroupRoot = new MenuItem { Header = "Open in new group", Icon = IconFor("Ctx.Icon.SplitGroup") };
            BuildAgentChoices(openGroupRoot, vm,
                agentId => WithMainVm(main => main.OpenInNewGroupWithAgentCommand.Execute((wt, agentId))),
                defaultShortcut: "Ctrl+Shift+↵");
            TreeContextMenu.Items.Add(openGroupRoot);
        }
        else
        {
            var multi = wt.Sessions.Count > 1;

            // Always offer "New session" — even when one already exists — so the user can
            // spawn a parallel Claude/Codex/Shell on the same worktree without hunting for it
            // under Restart / Duplicate.
            var spawnRoot = new MenuItem { Header = "New session", Icon = IconFor("Ctx.Icon.Plus") };
            BuildAgentChoices(spawnRoot, vm,
                agentId => WithMainVm(main => main.NewSessionCommand.Execute(agentId)),
                defaultShortcut: null);
            TreeContextMenu.Items.Add(spawnRoot);

            if (multi)
            {
                // Multiple sessions — turn Open / Restart into submenus listing each one
                // so the user picks the target instead of always hitting Sessions[0].
                var openRoot = new MenuItem { Header = "Open session", Icon = IconFor("Ctx.Icon.Terminal") };
                openRoot.Tag = "primary";
                foreach (var s in wt.Sessions)
                {
                    var label = SessionMenuLabel(s);
                    var captured = s;
                    openRoot.Items.Add(BuildItem(
                        label, "Ctx.Icon.Terminal", null,
                        () => WithMainVm(main => main.SelectedTab = main.AllTabs.FirstOrDefault(t => t.Descriptor.Id == captured.Descriptor.Id))));
                }
                TreeContextMenu.Items.Add(openRoot);
                var openGroupRoot2 = new MenuItem { Header = "Open in new group", Icon = IconFor("Ctx.Icon.SplitGroup") };
                BuildAgentChoices(openGroupRoot2, vm,
                    agentId => WithMainVm(main => main.OpenInNewGroupWithAgentCommand.Execute((wt, agentId))),
                    defaultShortcut: "Ctrl+Shift+↵");
                TreeContextMenu.Items.Add(openGroupRoot2);

                var restartRoot = new MenuItem { Header = "Restart session", Icon = IconFor("Ctx.Icon.Restart") };
                foreach (var s in wt.Sessions)
                {
                    var label = SessionMenuLabel(s);
                    var captured = s;
                    restartRoot.Items.Add(BuildItem(
                        label, "Ctx.Icon.Restart", null,
                        () => WithMainVm(main => main.RestartSessionCommand.Execute(captured))));
                }
                TreeContextMenu.Items.Add(restartRoot);
            }
            else
            {
                var openSession = BuildItem(
                    "Open session", "Ctx.Icon.Terminal", "↵",
                    () => WithMainVm(main =>
                    {
                        if (wt.Sessions.FirstOrDefault() is { } first)
                        {
                            main.SelectedTab = main.AllTabs.FirstOrDefault(t => t.Descriptor.Id == first.Descriptor.Id);
                        }
                    }));
                openSession.Tag = "primary";
                TreeContextMenu.Items.Add(openSession);
                var openGroupRoot2 = new MenuItem { Header = "Open in new group", Icon = IconFor("Ctx.Icon.SplitGroup") };
                BuildAgentChoices(openGroupRoot2, vm,
                    agentId => WithMainVm(main => main.OpenInNewGroupWithAgentCommand.Execute((wt, agentId))),
                    defaultShortcut: "Ctrl+Shift+↵");
                TreeContextMenu.Items.Add(openGroupRoot2);
                TreeContextMenu.Items.Add(BuildItem(
                    "Restart session", "Ctx.Icon.Restart", "Ctrl+R",
                    () => WithMainVm(main =>
                    {
                        if (wt.Sessions.FirstOrDefault() is { } first) { main.RestartSessionCommand.Execute(first); }
                    })));
            }
        }

        if (wt.HasHistory)
        {
            var reopenLast = BuildItem(
                "Reopen most recent closed session", "Ctx.Icon.Restart", null,
                () => WithMainVm(main =>
                {
                    if (wt.History.FirstOrDefault() is { } first)
                    {
                        main.ReopenClosedSessionCommand.Execute(first.Descriptor.Id);
                    }
                }));
            TreeContextMenu.Items.Add(reopenLast);
        }

        // ── GIT ──
        TreeContextMenu.Items.Add(BuildSeparator());
        TreeContextMenu.Items.Add(BuildGroupLabel("Git"));

        if (wt.HasPullRequest)
        {
            TreeContextMenu.Items.Add(BuildItem(
                $"Open pull request {wt.PrBadgeText}", "Ctx.Icon.PullRequest", null,
                () => wt.OpenPullRequestCommand.Execute(null)));
        }
        else
        {
            TreeContextMenu.Items.Add(BuildItem(
                "Create pull request", "Ctx.Icon.PullRequest", "Ctrl+Shift+P",
                () => vm.CreatePullRequestCommand.Execute(wt)));
        }

        if (!string.IsNullOrWhiteSpace(wt.Worktree.Branch))
        {
            TreeContextMenu.Items.Add(BuildItem(
                "Pull (fast-forward)", "Ctx.Icon.Pull", "Ctrl+Shift+F",
                () => vm.PullCommand.Execute(wt)));
        }
        var projectForWt = vm.Projects.FirstOrDefault(p => p.Id == wt.ProjectId);
        var defaultBranch = projectForWt is null ? "main" : projectForWt.DefaultBranch;
        if (!string.IsNullOrWhiteSpace(wt.Worktree.Branch)
            && !string.Equals(wt.Worktree.Branch, defaultBranch, StringComparison.OrdinalIgnoreCase))
        {
            TreeContextMenu.Items.Add(BuildItem(
                $"Rebase onto origin/{defaultBranch}…", "Ctx.Icon.Rebase", null,
                () => vm.RebaseOntoDefaultCommand.Execute(wt)));
        }
        else
        {
            TreeContextMenu.Items.Add(BuildItemDisabled($"Rebase onto origin/{defaultBranch}…", "Ctx.Icon.Rebase", null));
        }
        if (wt.IsDirty)
        {
            var discard = BuildItem(
                "Discard changes…", "Ctx.Icon.Trash", null,
                () => vm.DiscardChangesCommand.Execute(wt));
            discard.Tag = "danger";
            TreeContextMenu.Items.Add(discard);
        }
        else
        {
            TreeContextMenu.Items.Add(BuildItemDisabled("Discard changes…", "Ctx.Icon.Trash", null));
        }

        // ── REVEAL ──
        TreeContextMenu.Items.Add(BuildSeparator());
        TreeContextMenu.Items.Add(BuildGroupLabel("Reveal"));
        TreeContextMenu.Items.Add(BuildItem(
            "Reveal in File Explorer", "Ctx.Icon.Folder", null,
            () => vm.RevealInExplorerCommand.Execute(wt)));
        TreeContextMenu.Items.Add(BuildItem(
            "Open in Windows Terminal", "Ctx.Icon.WinTerminal", null,
            () => vm.OpenInWindowsTerminalCommand.Execute(wt)));

        // Copy → submenu
        var copyRoot = new MenuItem { Header = "Copy", Icon = IconFor("Ctx.Icon.Clipboard") };
        copyRoot.Items.Add(BuildItem(
            "Path", "Ctx.Icon.ClipboardPath", "Ctrl+Alt+C",
            () => vm.CopyPathCommand.Execute(wt)));
        if (!string.IsNullOrWhiteSpace(wt.Worktree.Branch))
        {
            copyRoot.Items.Add(BuildItem(
                "Branch name", "Ctx.Icon.Branch", null,
                () => vm.CopyBranchCommand.Execute(wt)));
        }
        if (wt.HasPullRequest)
        {
            copyRoot.Items.Add(BuildItem(
                "PR URL", "Ctx.Icon.Link", null,
                () => vm.CopyPullRequestUrlCommand.Execute(wt)));
        }
        TreeContextMenu.Items.Add(copyRoot);

        // ── tail ──
        TreeContextMenu.Items.Add(BuildSeparator());
        if (wt.IsPrimary)
        {
            TreeContextMenu.Items.Add(BuildItemDisabled("Rename worktree…", "Ctx.Icon.Pencil", "F2"));
        }
        else
        {
            TreeContextMenu.Items.Add(BuildItem(
                "Rename worktree…", "Ctx.Icon.Pencil", "F2",
                () => vm.RenameWorktreeCommand.Execute(wt)));
        }
        if (!wt.IsPrimary)
        {
            var remove = BuildItem(
                "Remove worktree…", "Ctx.Icon.Trash", "Del",
                () => vm.RemoveWorktreeCommand.Execute(wt));
            remove.Tag = "danger";
            TreeContextMenu.Items.Add(remove);
        }
    }

    private void BuildProjectMenu(SidebarViewModel vm, ProjectViewModel proj)
    {
        TreeContextMenu.Items.Add(BuildContextHeader("Accent.Primary", proj.Name, "project"));

        TreeContextMenu.Items.Add(BuildGroupLabel("Worktree"));
        TreeContextMenu.Items.Add(BuildItem(
            "New worktree from branch…", "Ctx.Icon.Plus", null,
            () => vm.AddWorktreeCommand.Execute(proj)));

        // Default-agent submenu.
        var agentRoot = new MenuItem { Header = "Set default agent", Icon = IconFor("Ctx.Icon.Terminal") };
        void AddAgentChoice(string header, string? id)
        {
            var item = new MenuItem
            {
                Header = header,
                IsCheckable = true,
                IsChecked = string.Equals(proj.DefaultAgentId, id, StringComparison.OrdinalIgnoreCase)
                            || (proj.DefaultAgentId is null && id is null),
            };
            item.Click += (_, _) => vm.SetProjectDefaultAgentCommand.Execute((proj, id));
            agentRoot.Items.Add(item);
        }
        AddAgentChoice("(global default)", null);
        AddAgentChoice("Shell", MainViewModel.ShellSentinel);
        foreach (var a in vm.AvailableAgents)
        {
            var header = string.IsNullOrWhiteSpace(a.Icon) ? a.DisplayName : $"{a.Icon}  {a.DisplayName}";
            AddAgentChoice(header, a.Id);
        }
        TreeContextMenu.Items.Add(agentRoot);

        TreeContextMenu.Items.Add(BuildSeparator());
        TreeContextMenu.Items.Add(BuildGroupLabel("Git"));
        TreeContextMenu.Items.Add(BuildItem(
            "Fetch all (prune)", "Ctx.Icon.Download", null,
            () => vm.FetchAllCommand.Execute(proj)));

        TreeContextMenu.Items.Add(BuildSeparator());
        TreeContextMenu.Items.Add(BuildGroupLabel("Reveal"));
        TreeContextMenu.Items.Add(BuildItem(
            "Reveal in File Explorer", "Ctx.Icon.Folder", null,
            () => vm.RevealInExplorerCommand.Execute(proj)));
        TreeContextMenu.Items.Add(BuildItem(
            "Open in Windows Terminal", "Ctx.Icon.WinTerminal", null,
            () => vm.OpenInWindowsTerminalCommand.Execute(proj)));
        TreeContextMenu.Items.Add(BuildItem(
            "Copy path", "Ctx.Icon.Clipboard", "Ctrl+Alt+C",
            () => vm.CopyPathCommand.Execute(proj)));

        TreeContextMenu.Items.Add(BuildSeparator());
        var remove = BuildItem(
            "Remove project", "Ctx.Icon.Trash", null,
            () => vm.RemoveProjectCommand.Execute(proj));
        remove.Tag = "danger";
        TreeContextMenu.Items.Add(remove);
    }

    private void BuildSessionMenu(SidebarViewModel vm, SessionTabViewModel session)
    {
        TreeContextMenu.Items.Add(BuildContextHeader(
            "Accent.Primary", session.DisplayName, session.AgentId ?? "shell"));

        TreeContextMenu.Items.Add(BuildGroupLabel("Session"));
        var focus = BuildItem(
            "Focus session", "Ctx.Icon.Terminal", "↵",
            () => WithMainVm(main => main.SelectedTab = main.AllTabs.FirstOrDefault(t => t.Descriptor.Id == session.Descriptor.Id)));
        focus.Tag = "primary";
        TreeContextMenu.Items.Add(focus);
        TreeContextMenu.Items.Add(BuildItem(
            "Restart session", "Ctx.Icon.Restart", "Ctrl+R",
            () => WithMainVm(main => main.RestartSessionCommand.Execute(session))));

        TreeContextMenu.Items.Add(BuildSeparator());
        var close = BuildItem(
            "Close session", "Ctx.Icon.Trash", "Ctrl+W",
            () => WithMainVm(main =>
            {
                var tab = main.AllTabs.FirstOrDefault(t => t.Descriptor.Id == session.Descriptor.Id);
                if (tab is not null) { main.CloseTabCommand.Execute(tab); }
            }));
        close.Tag = "danger";
        TreeContextMenu.Items.Add(close);
    }

    private void BuildHistorySessionMenu(SidebarViewModel vm, SessionTabViewModel session)
    {
        TreeContextMenu.Items.Add(BuildContextHeader(
            "Accent.Primary", session.DisplayName ?? "(closed session)", session.AgentId ?? "shell"));

        TreeContextMenu.Items.Add(BuildGroupLabel("History"));

        var reopen = BuildItem(
            "Reopen", "Ctx.Icon.Terminal", "↵",
            () => WithMainVm(main => main.ReopenClosedSessionCommand.Execute(session.Descriptor.Id)));
        reopen.Tag = "primary";
        TreeContextMenu.Items.Add(reopen);

        TreeContextMenu.Items.Add(BuildItem(
            "Rename…", "Ctx.Icon.Pencil", "F2",
            () => vm.RenameSessionCommand.Execute(session)));

        TreeContextMenu.Items.Add(BuildSeparator());
        var remove = BuildItem(
            "Remove from history", "Ctx.Icon.Trash", "Del",
            () => vm.RemoveSessionFromHistoryCommand.Execute(session));
        remove.Tag = "danger";
        TreeContextMenu.Items.Add(remove);
    }

    // ─────────────────────────── menu helpers ───────────────────────────

    /// <summary>
    /// Populates an "agent picker" submenu with a <b>Default</b> entry (null agent id —
    /// resolves to the project's <c>DefaultAgentId</c> / global default), a <b>Shell</b>
    /// entry (<c>ShellSentinel</c>), and one row per registered agent.
    ///
    /// <para>The callback receives the chosen agent id (or null for default). Host menu items
    /// forward it to <c>MainViewModel.NewSessionCommand</c> /
    /// <c>OpenInNewGroupWithAgentCommand</c>.</para>
    /// </summary>
    private static void BuildAgentChoices(MenuItem parent, SidebarViewModel vm, Action<string?> onPick, string? defaultShortcut)
    {
        var dflt = BuildItem("Default", "Ctx.Icon.Terminal", defaultShortcut, () => onPick(null));
        dflt.Tag = "primary";
        parent.Items.Add(dflt);
        parent.Items.Add(new Separator());
        parent.Items.Add(BuildItem("Shell", "Ctx.Icon.Terminal", null, () => onPick(MainViewModel.ShellSentinel)));
        foreach (var a in vm.AvailableAgents)
        {
            var header = string.IsNullOrWhiteSpace(a.Icon) ? a.DisplayName : $"{a.Icon}  {a.DisplayName}";
            var agentId = a.Id;
            parent.Items.Add(BuildItem(header, "Ctx.Icon.Terminal", null, () => onPick(agentId)));
        }
    }

    private static MenuItem BuildItem(string header, string iconKey, string? shortcut, Action onClick)
    {
        var mi = new MenuItem
        {
            Header = header,
            Icon = IconFor(iconKey),
            InputGestureText = shortcut ?? string.Empty,
        };
        mi.Click += (_, _) => onClick();
        return mi;
    }

    /// <summary>Icon-prefixed display name for a session inside the submenu (e.g. "✶ claude").</summary>
    private static string SessionMenuLabel(SessionTabViewModel s)
        => string.IsNullOrWhiteSpace(s.Icon) ? s.DisplayName : $"{s.Icon}  {s.DisplayName}";

    /// <summary>Disabled placeholder — item shown in the menu for design parity, 45% opacity.</summary>
    private static MenuItem BuildItemDisabled(string header, string iconKey, string? shortcut)
        => new()
        {
            Header = header,
            Icon = IconFor(iconKey),
            InputGestureText = shortcut ?? string.Empty,
            IsEnabled = false,
            ToolTip = "Coming soon",
        };

    /// <summary>Mono small-caps section label (SESSION / GIT / REVEAL). Non-interactive.</summary>
    private static MenuItem BuildGroupLabel(string text)
        => new()
        {
            Header = text,
            Tag = "group",
        };

    /// <summary>Contextual header row: status dot + branch + repo (muted, right-aligned).</summary>
    private static MenuItem BuildContextHeader(string dotBrushKey, string branch, string repo)
    {
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var dot = new Ellipse
        {
            Width = 6,
            Height = 6,
            VerticalAlignment = VerticalAlignment.Center,
            Fill = Application.Current?.TryFindResource(dotBrushKey) as Brush ?? Brushes.DeepSkyBlue,
        };
        Grid.SetColumn(dot, 0);
        grid.Children.Add(dot);

        var branchText = new TextBlock
        {
            Text = branch,
            Margin = new Thickness(8, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
            FontFamily = (FontFamily?)Application.Current?.TryFindResource("Fig.Font.Mono") ?? new FontFamily("Consolas"),
            FontSize = 11,
            Foreground = (Brush?)Application.Current?.TryFindResource("Text.Primary") ?? Brushes.White,
        };
        Grid.SetColumn(branchText, 1);
        grid.Children.Add(branchText);

        var repoText = new TextBlock
        {
            Text = repo,
            VerticalAlignment = VerticalAlignment.Center,
            FontSize = 10,
            Foreground = (Brush?)Application.Current?.TryFindResource("Text.Faint") ?? Brushes.Gray,
        };
        Grid.SetColumn(repoText, 3);
        grid.Children.Add(repoText);

        return new MenuItem
        {
            Header = grid,
            Tag = "header",
        };
    }

    private static Separator BuildSeparator() => new();

    /// <summary>Runs <paramref name="action"/> with the main-window view-model if present.
    /// Replaces the `Application.Current?.MainWindow?.DataContext is MainViewModel main` guard
    /// repeated across every context-menu lambda in this file.</summary>
    private static void WithMainVm(Action<MainViewModel> action)
    {
        if (Application.Current?.MainWindow?.DataContext is MainViewModel main) { action(main); }
    }

    /// <summary>Resolve an `Ctx.Icon.*` <see cref="StreamGeometry"/> resource into a Path element
    /// sized for MenuItem.Icon (14×14, stroked 1.4px, no fill). <see cref="Path.Stroke"/> is bound
    /// to the enclosing <see cref="ContentPresenter"/>'s <c>TextBlock.Foreground</c> attached
    /// property so the Item/Danger/SubHeader/Primary templates can swap the icon colour by setting
    /// that property on IconHost (hover → Accent.Primary, danger → Ctx.Danger, etc).</summary>
    private static Path? IconFor(string geometryKey)
    {
        if (Application.Current?.TryFindResource(geometryKey) is not Geometry geom) { return null; }
        var path = new Path
        {
            Data = geom,
            Width = 14,
            Height = 14,
            Stretch = Stretch.None,
            StrokeThickness = 1.4,
            StrokeStartLineCap = PenLineCap.Round,
            StrokeEndLineCap = PenLineCap.Round,
            StrokeLineJoin = PenLineJoin.Round,
            Fill = Brushes.Transparent,
            SnapsToDevicePixels = true,
        };
        path.SetBinding(Path.StrokeProperty, new Binding
        {
            RelativeSource = new RelativeSource(RelativeSourceMode.FindAncestor)
            {
                AncestorType = typeof(ContentPresenter),
            },
            Path = new PropertyPath("(0)", TextBlock.ForegroundProperty),
            FallbackValue = Application.Current?.TryFindResource("Text.Secondary") ?? Brushes.Gainsboro,
        });
        return path;
    }

    /// <summary>
    /// Brush key for the worktree context-menu header dot. Tracks the same two-state agent
    /// model as the row dot: any busy session or failing PR CI → Signal.Warn (red),
    /// otherwise Signal.Ok (green). Dirty-tree no longer pulls a distinct accent — selection
    /// blue is gone from the dot system.
    /// </summary>
    private static string WorktreeDotKey(WorktreeViewModel wt)
    {
        if (wt.HasBusySession) { return "Signal.Warn"; }
        if (wt.PullRequest is { CiStatus: CiStatus.Failure }) { return "Signal.Warn"; }
        return "Signal.Ok";
    }
}
