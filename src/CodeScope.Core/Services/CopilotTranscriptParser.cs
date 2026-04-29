using System.Text.Json;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// One parsed event from a Copilot CLI <c>~/.copilot/session-state/&lt;uuid&gt;/events.jsonl</c>
/// transcript. Copilot's event schema differs from Claude/Pi: events are typed via a top-level
/// <c>type</c> field (e.g. <c>session.start</c>, <c>assistant.message</c>, <c>assistant.turn_end</c>),
/// and token usage is split — <c>outputTokens</c> lives per <c>assistant.message</c>, while full
/// usage aggregates appear only in <c>session.shutdown</c>.
/// </summary>
public sealed record CopilotTranscriptEntry(
    string? SessionId,
    string? EventType,
    DateTimeOffset? Timestamp,
    int OutputTokens,
    string? Model = null,
    string? Cwd = null,
    bool HasToolRequests = false)
{
    /// <summary>True when this event carries assistant output token usage.</summary>
    public bool HasUsage => EventType == "assistant.message" && OutputTokens > 0;
}

/// <summary>
/// Parsed token usage aggregate from a <c>session.shutdown</c> event. Copilot only reports
/// full input/output/cache breakdowns in shutdown, not per-turn.
/// </summary>
public sealed record CopilotShutdownUsage(
    int InputTokens,
    int OutputTokens,
    int CacheReadTokens,
    int CacheWriteTokens,
    int ReasoningTokens,
    int CurrentTokens,
    int SystemTokens,
    int ConversationTokens);

/// <summary>
/// Pure static parser for Copilot CLI <c>events.jsonl</c> entries. No IO — callers feed lines;
/// <c>null</c> on blank or malformed lines.
/// </summary>
public static class CopilotTranscriptParser
{
    /// <summary>Parse a single Copilot <c>events.jsonl</c> line.</summary>
    public static CopilotTranscriptEntry? ParseLine(string line)
    {
        if (string.IsNullOrWhiteSpace(line)) { return null; }
        try
        {
            using var doc = JsonDocument.Parse(line);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) { return null; }

            var eventType = GetString(root, "type");
            var timestamp = ParseTimestamp(GetString(root, "timestamp"));

            if (!root.TryGetProperty("data", out var data) || data.ValueKind != JsonValueKind.Object)
            {
                return new CopilotTranscriptEntry(null, eventType, timestamp, 0);
            }

            // session.start carries the session id, cwd, and selected model.
            if (eventType == "session.start")
            {
                string? cwd = null;
                if (data.TryGetProperty("context", out var ctx) && ctx.ValueKind == JsonValueKind.Object)
                {
                    cwd = GetString(ctx, "cwd");
                }

                return new CopilotTranscriptEntry(
                    SessionId: GetString(data, "sessionId"),
                    EventType: eventType,
                    Timestamp: timestamp,
                    OutputTokens: 0,
                    Model: GetString(data, "selectedModel"),
                    Cwd: cwd);
            }

            // assistant.message carries per-turn outputTokens and optional toolRequests.
            if (eventType == "assistant.message")
            {
                var outputTokens = GetInt(data, "outputTokens");
                var hasToolRequests = data.TryGetProperty("toolRequests", out var tr)
                    && tr.ValueKind == JsonValueKind.Array
                    && tr.GetArrayLength() > 0;

                return new CopilotTranscriptEntry(
                    SessionId: null,
                    EventType: eventType,
                    Timestamp: timestamp,
                    OutputTokens: outputTokens,
                    Model: null,
                    HasToolRequests: hasToolRequests);
            }

            return new CopilotTranscriptEntry(null, eventType, timestamp, 0);
        }
        catch (JsonException ex)
        {
            System.Diagnostics.Debug.WriteLine($"[CopilotTranscriptParser] Skipping malformed JSONL line: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Parse the <c>session.shutdown</c> event's full usage aggregate. Returns <c>null</c> when
    /// <paramref name="line"/> is not a shutdown event or when parsing fails.
    /// </summary>
    public static CopilotShutdownUsage? ParseShutdownUsage(string line)
    {
        if (string.IsNullOrWhiteSpace(line)) { return null; }
        try
        {
            using var doc = JsonDocument.Parse(line);
            var root = doc.RootElement;
            if (GetString(root, "type") != "session.shutdown") { return null; }
            if (!root.TryGetProperty("data", out var data) || data.ValueKind != JsonValueKind.Object)
            {
                return null;
            }

            int inputTokens = 0, outputTokens = 0, cacheRead = 0, cacheWrite = 0, reasoning = 0;

            // Usage is nested under modelMetrics.<modelName>.usage.
            if (data.TryGetProperty("modelMetrics", out var mm) && mm.ValueKind == JsonValueKind.Object)
            {
                foreach (var modelEntry in mm.EnumerateObject())
                {
                    if (modelEntry.Value.ValueKind != JsonValueKind.Object) { continue; }
                    if (!modelEntry.Value.TryGetProperty("usage", out var usage)
                        || usage.ValueKind != JsonValueKind.Object)
                    {
                        continue;
                    }

                    inputTokens += GetInt(usage, "inputTokens");
                    outputTokens += GetInt(usage, "outputTokens");
                    cacheRead += GetInt(usage, "cacheReadTokens");
                    cacheWrite += GetInt(usage, "cacheWriteTokens");
                    reasoning += GetInt(usage, "reasoningTokens");
                }
            }

            return new CopilotShutdownUsage(
                inputTokens, outputTokens, cacheRead, cacheWrite, reasoning,
                GetInt(data, "currentTokens"),
                GetInt(data, "systemTokens"),
                GetInt(data, "conversationTokens"));
        }
        catch (JsonException) { return null; }
    }

    /// <summary>
    /// Read the <c>workspace.yaml</c> file in a session directory to extract the <c>cwd</c>.
    /// This is a lightweight YAML peek — we only read the <c>cwd:</c> line, no full YAML parser
    /// dependency.
    /// </summary>
    public static string? ReadCwdFromWorkspaceYaml(string yamlPath)
    {
        if (!File.Exists(yamlPath)) { return null; }
        try
        {
            foreach (var rawLine in File.ReadLines(yamlPath))
            {
                var trimmed = rawLine.AsSpan().TrimStart();
                if (trimmed.StartsWith("cwd:", StringComparison.OrdinalIgnoreCase))
                {
                    var value = trimmed[4..].Trim();
                    // Strip optional surrounding quotes.
                    if (value.Length >= 2
                        && ((value[0] == '"' && value[^1] == '"') || (value[0] == '\'' && value[^1] == '\'')))
                    {
                        value = value[1..^1];
                    }
                    return value.ToString();
                }
            }
            return null;
        }
        catch { return null; }
    }

    private static string? GetString(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static int GetInt(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt32(out var n) ? n : 0;

    private static DateTimeOffset? ParseTimestamp(string? raw) =>
        DateTimeOffset.TryParse(raw, out var dt) ? dt : null;
}
