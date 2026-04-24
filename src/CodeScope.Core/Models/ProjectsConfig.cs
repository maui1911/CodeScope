using System.Text.Json.Serialization;

namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// Root object persisted to %APPDATA%\CodeScope\projects.json.
/// </summary>
public sealed record ProjectsConfig
{
    /// <summary>Current schema version. Incremented by additive migrations.</summary>
    public int Version { get; init; } = CurrentVersion;

    public IReadOnlyList<AgentProfile> Agents { get; init; } = [];

    public IReadOnlyList<Project> Projects { get; init; } = [];

    /// <summary>Latest schema version known to this build.</summary>
    [JsonIgnore]
    public const int CurrentVersion = 1;
}
