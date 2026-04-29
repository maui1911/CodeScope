using System.Globalization;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace NoScope.CodeScope.Ui.Services;

public sealed class TaskbarBadgeService : ITaskbarBadgeService
{
    public void Apply(int busyCount, int agentTabCount)
    {
        var win = Application.Current?.MainWindow;
        if (win is null) { return; }
        if (win.TaskbarItemInfo is null) { win.TaskbarItemInfo = new System.Windows.Shell.TaskbarItemInfo(); }

        if (agentTabCount == 0)
        {
            win.TaskbarItemInfo.Overlay = null;
            win.TaskbarItemInfo.Description = string.Empty;
            return;
        }

        if (busyCount == 0)
        {
            win.TaskbarItemInfo.Overlay = BuildOverlay(digit: null, plus: false, fillKey: "Signal.Ok");
            win.TaskbarItemInfo.Description = "All agents idle";
            return;
        }

        var capped = busyCount > 9 ? "9" : busyCount.ToString(CultureInfo.InvariantCulture);
        win.TaskbarItemInfo.Overlay = BuildOverlay(digit: capped, plus: busyCount > 9, fillKey: "Signal.Warn");
        win.TaskbarItemInfo.Description = busyCount == 1 ? "1 agent working" : $"{busyCount} agents working";
    }

    private static BitmapSource BuildOverlay(string? digit, bool plus, string fillKey)
    {
        var fill = (Application.Current?.TryFindResource(fillKey) as Brush) ?? Brushes.Red;
        var ring = new SolidColorBrush(Color.FromArgb(102, 0, 0, 0)); // ~40% black for taskbar contrast
        ring.Freeze();

        var visual = new DrawingVisual();
        using (var dc = visual.RenderOpen())
        {
            // Filled disc + 1 px contrast ring centred at (8,8). Inner radius 7 leaves 1 px margin
            // for the ring (outer radius 7.5) so it sits cleanly inside the 16×16 frame.
            dc.DrawEllipse(fill, null, new Point(8, 8), 7, 7);
            dc.DrawEllipse(null, new Pen(ring, 1), new Point(8, 8), 7.5, 7.5);

            if (digit is not null)
            {
                var typeface = new Typeface(
                    new FontFamily("Segoe UI Variable, Segoe UI"),
                    FontStyles.Normal, FontWeights.Bold, FontStretches.Normal);

                var digitText = new FormattedText(
                    digit,
                    CultureInfo.InvariantCulture, FlowDirection.LeftToRight,
                    typeface, emSize: 10, Brushes.White, pixelsPerDip: 1.0)
                { TextAlignment = TextAlignment.Center };

                // Shift the digit slightly left when "+" is also drawn so the pair reads centred.
                var dx = plus ? 7.0 : 8.0;
                var dy = 8.0 - (digitText.Height / 2);
                dc.DrawText(digitText, new Point(dx, dy));

                if (plus)
                {
                    var plusText = new FormattedText(
                        "+",
                        CultureInfo.InvariantCulture, FlowDirection.LeftToRight,
                        typeface, emSize: 6, Brushes.White, pixelsPerDip: 1.0)
                    { TextAlignment = TextAlignment.Center };
                    var px = 12.5;
                    var py = 4.5 - (plusText.Height / 2);
                    dc.DrawText(plusText, new Point(px, py));
                }
            }
        }

        var rt = new RenderTargetBitmap(16, 16, 96, 96, PixelFormats.Pbgra32);
        rt.Render(visual);
        rt.Freeze();
        return rt;
    }
}
