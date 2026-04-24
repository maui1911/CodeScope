namespace NoScope.CodeScope.Core;

/// <summary>
/// Shared app-identity shims. When the environment variable <c>CODESCOPE_DEV</c> is set to a
/// truthy value at process start the folder name under <c>%APPDATA%</c> / <c>%LOCALAPPDATA%</c>
/// and the single-instance mutex name shift to a <c>.Dev</c> variant, so a locally-built
/// debug instance can run next to an installed v0.1.0+ without colliding on config, layout,
/// or the single-instance guard.
/// <para>
/// Resolved once at class-init so the rest of the code can treat it as a constant — toggling
/// the env var at runtime has no effect.
/// </para>
/// </summary>
public static class AppPaths
{
    /// <summary>Env var that flips the app into dev-mode paths/mutex. <c>1</c>, <c>true</c>, <c>yes</c> (case-insensitive) are truthy.</summary>
    public const string DevEnvVar = "CODESCOPE_DEV";

    /// <summary>True when the dev-mode env var was set at process start.</summary>
    public static bool IsDevMode { get; } = ParseDev(Environment.GetEnvironmentVariable(DevEnvVar));

    /// <summary><c>CodeScope</c> normally; <c>CodeScope.Dev</c> when <see cref="IsDevMode"/> is true.</summary>
    public static string AppFolderName { get; } = IsDevMode ? "CodeScope.Dev" : "CodeScope";

    /// <summary>Single-instance mutex name — gets a <c>.Dev</c> suffix in dev mode.</summary>
    public static string SingleInstanceMutexName { get; } =
        IsDevMode ? @"Global\CodeScope.SingleInstance.Dev" : @"Global\CodeScope.SingleInstance";

    private static bool ParseDev(string? value) =>
        !string.IsNullOrWhiteSpace(value)
        && (value.Equals("1", StringComparison.Ordinal)
            || value.Equals("true", StringComparison.OrdinalIgnoreCase)
            || value.Equals("yes", StringComparison.OrdinalIgnoreCase));
}
