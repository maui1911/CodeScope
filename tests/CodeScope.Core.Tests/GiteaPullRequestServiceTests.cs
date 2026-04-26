using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GiteaPullRequestServiceTests
{
    [Fact]
    public void ParseTeaPrJson_EmptyArray_Returns_Null()
    {
        GiteaPullRequestService.ParseTeaPrJson("[]", "feat/x").Should().BeNull();
    }

    [Fact]
    public void ParseTeaPrJson_EmptyString_Returns_Null()
    {
        GiteaPullRequestService.ParseTeaPrJson("", "feat/x").Should().BeNull();
    }

    [Fact]
    public void ParseTeaPrJson_Matches_On_Flat_Head_String()
    {
        // Shape produced by `tea pulls list --output json --fields index,state,url,head`.
        const string json = """
            [
              { "index": 42, "state": "open", "url": "https://gitea.example/acme/repo/pulls/42", "head": "feat/x" },
              { "index": 43, "state": "open", "url": "https://gitea.example/acme/repo/pulls/43", "head": "feat/y" }
            ]
            """;

        var pr = GiteaPullRequestService.ParseTeaPrJson(json, "feat/x");

        pr.Should().NotBeNull();
        pr!.Number.Should().Be(42);
        pr.State.Should().Be("open");
        pr.Url.Should().Be("https://gitea.example/acme/repo/pulls/42");
        pr.CiStatus.Should().Be(CiStatus.None);
    }

    [Fact]
    public void ParseTeaPrJson_Matches_On_Nested_Head_Ref()
    {
        // Shape produced by the raw Gitea REST API (head is an object with a ref field).
        const string json = """
            [
              { "number": 7, "state": "open", "html_url": "https://gitea.example/acme/repo/pulls/7",
                "head": { "ref": "bugfix/42" } }
            ]
            """;

        var pr = GiteaPullRequestService.ParseTeaPrJson(json, "bugfix/42");

        pr.Should().NotBeNull();
        pr!.Number.Should().Be(7);
        pr.Url.Should().Be("https://gitea.example/acme/repo/pulls/7");
    }

    [Fact]
    public void ParseTeaPrJson_No_Match_Returns_Null()
    {
        const string json = """
            [ { "index": 1, "state": "open", "url": "u", "head": "other" } ]
            """;

        GiteaPullRequestService.ParseTeaPrJson(json, "feat/x").Should().BeNull();
    }
}
