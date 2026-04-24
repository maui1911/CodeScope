using System.Text.Json;
using System.Text.Json.Serialization;
using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class ProjectStore : IProjectStore
{
    private static readonly JsonSerializerOptions SerializerOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.CamelCase) },
    };

    private readonly ILogger<ProjectStore> _logger;

    public ProjectStore(ILogger<ProjectStore> logger, string? overrideConfigPath = null)
    {
        _logger = logger;
        ConfigPath = overrideConfigPath ?? DefaultConfigPath();
    }

    public string ConfigPath { get; }

    public async Task<Result<ProjectsConfig>> LoadAsync(CancellationToken ct = default)
    {
        if (!File.Exists(ConfigPath))
        {
            _logger.LogInformation("Config {Path} not found; returning empty config", ConfigPath);
            return Result<ProjectsConfig>.Ok(new ProjectsConfig());
        }

        try
        {
            await using var stream = File.OpenRead(ConfigPath);
            var config = await JsonSerializer
                .DeserializeAsync<ProjectsConfig>(stream, SerializerOptions, ct)
                .ConfigureAwait(false);

            if (config is null)
            {
                return Result<ProjectsConfig>.Fail($"Config at {ConfigPath} deserialized to null");
            }

            var migrated = Migrate(config);
            return Result<ProjectsConfig>.Ok(migrated);
        }
        catch (JsonException ex)
        {
            _logger.LogError(ex, "Invalid JSON in {Path}", ConfigPath);
            return Result<ProjectsConfig>.Fail($"Invalid JSON in {ConfigPath}: {ex.Message}");
        }
        catch (IOException ex)
        {
            _logger.LogError(ex, "I/O error reading {Path}", ConfigPath);
            return Result<ProjectsConfig>.Fail($"I/O error reading {ConfigPath}: {ex.Message}");
        }
    }

    public async Task<Result<bool>> SaveAsync(ProjectsConfig config, CancellationToken ct = default)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(ConfigPath)!);

            var tmpPath = ConfigPath + ".tmp";
            await using (var stream = File.Create(tmpPath))
            {
                await JsonSerializer
                    .SerializeAsync(stream, config with { Version = ProjectsConfig.CurrentVersion }, SerializerOptions, ct)
                    .ConfigureAwait(false);
            }

            // File.Move with overwrite=true is atomic on NTFS when source and dest are on the same volume.
            File.Move(tmpPath, ConfigPath, overwrite: true);
            return Result<bool>.Ok(true);
        }
        catch (IOException ex)
        {
            _logger.LogError(ex, "I/O error writing {Path}", ConfigPath);
            return Result<bool>.Fail($"I/O error writing {ConfigPath}: {ex.Message}");
        }
    }

    /// <summary>
    /// Apply schema migrations in order. Idempotent — running twice is a no-op.
    /// </summary>
    private static ProjectsConfig Migrate(ProjectsConfig config)
    {
        var projects = config.Projects
            // Rule 1 (Phase 2): rename the Phase 1 "open-tabs" synthetic bucket to "unsorted".
            .Select(p => string.Equals(p.Id, "open-tabs", StringComparison.OrdinalIgnoreCase)
                ? p with { Id = "unsorted", Name = "Unsorted" }
                : p)
            // Rule 2 (Phase 3): guarantee a primary worktree when Path is non-empty.
            .Select(p =>
            {
                if (p.Worktrees.Count > 0 || string.IsNullOrEmpty(p.Path))
                {
                    return p;
                }

                var primary = new Models.Worktree
                {
                    Id = "primary",
                    Path = p.Path,
                    IsPrimary = true,
                    Branch = null,
                };
                return p with { Worktrees = [primary] };
            })
            .ToList();

        return config with { Projects = projects };
    }

    private static string DefaultConfigPath()
    {
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        return Path.Combine(appData, "CodeScope", "projects.json");
    }
}
