using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Dialogs;
using NoScope.CodeScope.Ui.Services;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Sidebar root VM. Projects → Worktrees → Sessions, mirrored from <see cref="ISessionStore"/>.
/// Implementation is split across partial files:
/// <list type="bullet">
///   <item><c>SidebarViewModel.Commands.cs</c> — <c>[RelayCommand]</c> actions bound to context menus.</item>
///   <item><c>SidebarViewModel.StoreSync.cs</c> — change-event projection into the observable tree.</item>
/// </list>
/// </summary>
public sealed partial class SidebarViewModel : ObservableObject
{
    private readonly ISessionStore _store;
    private readonly IPullRequestService? _pullRequests;
    private readonly IToastService? _toasts;
    private readonly IAgentRegistry? _agents;
    private readonly IGitService? _git;
    private readonly ILogger<SidebarViewModel> _logger;
    private readonly Func<string?> _pickFolder;
    private readonly Func<NewWorktreeRequest, NewWorktreeResult?> _pickNewWorktree;

    public SidebarViewModel(
        ISessionStore store,
        ILogger<SidebarViewModel> logger,
        Func<string?>? pickFolder = null,
        Func<NewWorktreeRequest, NewWorktreeResult?>? pickNewWorktree = null,
        IPullRequestService? pullRequests = null,
        IToastService? toasts = null,
        IAgentRegistry? agents = null,
        IGitService? git = null)
    {
        _store = store;
        _pullRequests = pullRequests;
        _toasts = toasts;
        _agents = agents;
        _git = git;
        _logger = logger;
        _pickFolder = pickFolder ?? (() => null);
        _pickNewWorktree = pickNewWorktree ?? (_ => null);
        Projects = [];
        Projects.CollectionChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(IsEmpty));
            OnPropertyChanged(nameof(HasProjects));
        };
        _store.Changed += OnStoreChanged;
    }

    public ObservableCollection<ProjectViewModel> Projects { get; }

    /// <summary>True when no projects are registered — drives the first-run empty-state views.</summary>
    public bool IsEmpty => Projects.Count == 0;

    /// <summary>Inverse of <see cref="IsEmpty"/>, for `Visibility` bindings that want the tree shown.</summary>
    public bool HasProjects => Projects.Count > 0;

    /// <summary>Exposed so the sidebar context menu can enumerate agents for "Set default agent".</summary>
    public IReadOnlyList<AgentProfile> AvailableAgents => _agents?.GetAll() ?? [];

    [ObservableProperty]
    private string _filter = string.Empty;

    partial void OnFilterChanged(string value)
    {
        foreach (var p in Projects) { p.ApplyFilter(value); }
    }

    [ObservableProperty]
    private ProjectViewModel? _selectedProject;

    [ObservableProperty]
    private WorktreeViewModel? _selectedWorktree;

    partial void OnSelectedWorktreeChanged(WorktreeViewModel? value)
        => WorktreeSelected?.Invoke(this, value);

    /// <summary>Raised whenever the user selects a new worktree in the sidebar.</summary>
    public event EventHandler<WorktreeViewModel?>? WorktreeSelected;

    /// <summary>
    /// Raised when the "Spawn session on creation" toggle is on in the New Worktree dialog
    /// and the worktree has landed in the store. <see cref="MainViewModel"/> subscribes and
    /// creates a session for the worktree using the project's default agent.
    /// </summary>
    public event EventHandler<WorktreeViewModel>? SpawnSessionRequested;

    internal void RaiseSpawnSessionRequested(WorktreeViewModel worktree)
        => SpawnSessionRequested?.Invoke(this, worktree);

    /// <summary>
    /// Hook wired by <see cref="MainViewModel.AttachSidebar"/> to close any tabs currently pinned
    /// to a worktree before it is removed. Without this, the pwsh processes hosting those tabs
    /// keep Windows file locks on the worktree directory and <c>git worktree remove</c> fails.
    ///
    /// <para>Returns a rollback delegate that the caller invokes if the destructive operation
    /// ultimately fails (git refuses + user declines the force retry). The rollback restores each
    /// soft-closed session and re-adds its tab to the original group, so a failed delete doesn't
    /// leave the user with a still-present worktree but an empty tab strip. A no-op lambda is
    /// returned when nothing was closed.</para>
    /// </summary>
    public Func<string, string, Task<Func<Task>>>? CloseWorktreeSessionsAsync { get; set; }

    private void Toast(string title, string message, ToastSeverity severity)
    {
        if (_toasts is null) { return; }
        _toasts.Show(new ToastRequest(severity, title, message));
    }

    /// <summary>
    /// Variant that accepts inline actions (e.g. Retry / Copy). Errors get a Copy
    /// action by default — the message is usually the raw stderr from git/gh which
    /// the user wants to paste into a bug report or chat. Pass <paramref name="retry"/>
    /// to add a primary "Retry" button that re-runs the failed command.
    /// </summary>
    private void ErrToast(string title, string message, Action? retry = null)
    {
        if (_toasts is null) { return; }
        var actions = new List<ToastAction>(2);
        if (retry is not null)
        {
            actions.Add(new ToastAction("Retry", retry, IsPrimary: true));
        }
        actions.Add(new ToastAction("Copy", () =>
        {
            try { System.Windows.Clipboard.SetText(message); }
            catch (Exception ex) { _logger.LogDebug(ex, "Toast copy failed"); }
        }));
        _toasts.Show(new ToastRequest(ToastSeverity.Err, title, message, Actions: actions));
    }
}
