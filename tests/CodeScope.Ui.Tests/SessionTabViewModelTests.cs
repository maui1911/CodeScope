using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class SessionTabViewModelTests
{
    private static SessionDescriptor MakeDescriptor(string id = "s1", string title = "main", params string[] args) =>
        new()
        {
            Id = id,
            WorkingDirectory = @"C:\repo",
            Shell = "pwsh.exe",
            ShellArgs = args,
            Title = title,
        };

    [Fact]
    public void Icon_DefaultsToBulletWhenNullOrWhitespace()
    {
        new SessionTabViewModel(MakeDescriptor(), null, null, null, icon: null).Icon.Should().Be("●");
        new SessionTabViewModel(MakeDescriptor(), null, null, null, icon: "").Icon.Should().Be("●");
        new SessionTabViewModel(MakeDescriptor(), null, null, null, icon: "   ").Icon.Should().Be("●");
    }

    [Fact]
    public void Icon_PreservesProvidedGlyph()
    {
        new SessionTabViewModel(MakeDescriptor(), null, null, null, icon: "🤖").Icon.Should().Be("🤖");
    }

    [Fact]
    public void DisplayName_FallsBackToDescriptorTitleWhenOverrideBlank()
    {
        var d = MakeDescriptor(title: "fallback-branch");
        new SessionTabViewModel(d, null, null, displayNameOverride: null).DisplayName.Should().Be("fallback-branch");
        new SessionTabViewModel(d, null, null, displayNameOverride: "").DisplayName.Should().Be("fallback-branch");
        new SessionTabViewModel(d, null, null, displayNameOverride: "  ").DisplayName.Should().Be("fallback-branch");
    }

    [Fact]
    public void DisplayName_HonoursOverride()
    {
        var d = MakeDescriptor(title: "ignored");
        new SessionTabViewModel(d, null, null, displayNameOverride: "explicit").DisplayName.Should().Be("explicit");
    }

    [Fact]
    public void CommandLine_NoArgs_IsJustShell()
    {
        var d = MakeDescriptor();
        new SessionTabViewModel(d, null, null, null).CommandLine.Should().Be("pwsh.exe");
    }

    [Fact]
    public void CommandLine_JoinsArgsWithSpaces()
    {
        var d = MakeDescriptor(args: new[] { "-NoLogo", "-NoExit" });
        new SessionTabViewModel(d, null, null, null).CommandLine.Should().Be("pwsh.exe -NoLogo -NoExit");
    }

    [Fact]
    public void AutomationId_UsesDisplayName_AndPrefixesTab()
    {
        var vm = new SessionTabViewModel(MakeDescriptor(title: "T"), null, null, "feature/x");
        vm.AutomationId.Should().Be("Tab_feature_x");
    }

    [Fact]
    public void AutomationId_FallsBackToDescriptorId_WhenDisplayNameBlank()
    {
        var vm = new SessionTabViewModel(MakeDescriptor(id: "abc-123", title: "T"), null, null, displayNameOverride: null);
        vm.DisplayName = "   "; // whitespace ⇒ AutomationId falls back to descriptor id

        vm.AutomationId.Should().Be("Tab_abc_123");
    }

    [Fact]
    public void AutomationId_RaisesPropertyChangeWhenDisplayNameChanges()
    {
        var vm = new SessionTabViewModel(MakeDescriptor(), null, null, "first");
        var changed = new List<string?>();
        vm.PropertyChanged += (_, e) => changed.Add(e.PropertyName);

        vm.DisplayName = "second";

        changed.Should().Contain(nameof(SessionTabViewModel.AutomationId));
        changed.Should().Contain(nameof(SessionTabViewModel.Title));
    }

    [Fact]
    public void Title_MirrorsDisplayName()
    {
        var vm = new SessionTabViewModel(MakeDescriptor(), null, null, "label");
        vm.Title.Should().Be("label");

        vm.DisplayName = "renamed";
        vm.Title.Should().Be("renamed");
    }

    [Fact]
    public void Rebind_SwapsDescriptorAndRefreshesCommandLine()
    {
        var vm = new SessionTabViewModel(MakeDescriptor(args: new[] { "-A" }), null, null, "n");
        vm.CommandLine.Should().Be("pwsh.exe -A");
        var fired = false;
        vm.PropertyChanged += (_, e) => { if (e.PropertyName == nameof(SessionTabViewModel.CommandLine)) { fired = true; } };

        vm.Rebind(MakeDescriptor(args: new[] { "-B", "-C" }));

        vm.CommandLine.Should().Be("pwsh.exe -B -C");
        fired.Should().BeTrue();
    }

    // ──── ClosedAtRelative time-bucket tests ─────────────────────────────

    private static SessionTabViewModel MakeClosedVm(int minutesAgo)
        => new(MakeDescriptor(), "proj", null, null)
        {
            ClosedAt = DateTimeOffset.UtcNow.AddMinutes(-minutesAgo),
        };

    [Fact]
    public void ClosedAtRelative_IsEmpty_ForLiveRows()
    {
        var vm = new SessionTabViewModel(MakeDescriptor(), null, null, null);
        // ClosedAt is null — live tab
        vm.ClosedAtRelative.Should().BeEmpty();
    }

    [Theory]
    [InlineData(0, "just now")]    // < 1 minute
    [InlineData(5, "5m ago")]      // 5 minutes
    [InlineData(120, "2h ago")]    // 2 hours
    [InlineData(1500, "yesterday")] // 25 hours — between 24 h and 48 h → "yesterday"
    [InlineData(4320, "3d ago")]   // 3 days (< 7 days)
    public void ClosedAtRelative_Returns_Expected_Bucket(int minutesAgo, string expected)
    {
        var vm = MakeClosedVm(minutesAgo);
        vm.ClosedAtRelative.Should().Be(expected);
    }

    [Fact]
    public void ClosedAtRelative_Returns_MonthDay_ForOldSessions()
    {
        // More than 7 days ago — should produce a "MMM d" formatted string (e.g. "Jan 5").
        var closedAt = new DateTimeOffset(2025, 1, 5, 12, 0, 0, TimeSpan.Zero);
        var vm = new SessionTabViewModel(MakeDescriptor(), "proj", null, null)
        {
            ClosedAt = closedAt,
        };
        // The result is formatted via LocalDateTime so we compare against the expected local representation.
        var expected = closedAt.LocalDateTime.ToString("MMM d");
        vm.ClosedAtRelative.Should().Be(expected);
    }
}
