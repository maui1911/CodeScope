using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class CommandPaletteViewModelTests
{
    private static PaletteAction A(string title, string? subtitle = null) =>
        new(title, subtitle, () => Task.CompletedTask);

    // ---------- Score (pure ranker) ----------

    [Fact]
    public void Score_EmptyNeedle_ReturnsZero()
    {
        CommandPaletteViewModel.Score("hello world", "").Should().Be(0);
    }

    [Fact]
    public void Score_EmptyHaystackNonEmptyNeedle_ReturnsMinusOne()
    {
        CommandPaletteViewModel.Score("", "hi").Should().Be(-1);
    }

    [Fact]
    public void Score_PrefixMatch_GetsHighest()
    {
        var prefix = CommandPaletteViewModel.Score("New session", "new");
        var middle = CommandPaletteViewModel.Score("Open new tab", "new");
        prefix.Should().BeGreaterThan(middle);
        prefix.Should().BeGreaterOrEqualTo(1500);  // 1000 + 500 prefix bonus
    }

    [Fact]
    public void Score_SubstringMatch_BeatsSubsequenceMatch()
    {
        var contig = CommandPaletteViewModel.Score("Toggle diff panel", "diff");
        var sub = CommandPaletteViewModel.Score("Defer initial fetches forever", "diff");
        contig.Should().BeGreaterThan(sub);
        contig.Should().BeGreaterOrEqualTo(1000);
    }

    [Fact]
    public void Score_SubsequenceMatch_ReturnsAtLeast100()
    {
        var s = CommandPaletteViewModel.Score("Reveal in Explorer", "rev");
        s.Should().BeGreaterOrEqualTo(100);
    }

    [Fact]
    public void Score_NoMatch_ReturnsMinusOne()
    {
        CommandPaletteViewModel.Score("Toggle diff panel", "xyz").Should().Be(-1);
    }

    [Fact]
    public void Score_IsCaseInsensitive()
    {
        CommandPaletteViewModel.Score("New Session", "NEW").Should().BeGreaterOrEqualTo(1500);
        CommandPaletteViewModel.Score("new session", "NEW").Should().BeGreaterOrEqualTo(1500);
    }

    [Fact]
    public void Score_PrefixSubstringBeatsLaterSubstring()
    {
        // Both match as substrings, but "tab" at index 0 of "tabletop" is the prefix arm
        // (1000 + 500 = 1500), while at index 5 of "Next tab" it's substring-elsewhere
        // (1000 + max(0, 200-5) = 1195).
        var prefix = CommandPaletteViewModel.Score("tabletop", "tab");
        var later  = CommandPaletteViewModel.Score("Next tab", "tab");
        prefix.Should().BeGreaterThan(later);
    }

    // ---------- Construction + filter behaviour ----------

    [Fact]
    public void Construction_PopulatesFilteredWithAllActions()
    {
        var vm = new CommandPaletteViewModel(new[] { A("alpha"), A("beta") });

        vm.Filtered.Select(p => p.Title).Should().Equal("alpha", "beta");
        vm.Query.Should().BeEmpty();
    }

    [Fact]
    public void Query_SubsequenceFiltersAndOrdersByScore()
    {
        var vm = new CommandPaletteViewModel(new[]
        {
            A("Toggle diff panel"),
            A("Defer fetches"),
            A("New session"),
        });

        vm.Query = "diff";

        vm.Filtered.Select(p => p.Title).Should().ContainInOrder("Toggle diff panel");
        vm.Filtered.Should().NotContain(p => p.Title == "New session");
    }

    [Fact]
    public void Query_NoMatch_ClearsFilteredAndUnsetsSelection()
    {
        var vm = new CommandPaletteViewModel(new[] { A("alpha"), A("beta") });

        vm.Query = "zzz";

        vm.Filtered.Should().BeEmpty();
        vm.Selected.Should().BeNull();
    }

    [Fact]
    public void Query_SetsSelectedToFirstFilteredAction()
    {
        var vm = new CommandPaletteViewModel(new[]
        {
            A("New session"),
            A("Toggle diff panel"),
        });

        vm.Query = "diff";

        vm.Selected.Should().NotBeNull();
        vm.Selected!.Title.Should().Be("Toggle diff panel");
    }

    // ---------- PaletteAction.Display ----------

    [Fact]
    public void PaletteAction_Display_TitleOnlyWhenSubtitleBlank()
    {
        new PaletteAction("Title", null, () => Task.CompletedTask).Display.Should().Be("Title");
        new PaletteAction("Title", "", () => Task.CompletedTask).Display.Should().Be("Title");
        new PaletteAction("Title", "  ", () => Task.CompletedTask).Display.Should().Be("Title");
    }

    [Fact]
    public void PaletteAction_Display_TitleAndSubtitleSeparated()
    {
        new PaletteAction("Open", "Ctrl+O", () => Task.CompletedTask).Display
            .Should().Be("Open   —   Ctrl+O");
    }
}
