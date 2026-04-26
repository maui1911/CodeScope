using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class PullRequestServiceDispatchTests
{
    [Theory]
    [InlineData("https://github.com/acme/repo.git", true)]
    [InlineData("git@github.com:acme/repo.git", true)]
    [InlineData("ssh://git@github.com/acme/repo", true)]
    [InlineData("https://GitHub.com/acme/repo", true)]
    [InlineData("https://gitea.example.com/acme/repo.git", false)]
    [InlineData("git@gitea.mycorp.net:acme/repo.git", false)]
    [InlineData("", false)]
    public void IsGitHubRemote_Detects_Host(string url, bool expected)
    {
        PullRequestService.IsGitHubRemote(url).Should().Be(expected);
    }

    [Theory]
    // Env var unset → falls back to github.com rule only.
    [InlineData("https://ghe.acme.internal/acme/repo.git", null, false)]
    [InlineData("https://ghe.acme.internal/acme/repo.git", "", false)]
    // Single host, exact.
    [InlineData("https://ghe.acme.internal/acme/repo.git", "ghe.acme.internal", true)]
    // Comma-separated list with whitespace.
    [InlineData("git@github.mycorp.com:a/b.git", " github.mycorp.com , ghe.internal ", true)]
    // Host in env but not in URL.
    [InlineData("https://gitea.example/a/b.git", "ghe.internal,github.mycorp.com", false)]
    // Always-on github.com still wins even when env is set to something else.
    [InlineData("https://github.com/a/b.git", "ghe.internal", true)]
    public void IsGitHubRemote_Reads_EnvVar_For_GHES(string url, string? env, bool expected)
    {
        PullRequestService.IsGitHubRemote(url, env).Should().Be(expected);
    }
}
