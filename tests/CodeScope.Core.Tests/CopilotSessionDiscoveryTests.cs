using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class CopilotSessionDiscoveryTests : IDisposable
{
    private readonly string _root;
    private readonly CopilotSessionDiscovery _sut;

    public CopilotSessionDiscoveryTests()
    {
        _root = Path.Combine(Path.GetTempPath(), $"copilot-disc-{Guid.NewGuid():N}");
        Directory.CreateDirectory(_root);
        _sut = new CopilotSessionDiscovery(NullLogger<CopilotSessionDiscovery>.Instance, _root);
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, true); } catch { }
    }

    [Fact]
    public void Watch_Discovers_Existing_Session_By_WorkspaceYaml()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "workspace.yaml"), $"""
            id: {sid}
            cwd: d:\Dev\my-project
            git_root: D:\Dev\my-project
            branch: main
            created_at: 2026-04-15T12:37:39.412Z
            """);
        // Touch directory timestamp so it's after 'since'.
        Directory.SetCreationTimeUtc(dir, DateTime.UtcNow);
        Directory.SetLastWriteTimeUtc(dir, DateTime.UtcNow);

        string? discoveredId = null;
        using var handle = _sut.Watch(@"d:\Dev\my-project",
            DateTimeOffset.UtcNow.AddMinutes(-1),
            (id, _) => discoveredId = id);

        // Poll fires immediately (TimeSpan.Zero initial delay).
        Thread.Sleep(600);

        discoveredId.Should().Be(sid);
    }

    [Fact]
    public void Watch_Does_Not_Fire_For_Different_Cwd()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "workspace.yaml"), $"""
            id: {sid}
            cwd: d:\Dev\other-project
            git_root: D:\Dev\other-project
            branch: main
            """);
        Directory.SetCreationTimeUtc(dir, DateTime.UtcNow);
        Directory.SetLastWriteTimeUtc(dir, DateTime.UtcNow);

        string? discoveredId = null;
        using var handle = _sut.Watch(@"d:\Dev\my-project",
            DateTimeOffset.UtcNow.AddMinutes(-1),
            (id, _) => discoveredId = id);

        Thread.Sleep(600);

        discoveredId.Should().BeNull();
    }

    [Fact]
    public void Watch_Falls_Back_To_EventsJsonl_When_No_Yaml()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        // No workspace.yaml — only events.jsonl with session.start.
        File.WriteAllText(Path.Combine(dir, "events.jsonl"),
            $$$"""{"type":"session.start","data":{"sessionId":"{{{sid}}}","selectedModel":"gpt-4o","context":{"cwd":"d:\\Dev\\fallback-project"}},"id":"a","timestamp":"2026-04-15T12:00:00.000Z","parentId":null}""");
        Directory.SetCreationTimeUtc(dir, DateTime.UtcNow);
        Directory.SetLastWriteTimeUtc(dir, DateTime.UtcNow);

        string? discoveredId = null;
        using var handle = _sut.Watch(@"d:\Dev\fallback-project",
            DateTimeOffset.UtcNow.AddMinutes(-1),
            (id, _) => discoveredId = id);

        Thread.Sleep(600);

        discoveredId.Should().Be(sid);
    }

    [Fact]
    public void Watch_Ignores_Non_Guid_Directories()
    {
        var dir = Path.Combine(_root, "not-a-uuid");
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "workspace.yaml"), """
            id: not-a-uuid
            cwd: d:\Dev\my-project
            """);
        Directory.SetCreationTimeUtc(dir, DateTime.UtcNow);
        Directory.SetLastWriteTimeUtc(dir, DateTime.UtcNow);

        string? discoveredId = null;
        using var handle = _sut.Watch(@"d:\Dev\my-project",
            DateTimeOffset.UtcNow.AddMinutes(-1),
            (id, _) => discoveredId = id);

        Thread.Sleep(600);

        discoveredId.Should().BeNull();
    }

    [Fact]
    public void Watch_Fires_Only_Once_Per_Session()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "workspace.yaml"), $"""
            id: {sid}
            cwd: d:\Dev\my-project
            """);
        Directory.SetCreationTimeUtc(dir, DateTime.UtcNow);
        Directory.SetLastWriteTimeUtc(dir, DateTime.UtcNow);

        var count = 0;
        using var handle = _sut.Watch(@"d:\Dev\my-project",
            DateTimeOffset.UtcNow.AddMinutes(-1),
            (_, _) => Interlocked.Increment(ref count));

        // Wait for multiple poll cycles.
        Thread.Sleep(1200);

        count.Should().Be(1);
    }

    [Fact]
    public void Dispose_Handle_Stops_Watch()
    {
        var sid = Guid.NewGuid().ToString();

        var handle = _sut.Watch(@"d:\Dev\test",
            DateTimeOffset.UtcNow.AddMinutes(-1),
            (_, _) => { });

        // Should not throw.
        handle.Dispose();
        handle.Dispose(); // idempotent
    }
}
