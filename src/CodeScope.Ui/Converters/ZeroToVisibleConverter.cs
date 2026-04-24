using System.Globalization;
using System.Windows;
using System.Windows.Data;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>Visible when the bound int is 0 (empty-state messages).</summary>
public sealed class ZeroToVisibleConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is int n && n == 0 ? Visibility.Visible : Visibility.Collapsed;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
