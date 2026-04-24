using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class ClaudeTranscriptParserTests
{
    [Fact]
    public void ParseLine_Assistant_With_Usage_Extracts_Tokens()
    {
        const string line = """
        {"type":"assistant","sessionId":"abc-123","timestamp":"2026-04-22T08:59:44.811Z","message":{"role":"assistant","usage":{"input_tokens":6,"cache_creation_input_tokens":20667,"cache_read_input_tokens":16850,"output_tokens":521}}}
        """;

        var entry = ClaudeTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.SessionId.Should().Be("abc-123");
        entry.Type.Should().Be("assistant");
        entry.InputTokens.Should().Be(6);
        entry.OutputTokens.Should().Be(521);
        entry.CacheCreationTokens.Should().Be(20667);
        entry.CacheReadTokens.Should().Be(16850);
        entry.HasUsage.Should().BeTrue();
        entry.BillableTokens.Should().Be(6 + 521 + 20667);
        entry.Timestamp.Should().NotBeNull();
    }

    [Fact]
    public void ParseLine_User_Entry_Has_No_Usage()
    {
        const string line = """
        {"type":"user","sessionId":"abc-123","timestamp":"2026-04-22T08:59:44.811Z","message":{"role":"user","content":"hi"}}
        """;

        var entry = ClaudeTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.HasUsage.Should().BeFalse();
        entry.BillableTokens.Should().Be(0);
    }

    [Fact]
    public void ParseLine_FileHistory_Snapshot_Returns_NonNull_But_No_Usage()
    {
        const string line = """
        {"type":"file-history-snapshot","messageId":"x","snapshot":{}}
        """;

        var entry = ClaudeTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.HasUsage.Should().BeFalse();
        entry.SessionId.Should().BeNull();
    }

    [Fact]
    public void ParseLine_Invalid_Json_Returns_Null()
    {
        ClaudeTranscriptParser.ParseLine("{not json").Should().BeNull();
        ClaudeTranscriptParser.ParseLine("").Should().BeNull();
        ClaudeTranscriptParser.ParseLine("   ").Should().BeNull();
    }

    [Theory]
    [InlineData(@"C:\dev\codescope", "C--dev-codescope")]
    [InlineData(@"C:\Users\maui", "C--Users-maui")]
    [InlineData(@"D:\work\my-repo", "D--work-my-repo")]
    [InlineData(@"C:\dev\codescope.worktrees\feat-x", "C--dev-codescope-worktrees-feat-x")]
    public void EncodeCwd_Matches_ClaudeCode_Convention(string input, string expected)
    {
        ClaudeTranscriptParser.EncodeCwd(input).Should().Be(expected);
    }
}
