using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitServiceTests
{
    [SkippableFact]
    public async Task GetVersion_Returns_Success_When_Git_On_Path()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH — skipping rather than false-passing");

        var service = new GitService(NullLogger<GitService>.Instance);

        var result = await service.GetVersionAsync();

        result.IsSuccess.Should().BeTrue(because: "git is on PATH for this run");
        result.Value.Should().StartWith("git version");
    }

    private static bool IsGitOnPath()
    {
        var paths = (Environment.GetEnvironmentVariable("PATH") ?? string.Empty)
            .Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries);
        var exeNames = OperatingSystem.IsWindows() ? new[] { "git.exe", "git.cmd" } : new[] { "git" };
        return paths.Any(p => exeNames.Any(n => File.Exists(Path.Combine(p, n))));
    }

    [Fact]
    public async Task GetVersion_Returns_Failure_When_Git_Missing()
    {
        var service = new GitService(NullLogger<GitService>.Instance, "definitely-not-a-real-git-binary");

        var result = await service.GetVersionAsync();

        result.IsFailure.Should().BeTrue();
        result.Error.Should().Contain("not found");
    }
}
