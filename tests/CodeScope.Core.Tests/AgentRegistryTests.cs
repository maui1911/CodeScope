using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class AgentRegistryTests
{
    [Fact]
    public void Default_Set_Includes_Claude_Codex_Opencode_Pi()
    {
        var registry = new AgentRegistry();

        var ids = registry.GetAll().Select(a => a.Id).ToList();
        ids.Should().Contain(["claude", "codex", "opencode", "pi"]);
    }

    [Fact]
    public void Pi_Profile_Has_Resume_Continue_And_SessionFlag()
    {
        var pi = new AgentRegistry().GetById("pi")!;
        pi.Command.Should().Be("pi");
        pi.ResumeArgs.Should().BeEquivalentTo(["-c"]);
        pi.ResumeByIdArgs.Should().BeEquivalentTo(["--session"]);
        pi.SessionIdFlag.Should().BeNull();
        pi.IsDefault.Should().BeFalse();
    }

    [Fact]
    public void OpenCode_Profile_Has_Resume_Continue_And_SessionFlag()
    {
        var oc = new AgentRegistry().GetById("opencode")!;
        oc.Command.Should().Be("opencode-cli");
        oc.ResumeArgs.Should().BeEquivalentTo(["--continue"]);
        oc.ResumeByIdArgs.Should().BeEquivalentTo(["--session"]);
        oc.SessionIdFlag.Should().BeNull();
        oc.IsDefault.Should().BeFalse();
    }

    [Fact]
    public void Default_Set_Has_Exactly_One_Default()
    {
        var registry = new AgentRegistry();

        registry.GetAll().Count(a => a.IsDefault).Should().Be(1);
        registry.GetDefault()!.Id.Should().Be("claude");
    }

    [Fact]
    public void GetById_Is_Case_Insensitive()
    {
        var registry = new AgentRegistry();

        registry.GetById("CLAUDE").Should().NotBeNull();
        registry.GetById("claude").Should().NotBeNull();
    }

    [Fact]
    public void Custom_List_Overrides_Defaults()
    {
        var registry = new AgentRegistry(
        [
            new AgentProfile
            {
                Id = "only",
                DisplayName = "Only",
                Command = "only",
                IsDefault = true,
            },
        ]);

        registry.GetAll().Should().ContainSingle(a => a.Id == "only");
        registry.GetById("claude").Should().BeNull();
    }

    [Fact]
    public void FromConfig_Empty_Falls_Back_To_Defaults()
    {
        var registry = AgentRegistry.FromConfig(new ProjectsConfig());
        registry.GetAll().Select(a => a.Id).Should().Contain(["claude", "codex", "opencode", "pi"]);
    }

    [Fact]
    public void FromConfig_Uses_Configured_Agents_When_Present()
    {
        var cfg = new ProjectsConfig
        {
            Agents =
            [
                new AgentProfile { Id = "mine", DisplayName = "Mine", Command = "mine", IsDefault = true },
            ],
        };
        var registry = AgentRegistry.FromConfig(cfg);

        registry.GetAll().Should().ContainSingle(a => a.Id == "mine");
        registry.GetDefault()!.Id.Should().Be("mine");
    }
}
