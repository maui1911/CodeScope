using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
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
/// in <c>MainWindow</c>'s title-bar row — this view just holds the per-tab
/// <c>SessionTabView</c> overlay. Click anywhere to transfer focus; drop a dragged tab
/// anywhere in the border to move the tab into this group.
/// </summary>
public partial class EditorGroupView : UserControl
{
    public EditorGroupView()
    {
        InitializeComponent();
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
