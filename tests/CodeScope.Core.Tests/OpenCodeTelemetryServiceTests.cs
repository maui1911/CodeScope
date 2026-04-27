using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class OpenCodeTelemetryServiceTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "cs-oc-telemetry-" + Guid.NewGuid().ToString("n"));

    public OpenCodeTelemetryServiceTests() => Directory.CreateDirectory(_root);

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { /* best effort */ }
    }

    private string MessageDirFor(string slug, string sessionId)
    {
        var dir = Path.Combine(_root, "project", slug, "storage", "message", sessionId);
        Directory.CreateDirectory(dir);
        return dir;
    }

    private static string AssistantJson(string id, string sessionId, long created, long? completed,
        int input, int output, int cacheRead, int cacheWrite, string model, bool pendingTool = false)
    {
        var toolPart = pendingTool
            ? "{\"type\":\"tool-invocation\",\"toolInvocation\":{\"state\":\"call\",\"toolName\":\"bash\",\"args\":{}}}"
            : "{\"type\":\"text\",\"text\":\"ok\"}";
        var completedField = completed is long c ? ",\"completed\":" + c : "";
        return "{\"id\":\"" + id + "\",\"role\":\"assistant\",\"parts\":[" + toolPart + "],"
            + "\"metadata\":{\"time\":{\"created\":" + created + completedField + "},"
            + "\"sessionID\":\"" + sessionId + "\",\"tool\":{},"
            + "\"assistant\":{\"system\":[],\"modelID\":\"" + model + "\",\"providerID\":\"anthropic\","
            + "\"path\":{\"cwd\":\"/c/x\",\"root\":\"/c/x\"},\"cost\":0,"
            + "\"tokens\":{\"input\":" + input + ",\"output\":" + output + ",\"reasoning\":0,"
            + "\"cache\":{\"read\":" + cacheRead + ",\"write\":" + cacheWrite + "}}}}}";
    }

    private static string UserJson(string id, string sessionId, long created)
        => "{\"id\":\"" + id + "\",\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"hi\"}],"
            + "\"metadata\":{\"time\":{\"created\":" + created + "},\"sessionID\":\"" + sessionId + "\",\"tool\":{}}}";

    [Fact]
    public void Register_Reads_Existing_Messages_And_Computes_Snapshot()
    {
        var sid = "ses-" + Guid.NewGuid().ToString("N")[..8];
        var dir = MessageDirFor("my-repo", sid);
        File.WriteAllText(Path.Combine(dir, "msg_1.json"), UserJson("msg_1", sid, 1_000_000));
        File.WriteAllText(Path.Combine(dir, "msg_2.json"),
            AssistantJson("msg_2", sid, 1_001_000, 1_002_000, input: 100, output: 50, cacheRead: 200, cacheWrite: 0, model: "claude-sonnet-4-5"));

        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root);

        ClaudeSessionTelemetry? last = null;
        svc.Updated += (_, t) => last = t;
        svc.Register(sid, @"C:\x");

        last.Should().NotBeNull();
        last!.SessionId.Should().Be(sid);
        last.TurnCount.Should().Be(1);
        last.ContextTokens.Should().Be(100 + 50 + 200);
        last.Activity.Should().Be(ClaudeActivityState.Idle);
        last.LastTurnDuration.Should().NotBeNull();
        last.ModelId.Should().Be("claude-sonnet-4-5");
    }

    [Fact]
    public void Register_With_Missing_Dir_Yields_Null_Snapshot()
    {
        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root);
        svc.Register("ghost", @"C:\nope");
        svc.GetSnapshot("ghost").Should().BeNull();
    }

    [Fact]
    public void Activity_Is_PendingToolUse_When_Latest_Assistant_Has_Open_Tool_Call()
    {
        var sid = "ses-pend";
        var dir = MessageDirFor("repo", sid);
        File.WriteAllText(Path.Combine(dir, "msg_u.json"), UserJson("msg_u", sid, 1));
        File.WriteAllText(Path.Combine(dir, "msg_a.json"),
            AssistantJson("msg_a", sid, 2, completed: null, input: 1, output: 1, cacheRead: 0, cacheWrite: 0, model: "x", pendingTool: true));

        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\repo");

        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.PendingToolUse);
    }

    [Fact]
    public void Activity_Is_Composing_When_Latest_Is_User()
    {
        var sid = "ses-comp";
        var dir = MessageDirFor("repo", sid);
        File.WriteAllText(Path.Combine(dir, "msg_a.json"),
            AssistantJson("msg_a", sid, 1, completed: 2, input: 1, output: 1, cacheRead: 0, cacheWrite: 0, model: "x"));
        File.WriteAllText(Path.Combine(dir, "msg_u.json"), UserJson("msg_u", sid, 3));

        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\repo");

        svc.GetSnapshot(sid)!.Activity.Should().Be(ClaudeActivityState.Composing);
    }

    [Fact]
    public async Task File_Created_After_Register_Is_Picked_Up()
    {
        var sid = "ses-late";
        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root, enablePolling: true);
        svc.Register(sid, @"C:\later");
        svc.GetSnapshot(sid).Should().BeNull();

        var dir = MessageDirFor("later-repo", sid);
        await File.WriteAllTextAsync(Path.Combine(dir, "msg_1.json"),
            AssistantJson("msg_1", sid, 100, completed: 200, input: 5, output: 5, cacheRead: 0, cacheWrite: 0, model: "x"));

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
        var sid = "ses-drop";
        var dir = MessageDirFor("repo", sid);
        File.WriteAllText(Path.Combine(dir, "msg_1.json"),
            AssistantJson("msg_1", sid, 1, completed: 2, input: 1, output: 1, cacheRead: 0, cacheWrite: 0, model: "x"));

        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\drop");
        svc.GetSnapshot(sid).Should().NotBeNull();

        svc.Unregister(sid);
        svc.GetSnapshot(sid).Should().BeNull();
    }
}
