using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// In-memory registry backed by a list passed at construction time.
/// Built defaults cover Claude Code, Codex and OpenCode.
/// </summary>
public sealed class AgentRegistry : IAgentRegistry
{
    private readonly IReadOnlyList<AgentProfile> _agents;

    public AgentRegistry(IEnumerable<AgentProfile>? agents = null)
    {
        _agents = agents?.ToList() ?? BuildDefaults();
    }

    public IReadOnlyList<AgentProfile> GetAll() => _agents;

    public AgentProfile? GetDefault() =>
        _agents.FirstOrDefault(a => a.IsDefault) ?? _agents.FirstOrDefault();

    public AgentProfile? GetById(string id) =>
        _agents.FirstOrDefault(a => string.Equals(a.Id, id, StringComparison.OrdinalIgnoreCase));

    /// <summary>Build a registry from the current config. Empty agent list → built-in defaults.</summary>
    public static AgentRegistry FromConfig(ProjectsConfig config)
    {
        return config.Agents.Count == 0
            ? new AgentRegistry()
            : new AgentRegistry(config.Agents);
    }

    private static List<AgentProfile> BuildDefaults() =>
    [
        new AgentProfile
        {
            Id = "claude",
            DisplayName = "Claude Code",
            Command = "claude",
            // Fresh launches (no known id) go through NewSessionArgs = [] → bare `claude`, and
            // `IClaudeSessionDiscovery` adopts whichever UUID the CLI mints. When we already
            // know the session id — cross-group drag, layout hydrate — we resume it via
            // `claude --resume <id>` (ResumeByIdArgs) so the conversation continues instead
            // of the user landing on a fresh agent. SessionIdFlag stays null because v2.1.118+
            // still refuses client-minted ids on new sessions.
            ResumeArgs = [],
            NewSessionArgs = [],
            SessionIdFlag = null,
            ResumeByIdArgs = ["--resume"],
            IsDefault = true,
            Icon = "✶",
            ContextWindowTokens = 1_000_000,
        },
        new AgentProfile
        {
            Id = "codex",
            DisplayName = "Codex",
            Command = "codex",
            ResumeArgs = ["resume"],
            NewSessionArgs = [],
            Icon = "◇",
        },
        new AgentProfile
        {
            Id = "opencode",
            DisplayName = "OpenCode",
            Command = "opencode",
            ResumeArgs = ["--continue"],
            NewSessionArgs = [],
            Icon = "◈",
        },
    ];
}
