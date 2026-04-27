using System.Text.Json;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// One parsed OpenCode message file. Unlike Claude/Pi (append-only JSONL), OpenCode persists each
/// message as its own JSON file under
/// <c>~/.local/share/opencode/project/&lt;slug&gt;/storage/message/&lt;sessionId&gt;/msg_&lt;id&gt;.json</c>,
/// so the unit of parse is one whole file.
/// </summary>
public sealed record OpenCodeMessageEntry(
    string? Id,
    string? SessionId,
    string? Role,
    DateTimeOffset? CreatedAt,
    DateTimeOffset? CompletedAt,
    int InputTokens,
    int OutputTokens,
    int ReasoningTokens,
    int CacheReadTokens,
    int CacheWriteTokens,
    string? ModelId,
    string? ProviderId,
    string? Cwd,
    bool HasPendingToolCall)
{
    /// <summary>True when this entry is an assistant turn carrying token usage.</summary>
    public bool HasUsage => Role == "assistant"
        && (InputTokens > 0 || OutputTokens > 0 || ReasoningTokens > 0 || CacheReadTokens > 0 || CacheWriteTokens > 0);

    /// <summary>Status-bar billable count — input + output + reasoning + cache-write. Cache-read excluded (already billed previously).</summary>
    public int BillableTokens => InputTokens + OutputTokens + ReasoningTokens + CacheWriteTokens;

    /// <summary>Total in-context tokens (mirrors Claude's ContextTokens calc): input + output + reasoning + cache.read + cache.write.</summary>
    public int ContextTokens => InputTokens + OutputTokens + ReasoningTokens + CacheReadTokens + CacheWriteTokens;
}

/// <summary>
/// Pure static parser for OpenCode message JSON. Each file is a single JSON object — the schema
/// is defined upstream in <c>packages/opencode/src/session/message.ts</c>:
/// <list type="bullet">
///   <item>top-level <c>id</c>, <c>role</c> ("user"|"assistant"), <c>parts</c></item>
///   <item><c>metadata.time.{created, completed?}</c> as Unix-ms numbers</item>
///   <item><c>metadata.sessionID</c></item>
///   <item><c>metadata.assistant</c> (assistant only) — <c>tokens.{input,output,reasoning,cache.{read,write}}</c>, <c>cost</c>, <c>modelID</c>, <c>providerID</c>, <c>path.{cwd,root}</c></item>
///   <item><c>parts[]</c> may contain a <c>ToolInvocationPart</c> with <c>state ∈ {"call","partial-call","result"}</c> — pre-result states mark a pending tool call</item>
/// </list>
/// Returns <c>null</c> for blank input or unparseable JSON.
/// </summary>
public static class OpenCodeMessageParser
{
    /// <summary>Parse a single OpenCode <c>msg_*.json</c> file content. Returns <c>null</c> for blank/garbage input.</summary>
    public static OpenCodeMessageEntry? ParseContent(string content)
    {
        if (string.IsNullOrWhiteSpace(content)) { return null; }
        try
        {
            using var doc = JsonDocument.Parse(content);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) { return null; }

            var id = GetString(root, "id");
            var role = GetString(root, "role");

            DateTimeOffset? created = null;
            DateTimeOffset? completed = null;
            string? sessionId = null;
            int input = 0, output = 0, reasoning = 0, cacheRead = 0, cacheWrite = 0;
            string? modelId = null;
            string? providerId = null;
            string? cwd = null;
            var pendingTool = false;

            if (root.TryGetProperty("metadata", out var meta) && meta.ValueKind == JsonValueKind.Object)
            {
                sessionId = GetString(meta, "sessionID");

                if (meta.TryGetProperty("time", out var time) && time.ValueKind == JsonValueKind.Object)
                {
                    created = ReadUnixMs(time, "created");
                    completed = ReadUnixMs(time, "completed");
                }

                if (meta.TryGetProperty("assistant", out var assistant) && assistant.ValueKind == JsonValueKind.Object)
                {
                    modelId = GetString(assistant, "modelID");
                    providerId = GetString(assistant, "providerID");
                    if (assistant.TryGetProperty("path", out var path) && path.ValueKind == JsonValueKind.Object)
                    {
                        cwd = GetString(path, "cwd");
                    }
                    if (assistant.TryGetProperty("tokens", out var tokens) && tokens.ValueKind == JsonValueKind.Object)
                    {
                        input = GetInt(tokens, "input");
                        output = GetInt(tokens, "output");
                        reasoning = GetInt(tokens, "reasoning");
                        if (tokens.TryGetProperty("cache", out var cache) && cache.ValueKind == JsonValueKind.Object)
                        {
                            cacheRead = GetInt(cache, "read");
                            cacheWrite = GetInt(cache, "write");
                        }
                    }
                }
            }

            // Pending tool detection: any ToolInvocationPart whose toolInvocation.state is not
            // "result" means the agent is mid-call — typically waiting on a permission prompt
            // in interactive mode. Conservative: only flag pending if assistant role; user
            // messages can carry tool parts too in some shapes but never wait.
            if (role == "assistant"
                && root.TryGetProperty("parts", out var parts)
                && parts.ValueKind == JsonValueKind.Array)
            {
                foreach (var part in parts.EnumerateArray())
                {
                    if (part.ValueKind != JsonValueKind.Object) { continue; }
                    if (GetString(part, "type") != "tool-invocation") { continue; }
                    if (!part.TryGetProperty("toolInvocation", out var inv) || inv.ValueKind != JsonValueKind.Object) { continue; }
                    var state = GetString(inv, "state");
                    if (state is "call" or "partial-call")
                    {
                        pendingTool = true;
                        break;
                    }
                }
            }

            return new OpenCodeMessageEntry(
                Id: id,
                SessionId: sessionId,
                Role: role,
                CreatedAt: created,
                CompletedAt: completed,
                InputTokens: input,
                OutputTokens: output,
                ReasoningTokens: reasoning,
                CacheReadTokens: cacheRead,
                CacheWriteTokens: cacheWrite,
                ModelId: modelId,
                ProviderId: providerId,
                Cwd: cwd,
                HasPendingToolCall: pendingTool);
        }
        catch (JsonException ex)
        {
            // OpenCode writes whole files atomically (rename-into-place), so partial reads are
            // rare but still possible during a save race.
            System.Diagnostics.Debug.WriteLine($"[OpenCodeMessageParser] Skipping malformed JSON: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Pulls the message id out of an OpenCode message file name like <c>msg_&lt;id&gt;.json</c>.
    /// Returns <c>null</c> for non-conforming names.
    /// </summary>
    public static string? ExtractMessageIdFromFileName(string fileName)
    {
        if (string.IsNullOrWhiteSpace(fileName)) { return null; }
        var stem = Path.GetFileNameWithoutExtension(fileName);
        return stem.StartsWith("msg_", StringComparison.Ordinal) && stem.Length > 4
            ? stem[4..]
            : null;
    }

    private static string? GetString(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static int GetInt(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt32(out var n) ? n : 0;

    private static DateTimeOffset? ReadUnixMs(JsonElement obj, string prop)
    {
        if (!obj.TryGetProperty(prop, out var v) || v.ValueKind != JsonValueKind.Number) { return null; }
        return v.TryGetInt64(out var ms)
            ? DateTimeOffset.FromUnixTimeMilliseconds(ms)
            : null;
    }
}
