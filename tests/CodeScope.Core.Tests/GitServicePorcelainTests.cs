using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitServicePorcelainTests
{
    [Fact]
    public void ParsePorcelain_Handles_Single_Primary_Worktree()
    {
        const string input = """
            worktree C:/proj
            HEAD abcdef1234
            branch refs/heads/main

            """;

        var result = GitService.ParsePorcelain(input);

        result.Should().ContainSingle();
        result[0].IsPrimary.Should().BeTrue();
        result[0].Path.Should().Be("C:/proj");
        result[0].Branch.Should().Be("main");
    }

    [Fact]
    public void ParsePorcelain_Handles_Multiple_Worktrees()
    {
        const string input = """
            worktree C:/proj
            HEAD abcdef1
            branch refs/heads/main

            worktree C:/proj.worktrees/feat-x
            HEAD deadbee
            branch refs/heads/feat/x

            """;

        var result = GitService.ParsePorcelain(input);

        result.Should().HaveCount(2);
        result[0].IsPrimary.Should().BeTrue();
        result[0].Branch.Should().Be("main");
        result[1].IsPrimary.Should().BeFalse();
        result[1].Branch.Should().Be("feat/x");
        result[1].Path.Should().Be("C:/proj.worktrees/feat-x");
    }

    [Fact]
    public void ParsePorcelain_Detached_HEAD_Has_Null_Branch()
    {
        const string input = """
            worktree C:/proj
            HEAD abcdef1
            detached

            """;

        var result = GitService.ParsePorcelain(input);

        result[0].Branch.Should().BeNull();
    }

    [Fact]
    public void ParsePorcelain_Handles_Missing_Trailing_Blank_Line()
    {
        const string input = "worktree C:/proj\nHEAD a\nbranch refs/heads/main";

        var result = GitService.ParsePorcelain(input);

        result.Should().ContainSingle();
        result[0].Path.Should().Be("C:/proj");
    }
}
