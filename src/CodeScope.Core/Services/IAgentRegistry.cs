using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Provides the known agent profiles. Phase 1 has a static default set; Phase 2 wires this to the config file.
/// <para>
/// The seam exists so later phases can swap in dynamically-reloadable registries
/// (file watchers, plugin-loaded agents) without touching every consumer, and so
/// tests can inject a fixed-profile stub through DI rather than the file-backed
/// <see cref="AgentRegistry.FromConfig"/> path. Today's sole production impl is
/// static, but the registration shape is intentional, not ceremonial.
/// </para>
/// </summary>
public interface IAgentRegistry
{
    IReadOnlyList<AgentProfile> GetAll();

    AgentProfile? GetDefault();

    AgentProfile? GetById(string id);
}
