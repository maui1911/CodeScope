namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Maps a Claude model id (as seen in <c>message.model</c> of the JSONL transcript) to its
/// nominal context-window capacity in tokens. Returns 0 when the id is unrecognised, which lets
/// callers fall back to <c>AgentProfile.ContextWindowTokens</c> or hide the cap in the UI.
///
/// Rules are deliberately loose — we match on substrings rather than a hard-coded allow-list so
/// point-release variants (<c>claude-opus-4-7-20260115</c>, <c>claude-sonnet-4-6</c>) Just Work
/// without a code change. The <c>1m</c> marker (from the extended-context SKU, e.g.
/// <c>claude-opus-4-7[1m]</c> or <c>-1m-</c> suffixes) upgrades the cap to 1M.
/// </summary>
public static class ClaudeModelCatalog
{
    /// <summary>Standard Claude context window for the 4.x family.</summary>
    public const int StandardContextTokens = 200_000;

    /// <summary>Extended-context variant capacity (1M SKU).</summary>
    public const int ExtendedContextTokens = 1_000_000;

    /// <summary>
    /// Returns the context-window capacity for <paramref name="modelId"/>, or 0 when unknown.
    /// Safe to call with null/empty input.
    /// </summary>
    public static int GetContextWindow(string? modelId)
    {
        if (string.IsNullOrWhiteSpace(modelId)) { return 0; }
        var id = modelId.ToLowerInvariant();

        // Explicit 1M marker always wins — overrides any family default (e.g. a future
        // `claude-sonnet-*-1m` SKU or the legacy `claude-opus-4-7[1m]` tag).
        if (id.Contains("1m")) { return ExtendedContextTokens; }

        // Sonnet / Haiku — 200k; no current 1M SKU.
        if (id.Contains("sonnet") || id.Contains("haiku")) { return StandardContextTokens; }

        // Claude 3.x Opus — 200k. Matches "claude-3-opus-*" without catching "claude-opus-4-*".
        if (id.Contains("claude-3")) { return StandardContextTokens; }

        // Opus 4.x (and later) ships the 1M context window by default in Claude Code CLI —
        // the transcript's `message.model` is a plain `claude-opus-4-7` with no `[1m]` suffix,
        // so we treat the Opus 4+ family as 1M by default.
        if (id.Contains("claude") && id.Contains("opus")) { return ExtendedContextTokens; }

        return 0;
    }
}
