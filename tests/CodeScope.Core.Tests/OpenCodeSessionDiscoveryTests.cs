using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class OpenCodeSessionDiscoveryTests : IDisposable
{
    private readonly string _root;
    private readonly string _cwd;

    public OpenCodeSessionDiscoveryTests()
    {
        _root = Path.Combine(Path.GetTempPath(), "codescope-oc-disc-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_root);
        _cwd = @"C:\dev\fake-oc-project";
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    private OpenCodeSessionDiscovery NewSut() =>
        new(NullLogger<OpenCodeSessionDiscovery>.Instance, _root);

    private string WriteAssistantMessage(string slug, string sessionId, string cwd)
    {
        var dir = Path.Combine(_root, "project", slug, "storage", "message", sessionId);
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, "msg_1.json");
        // Build the JSON via concatenation — embedded { } trip up raw-string interpolation.
        var json = "{\"id\":\"msg_1\",\"role\":\"assistant\",\"parts\":[{\"type\":\"text\",\"text\":\"hi\"}],"
            + "\"metadata\":{\"time\":{\"created\":1,\"completed\":2},\"sessionID\":\"" + sessionId + "\",\"tool\":{},"
            + "\"assistant\":{\"system\":[],\"modelID\":\"x\",\"providerID\":\"a\","
            + "\"path\":{\"cwd\":\"" + cwd.Replace("\\", "\\\\") + "\",\"root\":\"" + cwd.Replace("\\", "\\\\") + "\"},"
            + "\"cost\":0,\"tokens\":{\"input\":1,\"output\":1,\"reasoning\":0,\"cache\":{\"read\":0,\"write\":0}}}}}";
        File.WriteAllText(file, json);
        return file;
    }

    [Fact]
    public async Task Discovers_New_Session_With_Matching_Cwd()
    {
        var sut = NewSut();
        var tcs = new TaskCompletionSource<(string id, string path)>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (id, path) => tcs.TrySetResult((id, path)));

        var sessionId = "ses-" + Guid.NewGuid().ToString("N")[..10];
        var file = WriteAssistantMessage("fake-oc-project", sessionId, _cwd);

        var completed = await Task.WhenAny(tcs.Task, Task.Delay(3000));
        completed.Should().Be(tcs.Task);
        var (id, path) = await tcs.Task;
        id.Should().Be(sessionId);
        path.Should().Be(file);
    }

    [Fact]
    public async Task Ignores_Different_Cwd()
    {
        var sut = NewSut();
        var fired = false;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => fired = true);

        var sessionId = "ses-other";
        WriteAssistantMessage("other-project", sessionId, @"C:\some\other\repo");

        await Task.Delay(800);
        fired.Should().BeFalse();
    }

    [Fact]
    public async Task Matches_Cwd_Across_Slash_Variants()
    {
        var sut = NewSut();
        var tcs = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (id, _) => tcs.TrySetResult(id));

        var sessionId = "ses-xform";
        WriteAssistantMessage("variant-slug", sessionId, "/c/dev/fake-oc-project");

        var completed = await Task.WhenAny(tcs.Task, Task.Delay(3000));
        completed.Should().Be(tcs.Task);
        (await tcs.Task).Should().Be(sessionId);
    }

    [Fact]
    public async Task Ignores_Files_Older_Than_Since()
    {
        var sessionId = "ses-old";
        var file = WriteAssistantMessage("fake-oc-project", sessionId, _cwd);
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
    public async Task Callback_Fires_Once_Per_Session_Even_With_Multiple_Messages()
    {
        var sut = NewSut();
        var count = 0;
        using var handle = sut.Watch(_cwd, DateTimeOffset.UtcNow, (_, _) => Interlocked.Increment(ref count));

        var sessionId = "ses-multi";
        WriteAssistantMessage("fake-oc-project", sessionId, _cwd);

        await Task.Delay(500);

        // Append a second message — should NOT re-fire for the same session id.
        var dir = Path.Combine(_root, "project", "fake-oc-project", "storage", "message", sessionId);
        var json2 = "{\"id\":\"msg_2\",\"role\":\"user\",\"parts\":[],\"metadata\":{\"time\":{\"created\":3},\"sessionID\":\"" + sessionId + "\",\"tool\":{}}}";
        await File.WriteAllTextAsync(Path.Combine(dir, "msg_2.json"), json2);
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

        WriteAssistantMessage("fake-oc-project", "ses-late", _cwd);
        await Task.Delay(600);

        fired.Should().BeFalse();
    }
}
