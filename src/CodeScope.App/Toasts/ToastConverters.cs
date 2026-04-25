using System.Globalization;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.App.Toasts;

/// <summary>
/// Resolves the per-severity foreground brush from the static resource dictionary.
/// Each severity owns three brushes (foreground / tint bg / tint border) — using a
/// shared converter with a parameter (<c>Fg</c>, <c>Bg</c>, <c>Border</c>) keeps the
/// XAML compact instead of repeating four <c>DataTrigger</c> blocks per visual slot.
/// </summary>
[ValueConversion(typeof(ToastSeverity), typeof(Brush))]
public sealed class SeverityBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is not ToastSeverity severity) { return Brushes.Transparent; }
        var role = parameter as string ?? "Fg";
        var key = (severity, role) switch
        {
            (ToastSeverity.Info, "Fg") => "Severity.Info",
            (ToastSeverity.Info, "Bg") => "Severity.Info.Bg",
            (ToastSeverity.Info, "Border") => "Severity.Info.Border",
            (ToastSeverity.Ok, "Fg") => "Severity.Ok",
            (ToastSeverity.Ok, "Bg") => "Severity.Ok.Bg",
            (ToastSeverity.Ok, "Border") => "Severity.Ok.Border",
            (ToastSeverity.Warn, "Fg") => "Severity.Warn",
            (ToastSeverity.Warn, "Bg") => "Severity.Warn.Bg",
            (ToastSeverity.Warn, "Border") => "Severity.Warn.Border",
            (ToastSeverity.Err, "Fg") => "Severity.Err",
            (ToastSeverity.Err, "Bg") => "Severity.Err.Bg",
            (ToastSeverity.Err, "Border") => "Severity.Err.Border",
            _ => "Severity.Info",
        };
        return Application.Current.TryFindResource(key) as Brush ?? Brushes.Transparent;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

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
    private const string Info = "M 6 1.5 A 4.5 4.5 0 1 1 5.999 1.5 Z M 6 4 L 6 7";
    private const string Ok = "M 2.5 6.5 L 5 9 L 10 4";
    private const string Warn = "M 6 1.5 L 11 10 L 1 10 Z M 6 5 L 6 8";
    private const string Err = "M 6 1.5 A 4.5 4.5 0 1 1 5.999 1.5 Z M 3.5 3.5 L 8.5 8.5 M 8.5 3.5 L 3.5 8.5";

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var data = value switch
        {
            ToastSeverity.Info => Info,
            ToastSeverity.Ok => Ok,
            ToastSeverity.Warn => Warn,
            ToastSeverity.Err => Err,
            _ => Info,
        };
        return Geometry.Parse(data);
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

/// <summary>
/// Multiplies a 0..1 progress fraction by a target width (passed as <c>parameter</c>
/// so the same converter handles meters of any width). Used by the meter strip so its
/// inner fill drains right-to-left as <see cref="ToastItemViewModel.Progress"/> ticks
/// down. Returns 0 for non-positive inputs to avoid layout warnings.
/// </summary>
[ValueConversion(typeof(double), typeof(double))]
public sealed class ProgressToWidthConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var progress = value as double? ?? 0;
        var max = parameter switch
        {
            double d => d,
            string s when double.TryParse(s, NumberStyles.Float, CultureInfo.InvariantCulture, out var sd) => sd,
            _ => 380.0,
        };
        return Math.Max(0, Math.Min(1, progress)) * max;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
