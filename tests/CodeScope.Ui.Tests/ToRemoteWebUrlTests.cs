using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class ToRemoteWebUrlTests
{
    [Theory]
    [InlineData("https://github.com/owner/repo.git", "https://github.com/owner/repo")]
    [InlineData("https://github.com/owner/repo", "https://github.com/owner/repo")]
    [InlineData("http://gitea.local/owner/repo.git", "http://gitea.local/owner/repo")]
    [InlineData("https://github.com/owner/repo.git  ", "https://github.com/owner/repo")]
    public void Https_RemovesTrailingDotGit(string remote, string expected)
    {
        SidebarViewModel.ToRemoteWebUrl(remote).Should().Be(expected);
    }

    [Theory]
    [InlineData("git@github.com:owner/repo.git", "https://github.com/owner/repo")]
    [InlineData("git@github.com:owner/repo", "https://github.com/owner/repo")]
    [InlineData("git@gitea.internal:org/project.git", "https://gitea.internal/org/project")]
    public void ScpStyle_ConvertedToHttps(string remote, string expected)
    {
        SidebarViewModel.ToRemoteWebUrl(remote).Should().Be(expected);
    }

    [Theory]
    [InlineData("ssh://git@github.com/owner/repo.git", "https://github.com/owner/repo")]
    [InlineData("ssh://git@github.com/owner/repo", "https://github.com/owner/repo")]
    [InlineData("ssh://github.com/owner/repo.git", "https://github.com/owner/repo")]
    public void SshScheme_ConvertedToHttps(string remote, string expected)
    {
        SidebarViewModel.ToRemoteWebUrl(remote).Should().Be(expected);
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("ftp://example.com/repo")]
    [InlineData("not-a-url")]
    public void InvalidOrUnsupported_ReturnsNull(string remote)
    {
        SidebarViewModel.ToRemoteWebUrl(remote).Should().BeNull();
    }

}
