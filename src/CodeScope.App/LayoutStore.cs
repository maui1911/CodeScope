using System.IO;
using System.Text.Json;

namespace NoScope.CodeScope.App;

/// <summary>
/// Persists the multi-group layout to <c>%LocalAppData%/CodeScope/layout.json</c>:
/// how many editor groups existed at shutdown, which group each session lived in,
/// and which group had focus. Applied <i>before</i> session hydration so tabs land
/// in the right group directly — otherwise every tab would initially mount in
/// <c>Groups[0]</c> and a subsequent re-parent would kill and respawn its terminal.
/// </summary>
public static class LayoutStore
{
    public sealed record Layout(
        int GroupCount,
        int FocusedGroupIndex,
        Dictionary<string, int> SessionToGroup,
        double[]? GroupWidths = null);

    private static string FilePath
    {
        get
        {
            var dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), NoScope.CodeScope.Core.AppPaths.AppFolderName);
            Directory.CreateDirectory(dir);
            return Path.Combine(dir, "layout.json");
        }
    }

    public static Layout? Load()
    {
        try
        {
            if (!File.Exists(FilePath)) { return null; }
            var json = File.ReadAllText(FilePath);
            return JsonSerializer.Deserialize<Layout>(json);
        }
        catch (Exception ex) when (ex is IOException or JsonException)
        {
            System.Diagnostics.Debug.WriteLine($"[LayoutStore] Load: {ex.Message}");
            return null;
        }
    }

    public static void Save(Layout layout)
    {
        try
        {
            if (layout.GroupCount < 1) { return; }
            File.WriteAllText(FilePath, JsonSerializer.Serialize(layout));
        }
        catch (Exception ex)
        {
            // Best-effort — a write failure here shouldn't block shutdown, but we want it in traces.
            System.Diagnostics.Debug.WriteLine($"[LayoutStore] Save: {ex.Message}");
        }
    }
}
