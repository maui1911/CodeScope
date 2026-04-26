using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class AgentMenuEntryTests
{
    [Fact]
    public void Constructor_AssignsHeaderAndId()
    {
        var entry = new AgentMenuEntry("Claude", "claude");

        entry.Header.Should().Be("Claude");
        entry.Id.Should().Be("claude");
    }

    [Fact]
    public void RecordEquality_StructuralByValue()
    {
        var a = new AgentMenuEntry("Claude", "claude");
        var b = new AgentMenuEntry("Claude", "claude");
        var c = new AgentMenuEntry("Claude", "codex");

        a.Should().Be(b);
        a.GetHashCode().Should().Be(b.GetHashCode());
        a.Should().NotBe(c);
    }
}
