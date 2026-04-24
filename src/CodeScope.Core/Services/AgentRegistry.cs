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
            // Claude Code v2.1.118+ broke client-minted session ids and `--continue` / `--resume`
            // on fresh hydrates (no deferred tool marker; forks session id on /clear). CodeScope
            // now always launches `claude` bare and adopts whichever UUID the CLI picks by
            // watching the transcript directory — see `IClaudeSessionDiscovery`.
            ResumeArgs = [],
            NewSessionArgs = [],
            SessionIdFlag = null,
            ResumeByIdArgs = [],
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
