using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Reads and writes the projects.json config.
/// </summary>
public interface IProjectStore
{
    /// <summary>Absolute path of the backing file.</summary>
    string ConfigPath { get; }

    /// <summary>
    /// Load the config. If the file is missing, returns a default empty config (and does NOT create the file).
    /// Invalid JSON returns a failure result; the caller decides whether to back up and recreate.
    /// </summary>
    Task<Result<ProjectsConfig>> LoadAsync(CancellationToken ct = default);

    /// <summary>Atomically write the config (tmp-file + rename).</summary>
    Task<Result<bool>> SaveAsync(ProjectsConfig config, CancellationToken ct = default);
}
