using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Core.Tests;

public sealed class OpenCodeMessageParserTests
{
    [Fact]
    public void ParseContent_Assistant_Message_Extracts_Tokens_And_Path()
    {
        const string json = """
        {
          "id": "msg_1",
          "role": "assistant",
          "parts": [{"type":"text","text":"Hello"}],
          "metadata": {
            "time": {"created": 1733234567890, "completed": 1733234570000},
            "sessionID": "ses-abc",
            "tool": {},
            "assistant": {
              "system": [],
              "modelID": "claude-sonnet-4-5",
              "providerID": "anthropic",
              "path": {"cwd": "/c/dev/myrepo", "root": "/c/dev/myrepo"},
              "cost": 0.001,
              "tokens": {"input": 100, "output": 50, "reasoning": 0, "cache": {"read": 200, "write": 0}}
            }
          }
        }
        """;

        var entry = OpenCodeMessageParser.ParseContent(json);

        entry.Should().NotBeNull();
        entry!.Id.Should().Be("msg_1");
        entry.Role.Should().Be("assistant");
        entry.SessionId.Should().Be("ses-abc");
        entry.InputTokens.Should().Be(100);
        entry.OutputTokens.Should().Be(50);
        entry.CacheReadTokens.Should().Be(200);
        entry.CacheWriteTokens.Should().Be(0);
        entry.ReasoningTokens.Should().Be(0);
        entry.ModelId.Should().Be("claude-sonnet-4-5");
        entry.ProviderId.Should().Be("anthropic");
        entry.Cwd.Should().Be("/c/dev/myrepo");
        entry.CompletedAt.Should().NotBeNull();
        entry.CreatedAt.Should().NotBeNull();
        entry.HasUsage.Should().BeTrue();
        entry.HasPendingToolCall.Should().BeFalse();
        entry.ContextTokens.Should().Be(100 + 50 + 0 + 200 + 0);
    }

    [Fact]
    public void ParseContent_User_Message_Has_No_Usage()
    {
        const string json = """
        {
          "id": "msg_u1",
          "role": "user",
          "parts": [{"type":"text","text":"hi"}],
          "metadata": {
            "time": {"created": 1733234567000},
            "sessionID": "ses-abc",
            "tool": {}
          }
        }
        """;

        var entry = OpenCodeMessageParser.ParseContent(json);

        entry.Should().NotBeNull();
        entry!.Role.Should().Be("user");
        entry.HasUsage.Should().BeFalse();
        entry.CompletedAt.Should().BeNull();
        entry.CreatedAt.Should().NotBeNull();
    }

    [Fact]
    public void ParseContent_Assistant_Without_Completed_Marks_In_Flight()
    {
        const string json = """
        {
          "id": "msg_2",
          "role": "assistant",
          "parts": [],
          "metadata": {
            "time": {"created": 1733234567000},
            "sessionID": "s",
            "tool": {},
            "assistant": {
              "system": [], "modelID":"x","providerID":"a",
              "path": {"cwd":"/c/x","root":"/c/x"}, "cost": 0,
              "tokens": {"input": 1, "output": 0, "reasoning": 0, "cache":{"read":0,"write":0}}
            }
          }
        }
        """;

        var entry = OpenCodeMessageParser.ParseContent(json);
        entry!.CompletedAt.Should().BeNull();
        entry.CreatedAt.Should().NotBeNull();
    }

    [Fact]
    public void ParseContent_Detects_Pending_Tool_Call()
    {
        const string json = """
        {
          "id":"msg_3","role":"assistant",
          "parts":[
            {"type":"text","text":"running it"},
            {"type":"tool-invocation","toolInvocation":{"state":"call","toolName":"bash","args":{}}}
          ],
          "metadata":{
            "time":{"created":1},
            "sessionID":"s","tool":{},
            "assistant":{"system":[],"modelID":"x","providerID":"a","path":{"cwd":"/c","root":"/c"},"cost":0,
                         "tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}
          }
        }
        """;

        var entry = OpenCodeMessageParser.ParseContent(json);
        entry!.HasPendingToolCall.Should().BeTrue();
    }

    [Fact]
    public void ParseContent_Resolved_Tool_Call_Is_Not_Pending()
    {
        const string json = """
        {
          "id":"msg_4","role":"assistant",
          "parts":[
            {"type":"tool-invocation","toolInvocation":{"state":"result","toolName":"bash","args":{},"result":"ok"}}
          ],
          "metadata":{
            "time":{"created":1,"completed":2},
            "sessionID":"s","tool":{},
            "assistant":{"system":[],"modelID":"x","providerID":"a","path":{"cwd":"/c","root":"/c"},"cost":0,
                         "tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}
          }
        }
        """;

        var entry = OpenCodeMessageParser.ParseContent(json);
        entry!.HasPendingToolCall.Should().BeFalse();
    }

    [Fact]
    public void ParseContent_Garbage_Returns_Null()
    {
        OpenCodeMessageParser.ParseContent("{not json").Should().BeNull();
        OpenCodeMessageParser.ParseContent("").Should().BeNull();
        OpenCodeMessageParser.ParseContent("   ").Should().BeNull();
    }

    [Fact]
    public void ExtractMessageIdFromFileName_Pulls_Suffix()
    {
        OpenCodeMessageParser.ExtractMessageIdFromFileName("msg_abc123.json").Should().Be("abc123");
        OpenCodeMessageParser.ExtractMessageIdFromFileName("msg_01HXY9Z.json").Should().Be("01HXY9Z");
    }

    [Fact]
    public void ExtractMessageIdFromFileName_Rejects_Non_Conforming()
    {
        OpenCodeMessageParser.ExtractMessageIdFromFileName("session.json").Should().BeNull();
        OpenCodeMessageParser.ExtractMessageIdFromFileName("msg_.json").Should().BeNull();
        OpenCodeMessageParser.ExtractMessageIdFromFileName("").Should().BeNull();
    }
}
