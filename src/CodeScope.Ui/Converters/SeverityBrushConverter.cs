using System.Globalization;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.Ui.Converters;

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
