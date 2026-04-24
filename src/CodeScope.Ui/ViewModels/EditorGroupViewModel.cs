using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// A group of session tabs that share a workspace region. The whole window is a single
/// group today; multi-group split (one strip per group, side-by-side workspaces) is
/// introduced incrementally on top of this container.
///
/// Ownership model:
/// * <see cref="Tabs"/> is the canonical tab collection for this group.
/// * <see cref="SelectedTab"/> is the focused tab within the group.
/// * <see cref="IsFocused"/> marks which group window-global keyboard shortcuts target.
///
/// All cross-group orchestration (create / close / split / focus-transfer) lives on
/// <see cref="MainViewModel"/>; the group itself is a dumb container.
/// </summary>
public sealed partial class EditorGroupViewModel : ObservableObject
{
    public EditorGroupViewModel(ObservableCollection<SessionTabViewModel>? tabs = null)
    {
        Tabs = tabs ?? [];
        Id = Guid.NewGuid().ToString("N");
    }

    /// <summary>Stable identifier for persistence / diagnostics.</summary>
    public string Id { get; }

    public ObservableCollection<SessionTabViewModel> Tabs { get; }

    [ObservableProperty]
    private SessionTabViewModel? _selectedTab;

    /// <summary>
    /// Mirrors the group's selection onto per-tab <see cref="SessionTabViewModel.IsActive"/>
    /// so the workspace overlay inside <c>EditorGroupView</c> can gate visibility via
    /// a simple <c>{Binding IsActive}</c> trigger — each group shows exactly one terminal
    /// (its selected tab), independent of which group the window focus currently sits on.
    /// </summary>
    partial void OnSelectedTabChanged(SessionTabViewModel? oldValue, SessionTabViewModel? newValue)
    {
        if (oldValue is not null) { oldValue.IsActive = false; }
        if (newValue is not null) { newValue.IsActive = true; }
    }

    /// <summary>True when window-global shortcuts (Ctrl+T, Ctrl+W, Ctrl+1..9) apply to this group.</summary>
    [ObservableProperty]
    private bool _isFocused;
}
