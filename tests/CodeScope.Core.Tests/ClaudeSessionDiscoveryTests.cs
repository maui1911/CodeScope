using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class ClaudeSessionDiscoveryTests : IDisposable
{
    private readonly string _root;
    private readonly string _cwd;
    private readonly string _encodedDir;

    public ClaudeSessionDiscoveryTests()
    {
        _root = Path.Combine(Path.GetTempPath(), "codescope-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_root);
        _cwd = @"C:\dev\fake-project";
        _encodedDir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(_cwd));
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    private ClaudeSessionDiscovery NewSut() =>
        new(NullLogger<ClaudeSessionDiscovery>.Instance, _root);

    [Fact]
    public async Task Discovers_Jsonl_Created_After_Watch_Started()
    {
        var sut = NewSut();
        var tcs = new TaskCompletionSource<(string id, string path)>(TaskCreationOptions.RunContinuationsAsynchronously);

        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (id, path) => tcs.TrySetResult((id, path)));

        Directory.CreateDirectory(_encodedDir);
        var sessionId = Guid.NewGuid().ToString("D");
        var file = Path.Combine(_encodedDir, sessionId + ".jsonl");
        await File.WriteAllTextAsync(file, "{\"type\":\"user\"}\n");

        var completed = await Task.WhenAny(tcs.Task, Task.Delay(3000));
        completed.Should().Be(tcs.Task, "discovery should fire within 3s");
        var (id, path) = await tcs.Task;
        id.Should().Be(sessionId);
        path.Should().Be(file);
    }

    [Fact]
    public async Task Adopts_Preexisting_File_Via_Poll_Fallback()
    {
        // Poll fallback path — the jsonl already exists with ctime > since when the watcher attaches.
        Directory.CreateDirectory(_encodedDir);
        var since = DateTimeOffset.UtcNow.AddSeconds(-5);
        var sessionId = Guid.NewGuid().ToString("D");
        var file = Path.Combine(_encodedDir, sessionId + ".jsonl");
        await File.WriteAllTextAsync(file, "{}\n");

        var sut = NewSut();
        var tcs = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var handle = sut.Watch(_cwd, since, (id, _) => tcs.TrySetResult(id));

        var completed = await Task.WhenAny(tcs.Task, Task.Delay(3000));
        completed.Should().Be(tcs.Task);
        (await tcs.Task).Should().Be(sessionId);
    }

    [Fact]
    public async Task Ignores_Jsonl_Older_Than_Since()
    {
        Directory.CreateDirectory(_encodedDir);
        var sessionId = Guid.NewGuid().ToString("D");
        var file = Path.Combine(_encodedDir, sessionId + ".jsonl");
        await File.WriteAllTextAsync(file, "{}\n");
        var ancient = DateTime.UtcNow.AddDays(-30);
        File.SetCreationTimeUtc(file, ancient);
        File.SetLastWriteTimeUtc(file, ancient);

        var sut = NewSut();
        var fired = false;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);

        await Task.Delay(800);
        fired.Should().BeFalse("pre-existing file older than 'since' must be ignored");
    }

    [Fact]
    public async Task Ignores_Non_Uuid_Filenames()
    {
        var sut = NewSut();
        var fired = false;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);

        Directory.CreateDirectory(_encodedDir);
        await File.WriteAllTextAsync(Path.Combine(_encodedDir, "not-a-uuid.jsonl"), "{}");
        await Task.Delay(800);

        fired.Should().BeFalse();
    }

    [Fact]
    public async Task Callback_Fires_At_Most_Once()
    {
        var sut = NewSut();
        var count = 0;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => Interlocked.Increment(ref count));

        Directory.CreateDirectory(_encodedDir);
        var id = Guid.NewGuid().ToString("D");
        var file = Path.Combine(_encodedDir, id + ".jsonl");
        await File.WriteAllTextAsync(file, "{}");
        await Task.Delay(300);
        // Writes again — must not re-fire.
        await File.AppendAllTextAsync(file, "{}\n");
        await Task.Delay(500);

        count.Should().Be(1);
    }

    [Fact]
    public async Task Callback_Fires_For_Each_New_Jsonl_So_Clear_Rotations_Are_Adopted()
    {
        var sut = NewSut();
        var adopted = new List<string>();
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (id, _) =>
        {
            lock (adopted) { adopted.Add(id); }
        });

        Directory.CreateDirectory(_encodedDir);

        var id1 = Guid.NewGuid().ToString("D");
        await File.WriteAllTextAsync(Path.Combine(_encodedDir, id1 + ".jsonl"), "{}");

        await WaitForAsync(() => { lock (adopted) { return adopted.Count >= 1; } }, TimeSpan.FromSeconds(3));

        // Simulates what Claude Code does on `/clear`: a fresh session id ⇒ a fresh jsonl.
        var id2 = Guid.NewGuid().ToString("D");
        await File.WriteAllTextAsync(Path.Combine(_encodedDir, id2 + ".jsonl"), "{}");

        await WaitForAsync(() => { lock (adopted) { return adopted.Count >= 2; } }, TimeSpan.FromSeconds(3));

        lock (adopted)
        {
            adopted.Should().Contain(id1);
            adopted.Should().Contain(id2);
        }
    }

    private static async Task WaitForAsync(Func<bool> predicate, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            if (predicate()) { return; }
            await Task.Delay(50);
        }
    }

    [Fact]
    public async Task Dispose_Before_Discovery_Suppresses_Callback()
    {
        var sut = NewSut();
        var fired = false;
        var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);
        handle.Dispose();

        Directory.CreateDirectory(_encodedDir);
        var id = Guid.NewGuid().ToString("D");
        await File.WriteAllTextAsync(Path.Combine(_encodedDir, id + ".jsonl"), "{}");
        await Task.Delay(600);

        fired.Should().BeFalse();
    }
}
