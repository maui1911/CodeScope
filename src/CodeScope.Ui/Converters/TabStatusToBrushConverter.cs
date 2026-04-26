using System.Globalization;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>
/// <see cref="TabStatus"/> → brush. <c>Busy</c> → <c>Signal.Warn</c> (red, agent working);
/// <c>Ready</c> → <c>Signal.Ok</c> (green, awaiting your input).
/// </summary>
public sealed class TabStatusToBrushConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var key = value is TabStatus.Busy ? "Signal.Warn" : "Signal.Ok";
        return (Application.Current?.TryFindResource(key) as Brush) ?? Brushes.Transparent;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
