using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class CopilotTelemetryServiceTests : IDisposable
{
    private readonly string _root;
    private readonly CopilotTelemetryService _sut;

    public CopilotTelemetryServiceTests()
    {
        _root = Path.Combine(Path.GetTempPath(), $"copilot-tel-{Guid.NewGuid():N}");
        Directory.CreateDirectory(_root);
        _sut = new CopilotTelemetryService(NullLogger<CopilotTelemetryService>.Instance, _root);
    }

    public void Dispose()
    {
        _sut.Dispose();
        try { Directory.Delete(_root, true); } catch { }
    }

    private static string SessionStartLine(string sid, string model = "claude-opus-4.6", string cwd = @"d:\\test") =>
        $$$"""{"type":"session.start","data":{"sessionId":"{{{sid}}}","selectedModel":"{{{model}}}","context":{"cwd":"{{{cwd}}}"}},"id":"a","timestamp":"2026-04-15T12:00:00.000Z","parentId":null}""";

    [Fact]
    public void Register_Reads_Existing_Events()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, "events.jsonl"),
        [
            SessionStartLine(sid, cwd: @"d:\\Dev\\test"),
            """{"type":"user.message","data":{"content":"hello"},"id":"b","timestamp":"2026-04-15T12:00:01.000Z","parentId":"a"}""",
            """{"type":"assistant.turn_start","data":{"turnId":"0"},"id":"c","timestamp":"2026-04-15T12:00:02.000Z","parentId":"b"}""",
            """{"type":"assistant.message","data":{"messageId":"m1","content":"hi","toolRequests":[],"outputTokens":150},"id":"d","timestamp":"2026-04-15T12:00:03.000Z","parentId":"c"}""",
            """{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"e","timestamp":"2026-04-15T12:00:04.000Z","parentId":"d"}""",
        ]);

        _sut.Register(sid, @"d:\Dev\test");

        var snap = _sut.GetSnapshot(sid);
        snap.Should().NotBeNull();
        snap!.SessionId.Should().Be(sid);
        snap.TurnCount.Should().Be(1);
        snap.ContextTokens.Should().Be(150);
        snap.Activity.Should().Be(ClaudeActivityState.Idle);
        snap.ModelId.Should().Be("claude-opus-4.6");
    }

    [Fact]
    public void Activity_Transitions_Through_States()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, "events.jsonl"),
        [
            SessionStartLine(sid, "gpt-4o"),
            """{"type":"user.message","data":{"content":"do something"},"id":"b","timestamp":"2026-04-15T12:00:01.000Z","parentId":"a"}""",
        ]);

        _sut.Register(sid, @"d:\test");
        _sut.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.Composing);

        // Append assistant message with tool requests → PendingToolUse.
        File.AppendAllLines(Path.Combine(dir, "events.jsonl"),
        [
            """{"type":"assistant.message","data":{"messageId":"m1","content":"","toolRequests":[{"toolCallId":"t1","name":"bash","arguments":{},"type":"function"}],"outputTokens":100},"id":"c","timestamp":"2026-04-15T12:00:02.000Z","parentId":"b"}""",
        ]);
        // Re-register to force re-read (no FSWatcher in test mode).
        _sut.Unregister(sid);
        _sut.Register(sid, @"d:\test");
        _sut.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.PendingToolUse);
    }

    [Fact]
    public void ToolComplete_Transitions_To_Composing()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, "events.jsonl"),
        [
            SessionStartLine(sid, "gpt-4o"),
            """{"type":"user.message","data":{"content":"go"},"id":"b","timestamp":"2026-04-15T12:00:01.000Z","parentId":"a"}""",
            """{"type":"assistant.message","data":{"messageId":"m1","content":"","toolRequests":[{"toolCallId":"t1","name":"bash","arguments":{},"type":"function"}],"outputTokens":50},"id":"c","timestamp":"2026-04-15T12:00:02.000Z","parentId":"b"}""",
            """{"type":"tool.execution_start","data":{"toolCallId":"t1","toolName":"bash"},"id":"d","timestamp":"2026-04-15T12:00:03.000Z","parentId":"c"}""",
            """{"type":"tool.execution_complete","data":{"toolCallId":"t1","success":true},"id":"e","timestamp":"2026-04-15T12:00:04.000Z","parentId":"d"}""",
        ]);

        _sut.Register(sid, @"d:\test");
        _sut.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.Composing);
    }

    [Fact]
    public void Shutdown_Event_Updates_ContextTokens()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, "events.jsonl"),
        [
            SessionStartLine(sid),
            """{"type":"user.message","data":{"content":"go"},"id":"b","timestamp":"2026-04-15T12:00:01.000Z","parentId":"a"}""",
            """{"type":"assistant.message","data":{"messageId":"m1","content":"done","toolRequests":[],"outputTokens":100},"id":"c","timestamp":"2026-04-15T12:00:02.000Z","parentId":"b"}""",
            """{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"d","timestamp":"2026-04-15T12:00:03.000Z","parentId":"c"}""",
            """{"type":"session.shutdown","data":{"shutdownType":"routine","modelMetrics":{"claude-opus-4.6":{"requests":{"count":1,"cost":1},"usage":{"inputTokens":50000,"outputTokens":100,"cacheReadTokens":30000,"cacheWriteTokens":0,"reasoningTokens":0}}},"currentTokens":27417,"systemTokens":8557,"conversationTokens":508},"id":"e","timestamp":"2026-04-15T13:00:00.000Z","parentId":"d"}""",
        ]);

        _sut.Register(sid, @"d:\test");
        var snap = _sut.GetSnapshot(sid);
        snap.Should().NotBeNull();
        // Shutdown's currentTokens replaces accumulated outputTokens.
        snap!.ContextTokens.Should().Be(27417);
        snap.Activity.Should().Be(ClaudeActivityState.Idle);
    }

    [Fact]
    public void Unregister_Clears_Snapshot()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, "events.jsonl"),
        [
            SessionStartLine(sid, "gpt-4o"),
        ]);

        _sut.Register(sid, @"d:\test");
        _sut.GetSnapshot(sid).Should().NotBeNull();

        _sut.Unregister(sid);
        _sut.GetSnapshot(sid).Should().BeNull();
    }

    [Fact]
    public void Updated_Event_Fires_On_Register()
    {
        var sid = Guid.NewGuid().ToString();
        var dir = Path.Combine(_root, sid);
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, "events.jsonl"),
        [
            SessionStartLine(sid, "gpt-4o"),
            """{"type":"user.message","data":{"content":"hi"},"id":"b","timestamp":"2026-04-15T12:00:01.000Z","parentId":"a"}""",
        ]);

        ClaudeSessionTelemetry? received = null;
        _sut.Updated += (_, t) => received = t;

        _sut.Register(sid, @"d:\test");

        received.Should().NotBeNull();
        received!.SessionId.Should().Be(sid);
    }
}
