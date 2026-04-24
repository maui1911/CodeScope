using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitHubPullRequestServiceCreateTests
{
    [Fact]
    public void QuoteArg_Wraps_Plain_String()
        => ProcessRunner.QuoteArg("hello").Should().Be("\"hello\"");

    [Fact]
    public void QuoteArg_Escapes_Embedded_Quote()
        => ProcessRunner.QuoteArg("say \"hi\"").Should().Be("\"say \\\"hi\\\"\"");

    [Fact]
    public void ExtractLastUrl_Picks_Trailing_Url()
    {
        const string output = """
            Creating pull request for feat/x into main in acme/repo
            https://github.com/acme/repo/pull/42
            """;

        ProcessRunner.ExtractLastUrl(output)
            .Should().Be("https://github.com/acme/repo/pull/42");
    }

    [Fact]
    public void ExtractLastUrl_Returns_Null_For_No_Url()
        => ProcessRunner.ExtractLastUrl("no urls here").Should().BeNull();

    [Theory]
    [InlineData("https://github.com/acme/repo/pull/42", 42)]
    [InlineData("https://github.com/acme/repo/pull/0", 0)]
    [InlineData("nonsense", 0)]
    [InlineData("", 0)]
    public void ExtractPrNumberFromUrl_Parses_Trailing_Segment(string url, int expected)
        => GitHubPullRequestService.ExtractPrNumberFromUrl(url).Should().Be(expected);
}
