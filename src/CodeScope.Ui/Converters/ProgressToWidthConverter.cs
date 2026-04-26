using System.Globalization;
using System.Windows.Data;

namespace NoScope.CodeScope.Ui.Converters;

/// <summary>
/// Multiplies a 0..1 progress fraction by a target width (passed as <c>parameter</c>
/// so the same converter handles meters of any width). Used by the meter strip so its
/// inner fill drains right-to-left as the toast's <c>Progress</c> ticks down. Returns
/// 0 for non-positive inputs to avoid layout warnings.
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
