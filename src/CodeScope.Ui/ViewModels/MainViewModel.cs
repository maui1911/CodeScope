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
    private readonly IPiTelemetryService? _piTelemetry;
    private readonly IPiSessionDiscovery? _piDiscovery;
    private readonly IOpenCodeTelemetryService? _opencodeTelemetry;
    private readonly IOpenCodeSessionDiscovery? _opencodeDiscovery;
    private readonly ICopilotTelemetryService? _copilotTelemetry;
    private readonly ICopilotSessionDiscovery? _copilotDiscovery;
    private readonly IIdleToastNotifier? _idleNotifier;
    private readonly ITaskbarBadgeService? _taskbarBadge;
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
        IToastService? toasts = null,
        ISessionViewHostPool? sessionViewPool = null,
        IPiTelemetryService? piTelemetry = null,
        IPiSessionDiscovery? piDiscovery = null,
        IOpenCodeTelemetryService? opencodeTelemetry = null,
        IOpenCodeSessionDiscovery? opencodeDiscovery = null,
        ICopilotTelemetryService? copilotTelemetry = null,
        ICopilotSessionDiscovery? copilotDiscovery = null,
        IIdleToastNotifier? idleNotifier = null,
        ITaskbarBadgeService? taskbarBadge = null)
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
        _piTelemetry = piTelemetry;
        _piDiscovery = piDiscovery;
        _opencodeTelemetry = opencodeTelemetry;
        _opencodeDiscovery = opencodeDiscovery;
        _copilotTelemetry = copilotTelemetry;
        _copilotDiscovery = copilotDiscovery;
        _idleNotifier = idleNotifier;
        _taskbarBadge = taskbarBadge;
        SessionViewPool = sessionViewPool;
        if (_telemetry is not null) { _telemetry.Updated += OnTelemetryUpdated; }
        // All telemetry backends emit ClaudeSessionTelemetry — same handler routes any source
        // through ApplyTelemetry, which keys by AgentSessionId so backend identity doesn't matter.
        if (_piTelemetry is not null) { _piTelemetry.Updated += OnTelemetryUpdated; }
        if (_opencodeTelemetry is not null) { _opencodeTelemetry.Updated += OnTelemetryUpdated; }
        if (_copilotTelemetry is not null) { _copilotTelemetry.Updated += OnTelemetryUpdated; }
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

    /// <summary>
    /// Owns the lifecycle of <see cref="Views.SessionTabView"/> instances so a single view
    /// (and its inner HwndHost / ConPTY child) survives reparent across editor groups on
    /// drag-between-groups. May be null in unit tests / design-time; <c>EditorGroupView</c>
    /// degrades to per-group construction when null.
    /// </summary>
    public ISessionViewHostPool? SessionViewPool { get; }

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

    /// <summary>
    /// Mirror visibility into <see cref="OverviewViewModel.IsActive"/> so the Overview VM
    /// suppresses the per-tick rebuild while it isn't on stage. Issue #30.
    /// </summary>
    partial void OnIsOverviewVisibleChanged(bool value)
    {
        if (Overview is { } o) { o.IsActive = value; }
    }

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

        // Tab labels always follow the worktree's current branch (§4); we do not pass
        // session.DisplayName as an override. Sidebar history rows still honor it.
        var vm = new SessionTabViewModel(descriptor, project.Id, session.AgentId, displayNameOverride: null, icon: agent?.Icon);
        FocusedGroup.Tabs.Add(vm);
        SelectedTab = vm;
        BeginAgentAdoption(descriptor.Id, agent?.Id, targetFolder, DateTimeOffset.UtcNow);
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

            if (stored.AgentSessionId is { Length: > 0 } sid)
            {
                RegisterAgentTelemetry(stored.AgentId, sid, stored.WorktreePath);
            }
            BeginAgentAdoption(descriptor.Id, stored.AgentId, stored.WorktreePath, DateTimeOffset.UtcNow);
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

        if (closed.AgentSessionId is { Length: > 0 } sid)
        {
            RegisterAgentTelemetry(closed.AgentId, sid, worktree.Path);
        }
        BeginAgentAdoption(descriptor.Id, closed.AgentId, worktree.Path, DateTimeOffset.UtcNow);
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

        // Tab labels always follow the worktree's current branch (§4); we do not pass
        // session.DisplayName as an override. Sidebar history rows still honor it.
        var vm = new SessionTabViewModel(descriptor, project.Id, session.AgentId, displayNameOverride: null, icon: agent?.Icon);
        FocusedGroup.Tabs.Add(vm);
        SelectedTab = vm;
        BeginAgentAdoption(descriptor.Id, agent?.Id, folder, DateTimeOffset.UtcNow);
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
        if (storedForTab?.AgentSessionId is { Length: > 0 } tsid)
        {
            UnregisterAgentTelemetry(storedForTab.AgentId, tsid);
        }
        StopAgentAdoption(tab.Descriptor.Id);
        // The pool owns the SessionTabView; release here so ConPTY teardown actually runs.
        // (Removed Unloaded teardown on the view itself — see SessionTabView ctor.)
        SessionViewPool?.Release(tab.Descriptor.Id);

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
    /// Selects the tab whose persisted session has the given agent-side session id (Claude
    /// UUID etc.). Called by the host after a Windows toast click so the right tab is in
    /// front when the window restores. No-op if no live tab matches.
    /// </summary>
    public void ActivateSessionByAgentSessionId(string agentSessionId)
    {
        if (string.IsNullOrEmpty(agentSessionId)) { return; }
        var target = AllTabs.FirstOrDefault(t => FindStoredSession(t)?.AgentSessionId == agentSessionId);
        if (target is not null) { SelectedTab = target; }
    }

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
    /// <para>Cross-group moves preserve the underlying ConPTY child + scrollback: the
    /// <see cref="Views.SessionTabView"/> for the tab is owned by <see cref="ISessionViewHostPool"/>,
    /// and both groups' <c>ContentControl</c> resolve their content from that same pool. Moving
    /// the VM between <c>Tabs</c> collections triggers each group's <c>SelectedTab</c>-changed
    /// handler, which re-acquires the same view from the pool — WPF reparents the existing
    /// HwndHost (and the inner native HWND) under the new <c>ContentControl</c> rather than
    /// destroying it. No descriptor rebind, no agent resume, no telemetry hiccup.</para>
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

        sourceGroup.Tabs.Remove(tab);

        // Fix the source selection BEFORE the target adopts the view. Two paths
        // collapse to the same fix:
        //   (a) the GroupStripView ListBox's two-way SelectedItem binding nulled
        //       sourceGroup.SelectedTab when its current item left the collection
        //       (Selector default behaviour). Source would otherwise render blank
        //       even though more tabs remain.
        //   (b) the binding didn't fire (e.g. a non-selected tab was dragged but
        //       SelectedTab still happened to be the moved tab) — without a fresh
        //       value, the source EditorGroupView never raises a SelectedTab-changed
        //       event and its ContentControl.Content stays pinned to the moved view.
        //       The target's attach would then throw "element already has a logical
        //       parent". (Pool.Acquire defends against this anyway, but fixing it
        //       here keeps the source slot from looking stale for a frame.)
        // Always re-pick the last remaining tab in source unless source genuinely
        // selected a different non-removed tab.
        if (sourceGroup.SelectedTab is null || ReferenceEquals(sourceGroup.SelectedTab, tab))
        {
            sourceGroup.SelectedTab = sourceGroup.Tabs.Count > 0 ? sourceGroup.Tabs[^1] : null;
        }

        if (targetIndex < 0 || targetIndex > targetGroup.Tabs.Count)
        {
            targetGroup.Tabs.Add(tab);
        }
        else
        {
            targetGroup.Tabs.Insert(targetIndex, tab);
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

    /// <summary>
    /// Recompute the "Project · branch" label for every tab whose session lives in the given
    /// worktree. Source of truth is the freshly-mutated <see cref="Worktree.Branch"/> on the
    /// store — the same value the sidebar reads, which is why the sidebar already kept up while
    /// the tabs went stale (issue #21).
    /// </summary>
    private void RefreshTabTitlesForWorktree(string projectId, string worktreeId)
    {
        var project = _store.Projects.FirstOrDefault(p => p.Id == projectId);
        if (project is null) { return; }
        var worktree = project.Worktrees.FirstOrDefault(w => w.Id == worktreeId);
        if (worktree is null) { return; }

        var branch = string.IsNullOrWhiteSpace(worktree.Branch)
            ? (worktree.IsPrimary
                ? (string.IsNullOrWhiteSpace(project.DefaultBranch) ? "main" : project.DefaultBranch)
                : "(no branch)")
            : worktree.Branch;
        var title = $"{project.Name} · {branch}";

        foreach (var tab in AllTabs)
        {
            if (tab.ProjectId != projectId) { continue; }
            var session = project.Sessions.FirstOrDefault(s => s.Id == tab.Descriptor.Id);
            if (session?.WorktreeId != worktreeId) { continue; }
            tab.DisplayName = title;
        }
    }

    private void OnStoreChanged(object? sender, SessionStoreChange change)
    {
        void Apply()
        {
            // SessionAdded/SessionRenamed/Soft-close are intentionally not handled here. Tab membership is
            // owned by MainViewModel — NewSessionAsync, DuplicateTabAsync, TryRestoreSessionAsync, and
            // RestoreClosedWorktreeSessionsAsync each add their VM directly to FocusedGroup.Tabs and the
            // store mutation is the side-effect, not the source of truth. SessionRenamed is also ignored:
            // tab titles always follow the worktree's current branch (§4), so any manual rename done
            // through the sidebar (closed-session history) only affects the sidebar label. The sidebar
            // projection (SidebarViewModel.StoreSync) consumes the rename event for its own tree.
            switch (change)
            {
                case SessionStoreChange.Loaded loaded:
                    HydrateFromLoaded(loaded);
                    break;
                case SessionStoreChange.WorktreeStatusUpdated wtStatus:
                    // Branch can change underneath us — `git checkout other`, `git branch -m`, or the
                    // status poll picking up an externally-renamed branch — so we recompute the
                    // "Project · branch" label every time. Top-bar spec §4: dirty state is carried by
                    // the sidebar dot, not by the tab label.
                    RefreshTabTitlesForWorktree(wtStatus.ProjectId, wtStatus.WorktreeId);
                    break;
                case SessionStoreChange.WorktreeRenamed wtRenamed:
                    // A worktree rename is a cheap trigger to recompute titles so the tab strip stays
                    // consistent with the latest worktree state, even when the effective title is
                    // unchanged.
                    RefreshTabTitlesForWorktree(wtRenamed.ProjectId, wtRenamed.WorktreeId);
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
                    // External removal (e.g. cascade in SessionStore) must also release the
                    // pooled view — otherwise the ConPTY child lingers, holds its cwd, and
                    // breaks the next `git worktree remove`.
                    SessionViewPool?.Release(removed.SessionId);
                    // Note: _telemetry?.Unregister is NOT called here because telemetry is
                    // keyed by AgentSessionId, not SessionDescriptor.Id, and the store
                    // change payload only carries SessionDescriptor.Id. The normal close
                    // path (CloseTabAsync) already Unregisters using the correct key before
                    // this handler fires; the external-only path (store removes without
                    // CloseTabAsync) is a fire-and-forget edge case that can't recover the
                    // AgentSessionId without an extra lookup.
                    StopAgentAdoption(removed.SessionId);
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

                var vm = new SessionTabViewModel(descriptor, p.Id, s.AgentId, displayNameOverride: null);
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
                if (s.AgentSessionId is { Length: > 0 } sid)
                {
                    RegisterAgentTelemetry(s.AgentId, sid, s.WorktreePath);
                }
                BeginAgentAdoption(s.Id, s.AgentId, s.WorktreePath, now);
            }
        }
    }

    /// <summary>
    /// Starts a discovery watch on <paramref name="workingDir"/> and, on first adoption-worthy
    /// transcript event, persists the adopted id onto <paramref name="storedSessionId"/> and
    /// registers the matching telemetry tail. Routes by <paramref name="agentId"/> to the
    /// Claude, Pi, or OpenCode backend; no-op for other agents or when the relevant services
    /// aren't wired (tests / headless).
    /// </summary>
    private void BeginAgentAdoption(string storedSessionId, string? agentId, string workingDir, DateTimeOffset since)
    {
        if (string.IsNullOrWhiteSpace(workingDir)) { return; }

        IDisposable? handle = null;
        if (string.Equals(agentId, "claude", StringComparison.OrdinalIgnoreCase) && _discovery is not null)
        {
            // Claude Code rotates its session id on `/clear` by writing a fresh jsonl in the
            // same cwd dir; the watch stays alive for the tab's lifetime to catch each rotation.
            handle = _discovery.Watch(workingDir, since, (adoptedId, _path) =>
                ApplyAdoption(storedSessionId, adoptedId, agentId, workingDir));
        }
        else if (string.Equals(agentId, "pi", StringComparison.OrdinalIgnoreCase) && _piDiscovery is not null)
        {
            // Pi doesn't rotate ids on /clear (each `pi` invocation = its own session file),
            // but a long-running tab can still see fresh files when the user manually starts
            // a new conversation in the same cwd, so the same long-lived watch model applies.
            handle = _piDiscovery.Watch(workingDir, since, (adoptedId, _path) =>
                ApplyAdoption(storedSessionId, adoptedId, agentId, workingDir));
        }
        else if (string.Equals(agentId, "opencode", StringComparison.OrdinalIgnoreCase) && _opencodeDiscovery is not null)
        {
            // OpenCode mints its own session id and stores per-message JSON files. Adoption
            // fires once per session id (the first assistant message exposes the cwd we match
            // against). The watch stays alive for the tab's lifetime so a `/sessions` switch
            // inside OpenCode that creates a new session is also picked up.
            handle = _opencodeDiscovery.Watch(workingDir, since, (adoptedId, _path) =>
                ApplyAdoption(storedSessionId, adoptedId, agentId, workingDir));
        }
        else if (string.Equals(agentId, "copilot", StringComparison.OrdinalIgnoreCase) && _copilotDiscovery is not null)
        {
            // Copilot CLI stores each session in its own UUID-named directory under
            // ~/.copilot/session-state/. Adoption matches by cwd from workspace.yaml
            // or the session.start event in events.jsonl.
            handle = _copilotDiscovery.Watch(workingDir, since, (adoptedId, _path) =>
                ApplyAdoption(storedSessionId, adoptedId, agentId, workingDir));
        }

        if (handle is null) { return; }
        StopAgentAdoption(storedSessionId);
        _discoveryWatches[storedSessionId] = handle;
    }

    /// <summary>
    /// Adoption callback shared by every supported agent backend (Claude, Pi, OpenCode, Copilot):
    /// persist the new id onto the store row and (re-)register the matching telemetry
    /// tail. Marshals onto the dispatcher because the discovery callback fires on a
    /// watcher thread.
    /// </summary>
    private void ApplyAdoption(string storedSessionId, string adoptedId, string? agentId, string workingDir)
    {
        var app = Application.Current;
        async Task ApplyAsync()
        {
            var currentId = FindStoredSessionById(storedSessionId)?.AgentSessionId;
            if (string.Equals(currentId, adoptedId, StringComparison.OrdinalIgnoreCase))
            {
                // Already persisted — initial launch adoption + poll fallback can both fire.
                // Re-register defensively for the hydrate-on-startup case.
                RegisterAgentTelemetry(agentId, adoptedId, workingDir);
                return;
            }

            if (!string.IsNullOrEmpty(currentId)) { UnregisterAgentTelemetry(agentId, currentId!); }
            RegisterAgentTelemetry(agentId, adoptedId, workingDir);
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
    }

    /// <summary>Routes telemetry registration to the right backend by agent id.</summary>
    private void RegisterAgentTelemetry(string? agentId, string sessionId, string workingDir)
    {
        if (string.IsNullOrEmpty(sessionId)) { return; }
        if (string.Equals(agentId, "claude", StringComparison.OrdinalIgnoreCase))
        {
            _telemetry?.Register(sessionId, workingDir);
        }
        else if (string.Equals(agentId, "pi", StringComparison.OrdinalIgnoreCase))
        {
            _piTelemetry?.Register(sessionId, workingDir);
        }
        else if (string.Equals(agentId, "opencode", StringComparison.OrdinalIgnoreCase))
        {
            _opencodeTelemetry?.Register(sessionId, workingDir);
        }
        else if (string.Equals(agentId, "copilot", StringComparison.OrdinalIgnoreCase))
        {
            _copilotTelemetry?.Register(sessionId, workingDir);
        }
    }

    /// <summary>Routes telemetry unregister to the right backend. Idempotent — calls all when agent unknown.</summary>
    private void UnregisterAgentTelemetry(string? agentId, string sessionId)
    {
        if (string.IsNullOrEmpty(sessionId)) { return; }
        if (string.IsNullOrEmpty(agentId))
        {
            // Unknown source (cleanup path) — best-effort kick all backends.
            _telemetry?.Unregister(sessionId);
            _piTelemetry?.Unregister(sessionId);
            _opencodeTelemetry?.Unregister(sessionId);
            _copilotTelemetry?.Unregister(sessionId);
            return;
        }
        if (string.Equals(agentId, "claude", StringComparison.OrdinalIgnoreCase))
        {
            _telemetry?.Unregister(sessionId);
        }
        else if (string.Equals(agentId, "pi", StringComparison.OrdinalIgnoreCase))
        {
            _piTelemetry?.Unregister(sessionId);
        }
        else if (string.Equals(agentId, "opencode", StringComparison.OrdinalIgnoreCase))
        {
            _opencodeTelemetry?.Unregister(sessionId);
        }
        else if (string.Equals(agentId, "copilot", StringComparison.OrdinalIgnoreCase))
        {
            _copilotTelemetry?.Unregister(sessionId);
        }
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

    private void StopAgentAdoption(string storedSessionId)
    {
        if (_discoveryWatches.Remove(storedSessionId, out var handle))
        {
            try { handle.Dispose(); }
            catch (Exception ex) { _logger.LogTrace(ex, "StopAgentAdoption: dispose threw for {SessionId}", storedSessionId); }
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
        var prev = _lastActivity.TryGetValue(snap.SessionId, out var p) ? p : ClaudeActivityState.Unknown;
        _lastActivity[snap.SessionId] = snap.Activity;
        if (prev == snap.Activity) { return; }

        // Windows Action-Center toast on turn-complete — visible while the whole app is
        // minimized (which is the only time it actually fires; the notifier owns the
        // WindowState.Minimized gate). Independent of the in-app bell's SelectedTab
        // suppression: a minimized window means the user can't see the tab no matter which
        // one it is. AgentSessionId is the click-routing key.
        if (snap.Activity == ClaudeActivityState.Idle
            && prev is ClaudeActivityState.PendingToolUse or ClaudeActivityState.Composing
            && _idleNotifier is not null
            && !string.IsNullOrEmpty(snap.SessionId))
        {
            var detailLine = snap.LastTurnDuration is { } dur
                ? $"Turn complete · {FormatDuration(dur.TotalSeconds)}"
                : "Turn complete.";
            _idleNotifier.NotifyTurnComplete(snap.SessionId, tab.DisplayName, detailLine);
        }

        if (_notifications is null) { return; }
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
