using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class PullRequestServiceTests
{
    [Fact]
    public void ParseGhPrJson_EmptyArray_Returns_Null()
    {
        GitHubPullRequestService.ParseGhPrJson("[]").Should().BeNull();
    }

    [Fact]
    public void ParseGhPrJson_EmptyString_Returns_Null()
    {
        GitHubPullRequestService.ParseGhPrJson("").Should().BeNull();
    }

    [Fact]
    public void ParseGhPrJson_SuccessRollup_All_COMPLETED_SUCCESS()
    {
        const string json = """
            [
              {
                "number": 42,
                "state": "OPEN",
                "url": "https://github.com/acme/repo/pull/42",
                "statusCheckRollup": [
                  {"conclusion": "SUCCESS", "status": "COMPLETED"},
                  {"conclusion": "SUCCESS", "status": "COMPLETED"}
                ]
              }
            ]
            """;

        var pr = GitHubPullRequestService.ParseGhPrJson(json);

        pr.Should().NotBeNull();
        pr!.Number.Should().Be(42);
        pr.State.Should().Be("OPEN");
        pr.Url.Should().Be("https://github.com/acme/repo/pull/42");
        pr.CiStatus.Should().Be(CiStatus.Success);
    }

    [Fact]
    public void ParseGhPrJson_Pending_When_Any_Check_InProgress()
    {
        const string json = """
            [
              {
                "number": 7,
                "state": "OPEN",
                "url": "https://x",
                "statusCheckRollup": [
                  {"conclusion": "SUCCESS", "status": "COMPLETED"},
                  {"conclusion": "", "status": "IN_PROGRESS"}
                ]
              }
            ]
            """;

        GitHubPullRequestService.ParseGhPrJson(json)!.CiStatus.Should().Be(CiStatus.Pending);
    }

    [Theory]
    [InlineData("FAILURE")]
    [InlineData("CANCELLED")]
    [InlineData("TIMED_OUT")]
    [InlineData("ACTION_REQUIRED")]
    public void ParseGhPrJson_Failure_On_Any_Bad_Conclusion(string conclusion)
    {
        var json = $$"""
            [
              {
                "number": 1,
                "state": "OPEN",
                "url": "u",
                "statusCheckRollup": [
                  {"conclusion": "SUCCESS", "status": "COMPLETED"},
                  {"conclusion": "{{conclusion}}", "status": "COMPLETED"}
                ]
              }
            ]
            """;

        GitHubPullRequestService.ParseGhPrJson(json)!.CiStatus.Should().Be(CiStatus.Failure);
    }

    [Fact]
    public void ParseGhPrJson_NoRollupProperty_Returns_None()
    {
        const string json = """
            [ { "number": 1, "state": "OPEN", "url": "u" } ]
            """;

        GitHubPullRequestService.ParseGhPrJson(json)!.CiStatus.Should().Be(CiStatus.None);
    }

    [Fact]
    public void ParseGhPrJson_EmptyRollup_Returns_None()
    {
        const string json = """
            [ { "number": 1, "state": "OPEN", "url": "u", "statusCheckRollup": [] } ]
            """;

        GitHubPullRequestService.ParseGhPrJson(json)!.CiStatus.Should().Be(CiStatus.None);
    }
}
