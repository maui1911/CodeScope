using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class OverviewCardViewModelTests
{
    private static WorktreeViewModel MakeWt(string branch = "feat-x", bool dirty = false, int ahead = 0, int behind = 0)
    {
        var vm = new WorktreeViewModel("p1", new Worktree { Id = branch, Path = $@"C:\dev\repo.worktrees\{branch}", Branch = branch });
        if (dirty) { vm.IsDirty = true; }
        vm.Ahead = ahead;
        vm.Behind = behind;
        return vm;
    }

    private static SessionTabViewModel MakeSession(string id = "s") =>
        new(new SessionDescriptor { Id = id, WorkingDirectory = @"C:\r", Shell = "pwsh.exe", Title = id }, "p", null, id);

    private static OverviewCardViewModel MakeCard(
        OverviewCardState state = OverviewCardState.Idle,
        WorktreeViewModel? wt = null,
        int siblings = 1,
        string? agent = "claude") =>
        new("CodeScope", wt ?? MakeWt(), MakeSession(), state, siblings, agent);

    [Fact]
    public void Construction_FillsTrivialProjections()
    {
        var card = MakeCard();
        card.ProjectName.Should().Be("CodeScope");
        card.AgentDisplayName.Should().Be("claude");
        card.SiblingSessions.Should().Be(1);
    }

    [Fact]
    public void AgentDisplayName_FallsBackToShell_WhenBlank()
    {
        MakeCard(agent: null).AgentDisplayName.Should().Be("shell");
        MakeCard(agent: "").AgentDisplayName.Should().Be("shell");
        MakeCard(agent: "  ").AgentDisplayName.Should().Be("shell");
    }

    [Fact]
    public void DisplayTitle_CombinesProjectAndBranch()
    {
        var card = MakeCard(wt: MakeWt(branch: "main"));
        card.DisplayTitle.Should().Be("CodeScope · main");
    }

    [Theory]
    [InlineData(OverviewCardState.Active, "Accent.Primary")]
    [InlineData(OverviewCardState.Waiting, "Signal.Warn")]
    [InlineData(OverviewCardState.Idle, "Signal.Ok")]
    public void StateDotResourceKey_PerState(OverviewCardState state, string expected)
    {
        MakeCard(state).StateDotResourceKey.Should().Be(expected);
    }

    [Theory]
    [InlineData(OverviewCardState.Active, "Accent.Ring")]
    [InlineData(OverviewCardState.Waiting, "Accent.Ring")]
    [InlineData(OverviewCardState.Idle, "Surface.Canvas")]
    public void StateDotRingResourceKey_PerState(OverviewCardState state, string expected)
    {
        MakeCard(state).StateDotRingResourceKey.Should().Be(expected);
    }

    [Fact]
    public void ChangesLabel_TallyOfDirtyAheadBehind()
    {
        MakeCard(wt: MakeWt(dirty: false)).ChangesLabel.Should().Be("changes: 0");
        MakeCard(wt: MakeWt(dirty: true)).ChangesLabel.Should().Be("changes: 1");
        MakeCard(wt: MakeWt(dirty: true, ahead: 2)).ChangesLabel.Should().Be("changes: 2");
        MakeCard(wt: MakeWt(dirty: true, ahead: 2, behind: 1)).ChangesLabel.Should().Be("changes: 3");
    }

    [Fact]
    public void HasChanges_TrueWhenAnyOfDirtyAheadBehind()
    {
        MakeCard(wt: MakeWt()).HasChanges.Should().BeFalse();
        MakeCard(wt: MakeWt(dirty: true)).HasChanges.Should().BeTrue();
        MakeCard(wt: MakeWt(ahead: 1)).HasChanges.Should().BeTrue();
        MakeCard(wt: MakeWt(behind: 1)).HasChanges.Should().BeTrue();
    }

    [Fact]
    public void ElapsedLabel_DashWhenSingleSession()
    {
        MakeCard(siblings: 1).ElapsedLabel.Should().Be("—");
    }

    [Fact]
    public void ElapsedLabel_CountWhenMultipleSessions()
    {
        MakeCard(siblings: 3).ElapsedLabel.Should().Be("3 sess");
    }

    [Fact]
    public void TypeBadge_IsEmpty()
    {
        MakeCard().TypeBadge.Should().BeEmpty();
    }

    [Fact]
    public void PreviewLines_IncludeStatusAndPrSegments()
    {
        var card = MakeCard(wt: MakeWt(dirty: true), state: OverviewCardState.Active);

        card.PreviewLines.Should().HaveCount(6);
        card.PreviewLines[0].Kind.Should().Be(OverviewPreviewKind.Prompt);
        card.PreviewLines[2].Text.Should().StartWith("status ");
        card.PreviewLines[2].Kind.Should().Be(OverviewPreviewKind.Warn); // dirty
        card.PreviewLines[3].Text.Should().Contain("pr     no open PR");
    }

    [Fact]
    public void OpenCommand_FiresOpenRequestedWithSession()
    {
        var card = MakeCard();
        SessionTabViewModel? captured = null;
        card.OpenRequested += (_, s) => captured = s;

        card.OpenCommand.Execute(null);

        captured.Should().NotBeNull();
        captured.Should().BeSameAs(card.Session);
    }
}
