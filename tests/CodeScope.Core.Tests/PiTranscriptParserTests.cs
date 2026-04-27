using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class PiTranscriptParserTests
{
    [Fact]
    public void ParseLine_Session_Header_Extracts_Id_And_Cwd()
    {
        const string line = """
        {"type":"session","version":3,"id":"f1e2d3c4-aaaa-bbbb-cccc-1234567890ab","timestamp":"2026-04-22T08:00:00.000Z","cwd":"/c/dev/codescope"}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.Type.Should().Be("session");
        entry.SessionId.Should().Be("f1e2d3c4-aaaa-bbbb-cccc-1234567890ab");
        entry.Cwd.Should().Be("/c/dev/codescope");
        entry.HasUsage.Should().BeFalse();
    }

    [Fact]
    public void ParseLine_Assistant_Message_With_Usage_Extracts_Tokens()
    {
        const string line = """
        {"type":"message","id":"m1","parentId":"m0","timestamp":"2026-04-22T08:01:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"}],"provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":100,"output":50,"cacheRead":1000,"cacheWrite":200,"cost":{"total":0.001}},"stopReason":"stop"}}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.Type.Should().Be("message");
        entry.Role.Should().Be("assistant");
        entry.InputTokens.Should().Be(100);
        entry.OutputTokens.Should().Be(50);
        entry.CacheReadTokens.Should().Be(1000);
        entry.CacheCreationTokens.Should().Be(200);
        entry.HasUsage.Should().BeTrue();
        entry.StopReason.Should().Be("stop");
        entry.Model.Should().Be("claude-sonnet-4-5");
        entry.Provider.Should().Be("anthropic");
        entry.Timestamp.Should().NotBeNull();
    }

    [Fact]
    public void ParseLine_User_Message_Has_No_Usage()
    {
        const string line = """
        {"type":"message","id":"m2","timestamp":"2026-04-22T08:00:00.000Z","message":{"role":"user","content":"hello"}}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.Role.Should().Be("user");
        entry.HasUsage.Should().BeFalse();
        entry.InputTokens.Should().Be(0);
    }

    [Fact]
    public void ParseLine_ToolResult_Message_Recognised()
    {
        const string line = """
        {"type":"message","id":"m3","parentId":"m1","timestamp":"2026-04-22T08:02:00.000Z","message":{"role":"toolResult","toolCallId":"call_123","toolName":"bash","content":[{"type":"text","text":"ok"}],"isError":false}}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.Type.Should().Be("message");
        entry.Role.Should().Be("toolResult");
        entry.HasUsage.Should().BeFalse();
    }

    [Fact]
    public void ParseLine_Tool_Use_StopReason_Surfaces()
    {
        const string line = """
        {"type":"message","id":"m4","timestamp":"2026-04-22T08:03:00.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash"}],"provider":"anthropic","model":"x","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0},"stopReason":"tool_use"}}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.StopReason.Should().Be("tool_use");
        entry.HasUsage.Should().BeTrue();
    }

    [Fact]
    public void ParseLine_Compaction_Returns_NonNull_But_No_Usage()
    {
        const string line = """
        {"type":"compaction","id":"c1","timestamp":"2026-04-22T09:00:00.000Z","tokensBefore":80000}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.Type.Should().Be("compaction");
        entry.HasUsage.Should().BeFalse();
    }

    [Fact]
    public void ParseLine_ModelChange_Surfaces_Model()
    {
        const string line = """
        {"type":"model_change","id":"x1","timestamp":"2026-04-22T08:00:00.000Z","provider":"anthropic","modelId":"claude-opus-4-7"}
        """;

        var entry = PiTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.Type.Should().Be("model_change");
        entry.Model.Should().Be("claude-opus-4-7");
        entry.HasUsage.Should().BeFalse();
    }

    [Fact]
    public void ParseLine_Invalid_Json_Returns_Null()
    {
        PiTranscriptParser.ParseLine("{not json").Should().BeNull();
        PiTranscriptParser.ParseLine("").Should().BeNull();
        PiTranscriptParser.ParseLine("   ").Should().BeNull();
    }

    [Fact]
    public void ExtractSessionIdFromFileName_Pulls_Trailing_Uuid()
    {
        // Pi files: "<timestamp>_<uuid>.jsonl" — e.g. "2026-04-22T08-00-00_<uuid>.jsonl".
        var id = PiTranscriptParser.ExtractSessionIdFromFileName(
            "2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.jsonl");
        id.Should().Be("f1e2d3c4-aaaa-bbbb-cccc-1234567890ab");
    }

    [Fact]
    public void ExtractSessionIdFromFileName_Returns_Null_For_Garbage()
    {
        PiTranscriptParser.ExtractSessionIdFromFileName("not-a-pi-file.jsonl").Should().BeNull();
        PiTranscriptParser.ExtractSessionIdFromFileName("no-underscore.jsonl").Should().BeNull();
        PiTranscriptParser.ExtractSessionIdFromFileName("trailing_not-a-uuid.jsonl").Should().BeNull();
    }
}
