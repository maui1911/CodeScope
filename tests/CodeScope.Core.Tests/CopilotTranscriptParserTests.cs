namespace NoScope.CodeScope.Core.Tests;

using NoScope.CodeScope.Core.Services;

public sealed class CopilotTranscriptParserTests
{
    [Fact]
    public void ParseLine_SessionStart_Extracts_SessionId_Model_Cwd()
    {
        var line = """{"type":"session.start","data":{"sessionId":"11da9566-ac7e-43b8-a9d2-b159cc80ffe8","version":1,"producer":"copilot-agent","copilotVersion":"unknown","startTime":"2026-04-15T12:37:39.412Z","selectedModel":"claude-opus-4.6","reasoningEffort":"high","context":{"cwd":"d:\\Dev\\my-project","gitRoot":"D:\\Dev\\my-project","branch":"main"}},"id":"1cb8b667-c72f-47bf-ad4f-afc7dd9aa7bc","timestamp":"2026-04-15T12:37:39.432Z","parentId":null}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.EventType.Should().Be("session.start");
        entry.SessionId.Should().Be("11da9566-ac7e-43b8-a9d2-b159cc80ffe8");
        entry.Model.Should().Be("claude-opus-4.6");
        entry.Cwd.Should().Be(@"d:\Dev\my-project");
        entry.Timestamp.Should().NotBeNull();
    }

    [Fact]
    public void ParseLine_AssistantMessage_Extracts_OutputTokens_And_ToolRequests()
    {
        var line = """{"type":"assistant.message","data":{"messageId":"c479","content":"","toolRequests":[{"toolCallId":"t1","name":"powershell","arguments":{},"type":"function"}],"interactionId":"28c6","outputTokens":420,"requestId":"CF73"},"id":"412c","timestamp":"2026-04-15T12:37:52.785Z","parentId":"d391"}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.EventType.Should().Be("assistant.message");
        entry.OutputTokens.Should().Be(420);
        entry.HasToolRequests.Should().BeTrue();
        entry.HasUsage.Should().BeTrue();
    }

    [Fact]
    public void ParseLine_AssistantMessage_Without_ToolRequests()
    {
        var line = """{"type":"assistant.message","data":{"messageId":"abc","content":"Hello","toolRequests":[],"outputTokens":50},"id":"def","timestamp":"2026-04-15T13:00:00.000Z","parentId":"ghi"}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.HasToolRequests.Should().BeFalse();
        entry.OutputTokens.Should().Be(50);
    }

    [Fact]
    public void ParseLine_UserMessage_Returns_EventType()
    {
        var line = """{"type":"user.message","data":{"content":"hello"},"id":"xyz","timestamp":"2026-04-15T12:37:42.552Z","parentId":"abc"}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.EventType.Should().Be("user.message");
    }

    [Fact]
    public void ParseLine_AssistantTurnEnd_Returns_EventType()
    {
        var line = """{"type":"assistant.turn_end","data":{"turnId":"0"},"id":"abc","timestamp":"2026-04-15T12:38:35.863Z","parentId":"def"}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.EventType.Should().Be("assistant.turn_end");
    }

    [Fact]
    public void ParseLine_ToolExecutionStart_Returns_EventType()
    {
        var line = """{"type":"tool.execution_start","data":{"toolCallId":"t1","toolName":"powershell","arguments":{}},"id":"abc","timestamp":"2026-04-15T12:37:52.785Z","parentId":"def"}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.EventType.Should().Be("tool.execution_start");
    }

    [Fact]
    public void ParseLine_ToolExecutionComplete_Returns_EventType()
    {
        var line = """{"type":"tool.execution_complete","data":{"toolCallId":"t1","success":true},"id":"abc","timestamp":"2026-04-15T12:38:35.847Z","parentId":"def"}""";

        var entry = CopilotTranscriptParser.ParseLine(line);

        entry.Should().NotBeNull();
        entry!.EventType.Should().Be("tool.execution_complete");
    }

    [Fact]
    public void ParseLine_Null_Or_Blank_Returns_Null()
    {
        CopilotTranscriptParser.ParseLine("").Should().BeNull();
        CopilotTranscriptParser.ParseLine("   ").Should().BeNull();
        CopilotTranscriptParser.ParseLine(null!).Should().BeNull();
    }

    [Fact]
    public void ParseLine_Malformed_Json_Returns_Null()
    {
        CopilotTranscriptParser.ParseLine("{broken json").Should().BeNull();
    }

    [Fact]
    public void ParseShutdownUsage_Extracts_Full_Usage()
    {
        var line = """{"type":"session.shutdown","data":{"shutdownType":"routine","totalPremiumRequests":3,"totalApiDurationMs":12036,"modelMetrics":{"claude-opus-4.6":{"requests":{"count":2,"cost":3},"usage":{"inputTokens":84173,"outputTokens":308,"cacheReadTokens":27891,"cacheWriteTokens":0,"reasoningTokens":0}}},"currentModel":"claude-opus-4.6","currentTokens":27417,"systemTokens":8557,"conversationTokens":508,"toolDefinitionsTokens":18348},"id":"e1ae","timestamp":"2026-04-15T13:46:25.674Z","parentId":"6f4e"}""";

        var shutdown = CopilotTranscriptParser.ParseShutdownUsage(line);

        shutdown.Should().NotBeNull();
        shutdown!.InputTokens.Should().Be(84173);
        shutdown.OutputTokens.Should().Be(308);
        shutdown.CacheReadTokens.Should().Be(27891);
        shutdown.CurrentTokens.Should().Be(27417);
        shutdown.SystemTokens.Should().Be(8557);
        shutdown.ConversationTokens.Should().Be(508);
    }

    [Fact]
    public void ParseShutdownUsage_Non_Shutdown_Returns_Null()
    {
        var line = """{"type":"user.message","data":{"content":"hello"},"id":"xyz","timestamp":"2026-04-15T12:37:42.552Z","parentId":"abc"}""";

        CopilotTranscriptParser.ParseShutdownUsage(line).Should().BeNull();
    }

    [Fact]
    public void ReadCwdFromWorkspaceYaml_Extracts_Cwd()
    {
        var dir = Path.Combine(Path.GetTempPath(), $"copilot-test-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        var yaml = Path.Combine(dir, "workspace.yaml");
        File.WriteAllText(yaml, """
            id: 11da9566-ac7e-43b8-a9d2-b159cc80ffe8
            cwd: d:\Dev\my-project
            git_root: D:\Dev\my-project
            branch: main
            """);
        try
        {
            var cwd = CopilotTranscriptParser.ReadCwdFromWorkspaceYaml(yaml);
            cwd.Should().Be(@"d:\Dev\my-project");
        }
        finally { Directory.Delete(dir, true); }
    }

    [Fact]
    public void ReadCwdFromWorkspaceYaml_Missing_File_Returns_Null()
    {
        CopilotTranscriptParser.ReadCwdFromWorkspaceYaml("/nonexistent/workspace.yaml").Should().BeNull();
    }
}
