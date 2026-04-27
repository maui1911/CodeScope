using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class PiSessionDiscoveryTests : IDisposable
{
    private readonly string _root;
    private readonly string _cwd;

    public PiSessionDiscoveryTests()
    {
        _root = Path.Combine(Path.GetTempPath(), "codescope-pi-disc-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_root);
        _cwd = @"C:\dev\fake-pi-project";
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    private PiSessionDiscovery NewSut() =>
        new(NullLogger<PiSessionDiscovery>.Instance, _root);

    private string WriteSession(string subdir, string sessionId, string cwd)
    {
        var dir = Path.Combine(_root, subdir);
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, $"2026-04-22T08-00-00-000Z_{sessionId}.jsonl");
        File.WriteAllText(file,
            $$"""{"type":"session","version":3,"id":"{{sessionId}}","timestamp":"2026-04-22T08:00:00Z","cwd":"{{cwd.Replace("\\", "\\\\")}}"}""" + "\n");
        return file;
    }

    [Fact]
    public async Task Discovers_New_Session_File_With_Matching_Cwd()
    {
        var sut = NewSut();
        var tcs = new TaskCompletionSource<(string id, string path)>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (id, path) => tcs.TrySetResult((id, path)));

        var sessionId = Guid.NewGuid().ToString("D");
        var file = WriteSession("--C--dev-fake-pi-project--", sessionId, _cwd);

        var completed = await Task.WhenAny(tcs.Task, Task.Delay(3000));
        completed.Should().Be(tcs.Task, "discovery should fire within 3s");
        var (id, path) = await tcs.Task;
        id.Should().Be(sessionId);
        path.Should().Be(file);
    }

    [Fact]
    public async Task Ignores_Session_File_With_Different_Cwd()
    {
        var sut = NewSut();
        var fired = false;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);

        var sessionId = Guid.NewGuid().ToString("D");
        WriteSession("--unrelated--", sessionId, @"C:\some\other\path");

        await Task.Delay(800);
        fired.Should().BeFalse();
    }

    [Fact]
    public async Task Matches_Cwd_With_Forward_Slashes_And_Drive_Letter_Variants()
    {
        // Pi's header may write "/c/dev/fake-pi-project" or "C:/dev/..." depending on platform.
        // Canonicalisation should make any of those equivalent.
        var sut = NewSut();
        var tcs = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (id, _) => tcs.TrySetResult(id));

        var sessionId = Guid.NewGuid().ToString("D");
        WriteSession("--c-dev-fake-pi-project--", sessionId, "/c/dev/fake-pi-project");

        var completed = await Task.WhenAny(tcs.Task, Task.Delay(3000));
        completed.Should().Be(tcs.Task);
        (await tcs.Task).Should().Be(sessionId);
    }

    [Fact]
    public async Task Ignores_Files_Older_Than_Since()
    {
        var sessionId = Guid.NewGuid().ToString("D");
        var file = WriteSession("--C--dev-fake-pi-project--", sessionId, _cwd);
        var ancient = DateTime.UtcNow.AddDays(-30);
        File.SetCreationTimeUtc(file, ancient);
        File.SetLastWriteTimeUtc(file, ancient);

        var sut = NewSut();
        var fired = false;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);

        await Task.Delay(800);
        fired.Should().BeFalse();
    }

    [Fact]
    public async Task Ignores_Non_Pi_Filenames()
    {
        var sut = NewSut();
        var fired = false;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);

        var dir = Path.Combine(_root, "--C--dev-fake-pi-project--");
        Directory.CreateDirectory(dir);
        await File.WriteAllTextAsync(Path.Combine(dir, "garbage.jsonl"),
            $$"""{"type":"session","id":"x","cwd":"{{_cwd.Replace("\\", "\\\\")}}"}""");
        await Task.Delay(800);

        fired.Should().BeFalse();
    }

    [Fact]
    public async Task Callback_Fires_At_Most_Once_Per_File()
    {
        var sut = NewSut();
        var count = 0;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => Interlocked.Increment(ref count));

        var sessionId = Guid.NewGuid().ToString("D");
        var file = WriteSession("--C--dev-fake-pi-project--", sessionId, _cwd);

        await Task.Delay(500);
        await File.AppendAllTextAsync(file, "{\"type\":\"message\"}\n");
        await Task.Delay(800);

        count.Should().Be(1);
    }

    [Fact]
    public async Task Dispose_Suppresses_Future_Callbacks()
    {
        var sut = NewSut();
        var fired = false;
        var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);
        handle.Dispose();

        var sessionId = Guid.NewGuid().ToString("D");
        WriteSession("--C--dev-fake-pi-project--", sessionId, _cwd);
        await Task.Delay(600);

        fired.Should().BeFalse();
    }
}
