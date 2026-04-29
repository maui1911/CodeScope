using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitServiceCloneTests
{
    [SkippableFact]
    public async Task Clone_From_Local_Bare_Repo_Succeeds()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var src = Path.Combine(tmp.Path, "src.git");
        await RunGit(tmp.Path, $"init --bare -b main \"{src}\"");
        // Seed one commit so HEAD resolves after clone.
        var seed = Path.Combine(tmp.Path, "seed");
        Directory.CreateDirectory(seed);
        await RunGit(seed, "init -b main");
        await RunGit(seed, "config user.email test@test");
        await RunGit(seed, "config user.name test");
        await File.WriteAllTextAsync(Path.Combine(seed, "x.txt"), "hi");
        await RunGit(seed, "add .");
        await RunGit(seed, "commit -m seed");
        await RunGit(seed, $"remote add origin \"{src}\"");
        await RunGit(seed, "push -u origin main");

        var svc = new GitService(NullLogger<GitService>.Instance);
        var dest = Path.Combine(tmp.Path, "dest");
        Directory.CreateDirectory(dest);

        var result = await svc.CloneAsync(src, dest, "repo");

        result.IsSuccess.Should().BeTrue(result.IsFailure ? result.Error : "");
        result.Value.Should().Be(Path.Combine(dest, "repo"));
        File.Exists(Path.Combine(result.Value, ".git", "HEAD")).Should().BeTrue();
        File.Exists(Path.Combine(result.Value, "x.txt")).Should().BeTrue();
    }

    [SkippableFact]
    public async Task Clone_Fails_When_Target_Already_Exists_NonEmpty()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var dest = Path.Combine(tmp.Path, "parent");
        Directory.CreateDirectory(Path.Combine(dest, "repo"));
        await File.WriteAllTextAsync(Path.Combine(dest, "repo", "block.txt"), "x");

        var svc = new GitService(NullLogger<GitService>.Instance);

        var result = await svc.CloneAsync("https://example.invalid/x.git", dest, "repo");

        result.IsFailure.Should().BeTrue();
        result.Error.Should().NotBeNullOrWhiteSpace();
    }

    [SkippableFact]
    public async Task Clone_Fails_For_Garbage_Url()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var svc = new GitService(NullLogger<GitService>.Instance);

        var result = await svc.CloneAsync("not a url at all", tmp.Path, "repo");

        result.IsFailure.Should().BeTrue();
        result.Error.Should().NotBeNullOrWhiteSpace();
    }

    [SkippableFact]
    public async Task Clone_With_PreCancelled_Token_Throws_OperationCanceled()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var svc = new GitService(NullLogger<GitService>.Instance);
        using var cts = new CancellationTokenSource();
        cts.Cancel();

        Func<Task> act = () => svc.CloneAsync("https://example.invalid/x.git", tmp.Path, "repo", cts.Token);

        await act.Should().ThrowAsync<OperationCanceledException>();
    }

    private static bool IsGitOnPath()
    {
        var paths = (Environment.GetEnvironmentVariable("PATH") ?? string.Empty)
            .Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries);
        var exeNames = OperatingSystem.IsWindows() ? new[] { "git.exe", "git.cmd" } : new[] { "git" };
        return paths.Any(p => exeNames.Any(n => File.Exists(Path.Combine(p, n))));
    }

    private static async Task RunGit(string cwd, string args)
    {
        var psi = new System.Diagnostics.ProcessStartInfo("git", args)
        {
            WorkingDirectory = cwd,
            RedirectStandardError = true,
            RedirectStandardOutput = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        using var p = System.Diagnostics.Process.Start(psi)!;
        await p.WaitForExitAsync();
        if (p.ExitCode != 0)
        {
            throw new InvalidOperationException($"git {args}: {await p.StandardError.ReadToEndAsync()}");
        }
    }

    private sealed class TempDir : IDisposable
    {
        public string Path { get; } = System.IO.Path.Combine(System.IO.Path.GetTempPath(), $"cs-clone-{Guid.NewGuid():N}");
        public TempDir() => Directory.CreateDirectory(Path);
        public void Dispose()
        {
            try { Directory.Delete(Path, recursive: true); } catch { /* best effort */ }
        }
    }
}
