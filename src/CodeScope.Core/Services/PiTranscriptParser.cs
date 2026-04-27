using System.Text.Json;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// One parsed line from a pi-coding-agent <c>~/.pi/agent/sessions/--&lt;cwd&gt;--/&lt;ts&gt;_&lt;uuid&gt;.jsonl</c>
/// transcript. Pi's on-disk schema is richer than its <c>--mode json</c> stdout stream — top-level
/// types include <c>session</c> (header), <c>message</c> (with role + usage + stopReason),
/// <c>compaction</c>, <c>model_change</c>, <c>thinking_level_change</c>, <c>branch_summary</c>,
/// <c>label</c>, <c>custom</c>, <c>custom_message</c>. Fields outside what the status bar
/// consumes are intentionally dropped.
/// </summary>
public sealed record PiTranscriptEntry(
    string? SessionId,
    string? Type,
    string? Role,
    DateTimeOffset? Timestamp,
    int InputTokens,
    int OutputTokens,
    int CacheCreationTokens,
    int CacheReadTokens,
    string? StopReason = null,
    string? Model = null,
    string? Provider = null,
    string? Cwd = null)
{
    /// <summary>True when this entry carries assistant token usage.</summary>
    public bool HasUsage => Type == "message"
        && Role == "assistant"
        && (InputTokens > 0 || OutputTokens > 0 || CacheCreationTokens > 0 || CacheReadTokens > 0);

    /// <summary>Status-bar billable count — input + output + cache-write. Mirrors Claude semantics.</summary>
    public int BillableTokens => InputTokens + OutputTokens + CacheCreationTokens;
}

/// <summary>
/// Pure static parser for Pi session.jsonl entries. No IO. <c>null</c> on unparseable lines
/// (trailing partial flush, extension-emitted custom types we don't model, etc.).
/// </summary>
public static class PiTranscriptParser
{
    /// <summary>Parse a single Pi <c>session.jsonl</c> line. Returns <c>null</c> for blank lines or malformed JSON.</summary>
    public static PiTranscriptEntry? ParseLine(string line)
    {
        if (string.IsNullOrWhiteSpace(line)) { return null; }
        try
        {
            using var doc = JsonDocument.Parse(line);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) { return null; }

            var type = GetString(root, "type");
            var timestamp = ParseTimestamp(GetString(root, "timestamp"));

            // session header carries the canonical session id + cwd; all other entries' top-level
            // `id` is the line id, not the session id (we recover the session id from the file
            // name via ExtractSessionIdFromFileName).
            if (type == "session")
            {
                return new PiTranscriptEntry(
                    SessionId: GetString(root, "id"),
                    Type: type,
                    Role: null,
                    Timestamp: timestamp,
                    InputTokens: 0, OutputTokens: 0,
                    CacheCreationTokens: 0, CacheReadTokens: 0,
                    Cwd: GetString(root, "cwd"));
            }

            // model_change events carry the model id at root, not under message.
            if (type == "model_change")
            {
                return new PiTranscriptEntry(
                    SessionId: null,
                    Type: type,
                    Role: null,
                    Timestamp: timestamp,
                    InputTokens: 0, OutputTokens: 0,
                    CacheCreationTokens: 0, CacheReadTokens: 0,
                    Model: GetString(root, "modelId"),
                    Provider: GetString(root, "provider"));
            }

            string? role = null;
            int input = 0, output = 0, cacheCreate = 0, cacheRead = 0;
            string? stopReason = null;
            string? model = null;
            string? provider = null;

            if (type == "message"
                && root.TryGetProperty("message", out var msg)
                && msg.ValueKind == JsonValueKind.Object)
            {
                role = GetString(msg, "role");
                stopReason = GetString(msg, "stopReason");
                model = GetString(msg, "model");
                provider = GetString(msg, "provider");

                if (msg.TryGetProperty("usage", out var usage) && usage.ValueKind == JsonValueKind.Object)
                {
                    input = GetInt(usage, "input");
                    output = GetInt(usage, "output");
                    cacheRead = GetInt(usage, "cacheRead");
                    cacheCreate = GetInt(usage, "cacheWrite");
                }
            }

            return new PiTranscriptEntry(
                SessionId: null,
                Type: type,
                Role: role,
                Timestamp: timestamp,
                InputTokens: input, OutputTokens: output,
                CacheCreationTokens: cacheCreate, CacheReadTokens: cacheRead,
                StopReason: stopReason,
                Model: model,
                Provider: provider);
        }
        catch (JsonException ex)
        {
            // Mid-flush partial line — Pi flushes per event but a process exit can leave a
            // half-written tail. Trace and skip; the next read picks up the rest.
            System.Diagnostics.Debug.WriteLine($"[PiTranscriptParser] Skipping malformed JSONL line: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Pulls the trailing UUID out of a Pi session-file name like
    /// <c>2026-04-22T08-00-00-000Z_f1e2d3c4-aaaa-bbbb-cccc-1234567890ab.jsonl</c>. Pi names every
    /// session file with a timestamp prefix and an underscore separator before the UUID, so the
    /// portion after the last <c>_</c> minus the extension IS the session id. Returns
    /// <c>null</c> when the file name doesn't match the convention or the trailing token isn't
    /// a valid UUID — that lets the discovery layer skip extension-created sidecar files
    /// without picking up false positives.
    /// </summary>
    public static string? ExtractSessionIdFromFileName(string fileName)
    {
        if (string.IsNullOrWhiteSpace(fileName)) { return null; }
        var stem = Path.GetFileNameWithoutExtension(fileName);
        var underscore = stem.LastIndexOf('_');
        if (underscore < 0 || underscore == stem.Length - 1) { return null; }
        var id = stem[(underscore + 1)..];
        return Guid.TryParseExact(id, "D", out _) ? id : null;
    }

    private static string? GetString(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static int GetInt(JsonElement obj, string prop) =>
        obj.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt32(out var n) ? n : 0;

    private static DateTimeOffset? ParseTimestamp(string? raw) =>
        DateTimeOffset.TryParse(raw, out var dt) ? dt : null;
}
