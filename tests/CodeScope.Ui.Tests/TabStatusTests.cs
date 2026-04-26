using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class TabStatusTests
{
    [Fact]
    public void Enum_HasThreeStates()
    {
        Enum.GetValues<TabStatus>().Should().HaveCount(3);
    }

    [Fact]
    public void Enum_IdleIsDefaultZero()
    {
        // ApplyStatus relies on default(TabStatus) == Idle (the most common rest state).
        ((int)default(TabStatus)).Should().Be(0);
        default(TabStatus).Should().Be(TabStatus.Idle);
    }

    [Fact]
    public void Enum_NameRoundtrip()
    {
        Enum.Parse<TabStatus>("Active").Should().Be(TabStatus.Active);
        Enum.Parse<TabStatus>("Wait").Should().Be(TabStatus.Wait);
        Enum.Parse<TabStatus>("Idle").Should().Be(TabStatus.Idle);
    }
}
