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
    public void Enum_ReadyIsDefaultZero()
    {
        // SessionTabViewModel and ApplyStatus rely on default(TabStatus) == Ready (calm rest state).
        ((int)default(TabStatus)).Should().Be(0);
        default(TabStatus).Should().Be(TabStatus.Ready);
    }

    [Fact]
    public void Enum_NameRoundtrip()
    {
        Enum.Parse<TabStatus>("Ready").Should().Be(TabStatus.Ready);
        Enum.Parse<TabStatus>("Busy").Should().Be(TabStatus.Busy);
    }
}
