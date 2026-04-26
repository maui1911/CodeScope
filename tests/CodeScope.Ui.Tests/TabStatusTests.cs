using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class TabStatusTests
{
    [Fact]
    public void Enum_HasTwoStates()
    {
        Enum.GetValues<TabStatus>().Should().HaveCount(2);
    }

    [Fact]
    public void Enum_IdleIsDefaultZero()
    {
        // SessionTabViewModel and ApplyStatus rely on default(TabStatus) == Idle (calm rest state).
        ((int)default(TabStatus)).Should().Be(0);
        default(TabStatus).Should().Be(TabStatus.Idle);
    }

    [Fact]
    public void Enum_NameRoundtrip()
    {
        Enum.Parse<TabStatus>("Idle").Should().Be(TabStatus.Idle);
        Enum.Parse<TabStatus>("Busy").Should().Be(TabStatus.Busy);
    }
}
