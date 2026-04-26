using System.Collections.ObjectModel;
using System.IO;
using System.Windows;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Services;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Root VM. Subscribes to <see cref="ISessionStore"/>; both the tabs strip and the sidebar
/// are independent projections of the same store.
/// </summary>
public sealed partial class MainViewModel : ObservableObject
{
    public const string ShellSentinel = "shell";

    private readonly ISessionManager _sessionManager;
    private readonly ISessionStore _store;
    private readonly IAgentRegistry _agents;
    private readonly ILogger<MainViewModel> _logger;
    private readonly Func<string?> _pickFolder;
    private readonly Func<CancellationToken, Task>? _refresh;
    private readonly IClaudeTelemetryService? _telemetry;
    private readonly INotificationService? _notifications;
    private readonly IClaudeSessionDiscovery? _discovery;
    private readonly IToastService? _toasts;
    private readonly Dictionary<string, ClaudeActivityState> _lastActivity = [];
    // Per-tab discovery watcher — disposed on adoption or tab close.
    private readonly Dictionary<string, IDisposable> _discoveryWatches = [];

    public MainViewModel(
        ISessionManager sessionManager,
        ISessionStore store,
        IAgentRegistry agents,
        ILogger<MainViewModel> logger,
        Func<string?> pickFolder,
        Func<CancellationToken, Task>? refresh = null,
        IClaudeTelemetryService? telemetry = null,
        INotificationService? notifications = null,
        IClaudeSessionDiscovery? discovery = null,
        IToastService? toasts = null)
    {
        _sessionManager = sessionManager;
        _store = store;
        _agents = agents;
        _logger = logger;
        _pickFolder = pickFolder;
        _refresh = refresh;
        _telemetry = telemetry;
        _notifications = notifications;
        _discovery = discovery;
        _toasts = toasts;
        if (_telemetry is not null) { _telemetry.Updated += OnTelemetryUpdated; }
        if (_notifications is not null)
        {
            Notifications = new NotificationsViewModel(_notifications);
            Notifications.ActivateRequested += (_, entry) =>
            {
                if (entry.SessionId is null) { return; }
                var target = AllTabs.FirstOrDefault(t => FindStoredSession(t)?.AgentSessionId == entry.SessionId);
                if (target is not null) { SelectedTab = target; }
            };
        }

        Tabs = [];
        // Default group shares the same Tabs collection instance — the canonical store
        // for session tabs. When multi-group split lands, fresh groups will own their
        // own collections and Tabs here becomes a proxy for FocusedGroup.Tabs.
        var defaultGroup = new EditorGroupViewModel(Tabs) { IsFocused = true };
        Groups = [defaultGroup];
        _focusedGroup = defaultGroup;
        HookGroupSelection(defaultGroup);
        Groups.CollectionChanged += OnGroupsCollectionChanged;
        _store.Changed += OnStoreChanged;
    }

    /// <summary>
    /// Ctrl+1..8 → Tabs[0..7]; Ctrl+9 → last tab. Mirrors the browser convention.
    /// Parameter is a string because XAML CommandParameter literals are strings; it's parsed here.
    /// No-op if the requested index doesn't exist yet.
    /// </summary>
    [RelayCommand]
    private void SelectTabByIndex(string? indexText)
    {
        var tabs = FocusedGroup.Tabs;
        if (tabs.Count == 0 || !int.TryParse(indexText, out var index)) { return; }
        var target = index == 9 ? tabs.Count - 1 : Math.Min(index - 1, tabs.Count - 1);
        if (target < 0) { return; }
        SelectedTab = tabs[target];
    }

    /// <summary>F5 / "Refresh all": force both pollers to tick immediately.</summary>
    [RelayCommand]
    private async Task RefreshAllAsync()
    {
        if (_refresh is null) { return; }
        try
        {
            await _refresh(CancellationToken.None).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            _logger.LogDebug(ex, "RefreshAll failed");
        }
    }

    public ObservableCollection<SessionTabViewModel> Tabs { get; }

    public IAgentRegistry Agents => _agents;

    /// <summary>
    /// Rows for the split-button's "New session with agent…" dropdown: a synthetic Shell entry
    /// on top, then every registered agent. Refreshed on every get, so changes to the registry
    /// (future: edit-agents UI) show up next time the menu opens.
    /// </summary>
    public IReadOnlyList<AgentMenuEntry> AgentMenuEntries
    {
        get
        {
            var rows = new List<AgentMenuEntry> { new("Shell", ShellSentinel) };
            rows.AddRange(_agents.GetAll().Select(a =>
                new AgentMenuEntry(
                    string.IsNullOrWhiteSpace(a.Icon) ? a.DisplayName : $"{a.Icon}  {a.DisplayName}",
                    a.Id)));
            return rows;
        }
    }

    /// <summary>
    /// Sidebar view-model. Set exactly once via <see cref="AttachSidebar"/> during host
    /// startup (after DI resolution but before the first XAML binding fires); nullable so
    /// the contract is honest. Bindings that access it before attach see the empty state.
    /// </summary>
    public SidebarViewModel? Sidebar { get; private set; }

    /// <summary>Notification queue projection for the status-bar bell cluster (spec §11).</summary>
    public NotificationsViewModel? Notifications { get; }

    public DiffPanelViewModel? Diff { get; private set; }

    public OverviewViewModel? Overview { get; private set; }

    public void AttachDiffPanel(DiffPanelViewModel diff) => Diff = diff;

    /// <summary>
    /// Toggles the session grid that takes over the right column when the user hits
    /// <c>Ctrl+Shift+O</c> or the sidebar's Overview button. While visible, the tabs strip
    /// and per-tab terminals stay mounted underneath so sessions keep running.
    /// </summary>
    [ObservableProperty]
    private bool _isOverviewVisible;

    [RelayCommand]
    private void ToggleOverview() => IsOverviewVisible = !IsOverviewVisible;

    [RelayCommand]
    private void ShowOverview() => IsOverviewVisible = true;

    [RelayCommand]
    private void HideOverview() => IsOverviewVisible = false;

    [RelayCommand]
    private void FocusSidebarFilter()
    {
        // Walk the visual tree to find the sidebar view — it exposes a public focus helper.
        if (Application.Current?.MainWindow is not Window win) { return; }
        var view = FindDescendant<Views.SidebarView>(win);
        view?.FocusFilter();
    }

    private static T? FindDescendant<T>(System.Windows.DependencyObject root) where T : System.Windows.DependencyObject
    {
        var count = System.Windows.Media.VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < count; i++)
        {
            var child = System.Windows.Media.VisualTreeHelper.GetChild(root, i);
            if (child is T hit) { return hit; }
            var nested = FindDescendant<T>(child);
            if (nested is not null) { return nested; }
        }
        return null;
    }

    [RelayCommand]
    private void ToggleDiffPanel()
    {
        if (Diff is null) { return; }
        Diff.IsVisible = !Diff.IsVisible;
        if (Diff.IsVisible && Sidebar?.SelectedWorktree is { } wt) { Diff.AttachWorktree(wt); }
    }

    [ObservableProperty]
    private SessionTabViewModel? _selectedTab;

    /// <summary>Title bar text: "CodeScope — {tab}" when a tab is active, else just "CodeScope".</summary>
    public string WindowTitle
    {
        get
        {
            // Visual delimiter between an installed build and a side-by-side dev run —
            // CODESCOPE_DEV=1 also routes the config/layout/mutex to a `.Dev` namespace,
            // so the suffix mirrors that split in the titlebar.
            var brand = NoScope.CodeScope.Core.AppPaths.IsDevMode ? "CodeScope [dev]" : "CodeScope";
            return SelectedTab is { } t ? $"{brand} — {t.DisplayName}" : brand;
        }
    }

    partial void OnSelectedTabChanged(SessionTabViewModel? value)
    {
        // When SelectedTab is set externally (sidebar, palette), transfer focus to the
        // group that owns the tab and update its SelectedTab — that way the view that
        // actually renders the tab becomes visible.
        if (value is not null)
        {
            var owner = FindGroupContaining(value);
            if (owner is not null && !ReferenceEquals(owner, FocusedGroup))
            {
                FocusedGroup = owner;
            }
        }
        if (FocusedGroup is not null && !ReferenceEquals(FocusedGroup.SelectedTab, value))
        {
            FocusedGroup.SelectedTab = value;
        }
        OnPropertyChanged(nameof(WindowTitle));
        RaiseStatusBarChanged();
        // Focusing a tab implicitly acknowledges its pending notifications.
        if (value is not null && _notifications is not null)
        {
            var stored = FindStoredSession(value);
            if (stored?.AgentSessionId is { Length: > 0 } sid) { _notifications.MarkSessionRead(sid); }
        }
        // Tab.Status is purely a function of agent activity now — selection no longer
        // overrides it. The focused-tab visual treatment (pill bg + font weight) is
        // driven separately by ListBoxItem.IsSelected in the strip template.
    }

    [ObservableProperty]
    private string? _selectedProjectId;

    public void AttachSidebar(SidebarViewModel sidebar)
    {
        Sidebar = sidebar;
        OnPropertyChanged(nameof(Sidebar));

        WireOverview(sidebar);
        WireSidebarCallbacks(sidebar);
        WireStoreChangedToStatusBar();
        HookStatusBarSources();
    }

    /// <summary>Builds the OverviewViewModel and wires its focus/back signals to the main window.</summary>
    private void WireOverview(SidebarViewModel sidebar)
    {
        Overview = new OverviewViewModel(sidebar, Groups, _agents);
        Overview.FocusSessionRequested += (_, session) =>
        {
            SelectedTab = session;
            IsOverviewVisible = false;
        };
        Overview.BackRequested += (_, _) => IsOverviewVisible = false;
        OnPropertyChanged(nameof(Overview));
    }

    /// <summary>Hooks sidebar-driven callbacks (spawn-session, close-worktree-sessions).</summary>
    private void WireSidebarCallbacks(SidebarViewModel sidebar)
    {
        // Dialog → "Spawn session on creation" — fires after the store has inserted the
        // new worktree and the sidebar has selected it. NewSessionAsync() picks up the
        // current selection and uses the project's default agent.
        sidebar.SpawnSessionRequested += async (_, _) =>
        {
            await NewSessionAsync().ConfigureAwait(true);
        };

        // Worktree deletion needs every tab pinned to that worktree closed first so the
        // pwsh children release their Windows cwd lock. Snapshot/close/rollback choreography
        // lives in CloseWorktreeSessionsAsync / RestoreClosedWorktreeSessionsAsync.
        sidebar.CloseWorktreeSessionsAsync = CloseWorktreeSessionsAsync;
    }

    /// <summary>Recomputes status-bar metrics on every worktree-status / PR-state event.</summary>
    private void WireStoreChangedToStatusBar()
    {
        _store.Changed += (_, change) =>
        {
            if (change is SessionStoreChange.WorktreeStatusUpdated
                or SessionStoreChange.WorktreePullRequestUpdated
                or SessionStoreChange.WorktreeAdded
                or SessionStoreChange.WorktreeRemoved
                or SessionStoreChange.ProjectRemoved
                or SessionStoreChange.ProjectAdded
                or SessionStoreChange.Loaded)
            {
                if (Application.Current?.Dispatcher is { } d)
                {
                    d.BeginInvoke(RaiseStatusBarChanged);
                }
                else
                {
                    RaiseStatusBarChanged();
                }
            }
        };
    }

    public async Task InitializeAsync(CancellationToken ct = default)
    {
        await _store.LoadAsync(ct).ConfigureAwait(true);
    }

    [RelayCommand]
    private async Task NewSessionAsync(string? agentId = null)
    {
        // Resolve project + worktree + target folder. Priority:
        //   1. Sidebar's selected worktree (Phase 3) — use its path and owning project.
        //   2. Sidebar's selected project — use its primary worktree.
        //   3. Folder picker → folder becomes a new project in 'Unsorted'.
        Project? project = null;
        Worktree? worktree = null;
        string? folder = null;

        var selectedWorktree = Sidebar?.SelectedWorktree;
        if (selectedWorktree is not null)
        {
            project = _store.Projects.FirstOrDefault(p => p.Id == selectedWorktree.ProjectId);
            worktree = project?.Worktrees.FirstOrDefault(w => w.Id == selectedWorktree.Id);
            folder = worktree?.Path;
        }
        else if (!string.IsNullOrEmpty(SelectedProjectId))
        {
            project = _store.Projects.FirstOrDefault(p => p.Id == SelectedProjectId);
            worktree = project?.Worktrees.FirstOrDefault(w => w.IsPrimary) ?? project?.Worktrees.FirstOrDefault();
            folder = worktree?.Path ?? project?.Path;
        }

        if (string.IsNullOrWhiteSpace(folder))
        {
            folder = _pickFolder();
            if (string.IsNullOrWhiteSpace(folder)) { return; }

            project = _store.Projects.FirstOrDefault(p =>
                !string.IsNullOrEmpty(p.Path)
                && string.Equals(Path.GetFullPath(p.Path), Path.GetFullPath(folder), StringComparison.OrdinalIgnoreCase));

            if (project is null)
            {
                var unsorted = _store.Projects.FirstOrDefault(p => p.Id == "unsorted");
                if (unsorted is null)
                {
                    var created = await _store.AddProjectAsync(folder, displayName: null).ConfigureAwait(true);
                    if (created.IsFailure) { _logger.LogWarning("AddProject: {Error}", created.Error); return; }
                    project = created.Value;
                }
                else
                {
                    project = unsorted;
                }
            }
            worktree = project.Worktrees.FirstOrDefault(w => w.IsPrimary) ?? project.Worktrees.FirstOrDefault();
        }

        if (project is null) { return; }

        var resolvedAgentId = ResolveAgentIdForNewSession(project, agentId);

        // Resolve agent / shell from the same priority used above (explicit arg → project default
        // → global default). resolvedAgentId == ShellSentinel selects the shell; null falls back
        // to the global default profile when no agent matched the requested id.
        var useShell = string.Equals(resolvedAgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase);
        var agent = useShell
            ? null
            : (resolvedAgentId is { Length: > 0 } id ? _agents.GetById(id) : null) ?? _agents.GetDefault();
        var targetFolder = folder;
        // Claude Code (and any agent with SessionIdFlag) gets a CodeScope-minted UUID up
        // front so we can later resume with `--resume <id>` instead of `--continue`.
        var agentSessionId = AgentSupportsSessionId(agent) ? Guid.NewGuid().ToString() : null;
        var descriptor = agent is null
            ? _sessionManager.CreateShellSession(targetFolder)
            : _sessionManager.CreateAgentSession(targetFolder, agent, agentSessionId: agentSessionId);

        // Tab label format: `{project} · {branch}` — agent identity is carried by the
        // status dot + icon glyph, so the label stays focused on *what's being worked on*
        // rather than *which CLI is running it*.
        if (worktree?.Branch is { Length: > 0 } branch)
        {
            descriptor = descriptor with { Title = $"{project.Name} · {branch}" };
        }

        var session = new Session
        {
            Id = descriptor.Id,
            WorktreePath = descriptor.WorkingDirectory,
            WorktreeId = worktree?.Id,
            AgentId = agent?.Id ?? (useShell ? ShellSentinel : null),
            AgentSessionId = agentSessionId,
        };

        var added = await _store.AddSessionAsync(project.Id, session).ConfigureAwait(true);
        if (added.IsFailure)
        {
            _logger.LogWarning("AddSession: {Error}", added.Error);
            return;
        }

        var vm = new SessionTabViewModel(descriptor, project.Id, session.AgentId, session.DisplayName, agent?.Icon);
        FocusedGroup.Tabs.Add(vm);
        SelectedTab = vm;
        BeginClaudeAdoption(descriptor.Id, agent?.Id, targetFolder, DateTimeOffset.UtcNow);
        OnPropertyChanged(nameof(CanCloseGroup));
        CloseGroupCommand.NotifyCanExecuteChanged();
    }

    /// <summary>
    /// Resolves the agent id that <see cref="NewSessionAsync"/> would end up using, mirroring
    /// its priority (explicit arg → project default → global default). Returns
    /// <see cref="ShellSentinel"/> when the user explicitly picked the shell, the registered
    /// agent id when one matches, or <c>null</c> when no profile matches and no global default
    /// is registered. Reused by the explicit-reopen command in a later task.
    /// </summary>
    private string? ResolveAgentIdForNewSession(Project project, string? explicitAgentId)
    {
        if (!string.IsNullOrEmpty(explicitAgentId))
        {
            return string.Equals(explicitAgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase)
                ? ShellSentinel
                : _agents.GetById(explicitAgentId)?.Id;
        }
        if (string.Equals(project.DefaultAgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase))
        {
            return ShellSentinel;
        }
        if (!string.IsNullOrEmpty(project.DefaultAgentId))
        {
            return _agents.GetById(project.DefaultAgentId!)?.Id ?? _agents.GetDefault()?.Id;
        }
        return _agents.GetDefault()?.Id;
    }

    /// <summary>
    /// Closes every open tab pinned to <paramref name="worktreeId"/> so its pwsh children
    /// release the Windows cwd lock before the worktree is deleted. Returns a rollback
    /// <see cref="Func{Task}"/> the caller invokes if the deletion itself fails — the rollback
    /// re-inserts each soft-closed session in its original group/index.
    /// </summary>
    private async Task<Func<Task>> CloseWorktreeSessionsAsync(string projectId, string worktreeId)
    {
        var targetSessionIds = _store.Projects
            .FirstOrDefault(p => p.Id == projectId)?.Sessions
            .Where(s => s.WorktreeId == worktreeId && s.ClosedAt is null)
            .Select(s => s.Id)
            .ToHashSet() ?? [];
        if (targetSessionIds.Count == 0) { return () => Task.CompletedTask; }

        // Snapshot each target tab's (stored state · group · groupIndex · indexInGroup) so a
        // failed remove can reinsert them in place. FindStoredSession is called *before*
        // CloseTabAsync since soft-close erases the AgentSessionId from memory state the
        // restore needs. groupIndex is captured now because CloseTabAsync can auto-remove an
        // empty non-focused group — on rollback we use that index to re-insert the group.
        var snapshots = new List<(Session stored, EditorGroupViewModel group, int groupIndex, int indexInGroup)>();
        for (var gi = 0; gi < Groups.Count; gi++)
        {
            var group = Groups[gi];
            for (var i = 0; i < group.Tabs.Count; i++)
            {
                var tab = group.Tabs[i];
                if (!targetSessionIds.Contains(tab.Descriptor.Id)) { continue; }
                if (FindStoredSession(tab) is { } stored)
                {
                    snapshots.Add((stored, group, gi, i));
                }
            }
        }

        foreach (var (stored, _, _, _) in snapshots)
        {
            var tab = Groups.SelectMany(g => g.Tabs).FirstOrDefault(t => t.Descriptor.Id == stored.Id);
            if (tab is not null) { await CloseTabAsync(tab).ConfigureAwait(true); }
        }

        return () => RestoreClosedWorktreeSessionsAsync(projectId, snapshots);
    }

    /// <summary>
    /// Rollback partner of <see cref="CloseWorktreeSessionsAsync"/>. Only touches sessions that
    /// are *still* soft-closed at rollback time (a user might have manually resumed one between
    /// close and failure). Re-inserts any group that <see cref="CloseTabAsync"/> auto-removed.
    /// </summary>
    private async Task RestoreClosedWorktreeSessionsAsync(
        string projectId,
        List<(Session stored, EditorGroupViewModel group, int groupIndex, int indexInGroup)> snapshots)
    {
        // Group-level roll-back: CloseTabAsync auto-removes a now-empty non-focused group,
        // which orphans that EditorGroupViewModel. Re-insert any such group at its original
        // index before we start re-adding tabs — otherwise the restored tabs land in a
        // detached group that isn't in `Groups` and effectively disappear.
        foreach (var (_, group, groupIndex, _) in snapshots)
        {
            if (Groups.Contains(group)) { continue; }
            var targetIndex = Math.Clamp(groupIndex, 0, Groups.Count);
            Groups.Insert(targetIndex, group);
        }

        foreach (var (stored, group, _, indexInGroup) in snapshots)
        {
            var current = FindStoredSessionById(stored.Id);
            if (current is null || current.ClosedAt is null) { continue; }
            var restored = await _store.RestoreSessionAsync(stored.Id).ConfigureAwait(true);
            if (restored.IsFailure) { continue; }

            var agent = string.IsNullOrEmpty(stored.AgentId)
                || string.Equals(stored.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase)
                ? null
                : _agents.GetById(stored.AgentId!);
            var descriptor = agent is null
                ? _sessionManager.CreateShellSession(stored.WorktreePath, stored.Id)
                : _sessionManager.CreateAgentSession(stored.WorktreePath, agent, stored.Id,
                    resume: true, agentSessionId: stored.AgentSessionId);
            var vm = new SessionTabViewModel(descriptor, projectId, stored.AgentId, stored.DisplayName, agent?.Icon);
            var insertAt = Math.Clamp(indexInGroup, 0, group.Tabs.Count);
            group.Tabs.Insert(insertAt, vm);

            if (string.Equals(stored.AgentId, "claude", StringComparison.OrdinalIgnoreCase)
                && stored.AgentSessionId is { Length: > 0 } sid)
            {
                _telemetry?.Register(sid, stored.WorktreePath);
            }
            BeginClaudeAdoption(descriptor.Id, stored.AgentId, stored.WorktreePath, DateTimeOffset.UtcNow);
        }
        CloseGroupCommand.NotifyCanExecuteChanged();
        OnPropertyChanged(nameof(CanCloseGroup));
    }

    /// <summary>
    /// Restores a soft-closed session: clears <see cref="Session.ClosedAt"/> in the store and
    /// spawns a tab. Resumable agents (Claude/Codex) get a <c>--resume &lt;id&gt;</c> descriptor;
    /// shells respawn pwsh in the original cwd.
    /// Returns <c>false</c> when:
    /// <list type="bullet">
    ///   <item>the agent profile is no longer registered (non-shell session with unknown AgentId),</item>
    ///   <item>the worktree directory no longer exists on disk, or</item>
    ///   <item>the store rejects the restore (e.g. session id not found).</item>
    /// </list>
    /// </summary>
    private async Task<bool> TryRestoreSessionAsync(Project project, Worktree worktree, Session closed)
    {
        // Resolve agent / shell before touching the store so we never clear ClosedAt on a
        // session we can't actually reopen.
        var useShell = string.IsNullOrEmpty(closed.AgentId)
            || string.Equals(closed.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase);
        var agent = useShell ? null : _agents.GetById(closed.AgentId!);

        // Fix 2: non-shell AgentId with no matching profile — keep closed, do not create a
        // shell session with stale metadata.
        if (!useShell && agent is null)
        {
            _logger.LogWarning(
                "ReopenClosedSession: agent profile '{AgentId}' is no longer registered; " +
                "session '{SessionId}' kept closed.", closed.AgentId, closed.Id);
            return false;
        }

        // Fix 4: guard against a missing worktree directory — log and bail without clearing ClosedAt.
        if (!Directory.Exists(worktree.Path))
        {
            _logger.LogWarning(
                "ReopenClosedSession: worktree path '{Path}' no longer exists; " +
                "session '{SessionId}' kept closed.", worktree.Path, closed.Id);
            return false;
        }

        var restored = await _store.RestoreSessionAsync(closed.Id).ConfigureAwait(true);
        if (restored.IsFailure)
        {
            _logger.LogWarning("RestoreSession: {Error}", restored.Error);
            return false;
        }

        SessionDescriptor descriptor;
        if (agent is null)
        {
            descriptor = _sessionManager.CreateShellSession(worktree.Path, closed.Id);
        }
        else
        {
            descriptor = _sessionManager.CreateAgentSession(
                worktree.Path, agent, closed.Id, resume: true, agentSessionId: closed.AgentSessionId);
        }
        if (worktree.Branch is { Length: > 0 } branch)
        {
            descriptor = descriptor with { Title = $"{project.Name} · {branch}" };
        }

        var vm = new SessionTabViewModel(descriptor, project.Id, closed.AgentId, closed.DisplayName, agent?.Icon);
        FocusedGroup.Tabs.Add(vm);
        SelectedTab = vm;

        if (string.Equals(closed.AgentId, "claude", StringComparison.OrdinalIgnoreCase)
            && closed.AgentSessionId is { Length: > 0 } sid)
        {
            _telemetry?.Register(sid, worktree.Path);
        }
        BeginClaudeAdoption(descriptor.Id, closed.AgentId, worktree.Path, DateTimeOffset.UtcNow);
        OnPropertyChanged(nameof(CanCloseGroup));
        CloseGroupCommand.NotifyCanExecuteChanged();
        return true;
    }

    /// <summary>
    /// Public entry-point for reopening a closed session from the sidebar history surface.
    /// Looks up the parent project + worktree by id, does pre-flight checks (agent profile
    /// present, worktree directory exists), toasts on each failure, then delegates to
    /// <see cref="TryRestoreSessionAsync"/>. No-op when the session is no longer in the
    /// store (e.g. removed from history between right-click and click).
    /// </summary>
    [RelayCommand]
    private async Task ReopenClosedSessionAsync(string? sessionId)
    {
        if (string.IsNullOrEmpty(sessionId)) { return; }
        var hit = _store.Projects
            .SelectMany(p => p.Sessions.Select(s => (project: p, session: s)))
            .FirstOrDefault(x => x.session.Id == sessionId);
        if (hit.session is null || hit.session.ClosedAt is null) { return; }
        var worktree = hit.project.Worktrees.FirstOrDefault(w => w.Id == hit.session.WorktreeId)
                       ?? hit.project.Worktrees.FirstOrDefault(w => w.IsPrimary)
                       ?? hit.project.Worktrees.FirstOrDefault();
        if (worktree is null) { return; }

        // Pre-flight: agent profile check.
        var useShell = string.IsNullOrEmpty(hit.session.AgentId)
            || string.Equals(hit.session.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase);
        if (!useShell && _agents.GetById(hit.session.AgentId!) is null)
        {
            ErrToast(
                "Cannot reopen session",
                $"The '{hit.session.AgentId}' agent is no longer registered. " +
                "Remove this entry from history or restore the agent profile.");
            return;
        }

        // Pre-flight: directory check uses the session's stored WorktreePath, not the fallback
        // worktree's path. If the original directory is gone (deleted worktree, moved repo, etc.)
        // toast and bail regardless of which worktree object we resolved above.
        var sessionDir = hit.session.WorktreePath;
        if (!Directory.Exists(sessionDir))
        {
            ErrToast(
                "Cannot reopen session",
                $"Session directory '{sessionDir}' is gone. Remove this entry from history.");
            return;
        }

        // If the worktree's stored path differs from the session's WorktreePath (i.e. we fell
        // back to the primary/first worktree), use a synthetic Worktree with the correct path
        // so TryRestoreSessionAsync spawns the shell/agent in the right directory.
        var effectiveWorktree = string.Equals(worktree.Path, sessionDir, StringComparison.OrdinalIgnoreCase)
            ? worktree
            : worktree with { Path = sessionDir };

        var ok = await TryRestoreSessionAsync(hit.project, effectiveWorktree, hit.session).ConfigureAwait(true);
        if (!ok)
        {
            // TryRestoreSessionAsync returns false only when the store itself rejects the
            // restore (the two pre-flight paths above are caught above). Surface the failure
            // so the user knows the click wasn't a no-op.
            ErrToast("Reopen failed", "The session could not be restored. Check the log for details.");
        }
    }

    private void Toast(string title, string message, ToastSeverity severity)
    {
        if (_toasts is null) { return; }
        _toasts.Show(new ToastRequest(severity, title, message));
    }

    private void ErrToast(string title, string message)
    {
        if (_toasts is null) { return; }
        var actions = new List<ToastAction>
        {
            new("Copy", () =>
            {
                try { System.Windows.Clipboard.SetText(message); }
                catch (Exception ex) { _logger.LogDebug(ex, "Toast copy failed"); }
            }),
        };
        _toasts.Show(new ToastRequest(ToastSeverity.Err, title, message, Actions: actions));
    }

    /// <summary>
    /// Spawns a fresh session at the same worktree + agent as <paramref name="tab"/>.
    /// Use-case: keep one tab for long-running work and duplicate a parallel shell for ad-hoc commands.
    /// </summary>
    [RelayCommand]
    private async Task DuplicateTabAsync(SessionTabViewModel? tab)
    {
        tab ??= SelectedTab;
        if (tab is null) { return; }
        var folder = tab.Descriptor.WorkingDirectory;
        if (string.IsNullOrWhiteSpace(folder)) { return; }

        var project = _store.Projects.FirstOrDefault(p => p.Id == tab.ProjectId);
        if (project is null) { return; }
        var worktree = project.Worktrees.FirstOrDefault(w =>
            string.Equals(w.Path, folder, StringComparison.OrdinalIgnoreCase));

        var useShell = string.Equals(tab.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase);
        var agent = useShell || string.IsNullOrEmpty(tab.AgentId)
            ? null
            : _agents.GetById(tab.AgentId);

        // Duplicate semantically = a *fresh* conversation at the same worktree/agent, so
        // a new UUID (not the source tab's) if the agent supports session ids.
        var agentSessionId = AgentSupportsSessionId(agent) ? Guid.NewGuid().ToString() : null;
        var descriptor = agent is null
            ? _sessionManager.CreateShellSession(folder)
            : _sessionManager.CreateAgentSession(folder, agent, agentSessionId: agentSessionId);
        if (worktree?.Branch is { Length: > 0 } branch)
        {
            descriptor = descriptor with { Title = $"{project.Name} · {branch}" };
        }

        var session = new Session
        {
            Id = descriptor.Id,
            WorktreePath = descriptor.WorkingDirectory,
            WorktreeId = worktree?.Id,
            AgentId = agent?.Id ?? (useShell ? ShellSentinel : null),
            AgentSessionId = agentSessionId,
        };
        var added = await _store.AddSessionAsync(project.Id, session).ConfigureAwait(true);
        if (added.IsFailure) { _logger.LogWarning("DuplicateTab AddSession: {Error}", added.Error); return; }

        var vm = new SessionTabViewModel(descriptor, project.Id, session.AgentId, session.DisplayName, agent?.Icon);
        FocusedGroup.Tabs.Add(vm);
        SelectedTab = vm;
        BeginClaudeAdoption(descriptor.Id, agent?.Id, folder, DateTimeOffset.UtcNow);
        CloseGroupCommand.NotifyCanExecuteChanged();
    }

    [RelayCommand]
    private Task CloseTabAsync(SessionTabViewModel? tab) => CloseTabAsync(tab, hardRemove: false);

    /// <summary>
    /// Closes a tab. When <paramref name="hardRemove"/> is <c>false</c> (default) the session is
    /// <em>soft-closed</em> — the row stays in the store with <see cref="Session.ClosedAt"/> set,
    /// reachable from the worktree's history surface. Reopen logic in
    /// <c>ReopenClosedSessionAsync</c> resumes resumable agents via <c>--resume &lt;id&gt;</c> and
    /// respawns shells (and other non-resumable agents) in the original cwd. Callers that want
    /// the row gone for good (Restart) pass <c>true</c>.
    /// </summary>
    private async Task CloseTabAsync(SessionTabViewModel? tab, bool hardRemove)
    {
        tab ??= SelectedTab;
        if (tab is null)
        {
            // Ctrl+W on an empty focused group collapses the group: once every tab
            // is closed, another Ctrl+W removes the now-empty split.
            if (Groups.Count > 1 && FocusedGroup.Tabs.Count == 0) { CloseGroup(); }
            return;
        }

        // The tab lives in whichever group owns it — not necessarily the focused group
        // (a middle-click on a tab in another group hits this path too).
        var group = FindGroupContaining(tab) ?? FocusedGroup;
        var index = group.Tabs.IndexOf(tab);
        if (index >= 0) { group.Tabs.RemoveAt(index); }
        var storedForTab = FindStoredSession(tab);
        if (storedForTab?.AgentSessionId is { Length: > 0 } tsid) { _telemetry?.Unregister(tsid); }
        StopClaudeAdoption(tab.Descriptor.Id);

        // History: every closed session is preserved (soft-close) unless the caller asks for a
        // hard remove (Restart is the only such caller today). Reopen handles three buckets:
        // shells respawn pwsh in the original cwd; agents with ResumeByIdArgs + AgentSessionId
        // resume via --resume <id>; agents without resume support also respawn fresh.
        // Sessions whose store row vanished mid-flight (storedForTab is null) fall through to
        // RemoveSessionAsync — there is nothing to soft-close.
        var canSoftClose = !hardRemove && storedForTab is not null;
        if (canSoftClose)
        {
            await _store.SoftCloseSessionAsync(tab.Descriptor.Id).ConfigureAwait(true);
        }
        else
        {
            await _store.RemoveSessionAsync(tab.Descriptor.Id).ConfigureAwait(true);
        }

        // Auto-collapse a group when its last tab closes and another group exists — the
        // invariant is "groups exist to hold sessions; an empty one adds visual noise".
        if (group.Tabs.Count == 0 && Groups.Count > 1)
        {
            var gi = Groups.IndexOf(group);
            var neighbour = Groups[gi == 0 ? 1 : gi - 1];
            Groups.Remove(group);
            FocusedGroup = neighbour;
            SelectedTab = neighbour.SelectedTab;
        }
        else
        {
            var next = group.Tabs.Count == 0 ? null : group.Tabs[Math.Min(index, group.Tabs.Count - 1)];
            group.SelectedTab = next;
            if (ReferenceEquals(group, FocusedGroup)) { SelectedTab = next; }
        }
        CloseGroupCommand.NotifyCanExecuteChanged();
        OnPropertyChanged(nameof(CanCloseGroup));
    }

    private EditorGroupViewModel? FindGroupContaining(SessionTabViewModel tab)
        => Groups.FirstOrDefault(g => g.Tabs.Contains(tab));

    /// <summary>
    /// True when <paramref name="agent"/> accepts a caller-supplied session id on launch
    /// — i.e. has a <see cref="AgentProfile.SessionIdFlag"/>. Drives whether
    /// <c>NewSessionAsync</c> / <c>DuplicateTabAsync</c> mint a UUID to persist.
    /// </summary>
    private static bool AgentSupportsSessionId(AgentProfile? agent)
        => !string.IsNullOrEmpty(agent?.SessionIdFlag);

    /// <summary>
    /// Walks the store for the persisted <see cref="Session"/> backing <paramref name="tab"/>.
    /// Returns null when the tab is transient (not yet persisted) or the session was removed.
    /// </summary>
    private Session? FindStoredSession(SessionTabViewModel tab)
    {
        foreach (var p in _store.Projects)
        {
            foreach (var s in p.Sessions)
            {
                if (s.Id == tab.Descriptor.Id) { return s; }
            }
        }
        return null;
    }

    /// <summary>
    /// Transfers <paramref name="tab"/> from its current group into <paramref name="targetGroup"/>.
    /// Called from <c>EditorGroupView</c>'s drop handler.
    ///
    /// <para><b>Known v1 limitation:</b> the underlying pwsh process restarts. The source
    /// group's <c>ItemsControl</c> destroys its <c>ContentPresenter</c> for the tab when
    /// the VM leaves its <c>Tabs</c>, which unloads the hosted <c>EasyTerminalControl</c>
    /// and tears down the native HWND; the target group then creates a fresh one. The
    /// tab's title and worktree persist, but agent state resets. Preserving the HWND
    /// across groups needs a shared hosting pool — see the follow-up note in
    /// <c>docs/HANDOFF.md</c>.</para>
    /// </summary>
    public void MoveTabToGroup(SessionTabViewModel tab, EditorGroupViewModel targetGroup, int targetIndex = -1)
    {
        var sourceGroup = FindGroupContaining(tab);
        if (sourceGroup is null) { return; }
        if (ReferenceEquals(sourceGroup, targetGroup))
        {
            // Same-group drop: reorder (clamp index, no-op if unchanged).
            if (targetIndex < 0 || targetIndex > targetGroup.Tabs.Count - 1) { return; }
            var currentIdx = targetGroup.Tabs.IndexOf(tab);
            if (currentIdx == targetIndex) { return; }
            targetGroup.Tabs.Move(currentIdx, targetIndex);
            return;
        }

        // Cross-group moves force the terminal to respawn (HWND lifecycle — see doc comment
        // above). Rebind the descriptor to use ResumeArgs so Claude Code / codex / OpenCode
        // pick up the previous conversation in this working directory instead of starting
        // fresh. Shell-only tabs need no rebinding.
        var agent = string.IsNullOrEmpty(tab.AgentId)
                    || string.Equals(tab.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase)
            ? null
            : _agents.GetById(tab.AgentId);
        if (agent is not null)
        {
            var old = tab.Descriptor;
            var storedAgentId = FindStoredSession(tab)?.AgentSessionId;
            var resumed = _sessionManager.CreateAgentSession(old.WorkingDirectory, agent, id: old.Id, resume: true, agentSessionId: storedAgentId)
                with { Title = old.Title };
            tab.Rebind(resumed);
            // Resume-by-id (e.g. `claude --resume <id>`) appends to the existing jsonl, so the
            // telemetry watcher keeps tracking the same file across the pwsh respawn. Only when
            // we can't resume by id do we need to tear the tail down and rediscover a fresh one.
            var resumesById = storedAgentId is { Length: > 0 } && agent.ResumeByIdArgs.Count > 0;
            if (!resumesById)
            {
                if (storedAgentId is { Length: > 0 } oldId) { _telemetry?.Unregister(oldId); }
                BeginClaudeAdoption(tab.Descriptor.Id, agent.Id, old.WorkingDirectory, DateTimeOffset.UtcNow);
            }
        }

        sourceGroup.Tabs.Remove(tab);
        if (targetIndex < 0 || targetIndex > targetGroup.Tabs.Count)
        {
            targetGroup.Tabs.Add(tab);
        }
        else
        {
            targetGroup.Tabs.Insert(targetIndex, tab);
        }

        // Fix up selections: source falls back to its last remaining tab, target
        // adopts the moved one + takes window focus.
        if (sourceGroup.SelectedTab is null && sourceGroup.Tabs.Count > 0)
        {
            sourceGroup.SelectedTab = sourceGroup.Tabs[^1];
        }
        targetGroup.SelectedTab = tab;

        // Auto-collapse the source if it became empty — matches the CloseTab rule so
        // drag-the-last-tab-away behaves the same as close-the-last-tab.
        if (sourceGroup.Tabs.Count == 0 && Groups.Count > 1)
        {
            Groups.Remove(sourceGroup);
        }

        FocusedGroup = targetGroup;
        CloseGroupCommand.NotifyCanExecuteChanged();
        OnPropertyChanged(nameof(CanCloseGroup));
    }

    /// <summary>
    /// Kills <paramref name="tab"/> and re-spawns a fresh session at the same worktree with the
    /// same agent. Used from the sidebar context menu "Restart session" action — a recovery
    /// hatch for an agent process that got stuck without losing the surrounding tab layout.
    /// </summary>
    [RelayCommand]
    private async Task RestartSessionAsync(SessionTabViewModel? tab)
    {
        tab ??= SelectedTab;
        if (tab is null) { return; }

        await DuplicateTabAsync(tab).ConfigureAwait(true);
        // Hard-remove so the old row doesn't linger in history: restart semantically discards
        // prior conversation state, so the closed session must not appear in the explicit
        // history surface where the user would otherwise reopen it via RestoreSessionAsync.
        await CloseTabAsync(tab, hardRemove: true).ConfigureAwait(true);
    }

    [RelayCommand]
    private void NextTab()
    {
        var tabs = FocusedGroup.Tabs;
        if (tabs.Count < 2 || SelectedTab is null) { return; }
        var idx = tabs.IndexOf(SelectedTab);
        if (idx < 0) { return; }
        SelectedTab = tabs[(idx + 1) % tabs.Count];
    }

    [RelayCommand]
    private void PrevTab()
    {
        var tabs = FocusedGroup.Tabs;
        if (tabs.Count < 2 || SelectedTab is null) { return; }
        var idx = tabs.IndexOf(SelectedTab);
        if (idx < 0) { return; }
        SelectedTab = tabs[(idx - 1 + tabs.Count) % tabs.Count];
    }

    [RelayCommand]
    private async Task RenameSessionAsync(SessionTabViewModel? tab)
    {
        tab ??= SelectedTab;
        if (tab is null) { return; }
        var newName = Dialogs.RenameDialog.Prompt(tab.DisplayName);
        if (newName is null) { return; }
        await _store.RenameSessionAsync(tab.Descriptor.Id, newName).ConfigureAwait(true);
    }

    private void OnStoreChanged(object? sender, SessionStoreChange change)
    {
        void Apply()
        {
            // SessionAdded/SessionRenamed are intentionally not handled here. Tab membership is owned by
            // MainViewModel — NewSessionAsync, DuplicateTabAsync, TryRestoreSessionAsync, and
            // RestoreClosedWorktreeSessionsAsync each add their VM directly to FocusedGroup.Tabs and the
            // store mutation is the side-effect, not the source of truth. The sidebar projection
            // (SidebarViewModel.StoreSync) consumes those events for its own tree.
            switch (change)
            {
                case SessionStoreChange.Loaded loaded:
                    HydrateFromLoaded(loaded);
                    break;
                case SessionStoreChange.SessionRenamed renamed:
                    var t = AllTabs.FirstOrDefault(x => x.Descriptor.Id == renamed.SessionId);
                    if (t is not null)
                    {
                        t.DisplayName = renamed.NewName ?? t.Descriptor.Title;
                    }
                    break;
                case SessionStoreChange.WorktreeStatusUpdated wtStatus:
                    // Reflect dirty state on tab titles bound to this worktree.
                    foreach (var tab in AllTabs)
                    {
                        var p = _store.Projects.FirstOrDefault(x => x.Id == tab.ProjectId);
                        var session = p?.Sessions.FirstOrDefault(s => s.Id == tab.Descriptor.Id);
                        if (session?.WorktreeId != wtStatus.WorktreeId) { continue; }

                        // Top-bar spec §4: tab label is just "Project · branch". Dirty state
                        // is carried by the sidebar ("chg" slug) and the status dot — no glyph
                        // suffixing the title.
                        tab.DisplayName = tab.Descriptor.Title;
                    }
                    break;
                case SessionStoreChange.SessionRemoved removed:
                    foreach (var g in Groups)
                    {
                        var tr = g.Tabs.FirstOrDefault(x => x.Descriptor.Id == removed.SessionId);
                        if (tr is not null)
                        {
                            g.Tabs.Remove(tr);
                            break;
                        }
                    }
                    CloseGroupCommand.NotifyCanExecuteChanged();
                    break;
            }
        }

        if (Application.Current?.Dispatcher is { } dispatcher && !dispatcher.CheckAccess())
        {
            dispatcher.Invoke(Apply);
        }
        else
        {
            Apply();
        }
    }

    private void HydrateFromLoaded(SessionStoreChange.Loaded loaded)
    {
        // If PrepareLayoutFromPersistence ran, Groups already has the right count and
        // every tab below routes to its saved group via _pendingSessionToGroup. Otherwise
        // tabs all land in Groups[0] (the shared-instance default group).
        foreach (var g in Groups) { g.Tabs.Clear(); }
        Tabs.Clear();
        foreach (var p in loaded.Projects)
        {
            foreach (var s in p.Sessions)
            {
                // Closed sessions are kept on disk for explicit reopen via the sidebar
                // history surface — they don't materialise as tabs at startup so we don't
                // spawn dozens of ghost pwsh children.
                if (s.ClosedAt is not null) { continue; }
                if (!Directory.Exists(s.WorktreePath)) { continue; }
                var agent = string.IsNullOrEmpty(s.AgentId) || string.Equals(s.AgentId, ShellSentinel, StringComparison.OrdinalIgnoreCase)
                    ? null
                    : _agents.GetById(s.AgentId);
                // resume=true on hydration so Claude Code / codex / OpenCode reopen the
                // conversation that was alive in that working directory when CodeScope last
                // closed — matches the drag-move behaviour and means app-restart isn't a
                // context-destroying event.
                var descriptor = agent is null
                    ? _sessionManager.CreateShellSession(s.WorktreePath, s.Id)
                    : _sessionManager.CreateAgentSession(s.WorktreePath, agent, s.Id, resume: true, agentSessionId: s.AgentSessionId);

                // Title reflects `{project} · {branch}` when we have the branch.
                var wt = p.Worktrees.FirstOrDefault(w => w.Id == s.WorktreeId);
                if (wt?.Branch is { Length: > 0 } branch)
                {
                    descriptor = descriptor with { Title = $"{p.Name} · {branch}" };
                }

                var vm = new SessionTabViewModel(descriptor, p.Id, s.AgentId, s.DisplayName);
                var targetIdx = 0;
                if (_pendingSessionToGroup is not null
                    && _pendingSessionToGroup.TryGetValue(s.Id, out var saved)
                    && saved >= 0 && saved < Groups.Count)
                {
                    targetIdx = saved;
                }
                Groups[targetIdx].Tabs.Add(vm);
            }
        }

        // Drop groups that ended up empty after routing — e.g. a persisted layout
        // mapped a session to a group, but that session was since deleted from
        // projects.json, or a worktree was removed. Without this the user sees a
        // dead empty column on the right at every startup. Keep at least one group.
        for (var i = Groups.Count - 1; i >= 0 && Groups.Count > 1; i--)
        {
            if (Groups[i].Tabs.Count == 0) { Groups.RemoveAt(i); }
        }

        // Restore focus to the persisted group (clamped to surviving group count).
        // Each group's SelectedTab mirrors its first tab; window-global SelectedTab
        // then follows the focused group's selection through OnFocusedGroupChanged.
        foreach (var g in Groups)
        {
            if (g.SelectedTab is null && g.Tabs.Count > 0) { g.SelectedTab = g.Tabs[0]; }
        }
        var focusIdx = Math.Clamp(_pendingFocusedIndex, 0, Groups.Count - 1);
        FocusedGroup = Groups[focusIdx];
        SelectedTab = FocusedGroup.SelectedTab ?? Groups.SelectMany(g => g.Tabs).FirstOrDefault();

        // One-shot: consume the layout so subsequent SessionStoreChange.Loaded events
        // (rare — usually only at startup) don't re-apply stale mappings.
        _pendingSessionToGroup = null;
        _pendingFocusedIndex = 0;

        CloseGroupCommand.NotifyCanExecuteChanged();
        OnPropertyChanged(nameof(CanCloseGroup));

        // Hydrate path: persisted AgentSessionId values from pre-session-17 builds point at
        // abandoned transcripts (Claude Code v2.1.118+ rotates ids on /clear and on resume).
        // Kick off a fresh adoption watch per Claude tab — discovery supplants the persisted
        // id the moment the new pwsh session writes its first jsonl line.
        var now = DateTimeOffset.UtcNow;
        foreach (var p in loaded.Projects)
        {
            foreach (var s in p.Sessions)
            {
                // Skip watches for soft-closed sessions — no tab to update.
                if (s.ClosedAt is not null) { continue; }

                // Eager telemetry register for hydrated Claude tabs. Resume-by-id (`claude
                // --resume <id>`) reuses the existing jsonl, so ClaudeSessionDiscovery's
                // `since >= file.LastWrite` filter skips it and the status bar would stay
                // frozen until the user fires the next turn. Registering here synchronously
                // replays the persisted transcript and seeds the snapshot immediately; the
                // adoption watch still runs in parallel to catch `/clear` rotations.
                if (string.Equals(s.AgentId, "claude", StringComparison.OrdinalIgnoreCase)
                    && s.AgentSessionId is { Length: > 0 } sid)
                {
                    _telemetry?.Register(sid, s.WorktreePath);
                }
                BeginClaudeAdoption(s.Id, s.AgentId, s.WorktreePath, now);
            }
        }
    }

    /// <summary>
    /// Starts a discovery watch on <paramref name="workingDir"/> and, on first new jsonl,
    /// persists the adopted id onto <paramref name="storedSessionId"/> and registers the
    /// Claude telemetry tail. No-op for non-Claude agents or when discovery isn't wired
    /// (tests / headless).
    /// </summary>
    private void BeginClaudeAdoption(string storedSessionId, string? agentId, string workingDir, DateTimeOffset since)
    {
        if (_discovery is null) { return; }
        if (!string.Equals(agentId, "claude", StringComparison.OrdinalIgnoreCase)) { return; }
        if (string.IsNullOrWhiteSpace(workingDir)) { return; }

        StopClaudeAdoption(storedSessionId);

        // Watch stays alive for the tab's lifetime: Claude Code rotates its session id on
        // `/clear` by writing a fresh jsonl in the same cwd dir. Each rotation fires the
        // callback with the new id; we unregister the old telemetry tail, persist the new
        // id, and register the new tail so the status bar keeps tracking. Disposed on
        // tab close / cross-group drop via StopClaudeAdoption.
        var handle = _discovery.Watch(workingDir, since, (adoptedId, _path) =>
        {
            var app = Application.Current;
            async Task ApplyAsync()
            {
                // Skip if this id is already the one we're persisting — the initial launch
                // adoption fires on startup and the poll fallback can re-fire on a live file.
                var currentId = FindStoredSessionById(storedSessionId)?.AgentSessionId;
                if (string.Equals(currentId, adoptedId, StringComparison.OrdinalIgnoreCase))
                {
                    // Still make sure telemetry is registered — a just-loaded hydrate may need it.
                    _telemetry?.Register(adoptedId, workingDir);
                    return;
                }

                if (!string.IsNullOrEmpty(currentId)) { _telemetry?.Unregister(currentId!); }
                _telemetry?.Register(adoptedId, workingDir);
                var result = await _store.UpdateAgentSessionIdAsync(storedSessionId, adoptedId).ConfigureAwait(true);
                if (result.IsFailure)
                {
                    _logger.LogDebug("Adoption persist failed for {Sid}: {Error}", storedSessionId, result.Error);
                }
            }
            if (app?.Dispatcher is { } d && !d.CheckAccess())
            {
                d.BeginInvoke(new Action(() => { _ = ApplyAsync(); }));
            }
            else
            {
                _ = ApplyAsync();
            }
        });
        _discoveryWatches[storedSessionId] = handle;
    }

    private Session? FindStoredSessionById(string storedSessionId)
    {
        foreach (var p in _store.Projects)
        {
            foreach (var s in p.Sessions)
            {
                if (s.Id == storedSessionId) { return s; }
            }
        }
        return null;
    }

    private void StopClaudeAdoption(string storedSessionId)
    {
        if (_discoveryWatches.Remove(storedSessionId, out var handle))
        {
            try { handle.Dispose(); }
            catch (Exception ex) { _logger.LogTrace(ex, "StopClaudeAdoption: dispose threw for {SessionId}", storedSessionId); }
        }
    }

    /// <summary>
    /// Telemetry update dispatch. Service raises on a non-UI thread (FileSystemWatcher pool),
    /// so marshal onto the dispatcher before touching ObservableObject properties.
    /// </summary>
    private void OnTelemetryUpdated(object? sender, ClaudeSessionTelemetry snap)
    {
        var app = Application.Current;
        if (app?.Dispatcher is { } d && !d.CheckAccess())
        {
            d.BeginInvoke(() => ApplyTelemetry(snap));
            return;
        }
        ApplyTelemetry(snap);
    }

    /// <summary>
    /// Emits notification entries on semantic transitions of a session's activity FSM:
    /// <list type="bullet">
    ///   <item>* → <c>PendingToolUse</c> → "Needs attention" (Wait pulse is visual-only; this persists).</item>
    ///   <item><c>PendingToolUse</c>/<c>Composing</c> → <c>Idle</c> → "Ready" (response delivered).</item>
    /// </list>
    /// Suppresses the notification when the owning tab is currently focused — the user is
    /// watching that session, so the bell adds noise rather than signal.
    /// </summary>
    private void PushActivityNotification(SessionTabViewModel tab, ClaudeSessionTelemetry snap)
    {
        if (_notifications is null) { return; }
        var prev = _lastActivity.TryGetValue(snap.SessionId, out var p) ? p : ClaudeActivityState.Unknown;
        _lastActivity[snap.SessionId] = snap.Activity;
        if (prev == snap.Activity) { return; }
        // Don't pester the user about the tab they're actively staring at.
        if (ReferenceEquals(tab, SelectedTab)) { return; }

        NotificationKind? kind = null;
        string title = string.Empty;
        string detail = string.Empty;
        switch (snap.Activity)
        {
            case ClaudeActivityState.PendingToolUse:
                kind = NotificationKind.SessionWaiting;
                title = "Needs attention";
                detail = "Agent paused on a tool prompt.";
                break;
            case ClaudeActivityState.Idle when prev is ClaudeActivityState.PendingToolUse or ClaudeActivityState.Composing:
                kind = NotificationKind.SessionReady;
                title = "Ready";
                detail = snap.LastTurnDuration is { } d
                    ? $"Turn complete · {FormatDuration(d.TotalSeconds)}"
                    : "Turn complete.";
                break;
        }
        if (kind is null) { return; }

        _notifications.Push(new NotificationEntry(
            Id: Guid.NewGuid().ToString("N"),
            SessionId: snap.SessionId,
            SessionTitle: tab.DisplayName,
            Kind: kind.Value,
            Title: title,
            Detail: detail,
            Timestamp: DateTimeOffset.Now,
            IsRead: false));
    }

    private void ApplyActivityToStatus(SessionTabViewModel tab, ClaudeActivityState activity)
    {
        tab.Status = activity switch
        {
            ClaudeActivityState.Composing => TabStatus.Busy,
            ClaudeActivityState.PendingToolUse => TabStatus.Busy,
            ClaudeActivityState.Idle => TabStatus.Idle,
            _ => tab.Status,
        };
    }

    private void ApplyTelemetry(ClaudeSessionTelemetry snap)
    {
        string? matchedTabId = null;
        foreach (var tab in AllTabs)
        {
            var stored = FindStoredSession(tab);
            if (stored?.AgentSessionId == snap.SessionId)
            {
                tab.TokensUsed = snap.ContextTokens;
                tab.TurnCount = snap.TurnCount;
                tab.LastTurnDurationSec = snap.LastTurnDuration?.TotalSeconds ?? 0;
                if (snap.ContextWindowTokens > 0) { tab.ContextWindowTokens = snap.ContextWindowTokens; }
                ApplyActivityToStatus(tab, snap.Activity);
                PushActivityNotification(tab, snap);
                matchedTabId = tab.Descriptor.Id;
                break;
            }
        }

        // Sidebar's worktree.Sessions holds mirror VMs with independent Status fields;
        // keep them in lockstep so the sidebar dot (+ its pulse storyboard) lights up
        // when the focused tab's agent enters Wait.
        if (matchedTabId is not null && Sidebar is not null)
        {
            foreach (var p in Sidebar.Projects)
            {
                foreach (var w in p.Worktrees)
                {
                    foreach (var s in w.Sessions)
                    {
                        if (s.Descriptor.Id == matchedTabId)
                        {
                            ApplyActivityToStatus(s, snap.Activity);
                        }
                    }
                }
            }
        }
    }
}
