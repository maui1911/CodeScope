using System.Text.Json;
using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <summary>GitHub provider for <see cref="IPullRequestService"/>: shells out to <c>gh</c>.</summary>
public sealed class GitHubPullRequestService : IGitHubPullRequestService
{
    private readonly ILogger<GitHubPullRequestService> _logger;
    private readonly string _ghExecutable;

    public GitHubPullRequestService(ILogger<GitHubPullRequestService> logger, string ghExecutable = "gh")
    {
        _logger = logger;
        _ghExecutable = ghExecutable;
    }

    public async Task<Result<PullRequestInfo>> CreateForBranchAsync(string repoPath, string branch, string? title, string? body, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(branch))
        {
            return Result<PullRequestInfo>.Fail("Branch is required");
        }

        // --fill auto-populates title/body from commit messages when no explicit values are given.
        var args = string.IsNullOrWhiteSpace(title) && string.IsNullOrWhiteSpace(body)
            ? $"pr create --head {branch} --fill"
            : $"pr create --head {branch} --title {ProcessRunner.QuoteArg(title ?? "")} --body {ProcessRunner.QuoteArg(body ?? "")}";

        var created = await RunAsync(repoPath, args, ct).ConfigureAwait(false);
        if (created.IsFailure)
        {
            return Result<PullRequestInfo>.Fail(created.Error);
        }

        // `gh pr create` prints the PR URL on the last non-empty stdout line.
        var url = ProcessRunner.ExtractLastUrl(created.Value);

        // Re-fetch for full state (CI rollup etc.). If the query fails, fall back to a minimal record.
        var refetched = await GetOpenPrForBranchAsync(repoPath, branch, ct).ConfigureAwait(false);
        if (refetched.IsSuccess && refetched.Value is { } info)
        {
            return Result<PullRequestInfo>.Ok(info);
        }

        return Result<PullRequestInfo>.Ok(new PullRequestInfo
        {
            Number = ExtractPrNumberFromUrl(url),
            State = "OPEN",
            Url = url ?? string.Empty,
            CiStatus = CiStatus.None,
        });
    }

    /// <summary>Parses "/pull/42" style trailing segment from a GitHub PR URL.</summary>
    internal static int ExtractPrNumberFromUrl(string? url)
    {
        if (string.IsNullOrWhiteSpace(url)) { return 0; }
        var slash = url.LastIndexOf('/');
        return slash >= 0 && int.TryParse(url.AsSpan(slash + 1), out var n) ? n : 0;
    }

    public async Task<Result<PullRequestInfo?>> GetOpenPrForBranchAsync(string repoPath, string branch, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(branch))
        {
            return Result<PullRequestInfo?>.Ok(null);
        }

        var args = $"pr list --head {branch} --state open --json number,state,url,statusCheckRollup --limit 1";
        var output = await RunAsync(repoPath, args, ct).ConfigureAwait(false);
        if (output.IsFailure)
        {
            return Result<PullRequestInfo?>.Fail(output.Error);
        }

        try
        {
            return Result<PullRequestInfo?>.Ok(ParseGhPrJson(output.Value));
        }
        catch (JsonException ex)
        {
            _logger.LogWarning(ex, "gh pr list returned unparseable JSON: {Raw}", output.Value);
            return Result<PullRequestInfo?>.Fail($"gh pr list: invalid JSON: {ex.Message}");
        }
    }

    /// <summary>
    /// Parses <c>gh pr list --json number,state,url,statusCheckRollup</c> — a JSON array.
    /// Empty array → no PR. Otherwise returns the first entry with a CI rollup.
    /// </summary>
    internal static PullRequestInfo? ParseGhPrJson(string json)
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

        var first = doc.RootElement[0];
        var number = first.TryGetProperty("number", out var n) && n.ValueKind == JsonValueKind.Number
            ? n.GetInt32() : 0;
        var state = first.TryGetProperty("state", out var s) && s.ValueKind == JsonValueKind.String
            ? s.GetString() ?? "" : "";
        var url = first.TryGetProperty("url", out var u) && u.ValueKind == JsonValueKind.String
            ? u.GetString() ?? "" : "";

        var ci = CiStatus.None;
        if (first.TryGetProperty("statusCheckRollup", out var rollup) && rollup.ValueKind == JsonValueKind.Array)
        {
            ci = RollupCi(rollup);
        }

        return new PullRequestInfo
        {
            Number = number,
            State = state,
            Url = url,
            CiStatus = ci,
        };
    }

    /// <summary>
    /// Rolls up gh's <c>statusCheckRollup</c>:
    ///   - any FAILURE / CANCELLED / TIMED_OUT / ACTION_REQUIRED / STARTUP_FAILURE ⇒ Failure
    ///   - else any non-COMPLETED status ⇒ Pending
    ///   - else (all SUCCESS) ⇒ Success; empty ⇒ None.
    /// </summary>
    private static CiStatus RollupCi(JsonElement rollup)
    {
        if (rollup.GetArrayLength() == 0) { return CiStatus.None; }

        var hasPending = false;
        foreach (var check in rollup.EnumerateArray())
        {
            var conclusion = check.TryGetProperty("conclusion", out var c) && c.ValueKind == JsonValueKind.String
                ? c.GetString() ?? "" : "";
            var status = check.TryGetProperty("status", out var st) && st.ValueKind == JsonValueKind.String
                ? st.GetString() ?? "" : "";

            if (conclusion is "FAILURE" or "CANCELLED" or "TIMED_OUT" or "ACTION_REQUIRED" or "STARTUP_FAILURE")
            {
                return CiStatus.Failure;
            }

            if (!string.Equals(status, "COMPLETED", StringComparison.Ordinal))
            {
                hasPending = true;
            }
        }

        return hasPending ? CiStatus.Pending : CiStatus.Success;
    }

    private Task<Result<string>> RunAsync(string cwd, string args, CancellationToken ct)
        => ProcessRunner.RunAsync(_ghExecutable, cwd, args, toolLabel: "gh", _logger, ct);
}
