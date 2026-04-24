using System.Globalization;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>
/// <see cref="TabStatus"/> → brush. Looks up <c>Accent.Primary</c> / <c>Signal.Ok</c> / <c>Signal.Warn</c>
/// from app resources so the tab-strip status dot tracks the design tokens without hardcoding colors.
/// </summary>
public sealed class TabStatusToBrushConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var key = value switch
        {
            TabStatus.Active => "Accent.Primary",
            TabStatus.Wait   => "Signal.Warn",
            _                => "Signal.Ok",
        };
        return (Application.Current?.TryFindResource(key) as Brush) ?? Brushes.Transparent;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
