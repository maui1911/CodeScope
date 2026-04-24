using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Views;

/// <summary>
/// One editor group's tab strip, hosted in the title-bar row of MainWindow. Handles
/// tab selection, new-session button, agent dropdown, middle-click close, wheel scroll,
/// and drag-source for the tab it's showing. Drop targets live on the EditorGroupView
/// workspace column and on peer strips in Row 0.
/// </summary>
public partial class GroupStripView : UserControl
{
    public GroupStripView()
    {
        InitializeComponent();
    }

    /// <summary>Any mouse-down on the strip transfers focus to its owning group.</summary>
    private void OnStripPreviewMouseDown(object sender, MouseButtonEventArgs e)
    {
        if (DataContext is not EditorGroupViewModel group) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }
        if (ReferenceEquals(main.FocusedGroup, group)) { return; }
        main.FocusGroupCommand.Execute(group);
    }

    /// <summary>Middle-click closes the tab under cursor.</summary>
    private void OnTabMouseDown(object sender, MouseButtonEventArgs e)
    {
        if (e.ChangedButton != MouseButton.Middle) { return; }
        if (sender is not FrameworkElement { DataContext: SessionTabViewModel tab }) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }
        if (main.CloseTabCommand.CanExecute(tab))
        {
            main.CloseTabCommand.Execute(tab);
            e.Handled = true;
        }
    }

    /// <summary>Wheel-scrolls the strip horizontally (no scrollbar in the spec).</summary>
    private void OnTabStripPreviewWheel(object sender, MouseWheelEventArgs e)
    {
        if (sender is not DependencyObject root) { return; }
        var sv = FindVisualChild<ScrollViewer>(root);
        if (sv is null) { return; }
        var offset = sv.HorizontalOffset - (e.Delta / 120.0 * 48.0);
        sv.ScrollToHorizontalOffset(Math.Max(0, Math.Min(sv.ScrollableWidth, offset)));
        e.Handled = true;
    }

    // ── Accent rail motion ──────────────────────────────────────
    // The active accent rail is a single Border (named TabRail) in a sibling Canvas of
    // StripList. On selection change we compute the selected ListBoxItem's bounds
    // translated into the Canvas's coordinate space and animate Canvas.Left + Width
    // (200ms ease-out). First paint snaps without animation so the rail doesn't fly in.
    private static readonly TimeSpan RailMotion = TimeSpan.FromMilliseconds(200);
    private static readonly IEasingFunction RailEasing = new CubicEase { EasingMode = EasingMode.EaseOut };
    private bool _railInitialised;

    private void OnStripListLoaded(object sender, RoutedEventArgs e) => UpdateRail(animate: false);

    private void OnStripListSizeChanged(object sender, SizeChangedEventArgs e) => UpdateRail(animate: false);

    private void OnStripSelectionChanged(object sender, SelectionChangedEventArgs e) =>
        UpdateRail(animate: _railInitialised);

    private void UpdateRail(bool animate)
    {
        if (StripList is null || TabRail is null) { return; }

        var item = StripList.SelectedIndex < 0
            ? null
            : StripList.ItemContainerGenerator.ContainerFromIndex(StripList.SelectedIndex) as ListBoxItem;

        // Containers aren't realised until after Loaded + a layout pass — fade out
        // and defer to the next layout tick so the first paint doesn't stamp a
        // zero-width rail at x=0.
        if (item is null || item.ActualWidth <= 0)
        {
            TabRail.Opacity = 0;
            if (StripList.SelectedIndex >= 0)
            {
                Dispatcher.BeginInvoke(new Action(() => UpdateRail(animate)),
                    System.Windows.Threading.DispatcherPriority.Loaded);
            }
            return;
        }

        var origin = item.TranslatePoint(new Point(0, 0), StripList);
        var targetLeft = origin.X;
        var targetWidth = item.ActualWidth;

        if (!animate)
        {
            Canvas.SetLeft(TabRail, targetLeft);
            TabRail.Width = targetWidth;
            TabRail.Opacity = 1;
            _railInitialised = true;
            return;
        }

        TabRail.BeginAnimation(Canvas.LeftProperty, new DoubleAnimation
        {
            To = targetLeft,
            Duration = RailMotion,
            EasingFunction = RailEasing,
        });
        TabRail.BeginAnimation(FrameworkElement.WidthProperty, new DoubleAnimation
        {
            To = targetWidth,
            Duration = RailMotion,
            EasingFunction = RailEasing,
        });
        TabRail.Opacity = 1;
    }

    private void OnAgentDropdownClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { ContextMenu: { } menu } btn)
        {
            menu.PlacementTarget = btn;
            menu.Placement = PlacementMode.Bottom;
            menu.IsOpen = true;
        }
    }

    // ── Drag source ─────────────────────────────────────────────
    private Point _dragOrigin;
    private SessionTabViewModel? _dragCandidate;
    private bool _dragInFlight;

    private void OnStripPreviewLeftDown(object sender, MouseButtonEventArgs e)
    {
        if (sender is not ListBox list) { return; }
        var tab = HitTestTab(list, e.GetPosition(list));
        if (tab is null) { return; }
        _dragOrigin = e.GetPosition(null);
        _dragCandidate = tab;
    }

    private void OnStripPreviewMouseMove(object sender, MouseEventArgs e)
    {
        if (_dragCandidate is null || _dragInFlight) { return; }
        if (e.LeftButton != MouseButtonState.Pressed)
        {
            _dragCandidate = null;
            return;
        }

        var p = e.GetPosition(null);
        if (Math.Abs(p.X - _dragOrigin.X) < SystemParameters.MinimumHorizontalDragDistance
            && Math.Abs(p.Y - _dragOrigin.Y) < SystemParameters.MinimumVerticalDragDistance)
        {
            return;
        }

        var payload = _dragCandidate;
        _dragCandidate = null;
        _dragInFlight = true;
        try
        {
            var data = new DataObject(TabDragData.Format, payload);
            DragDrop.DoDragDrop((DependencyObject)sender, data, DragDropEffects.Move);
        }
        finally
        {
            _dragInFlight = false;
        }
    }

    // ── Drop target (when a tab is dragged onto THIS strip) ────
    private void OnStripDragOver(object sender, DragEventArgs e) => TabDragData.HandleDragOver(e);

    private void OnStripDrop(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(TabDragData.Format)) { return; }
        if (e.Data.GetData(TabDragData.Format) is not SessionTabViewModel source) { return; }
        if (DataContext is not EditorGroupViewModel targetGroup) { return; }
        if (Application.Current?.MainWindow?.DataContext is not MainViewModel main) { return; }

        var targetIndex = ResolveDropIndex(e, targetGroup);
        main.MoveTabToGroup(source, targetGroup, targetIndex);
        e.Handled = true;
    }

    private int ResolveDropIndex(DragEventArgs e, EditorGroupViewModel target)
    {
        if (StripList is null) { return -1; }
        var pt = e.GetPosition(StripList);
        for (var i = 0; i < target.Tabs.Count; i++)
        {
            if (StripList.ItemContainerGenerator.ContainerFromIndex(i) is not ListBoxItem item) { continue; }
            var center = item.TranslatePoint(new Point(item.ActualWidth / 2, 0), StripList);
            if (pt.X < center.X) { return i; }
        }
        return -1;
    }

    private static SessionTabViewModel? HitTestTab(ListBox list, Point pt)
    {
        var hit = list.InputHitTest(pt) as DependencyObject;
        while (hit is not null && hit is not ListBoxItem)
        {
            hit = VisualTreeHelper.GetParent(hit);
        }
        return (hit as ListBoxItem)?.DataContext as SessionTabViewModel;
    }

    private static T? FindVisualChild<T>(DependencyObject parent) where T : DependencyObject
    {
        var count = VisualTreeHelper.GetChildrenCount(parent);
        for (var i = 0; i < count; i++)
        {
            var child = VisualTreeHelper.GetChild(parent, i);
            if (child is T typed) { return typed; }
            var nested = FindVisualChild<T>(child);
            if (nested is not null) { return nested; }
        }
        return null;
    }
}
