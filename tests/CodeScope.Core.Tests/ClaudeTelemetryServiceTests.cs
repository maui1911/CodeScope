using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Microsoft.Extensions.Logging.Abstractions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class ClaudeTelemetryServiceTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "cs-telemetry-" + Guid.NewGuid().ToString("n"));

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { /* best effort */ }
    }

    [Fact]
    public void Register_Reads_Existing_Transcript_And_Raises_Updated()
    {
        const string sessionId = "abc-123";
        var cwd = @"C:\dev\myrepo";
        var dir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(cwd));
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, sessionId + ".jsonl");
        File.WriteAllLines(file,
        [
            """{"type":"user","sessionId":"abc-123","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"hi"}}""",
            """{"type":"assistant","sessionId":"abc-123","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":50}}}""",
            """{"type":"assistant","sessionId":"abc-123","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":200}}}""",
        ]);

        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);

        ClaudeSessionTelemetry? last = null;
        svc.Updated += (_, t) => last = t;

        svc.Register(sessionId, cwd);

        last.Should().NotBeNull();
        last!.SessionId.Should().Be(sessionId);
        last.TurnCount.Should().Be(2);
        // Billable: (10+20+100) + (5+15+0) = 150
        last.TotalTokens.Should().Be(150);
        last.LastTurnAt.Should().NotBeNull();
        // Pair: user@08:00:00 → assistant@08:01:00 → 1 min; then later user absent between
        // 08:01 and 08:02 so the duration from 08:00 (still the last-seen user turn) holds.
        last.LastTurnDuration.Should().NotBeNull();
        // Transcript ends on end_turn → Idle.
        last.Activity.Should().Be(ClaudeActivityState.Idle);

        svc.GetSnapshot(sessionId).Should().BeEquivalentTo(last);
    }

    [Fact]
    public void Register_Tolerates_Missing_Transcript()
    {
        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);
        svc.Register("no-such-id", @"C:\dev\nothing");
        svc.GetSnapshot("no-such-id").Should().BeNull();
    }

    [Fact]
    public void ActivityState_Is_PendingToolUse_When_Transcript_Ends_On_Tool_Use()
    {
        const string sessionId = "pend";
        var cwd = @"C:\pend\repo";
        var dir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(cwd));
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, sessionId + ".jsonl"),
        [
            """{"type":"user","sessionId":"pend","timestamp":"2026-04-22T08:00:00Z","message":{"role":"user","content":"run it"}}""",
            """{"type":"assistant","sessionId":"pend","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}""",
        ]);

        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);
        svc.Register(sessionId, cwd);

        svc.GetSnapshot(sessionId)!.Activity.Should().Be(ClaudeActivityState.PendingToolUse);
    }

    [Fact]
    public void ActivityState_Is_Composing_When_Transcript_Ends_On_User_Message()
    {
        const string sessionId = "comp";
        var cwd = @"C:\comp\repo";
        var dir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(cwd));
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, sessionId + ".jsonl"),
        [
            """{"type":"assistant","sessionId":"comp","timestamp":"2026-04-22T08:01:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}""",
            """{"type":"user","sessionId":"comp","timestamp":"2026-04-22T08:02:00Z","message":{"role":"user","content":"follow-up"}}""",
        ]);

        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);
        svc.Register(sessionId, cwd);

        svc.GetSnapshot(sessionId)!.Activity.Should().Be(ClaudeActivityState.Composing);
    }

    [Fact]
    public async Task Polling_Picks_Up_Appended_Lines_Without_FsWatcher_Event()
    {
        const string sessionId = "poll";
        var cwd = @"C:\poll\repo";
        var dir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(cwd));
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, sessionId + ".jsonl");
        File.WriteAllLines(file,
        [
            """{"type":"assistant","sessionId":"poll","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}""",
        ]);

        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root, enablePolling: true);
        svc.Register(sessionId, cwd);
        svc.GetSnapshot(sessionId)!.Activity.Should().Be(ClaudeActivityState.Idle);

        // Append a tool_use turn. Do NOT rely on FSWatcher — we just want the poll to catch it.
        await File.AppendAllTextAsync(file,
            """{"type":"user","sessionId":"poll","timestamp":"2026-04-22T08:01:00Z","message":{"role":"user","content":"go"}}""" + "\n" +
            """{"type":"assistant","sessionId":"poll","timestamp":"2026-04-22T08:02:00Z","message":{"role":"assistant","stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}""" + "\n");

        // Poll fires every 250 ms; give it a few cycles of headroom to absorb CI jitter.
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
        while (DateTime.UtcNow < deadline &&
               svc.GetSnapshot(sessionId)?.Activity != ClaudeActivityState.PendingToolUse)
        {
            await Task.Delay(100);
        }

        svc.GetSnapshot(sessionId)!.Activity.Should().Be(ClaudeActivityState.PendingToolUse);
    }

    [Fact]
    public void Telemetry_Surfaces_Context_Window_From_Assistant_Model()
    {
        const string sessionId = "model";
        var cwd = @"C:\model\repo";
        var dir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(cwd));
        Directory.CreateDirectory(dir);
        File.WriteAllLines(Path.Combine(dir, sessionId + ".jsonl"),
        [
            """{"type":"assistant","sessionId":"model","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","model":"claude-opus-4-7[1m]","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}""",
        ]);

        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);
        svc.Register(sessionId, cwd);

        var snap = svc.GetSnapshot(sessionId)!;
        snap.ModelId.Should().Be("claude-opus-4-7[1m]");
        snap.ContextWindowTokens.Should().Be(1_000_000);
    }

    [Fact]
    public void Unregister_Drops_Snapshot()
    {
        const string sessionId = "sid";
        var cwd = @"C:\tmp\repo";
        var dir = Path.Combine(_root, ClaudeTranscriptParser.EncodeCwd(cwd));
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, sessionId + ".jsonl"),
            """{"type":"assistant","sessionId":"sid","timestamp":"2026-04-22T08:00:00Z","message":{"role":"assistant","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}""" + "\n");

        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);
        svc.Register(sessionId, cwd);
        svc.GetSnapshot(sessionId).Should().NotBeNull();

        svc.Unregister(sessionId);
        svc.GetSnapshot(sessionId).Should().BeNull();
    }
}
