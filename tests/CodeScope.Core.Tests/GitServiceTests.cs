using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Microsoft.Extensions.Logging.Abstractions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitServiceTests
{
    [Fact]
    public async Task GetVersion_Returns_Success_When_Git_On_Path()
    {
        var service = new GitService(NullLogger<GitService>.Instance);

        var result = await service.GetVersionAsync();

        result.IsSuccess.Should().BeTrue(because: "git is expected to be on PATH on dev machines");
        result.Value.Should().StartWith("git version");
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
