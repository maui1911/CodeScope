using System.IO;
using System.Text.Json;
using System.Windows;

namespace NoScope.CodeScope.App;

/// <summary>
/// Persists the main window's bounds + state to <c>%LocalAppData%/CodeScope/window.json</c>.
/// Fire-and-forget on Save; missing/corrupt file on Load returns null and the caller falls
/// back to the XAML defaults.
/// </summary>
public static class WindowGeometryStore
{
    public sealed record WindowGeometry(double Left, double Top, double Width, double Height, string State);

    private static string FilePath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        NoScope.CodeScope.Core.AppPaths.AppFolderName,
        "window.json");

    public static WindowGeometry? Load()
    {
        try
        {
            if (!File.Exists(FilePath)) { return null; }
            var json = File.ReadAllText(FilePath);
            return JsonSerializer.Deserialize<WindowGeometry>(json);
        }
        catch (Exception ex) when (ex is IOException or JsonException)
        {
            System.Diagnostics.Debug.WriteLine($"[WindowGeometryStore] Load: {ex.Message}");
            return null;
        }
    }

    public static void Save(Window w)
    {
        try
        {
            // RestoreBounds reflects the un-maximized rectangle even when WindowState == Maximized.
            var rect = w.WindowState == WindowState.Normal
                ? new Rect(w.Left, w.Top, w.Width, w.Height)
                : w.RestoreBounds;
            if (rect.Width < 400 || rect.Height < 300) { return; } // sanity: don't save a collapsed rect.

            var geo = new WindowGeometry(rect.Left, rect.Top, rect.Width, rect.Height, w.WindowState.ToString());
            Directory.CreateDirectory(Path.GetDirectoryName(FilePath)!);
            File.WriteAllText(FilePath, JsonSerializer.Serialize(geo));
        }
        catch (Exception ex) when (ex is IOException or JsonException)
        {
            // Persistence is best-effort; a write failure here shouldn't block shutdown, but trace it.
            System.Diagnostics.Debug.WriteLine($"[WindowGeometryStore] Save: {ex.Message}");
        }
    }

    /// <summary>
    /// Applies a saved geometry if it sits on (or overlaps) a currently-connected monitor.
    /// Discards saves that would drop the window off-screen (e.g. after unplugging a display).
    /// </summary>
    public static void Apply(Window w, WindowGeometry? geo)
    {
        if (geo is null) { return; }

        var virtualLeft = SystemParameters.VirtualScreenLeft;
        var virtualTop = SystemParameters.VirtualScreenTop;
        var virtualRight = virtualLeft + SystemParameters.VirtualScreenWidth;
        var virtualBottom = virtualTop + SystemParameters.VirtualScreenHeight;
        var centreX = geo.Left + geo.Width / 2;
        var centreY = geo.Top + geo.Height / 2;
        var onScreen = centreX >= virtualLeft && centreX <= virtualRight
                       && centreY >= virtualTop && centreY <= virtualBottom;
        if (!onScreen) { return; }

        w.WindowStartupLocation = WindowStartupLocation.Manual;
        w.Left = geo.Left;
        w.Top = geo.Top;
        w.Width = geo.Width;
        w.Height = geo.Height;

        if (Enum.TryParse<WindowState>(geo.State, out var state)
            && state is WindowState.Maximized or WindowState.Normal)
        {
            w.WindowState = state;
        }
    }
}
