using System.Globalization;
using System.Windows;
using System.Windows.Data;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>
/// Maps <c>true → Collapsed</c>, <c>false → Visible</c>. Used by NewWorktreeDialog's popup to
/// hide the row grid when a row is a group-label sentinel and vice versa.
/// </summary>
public sealed class InverseBoolToVisibilityConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is true ? Visibility.Collapsed : Visibility.Visible;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
