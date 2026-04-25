using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;

namespace NoScope.CodeScope.App.Toasts;

/// <summary>
/// Host that owns the toast <see cref="Popup"/> and computes its bottom-right
/// placement relative to the parent window. The placement callback fires once per
/// open; we re-trigger it on every parent SizeChanged / LocationChanged so the popup
/// tracks window resizes and screen-space moves.
/// </summary>
public partial class ToastHostView : UserControl
{
    /// <summary>Inset from the parent window's bottom-right (spec §08 "anchor 28px").</summary>
    private const double Inset = 28.0;

    private Window? _parent;

    public ToastHostView()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
        Popup.CustomPopupPlacementCallback = PlaceBottomRight;
        // Stack-level hover: a single enter/leave on the items host pauses every
        // visible toast at once (spec §04). Subscribing here — instead of on each
        // ToastView — guarantees the gap between two toasts also counts as "hover"
        // because the StackPanel itself is the listening element.
        ItemsHost.MouseEnter += OnStackEnter;
        ItemsHost.MouseLeave += OnStackLeave;
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        _parent = Window.GetWindow(this);
        if (_parent is null) { return; }
        // The popup's PlacementTarget needs a WPF FrameworkElement that the popup's
        // size-tracking code can measure. The window itself works because Custom
        // placement gets its targetSize from the placement target's render size.
        Popup.PlacementTarget = _parent;
        _parent.SizeChanged += OnParentChanged;
        _parent.LocationChanged += OnParentChanged;
        _parent.StateChanged += OnParentChanged;
        Popup.IsOpen = true;
        ReplacePopup();
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        if (_parent is not null)
        {
            _parent.SizeChanged -= OnParentChanged;
            _parent.LocationChanged -= OnParentChanged;
            _parent.StateChanged -= OnParentChanged;
            _parent = null;
        }
        Popup.IsOpen = false;
    }

    private void OnParentChanged(object? sender, EventArgs e) => ReplacePopup();

    private void OnStackEnter(object sender, MouseEventArgs e)
    {
        if (DataContext is not ToastService svc) { return; }
        foreach (var item in svc.Items) { item.Pause(); }
    }

    private void OnStackLeave(object sender, MouseEventArgs e)
    {
        if (DataContext is not ToastService svc) { return; }
        foreach (var item in svc.Items) { item.Resume(); }
    }

    /// <summary>
    /// WPF popups don't reposition automatically when their PlacementTarget moves or
    /// resizes. Toggling <see cref="Popup.HorizontalOffset"/> forces a layout pass
    /// without flickering. Cheap, no visible re-open animation.
    /// </summary>
    private void ReplacePopup()
    {
        var horizontal = Popup.HorizontalOffset;
        Popup.HorizontalOffset = horizontal + 1;
        Popup.HorizontalOffset = horizontal;
    }

    /// <summary>
    /// Position the popup so its bottom-right corner sits at the window's bottom-right
    /// minus a 28px gutter. <paramref name="targetSize"/> is the parent window's
    /// render size; <paramref name="popupSize"/> is the measured popup contents.
    /// </summary>
    private static CustomPopupPlacement[] PlaceBottomRight(Size popupSize, Size targetSize, Point offset)
    {
        var x = targetSize.Width - popupSize.Width - Inset;
        var y = targetSize.Height - popupSize.Height - Inset;
        return [new CustomPopupPlacement(new Point(x, y), PopupPrimaryAxis.None)];
    }
}
