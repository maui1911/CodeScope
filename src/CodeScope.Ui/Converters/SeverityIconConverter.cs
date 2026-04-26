using System.Globalization;
using System.Windows.Data;
using System.Windows.Media;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>
/// Picks the right <see cref="Geometry"/> path for the severity icon. The four
/// glyphs match the SVGs in spec §03 (info dot, ok check, warn triangle, err cross).
/// </summary>
[ValueConversion(typeof(ToastSeverity), typeof(Geometry))]
public sealed class SeverityIconConverter : IValueConverter
{
    // Icons are drawn into a 12×12 viewbox so a single Path Data string fits inside
    // the 24×24 tinted tile without further scaling. Strokes ride on the parent Path's
    // Stroke + StrokeThickness so the color flows from the severity converter above.
    // Cached + Frozen so every toast in the stack shares the same Geometry instance —
    // Geometry.Parse is non-trivial and the WPF render thread plays nicer with frozen
    // freezables, so this is a "free" win.
    private static readonly Geometry InfoIcon = FreezeParse("M 6 1.5 A 4.5 4.5 0 1 1 5.999 1.5 Z M 6 4 L 6 7");
    private static readonly Geometry OkIcon = FreezeParse("M 2.5 6.5 L 5 9 L 10 4");
    private static readonly Geometry WarnIcon = FreezeParse("M 6 1.5 L 11 10 L 1 10 Z M 6 5 L 6 8");
    private static readonly Geometry ErrIcon = FreezeParse("M 6 1.5 A 4.5 4.5 0 1 1 5.999 1.5 Z M 3.5 3.5 L 8.5 8.5 M 8.5 3.5 L 3.5 8.5");

    private static Geometry FreezeParse(string data)
    {
        var g = Geometry.Parse(data);
        if (g.CanFreeze) { g.Freeze(); }
        return g;
    }

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value switch
        {
            ToastSeverity.Info => InfoIcon,
            ToastSeverity.Ok => OkIcon,
            ToastSeverity.Warn => WarnIcon,
            ToastSeverity.Err => ErrIcon,
            _ => InfoIcon,
        };

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
