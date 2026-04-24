using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;

namespace NoScope.CodeScope.Ui.Dialogs;

/// <summary>
/// In-app modal primitive — the single source of dialog visuals in CodeScope. Every prompt,
/// destructive confirm, or info note composes from the same 7 slots: icon · eyebrow · title ·
/// close · body · hint · footer (ghost → primary). See
/// <c>docs/design/html/CodeScope - Dialog System.html</c> for the full spec; this class is the
/// <see cref="Flavor.Confirm"/>, <see cref="Flavor.Destructive"/>, and <see cref="Flavor.Info"/>
/// implementation — form/choice flavors stay in their own dialog files.
/// </summary>
public partial class ConfirmDialog : Window
{
    public enum Flavor
    {
        /// <summary>Reversible action, primary CTA accent-blue. Esc cancels, scrim click cancels.</summary>
        Confirm,
        /// <summary>Irreversible — no close icon, no scrim dismiss, Enter does not auto-confirm, red button.</summary>
        Destructive,
        /// <summary>Non-blocking success/info — green eyebrow, check icon, OK-only.</summary>
        Info,
    }

    public enum Size { Sm, Md, Lg, Xl }

    private bool _strict;

    private void OnChromeDrag(object sender, MouseButtonEventArgs e)
    {
        if (e.ChangedButton == MouseButton.Left) { DragMove(); }
    }

    private ConfirmDialog(
        Flavor flavor,
        Size size,
        string title,
        string body,
        string confirmLabel,
        string? cancelLabel,
        string? hint)
    {
        InitializeComponent();
        Width = size switch
        {
            Size.Sm => 420,
            Size.Md => 480,
            Size.Lg => 560,
            Size.Xl => 640,
            _ => 480,
        };
        // Account for the 30px shadow margin around the surface — the visible dialog is still
        // the spec width (420/480/560/640), but the Window is wider to make room for the shadow.
        Width += 60;

        TitleText.Text = title;
        BodyText.Text = body;
        ConfirmButton.Content = confirmLabel;
        if (cancelLabel is null)
        {
            CancelButton.Visibility = Visibility.Collapsed;
        }
        else
        {
            CancelButton.Content = cancelLabel;
        }

        if (hint is null)
        {
            HintText.Visibility = Visibility.Collapsed;
        }
        else
        {
            HintText.Text = hint;
            HintText.Visibility = Visibility.Visible;
        }

        ApplyFlavor(flavor);
    }

    /// <summary>Reversible confirmation. Returns <c>true</c> on confirm, <c>false</c> on cancel/Esc/close.</summary>
    public static bool Confirm(
        string title,
        string body,
        string confirmLabel = "OK",
        string cancelLabel = "Cancel",
        string? hint = null,
        Size size = Size.Md,
        Window? owner = null)
        => Show(Flavor.Confirm, title, body, confirmLabel, cancelLabel, hint, size, owner);

    /// <summary>Irreversible destructive confirmation. <paramref name="confirmLabel"/> becomes red.</summary>
    public static bool Destructive(
        string title,
        string body,
        string confirmLabel,
        string cancelLabel = "Cancel",
        string? hint = null,
        Size size = Size.Md,
        Window? owner = null)
        => Show(Flavor.Destructive, title, body, confirmLabel, cancelLabel, hint, size, owner);

    /// <summary>OK-only informational dialog. Hides the cancel button.</summary>
    public static void Inform(
        string title,
        string body,
        string okLabel = "OK",
        string? hint = null,
        Size size = Size.Sm,
        Window? owner = null)
        => Show(Flavor.Info, title, body, okLabel, cancelLabel: null, hint, size, owner);

    private static bool Show(
        Flavor flavor,
        string title,
        string body,
        string confirmLabel,
        string? cancelLabel,
        string? hint,
        Size size,
        Window? owner)
    {
        var dlg = new ConfirmDialog(flavor, size, title, body, confirmLabel, cancelLabel, hint)
        {
            Owner = owner ?? Application.Current?.MainWindow,
        };
        return dlg.ShowDialog() == true;
    }

    private void ApplyFlavor(Flavor flavor)
    {
        // Icon glyph data — 16×16 SVG paths transcribed from the spec. Each entry is:
        //   (eyebrow text, eyebrow brush key, icon path data, icon stroke key, icon bg key, icon brd key)
        var (eyebrow, eyebrowKey, glyph, strokeKey, bgKey, brdKey, glyphStrokeThickness) = flavor switch
        {
            // Info flavor in the spec uses the green check — §02 "Info / Success".
            Flavor.Info => (
                "SESSION READY",
                "Dlg.Ok",
                "M 3.5 8.5 L 7 12 L 13 5",
                "Dlg.Ok",
                "Dlg.IconBg.Ok",
                "Dlg.IconBrd.Ok",
                1.7),
            // Destructive flavor: warn triangle with exclamation.
            Flavor.Destructive => (
                "DESTRUCTIVE",
                "Dlg.Danger",
                "M 8 2 L 14.5 13 L 1.5 13 Z M 8 6 L 8 9.5 M 8 11.5 L 8 11.52",
                "Dlg.Danger",
                "Dlg.IconBg.Warn",
                "Dlg.IconBrd.Warn",
                1.5),
            // Default Confirm: circle with 'i'.
            _ => (
                "CONFIRM",
                "Dlg.Text3",
                "M 8 2 A 6 6 0 1 1 7.99 2 Z M 8 5 L 8 8.5 M 8 11 L 8 11.02",
                "Dlg.Accent",
                "Dlg.IconBg.Info",
                "Dlg.IconBrd.Info",
                1.5),
        };

        EyebrowText.Text = eyebrow;
        ApplyBrush(EyebrowText, TextBlock.ForegroundProperty, eyebrowKey);
        IconGlyph.Data = Geometry.Parse(glyph);
        IconGlyph.StrokeThickness = glyphStrokeThickness;
        ApplyBrush(IconGlyph, System.Windows.Shapes.Shape.StrokeProperty, strokeKey);
        ApplyBrush(IconTile, Border.BackgroundProperty, bgKey);
        ApplyBrush(IconTile, Border.BorderBrushProperty, brdKey);

        // Destructive-flavor behavior per spec §04:
        //   • no close icon
        //   • no Esc cancel (via CancelButton.IsCancel=false)
        //   • no ↵ auto-confirm (via ConfirmButton.IsDefault=false)
        //   • primary swaps to the red destructive button
        _strict = flavor == Flavor.Destructive;
        if (_strict)
        {
            CloseButton.Visibility = Visibility.Collapsed;
            ConfirmButton.IsDefault = false;
            CancelButton.IsCancel = false;
            if (TryFindResource("Dlg.Btn.Destructive") is Style destStyle)
            {
                ConfirmButton.Style = destStyle;
            }
        }
    }

    private void ApplyBrush(DependencyObject target, DependencyProperty property, string resourceKey)
    {
        if (TryFindResource(resourceKey) is Brush brush)
        {
            target.SetValue(property, brush);
        }
    }

    private void OnConfirm(object sender, RoutedEventArgs e)
    {
        DialogResult = true;
        Close();
    }

    private void OnCancel(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}
