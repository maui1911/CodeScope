using System.ComponentModel;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using NoScope.CodeScope.Ui.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Views;

/// <summary>
/// Shared in-process drag format — carries a <see cref="SessionTabViewModel"/> reference.
/// WPF doesn't need to serialize for same-process drags.
/// </summary>
public static class TabDragData
{
    public const string Format = "CodeScope.SessionTab";

    /// <summary>Shared DragOver handler: accept the drop iff the payload carries a tab.</summary>
    public static void HandleDragOver(DragEventArgs e)
    {
        if (e.Data.GetDataPresent(Format))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
        }
        else
        {
            e.Effects = DragDropEffects.None;
        }
    }
}

/// <summary>
/// A single editor group's workspace column. The tab strip is window-global and lives
/// in <c>MainWindow</c>'s title-bar row — this view just hosts a single
/// <see cref="ContentControl"/> that displays the group's selected
/// <see cref="SessionTabView"/>. Click anywhere to transfer focus; drop a dragged tab
/// anywhere in the border to move the tab into this group.
///
/// <para>The displayed <see cref="SessionTabView"/> is resolved from
/// <see cref="ISessionViewHostPool"/> by <see cref="SessionTabViewModel.Descriptor"/>.Id, not
/// instantiated by a <c>DataTemplate</c>. That preserves the inner <c>HwndHost</c> across
/// reparent so dragging a tab between groups doesn't restart the ConPTY child.</para>
/// </summary>
public partial class EditorGroupView : UserControl
{
    private EditorGroupViewModel? _boundGroup;

    public EditorGroupView()
    {
        InitializeComponent();
        DataContextChanged += OnDataContextChanged;
        Loaded += (_, _) =>
        {
            // Defensive re-subscribe. Unloaded fires on transient tree transitions
            // (window resize, splitter, tab strip restructure), and the symmetric
            // OnUnloaded handler unhooks our PropertyChanged subscription. If we
            // don't re-hook here, SelectedTab changes after a transient unload-then-
            // reload would be silently dropped.
            if (_boundGroup is not null)
            {
                _boundGroup.PropertyChanged -= OnGroupPropertyChanged;
                _boundGroup.PropertyChanged += OnGroupPropertyChanged;
            }
            SyncActiveSlot();
        };
        Unloaded += OnUnloaded;
    }

    private void OnDataContextChanged(object sender, DependencyPropertyChangedEventArgs e)
    {
        if (_boundGroup is not null) { _boundGroup.PropertyChanged -= OnGroupPropertyChanged; }
        _boundGroup = e.NewValue as EditorGroupViewModel;
        if (_boundGroup is not null) { _boundGroup.PropertyChanged += OnGroupPropertyChanged; }
        SyncActiveSlot();
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        // Unhook to avoid leaking handlers on a permanently-removed group VM (and to
        // avoid double-firing when Loaded re-subscribes). Loaded re-subscribes
        // defensively if _boundGroup is still attached, so this is safe even on a
        // transient unload-reload cycle.
        if (_boundGroup is not null) { _boundGroup.PropertyChanged -= OnGroupPropertyChanged; }
        // IMPORTANT: do NOT call Teardown() on the hosted SessionTabView — the pool owns
        // its lifecycle. Also do NOT clear ActiveSlot.Content: a transient unload (e.g.
        // splitter drag, window resize) would orphan a live terminal mid-flight. When
        // some other group's selection changes and the pool's Acquire() returns this
        // view, DetachFromParent() will pull it out of this slot atomically. Until then
        // the stale Content reference is harmless because nothing renders us while
        // we're unloaded.
    }

    private void OnGroupPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(EditorGroupViewModel.SelectedTab)) { SyncActiveSlot(); }
    }

    /// <summary>
    /// Pulls the current <see cref="EditorGroupViewModel.SelectedTab"/>'s view from the pool
    /// and slots it into <c>ActiveSlot</c>. <see cref="ISessionViewHostPool.Acquire"/>
    /// guarantees the returned view has no logical parent — it detaches from any previous
    /// host first — so the assignment to <c>ActiveSlot.Content</c> is always safe even if a
    /// sibling group hadn't yet released the view via its own SelectedTab change.
    /// </summary>
    private void SyncActiveSlot()
    {
        if (_boundGroup?.SelectedTab is not { } tab)
        {
            ActiveSlot.Content = null;
            return;
        }

        var pool = ResolvePool();
        if (pool is null)
        {
            // No pool resolved (design-time, unit tests without DI). Fall back to per-group
            // construction so the surface still renders something — diagnostic only.
            if (ActiveSlot.Content is not SessionTabView)
            {
                ActiveSlot.Content = new SessionTabView { DataContext = tab };
            }
            else if (ActiveSlot.Content is SessionTabView v && !ReferenceEquals(v.DataContext, tab))
            {
                v.DataContext = tab;
            }
            return;
        }

        var view = pool.Acquire(tab.Descriptor.Id, () => new SessionTabView { DataContext = tab });
        // Belt-and-braces: keep DataContext in sync. Should be a no-op since the pool entry
        // was created with this same VM, but Rebind() can swap the descriptor without
        // changing the VM identity, and a new pool entry created by another group will
        // already have the right DataContext.
        if (!ReferenceEquals(view.DataContext, tab)) { view.DataContext = tab; }

        if (ReferenceEquals(ActiveSlot.Content, view)) { return; }
        ActiveSlot.Content = view;
    }

    private static ISessionViewHostPool? ResolvePool()
    {
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return null; }
        return main.SessionViewPool;
    }

    /// <summary>Any mouse-down in this group transfers focus to it so the global strip flips.</summary>
    private void OnGroupPreviewMouseDown(object sender, MouseButtonEventArgs e)
    {
        FocusOwningGroup();
    }

    /// <summary>
    /// Backstop for clicks that land inside the terminal's <c>HwndHost</c> child — those
    /// clicks never reach <see cref="OnGroupPreviewMouseDown"/> because the native window
    /// captures the mouse message before WPF's input system sees it. They DO move keyboard
    /// focus into the HwndHost, however, and WPF raises <c>GotKeyboardFocus</c> on every
    /// ancestor up the visual tree — including this UserControl. So whenever any element
    /// inside this group acquires keyboard focus, flip the focused group.
    /// </summary>
    private void OnGroupGotKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        FocusOwningGroup();
    }

    private void FocusOwningGroup()
    {
        if (DataContext is not EditorGroupViewModel group) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }
        if (ReferenceEquals(main.FocusedGroup, group)) { return; }
        main.FocusGroupCommand.Execute(group);
    }

    /// <summary>Accepts a tab being dragged in from any group (incoming via window-global strip).</summary>
    private void OnStripDragOver(object sender, DragEventArgs e) => TabDragData.HandleDragOver(e);

    /// <summary>Drop on the group's workspace moves the tab into this group (append).</summary>
    private void OnStripDrop(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(TabDragData.Format)) { return; }
        if (e.Data.GetData(TabDragData.Format) is not SessionTabViewModel source) { return; }
        if (DataContext is not EditorGroupViewModel targetGroup) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }

        main.MoveTabToGroup(source, targetGroup);
        e.Handled = true;
    }
}
