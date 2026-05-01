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
    public async Task Recompute_Aggregates_Are_Incremental_Across_Many_Messages()
    {
        // Issue #31: retention is bounded by three CreatedAt-keyed candidates + a turn count,
        // not the previous unbounded Entries list. Writing 100 messages and then one more must
        // produce TurnCount == 51 — if the watermark/SeenAtWatermark gating is wrong, the new
        // file would re-trigger a parse of every prior file (turn count would balloon).
        var sid = "ses-many";
        var dir = MessageDirFor("repo", sid);

        for (var i = 0; i < 50; i++)
        {
            File.WriteAllText(Path.Combine(dir, $"msg_u_{i:D3}.json"), UserJson($"u_{i}", sid, 1000 + i * 2));
            File.WriteAllText(Path.Combine(dir, $"msg_a_{i:D3}.json"),
                AssistantJson($"a_{i}", sid, 1000 + i * 2 + 1, completed: 1000 + i * 2 + 1,
                    input: 1, output: 1, cacheRead: 0, cacheWrite: 0, model: "x"));
        }

        using var svc = new OpenCodeTelemetryService(
            NullLogger<OpenCodeTelemetryService>.Instance, _root, enablePolling: true);
        svc.Register(sid, @"C:\repo");

        svc.GetSnapshot(sid)!.TurnCount.Should().Be(50);

        // Add one more turn — count must increment to 51, never re-counting prior files.
        var newCreated = 1000 + 50 * 2 + 100;
        await File.WriteAllTextAsync(Path.Combine(dir, "msg_a_extra.json"),
            AssistantJson("a_extra", sid, newCreated, completed: newCreated + 1,
                input: 1, output: 1, cacheRead: 0, cacheWrite: 0, model: "x"));

        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
        while (DateTime.UtcNow < deadline && (svc.GetSnapshot(sid)?.TurnCount ?? 0) < 51)
        {
            await Task.Delay(100);
        }

        svc.GetSnapshot(sid)!.TurnCount.Should().Be(51);
    }

    [Fact]
    public void OutOfOrder_Mtimes_Are_All_Parsed_In_Single_Walk()
    {
        // Issue #31 follow-up: Directory.EnumerateFiles is not mtime-ordered. If the watermark
        // were advanced mid-loop, an older sibling returned later in the iteration would be
        // filtered against the freshly-bumped watermark and skipped permanently — its
        // CreatedAt entry never reaching LastUser/LastEntry/TurnCount. This test forces that
        // exact ordering by writing the assistant file FIRST (so it sorts first by name) and
        // then back-dating the user file's LastWriteTimeUtc to a strictly earlier instant.
        var sid = "ses-ooo";
        var dir = MessageDirFor("repo", sid);

        // Assistant file enumerated first (alphabetical: msg_a < msg_u). Newest mtime wins.
        var assistantPath = Path.Combine(dir, "msg_a.json");
        File.WriteAllText(assistantPath,
            AssistantJson("msg_a", sid, 200, completed: 300, input: 7, output: 9, cacheRead: 0, cacheWrite: 0, model: "x"));
        var userPath = Path.Combine(dir, "msg_u.json");
        File.WriteAllText(userPath, UserJson("msg_u", sid, 100));

        // Force out-of-order mtimes: assistant strictly NEWER than user, but enumerated first.
        var now = DateTime.UtcNow;
        File.SetLastWriteTimeUtc(userPath, now - TimeSpan.FromSeconds(10));
        File.SetLastWriteTimeUtc(assistantPath, now);

        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root);
        svc.Register(sid, @"C:\repo");

        var snap = svc.GetSnapshot(sid);
        snap.Should().NotBeNull();
        // Both files must contribute: TurnCount=1 (assistant has usage), and the user→assistant
        // pair must produce a non-null LastTurnDuration. If the user file is skipped, LastUser
        // stays null and LastTurnDuration stays null even though the assistant was parsed.
        snap!.TurnCount.Should().Be(1);
        snap.LastTurnDuration.Should().NotBeNull(
            "the user file (older mtime, enumerated second) must still be parsed so the user→assistant pair yields a duration");
        snap.Activity.Should().Be(ClaudeActivityState.Idle);
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
