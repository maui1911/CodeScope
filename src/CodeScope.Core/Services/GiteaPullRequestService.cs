using System.Text.Json;
using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Gitea provider for <see cref="IPullRequestService"/>: shells out to <c>tea</c>.
/// Uses <c>tea pulls list --state open --output json --fields index,state,url,head</c>.
/// CI rollup is intentionally <see cref="CiStatus.None"/>: <c>tea</c>'s check reporting varies
/// between versions (<c>tea pulls status</c> / <c>tea notifications</c>) and adding a second
/// query doubles the poll cost. Track <see cref="PullRequestInfo"/>'s per-check details via
/// Gitea's REST API if/when a Gitea user needs this — we'd rather ship None than wrong.
/// </summary>
public sealed class GiteaPullRequestService : IGiteaPullRequestService
{
    private readonly ILogger<GiteaPullRequestService> _logger;
    private readonly string _teaExecutable;

    public GiteaPullRequestService(ILogger<GiteaPullRequestService> logger, string teaExecutable = "tea")
    {
        _logger = logger;
        _teaExecutable = teaExecutable;
    }

    public async Task<Result<PullRequestInfo>> CreateForBranchAsync(string repoPath, string branch, string? title, string? body, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(branch))
        {
            return Result<PullRequestInfo>.Fail("Branch is required");
        }

        // tea requires a title; synthesise one from the branch name when the caller omits it.
        var effectiveTitle = string.IsNullOrWhiteSpace(title) ? branch : title!;
        var args = string.IsNullOrWhiteSpace(body)
            ? $"pulls create --head {branch} --title {ProcessRunner.QuoteArg(effectiveTitle)}"
            : $"pulls create --head {branch} --title {ProcessRunner.QuoteArg(effectiveTitle)} --description {ProcessRunner.QuoteArg(body!)}";

        var created = await RunAsync(repoPath, args, ct).ConfigureAwait(false);
        if (created.IsFailure)
        {
            return Result<PullRequestInfo>.Fail(created.Error);
        }

        var refetched = await GetOpenPrForBranchAsync(repoPath, branch, ct).ConfigureAwait(false);
        if (refetched.IsSuccess && refetched.Value is { } info)
        {
            return Result<PullRequestInfo>.Ok(info);
        }

        // Fall back to whatever URL tea printed; Gitea doesn't give us a reliable PR number without a re-fetch.
        var url = ProcessRunner.ExtractLastUrl(created.Value) ?? string.Empty;
        return Result<PullRequestInfo>.Ok(new PullRequestInfo
        {
            Number = 0,
            State = "open",
            Url = url,
            CiStatus = CiStatus.None,
        });
    }

    public async Task<Result<PullRequestInfo?>> GetOpenPrForBranchAsync(string repoPath, string branch, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(branch))
        {
            return Result<PullRequestInfo?>.Ok(null);
        }

        // tea's filtering is less granular than gh: we list open PRs and match on the head branch client-side.
        var args = "pulls list --state open --output json --fields index,state,url,head";
        var output = await RunAsync(repoPath, args, ct).ConfigureAwait(false);
        if (output.IsFailure)
        {
            return Result<PullRequestInfo?>.Fail(output.Error);
        }

        try
        {
            return Result<PullRequestInfo?>.Ok(ParseTeaPrJson(output.Value, branch));
        }
        catch (JsonException ex)
        {
            _logger.LogWarning(ex, "tea pulls list returned unparseable JSON: {Raw}", output.Value);
            return Result<PullRequestInfo?>.Fail($"tea pulls list: invalid JSON: {ex.Message}");
        }
    }

    /// <summary>
    /// Parses <c>tea pulls list --output json</c> — a JSON array of PR objects.
    /// Field names vary across tea versions, so we accept either <c>index|number</c>
    /// and <c>url|html_url</c>. Returns the first PR whose head branch matches.
    /// </summary>
    internal static PullRequestInfo? ParseTeaPrJson(string json, string branch)
    {
        if (string.IsNullOrWhiteSpace(json))
        {
            return null;
        }

        using var doc = JsonDocument.Parse(json);
        if (doc.RootElement.ValueKind != JsonValueKind.Array || doc.RootElement.GetArrayLength() == 0)
        {
            return null;
        }

        foreach (var pr in doc.RootElement.EnumerateArray())
        {
            var head = ExtractHeadRef(pr);
            if (!string.Equals(head, branch, StringComparison.Ordinal))
            {
                continue;
            }

            return new PullRequestInfo
            {
                Number = ReadInt(pr, "index") ?? ReadInt(pr, "number") ?? 0,
                State = ReadString(pr, "state") ?? "",
                Url = ReadString(pr, "url") ?? ReadString(pr, "html_url") ?? "",
                CiStatus = CiStatus.None,
            };
        }

        return null;
    }

    /// <summary>
    /// Gitea's pull payload nests head branch under <c>head.ref</c> (REST) or exposes a flat
    /// <c>head</c> string (tea list). Handle both.
    /// </summary>
    private static string? ExtractHeadRef(JsonElement pr)
    {
        if (!pr.TryGetProperty("head", out var head))
        {
            return null;
        }
        if (head.ValueKind == JsonValueKind.String)
        {
            return head.GetString();
        }
        if (head.ValueKind == JsonValueKind.Object && head.TryGetProperty("ref", out var r) && r.ValueKind == JsonValueKind.String)
        {
            return r.GetString();
        }
        return null;
    }

    private static int? ReadInt(JsonElement e, string prop)
        => e.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetInt32() : null;

    private static string? ReadString(JsonElement e, string prop)
        => e.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private Task<Result<string>> RunAsync(string cwd, string args, CancellationToken ct)
        => ProcessRunner.RunAsync(_teaExecutable, cwd, args, toolLabel: "tea", _logger, ct);
}
