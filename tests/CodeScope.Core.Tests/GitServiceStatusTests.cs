using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitServiceStatusTests
{
    [Fact]
    public void ParseStatusV2_Clean_Branch_No_Upstream()
    {
        const string input = """
            # branch.oid abc123
            # branch.head main

            """;

        var s = GitService.ParseStatusV2(input);

        s.Branch.Should().Be("main");
        s.IsDirty.Should().BeFalse();
        s.Ahead.Should().Be(0);
        s.Behind.Should().Be(0);
    }

    [Fact]
    public void ParseStatusV2_Ahead_Behind_Populated()
    {
        const string input = """
            # branch.oid abc
            # branch.head feat/x
            # branch.upstream origin/feat/x
            # branch.ab +3 -1

            """;

        var s = GitService.ParseStatusV2(input);

        s.Branch.Should().Be("feat/x");
        s.Ahead.Should().Be(3);
        s.Behind.Should().Be(1);
        s.IsDirty.Should().BeFalse();
    }

    [Fact]
    public void ParseStatusV2_Dirty_On_Any_Change_Line()
    {
        const string input = """
            # branch.oid abc
            # branch.head main
            1 .M N... 100644 100644 100644 aaa bbb README.md
            ? new-file.txt
            """;

        var s = GitService.ParseStatusV2(input);

        s.IsDirty.Should().BeTrue();
    }

    [Fact]
    public void ParseStatusV2_Detached_HEAD_Has_Null_Branch()
    {
        const string input = """
            # branch.oid abc
            # branch.head (detached)

            """;

        var s = GitService.ParseStatusV2(input);

        s.Branch.Should().BeNull();
    }

    [Fact]
    public void ParseNumstat_Sums_Additions_Removals_And_Counts_Files()
    {
        const string input = "3\t1\tsrc/a.cs\n10\t0\tsrc/b.cs\n-\t-\tassets/logo.png\n";

        var (added, removed, files) = GitService.ParseNumstat(input);

        added.Should().Be(13);
        removed.Should().Be(1);
        files.Should().Be(3);
    }

    [Fact]
    public void ParseNumstat_Empty_Yields_Zeros()
    {
        var (added, removed, files) = GitService.ParseNumstat("");
        added.Should().Be(0);
        removed.Should().Be(0);
        files.Should().Be(0);
    }
}
