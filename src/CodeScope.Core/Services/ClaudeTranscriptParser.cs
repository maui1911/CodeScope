using System.Text.Json;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Per-line entry parsed from a Claude Code <c>~/.claude/projects/&lt;cwd&gt;/&lt;session_id&gt;.jsonl</c>
/// transcript. Only the fields CodeScope's status bar consumes are surfaced — token usage
/// on assistant turns and the message timestamp for turn-runtime calculations.
/// </summary>
public sealed record TranscriptEntry(
    string? SessionId,
    string? Type,
    DateTimeOffset? Timestamp,
    int InputTokens,
    int OutputTokens,
    int CacheCreationTokens,
    int CacheReadTokens,
    string? StopReason = null,
    bool UserCarriesToolResult = false,
    string? Model = null)
{
    /// <summary>True when this entry reports an assistant turn's token usage.</summary>
    public bool HasUsage => Type == "assistant"
        && (InputTokens > 0 || OutputTokens > 0 || CacheCreationTokens > 0 || CacheReadTokens > 0);

    /// <summary>
    /// "Effective" token count for the status bar — input + output + cache-creation. Cache-read is
    /// excluded because it reflects cached input already counted in prior turns.
    /// </summary>
    public int BillableTokens => InputTokens + OutputTokens + CacheCreationTokens;
}

/// <summary>
/// Pure static parser for Claude Code transcript JSONL. No IO — callers feed lines;
/// a <c>null</c> result means the line is not a recognisable entry (file-history snapshots,
/// corrupted trailing bytes mid-flush, etc.).
/// </summary>
public static class ClaudeTranscriptParser
{
    public static TranscriptEntry? ParseLine(string line)
    {
        if (string.IsNullOrWhiteSpace(line)) { return null; }
        try
        {
            using var doc = JsonDocument.Parse(line);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) { return null; }

            var type = GetString(root, "type");
            var sessionId = GetString(root, "sessionId");
            var timestamp = ParseTimestamp(GetString(root, "timestamp"));

            int input = 0, output = 0, cacheCreate = 0, cacheRead = 0;
            string? stopReason = null;
            string? model = null;
            var userCarriesToolResult = false;

            if (root.TryGetProperty("message", out var msg) && msg.ValueKind == JsonValueKind.Object)
            {
                if (msg.TryGetProperty("usage", out var usage) && usage.ValueKind == JsonValueKind.Object)
                {
                    input = GetInt(usage, "input_tokens");
                    output = GetInt(usage, "output_tokens");
                    cacheCreate = GetInt(usage, "cache_creation_input_tokens");
                    cacheRead = GetInt(usage, "cache_read_input_tokens");
                }

                stopReason = GetString(msg, "stop_reason");
                model = GetString(msg, "model");

                // User messages that respond to a tool_use pause carry a content array whose
                // items include `{"type":"tool_result",...}`. Plain text prompts have a string
                // content. Detecting this lets the telemetry service close an open "waiting on
                // tool" state when the agent's tool call has been serviced.
                if (type == "user"
                    && msg.TryGetProperty("content", out var content)
                    && content.ValueKind == JsonValueKind.Array)
                {
                    foreach (var item in content.EnumerateArray())
                    {
                        if (item.ValueKind == JsonValueKind.Object
                            && GetString(item, "type") == "tool_result")
                        {
                            userCarriesToolResult = true;
                            break;
                        }
                    }
                }
            }

            return new TranscriptEntry(
                sessionId, type, timestamp,
                input, output, cacheCreate, cacheRead,
                stopReason, userCarriesToolResult, model);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>
    /// Encodes an absolute Windows path to its <c>~/.claude/projects/&lt;name&gt;</c> directory name —
    /// Claude Code replaces <c>:</c>, <c>\</c>, <c>/</c>, and <c>.</c> with <c>-</c>. E.g.
    /// <c>C:\dev\codescope</c> → <c>C--dev-codescope</c> and
    /// <c>C:\dev\codescope.worktrees\feat-x</c> → <c>C--dev-codescope-worktrees-feat-x</c>.
    /// </summary>
    public static string EncodeCwd(string absolutePath)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(absolutePath);
        return absolutePath
            .Replace(':', '-')
            .Replace('\\', '-')
            .Replace('/', '-')
            .Replace('.', '-');
    }

    private static string? GetString(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static int GetInt(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt32(out var n) ? n : 0;

    private static DateTimeOffset? ParseTimestamp(string? raw) =>
        DateTimeOffset.TryParse(raw, out var dt) ? dt : null;
}
