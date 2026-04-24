using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Dialogs;
using Microsoft.Extensions.Logging;
using Wpf.Ui;
using Wpf.Ui.Controls;

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
    private readonly ISnackbarService? _snackbar;
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
        ISnackbarService? snackbar = null,
        IAgentRegistry? agents = null,
        IGitService? git = null)
    {
        _store = store;
        _pullRequests = pullRequests;
        _snackbar = snackbar;
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

    private void Toast(string title, string message, ControlAppearance appearance)
    {
        if (_snackbar is null) { return; }
        _snackbar.Show(title, message, appearance, icon: null, TimeSpan.FromSeconds(4));
    }
}
