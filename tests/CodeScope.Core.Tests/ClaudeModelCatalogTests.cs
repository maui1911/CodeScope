using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class ClaudeModelCatalogTests
{
    [Theory]
    [InlineData("claude-opus-4-7[1m]", 1_000_000)]
    [InlineData("claude-opus-4-7-1m-20260115", 1_000_000)]
    [InlineData("claude-sonnet-4-6-1m", 1_000_000)]
    // Plain Opus 4.x model ids — Claude Code CLI uses 1M by default without embedding a tag.
    [InlineData("claude-opus-4-7", 1_000_000)]
    [InlineData("claude-opus-4-7-20260115", 1_000_000)]
    // Claude 3 Opus keeps the 200k cap.
    [InlineData("claude-3-opus-20240229", 200_000)]
    [InlineData("claude-sonnet-4-6", 200_000)]
    [InlineData("claude-haiku-4-5-20251001", 200_000)]
    public void Known_Models_Resolve_To_Expected_Capacity(string id, int expected) =>
        ClaudeModelCatalog.GetContextWindow(id).Should().Be(expected);

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("gpt-4o")]
    [InlineData("some-random-string")]
    public void Unknown_Or_Empty_Returns_Zero(string? id) =>
        ClaudeModelCatalog.GetContextWindow(id).Should().Be(0);
}
