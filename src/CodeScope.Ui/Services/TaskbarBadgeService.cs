using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Default <see cref="ITaskbarBadgeService"/> implementation. Looks up the active
/// <see cref="Application.MainWindow"/> on each call and assigns
/// <c>TaskbarItemInfo.Overlay</c> / <c>Description</c>. Rendering is pure WPF —
/// <see cref="DrawingVisual"/> rasterised via <see cref="RenderTargetBitmap"/>.
/// </summary>
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

        var capped = busyCount > 9 ? "9" : busyCount.ToString();
        win.TaskbarItemInfo.Overlay = BuildOverlay(digit: capped, plus: busyCount > 9, fillKey: "Signal.Warn");
        win.TaskbarItemInfo.Description = $"{busyCount} agents working";
    }

    private static BitmapSource BuildOverlay(string? digit, bool plus, string fillKey)
    {
        // Filled in Task 3.
        var rt = new RenderTargetBitmap(16, 16, 96, 96, PixelFormats.Pbgra32);
        rt.Freeze();
        return rt;
    }
}
