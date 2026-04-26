using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class WorktreeViewModelTests
{
    private static Worktree MakeWt(string id = "feat-x", bool primary = false, string? branch = "feat-x") =>
        new() { Id = id, Path = $@"C:\dev\repo.worktrees\{id}", Branch = branch, IsPrimary = primary };

    private static WorktreeViewModel MakeVm(bool primary = false, string? branch = "feat-x")
        => new("p1", MakeWt("feat-x", primary, branch));

    // ---------- DisplayBranch ----------

    [Fact]
    public void DisplayBranch_UsesBranchWhenSet()
    {
        MakeVm(branch: "feature/x").DisplayBranch.Should().Be("feature/x");
    }

    [Fact]
    public void DisplayBranch_PrimaryFallsBackToMain()
    {
        MakeVm(primary: true, branch: null).DisplayBranch.Should().Be("main");
    }

    [Fact]
    public void DisplayBranch_NonPrimaryNullBranch_ShowsNoBranchLabel()
    {
        MakeVm(primary: false, branch: null).DisplayBranch.Should().Be("(no branch)");
    }

    // ---------- AutomationId ----------

    [Fact]
    public void AutomationId_CombinesProjectAndBranchTokens()
    {
        var vm = new WorktreeViewModel("codescope", MakeWt(branch: "feature/x"));
        vm.AutomationId.Should().Be("Worktree_codescope__feature_x");
    }

    // ---------- DotState ----------

    [Fact]
    public void DotState_NoSessions_IsRest()
    {
        MakeVm().DotState.Should().Be("rest");
    }

    [Fact]
    public void DotState_WithReadySession_IsReady()
    {
        var vm = MakeVm();
        vm.Sessions.Add(MakeSessionTab(TabStatus.Ready));
        vm.DotState.Should().Be("ready");
    }

    [Fact]
    public void DotState_BusySession_IsBusy()
    {
        var vm = MakeVm();
        vm.Sessions.Add(MakeSessionTab(TabStatus.Busy));
        vm.DotState.Should().Be("busy");
    }

    [Fact]
    public void DotState_FailingCiAlone_DoesNotSurfaceOnAgentDot()
    {
        // Agent dot is reserved for agent state; CI failure shows up via the StatusLabel
        // slug ("ci!") instead so the two state machines never collide.
        var vm = MakeVm();
        vm.PullRequest = MakePr(CiStatus.Failure);
        vm.DotState.Should().Be("rest");
    }

    // ---------- DirtyGlyph ----------

    [Fact]
    public void DirtyGlyph_FlipsWithIsDirty()
    {
        var vm = MakeVm();
        vm.DirtyGlyph.Should().Be("●");
        vm.IsDirty = true;
        vm.DirtyGlyph.Should().Be("✎");
    }

    // ---------- CiGlyph ----------

    [Theory]
    [InlineData(CiStatus.Success, "✓")]
    [InlineData(CiStatus.Pending, "◐")]
    [InlineData(CiStatus.Failure, "✗")]
    [InlineData(CiStatus.None, "·")]
    public void CiGlyph_MapsStatusToGlyph(CiStatus status, string expected)
    {
        var vm = MakeVm();
        vm.PullRequest = MakePr(status);
        vm.CiGlyph.Should().Be(expected);
    }

    [Fact]
    public void CiGlyph_NoPullRequest_IsEmpty()
    {
        // The switch-default returns string.Empty when there's no PR / unknown status.
        MakeVm().CiGlyph.Should().BeEmpty();
    }

    // ---------- PrBadgeText ----------

    [Fact]
    public void PrBadgeText_EmptyWhenNoPullRequest()
    {
        MakeVm().PrBadgeText.Should().BeEmpty();
    }

    [Fact]
    public void PrBadgeText_RendersHashNumberWhenSet()
    {
        var vm = MakeVm();
        vm.PullRequest = MakePr(CiStatus.None, number: 42);
        vm.PrBadgeText.Should().Be("#42");
    }

    [Fact]
    public void PrBadgeText_EmptyWhenPrNumberIsZero()
    {
        var vm = MakeVm();
        vm.PullRequest = MakePr(CiStatus.None, number: 0);
        vm.PrBadgeText.Should().BeEmpty();
    }

    // ---------- HasPullRequest ----------

    [Fact]
    public void HasPullRequest_TracksPullRequestProperty()
    {
        var vm = MakeVm();
        vm.HasPullRequest.Should().BeFalse();
        vm.PullRequest = MakePr(CiStatus.Success);
        vm.HasPullRequest.Should().BeTrue();
    }

    // ---------- AheadBehindText ----------

    [Fact]
    public void AheadBehindText_BothZero_IsEmpty()
    {
        MakeVm().AheadBehindText.Should().BeEmpty();
    }

    [Fact]
    public void AheadBehindText_AheadOnly()
    {
        var vm = MakeVm();
        vm.Ahead = 3;
        vm.AheadBehindText.Should().Be("↑3");
    }

    [Fact]
    public void AheadBehindText_BehindOnly()
    {
        var vm = MakeVm();
        vm.Behind = 2;
        vm.AheadBehindText.Should().Be("↓2");
    }

    [Fact]
    public void AheadBehindText_BothNonZero()
    {
        var vm = MakeVm();
        vm.Ahead = 3;
        vm.Behind = 2;
        vm.AheadBehindText.Should().Be("↑3 ↓2");
    }

    // ---------- AddedRemovedText ----------

    [Fact]
    public void AddedRemovedText_NoChange_IsEmpty()
    {
        MakeVm().AddedRemovedText.Should().BeEmpty();
    }

    [Fact]
    public void AddedRemovedText_AddedOnly()
    {
        var vm = MakeVm();
        vm.Added = 5;
        vm.AddedRemovedText.Should().Be("+5");
    }

    [Fact]
    public void AddedRemovedText_RemovedOnly()
    {
        var vm = MakeVm();
        vm.Removed = 4;
        vm.AddedRemovedText.Should().Be("−4");
    }

    [Fact]
    public void AddedRemovedText_BothNonZero()
    {
        var vm = MakeVm();
        vm.Added = 5;
        vm.Removed = 4;
        vm.AddedRemovedText.Should().Be("+5 −4");
    }

    [Fact]
    public void AddedRemovedText_BinaryOnly_ShowsTildeChangedFiles()
    {
        var vm = MakeVm();
        vm.ChangedFiles = 3;
        vm.AddedRemovedText.Should().Be("~3");
    }

    // ---------- StatusLabel ----------

    [Fact]
    public void StatusLabel_CleanInSync_IsIdle()
    {
        MakeVm().StatusLabel.Should().Be("idle");
    }

    [Fact]
    public void StatusLabel_Dirty_IsChg()
    {
        var vm = MakeVm();
        vm.IsDirty = true;
        vm.StatusLabel.Should().Be("chg");
    }

    [Fact]
    public void StatusLabel_OutOfSyncCleanShowsAheadBehind()
    {
        var vm = MakeVm();
        vm.Ahead = 1;
        vm.StatusLabel.Should().Be("↑1");
    }

    [Fact]
    public void StatusLabel_FailingCi_ShowsCiSlug()
    {
        var vm = MakeVm();
        vm.IsDirty = true;
        vm.PullRequest = MakePr(CiStatus.Failure);
        vm.StatusLabel.Should().Be("ci!");
    }

    [Fact]
    public void StatusLabel_BusySession_OverridesDirtyAndCi()
    {
        var vm = MakeVm();
        vm.IsDirty = true;
        vm.PullRequest = MakePr(CiStatus.Failure);
        vm.Sessions.Add(MakeSessionTab(TabStatus.Busy));
        vm.StatusLabel.Should().Be("busy");
    }

    // ---------- ApplyStatus ----------

    [Fact]
    public void ApplyStatus_ProjectsAllFieldsAndUpdatesBranch()
    {
        var vm = MakeVm(branch: "old");
        var status = new WorktreeStatus
        {
            Branch = "new",
            IsDirty = true,
            Added = 2,
            Removed = 1,
            ChangedFiles = 3,
            Ahead = 1,
            Behind = 0,
        };

        vm.ApplyStatus(status);

        vm.IsDirty.Should().BeTrue();
        vm.Added.Should().Be(2);
        vm.Removed.Should().Be(1);
        vm.ChangedFiles.Should().Be(3);
        vm.Ahead.Should().Be(1);
        vm.Behind.Should().Be(0);
        vm.DisplayBranch.Should().Be("new");
    }

    // ---------- helpers ----------

    private static SessionTabViewModel MakeSessionTab(TabStatus status)
    {
        var d = new NoScope.CodeScope.Core.Services.SessionDescriptor
        {
            Id = "s",
            WorkingDirectory = @"C:\repo",
            Shell = "pwsh.exe",
            Title = "t",
        };
        var t = new SessionTabViewModel(d, "p1", null, "t");
        t.Status = status;
        return t;
    }

    private static PullRequestInfo MakePr(CiStatus ci, int number = 1) =>
        new() { Number = number, State = "OPEN", Url = "https://x/y/pull/" + number, CiStatus = ci };
}
