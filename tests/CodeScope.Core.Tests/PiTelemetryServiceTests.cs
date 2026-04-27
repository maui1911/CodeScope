using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class PiTelemetryServiceTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "cs-pi-telemetry-" + Guid.NewGuid().ToString("n"));

    public PiTelemetryServiceTests() => Directory.CreateDirectory(_root);

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { /* best effort */ }
    }

    private string WriteSession(string sessionId, string subdirEncoded, IEnumerable<string> lines)
    {
        var dir = Path.Combine(_root, subdirEncoded);
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, $"2026-04-22T08-00-00-000Z_{sessionId}.jsonl");
        File.WriteAllLines(file, lines);
        return file;
    }

    [Fact]
    public void Register_Reads_Existing_Transcript_And_Raises_Updated()
    {
        var sessionId = Guid.NewGuid().ToString("D");
        WriteSession(sessionId, "--C--dev-myrepo--",
        [
            $$"""{"type":"session","version":3,"id":"{{sessionId}}","timestamp":"2026-04-22T08:00:00Z","cwd":"/c/dev/myrepo"}""",
            """{"type":"message","id":"u1","timestamp":"2026-04-22T08:00:01Z","message":{"role":"user","content":"hi"}}""",
            """{"type":"message","id":"a1","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","provider":"anthropic","model":"x","usage":{"input":10,"output":20,"cacheRead":50,"cacheWrite":100},"stopReason":"tool_use"}}""",
            """{"type":"message","id":"a2","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","provider":"anthropic","model":"x","usage":{"input":5,"output":15,"cacheRead":200,"cacheWrite":0},"stopReason":"stop"}}""",
        ]);

        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root);

        ClaudeSessionTelemetry? last = null;
        svc.Updated += (_, t) => last = t;

        svc.Register(sessionId, @"C:\dev\myrepo");

        last.Should().NotBeNull();
        last!.SessionId.Should().Be(sessionId);
        last.TurnCount.Should().Be(2);
        // Latest assistant turn: 5 + 15 + 0 + 200 = 220.
        last.ContextTokens.Should().Be(220);
        last.Activity.Should().Be(ClaudeActivityState.Idle);
        svc.GetSnapshot(sessionId).Should().BeEquivalentTo(last);
    }

    [Fact]
    public void Register_Tolerates_Missing_Transcript()
    {
        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root);
        svc.Register(Guid.NewGuid().ToString("D"), @"C:\nope");
        // No file → no snapshot yet, but no exception.
        svc.GetSnapshot("ghost").Should().BeNull();
    }

    [Fact]
    public void ActivityState_Is_PendingToolUse_When_Last_Assistant_Stop_Is_Tool_Use()
    {
        var sid = Guid.NewGuid().ToString("D");
        WriteSession(sid, "--pend--",
        [
            """{"type":"message","id":"u","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"go"}}""",
            """{"type":"message","id":"a","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","model":"x","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0},"stopReason":"tool_use"}}""",
        ]);

        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\pend");

        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.PendingToolUse);
    }

    [Fact]
    public void ActivityState_Is_Composing_After_ToolResult_Even_With_Pending_Before()
    {
        var sid = Guid.NewGuid().ToString("D");
        WriteSession(sid, "--comp--",
        [
            """{"type":"message","id":"u","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"go"}}""",
            """{"type":"message","id":"a","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0},"stopReason":"tool_use"}}""",
            """{"type":"message","id":"tr","timestamp":"2026-04-22T08:02:00Z","message":{"role":"toolResult","toolCallId":"c","toolName":"bash","content":[]}}""",
        ]);

        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\comp");

        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.Composing);
    }

    [Fact]
    public async Task Polling_Picks_Up_Appended_Lines()
    {
        var sid = Guid.NewGuid().ToString("D");
        var file = WriteSession(sid, "--poll--",
        [
            """{"type":"message","id":"a","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0},"stopReason":"stop"}}""",
        ]);

        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root, enablePolling: true);
        svc.Register(sid, @"C:\poll");
        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.Idle);

        await File.AppendAllTextAsync(file,
            """{"type":"message","id":"u","timestamp":"2026-04-22T08:01:00Z","message":{"role":"user","content":"go"}}""" + "\n" +
            """{"type":"message","id":"a2","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0},"stopReason":"tool_use"}}""" + "\n");

        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
        while (DateTime.UtcNow < deadline &&
               svc.GetSnapshot(sid)?.Activity != ClaudeActivityState.PendingToolUse)
        {
            await Task.Delay(100);
        }

        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.PendingToolUse);
    }

    [Fact]
    public async Task File_Created_After_Register_Is_Picked_Up_By_Watcher()
    {
        // Fresh-launch race: Register fires before pi has written its first line.
        var sid = Guid.NewGuid().ToString("D");
        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root, enablePolling: true);
        svc.Register(sid, @"C:\later");
        svc.GetSnapshot(sid).Should().BeNull();

        var dir = Path.Combine(_root, "--C--later--");
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, $"2026-04-22T09-00-00-000Z_{sid}.jsonl");
        await File.WriteAllTextAsync(file,
            $$"""{"type":"session","version":3,"id":"{{sid}}","timestamp":"2026-04-22T09:00:00Z","cwd":"/c/later"}""" + "\n" +
            """{"type":"message","id":"a","timestamp":"2026-04-22T09:00:01Z","message":{"role":"assistant","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0},"stopReason":"stop"}}""" + "\n");

        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
        while (DateTime.UtcNow < deadline && svc.GetSnapshot(sid) is null)
        {
            await Task.Delay(100);
        }

        svc.GetSnapshot(sid).Should().NotBeNull();
        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.Idle);
    }

    [Fact]
    public void Unregister_Drops_Snapshot()
    {
        var sid = Guid.NewGuid().ToString("D");
        WriteSession(sid, "--drop--",
        [
            """{"type":"message","id":"a","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0},"stopReason":"stop"}}""",
        ]);

        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\drop");
        svc.GetSnapshot(sid).Should().NotBeNull();

        svc.Unregister(sid);
        svc.GetSnapshot(sid).Should().BeNull();
    }
}
