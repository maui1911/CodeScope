using System.Globalization;
using System.Windows;
using System.Windows.Data;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>Collapses a bound element when the source value is null or an empty string.</summary>
public sealed class NullToVisibilityConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is null || (value is string s && string.IsNullOrWhiteSpace(s))
            ? Visibility.Collapsed
            : Visibility.Visible;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
