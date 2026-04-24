using System.Reflection;

namespace NoScope.CodeScope.App;

/// <summary>
/// Exposes the product version for UI binding. The value is baked into the
/// assembly at build time from <c>git describe --tags --always --dirty</c>
/// (see <c>Directory.Build.targets</c>), so tagging <c>v0.1</c> and rebuilding
/// flips every consumer automatically.
/// </summary>
public static class VersionInfo
{
    /// <summary>Short chrome-ready form, e.g. <c>V0.1</c> or <c>V0.1-5-gabc1234</c>.</summary>
    public static string Display { get; } = ComputeDisplay();

    private static string ComputeDisplay()
    {
        var raw = typeof(VersionInfo).Assembly
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()?
            .InformationalVersion ?? "0.0";

        // SourceLink appends build metadata as "+commitSha" — drop it.
        var plus = raw.IndexOf('+');
        if (plus >= 0) { raw = raw[..plus]; }

        // Normalise the conventional leading 'v' so the chrome label renders as "V0.1".
        if (raw.StartsWith('v') || raw.StartsWith('V')) { raw = raw[1..]; }

        return $"V{raw}";
    }
}
