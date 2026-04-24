using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Composite <see cref="IPullRequestService"/> that dispatches to a provider based on
/// the repo's <c>origin</c> remote URL. GitHub remotes go to <see cref="GitHubPullRequestService"/>,
/// anything else is assumed to be Gitea and goes to <see cref="GiteaPullRequestService"/>.
/// </summary>
public sealed class PullRequestService : IPullRequestService
{
    private readonly IGitService _git;
    private readonly IGitHubPullRequestService _github;
    private readonly IGiteaPullRequestService _gitea;
    private readonly ILogger<PullRequestService> _logger;

    public PullRequestService(
        IGitService git,
        IGitHubPullRequestService github,
        IGiteaPullRequestService gitea,
        ILogger<PullRequestService> logger)
    {
        _git = git;
        _github = github;
        _gitea = gitea;
        _logger = logger;
    }

    public async Task<Result<PullRequestInfo?>> GetOpenPrForBranchAsync(string repoPath, string branch, CancellationToken ct = default)
    {
        var remote = await _git.GetRemoteUrlAsync(repoPath, "origin", ct).ConfigureAwait(false);
        if (remote.IsFailure || remote.Value is null)
        {
            _logger.LogDebug("No origin remote for {Repo}; skipping PR lookup", repoPath);
            return Result<PullRequestInfo?>.Ok(null);
        }

        var provider = SelectProvider(remote.Value);
        return await provider.GetOpenPrForBranchAsync(repoPath, branch, ct).ConfigureAwait(false);
    }

    public async Task<Result<PullRequestInfo>> CreateForBranchAsync(string repoPath, string branch, string? title, string? body, CancellationToken ct = default)
    {
        var remote = await _git.GetRemoteUrlAsync(repoPath, "origin", ct).ConfigureAwait(false);
        if (remote.IsFailure || remote.Value is null)
        {
            return Result<PullRequestInfo>.Fail($"No 'origin' remote configured for {repoPath}");
        }

        var provider = SelectProvider(remote.Value);
        return await provider.CreateForBranchAsync(repoPath, branch, title, body, ct).ConfigureAwait(false);
    }

    /// <summary>
    /// GitHub when the remote host is github.com (SSH or HTTPS) or appears in
    /// <c>CODESCOPE_GITHUB_HOSTS</c>; Gitea otherwise. The env var is comma-separated
    /// (e.g. <c>github.mycorp.com,ghe.internal</c>) and enables GHES users to route
    /// their self-hosted GitHub through the <c>gh</c> provider.
    /// </summary>
    internal IPullRequestService SelectProvider(string remoteUrl)
        => IsGitHubRemote(remoteUrl) ? _github : _gitea;

    internal static bool IsGitHubRemote(string remoteUrl)
        => IsGitHubRemote(remoteUrl, Environment.GetEnvironmentVariable("CODESCOPE_GITHUB_HOSTS"));

    internal static bool IsGitHubRemote(string remoteUrl, string? extraHostsEnv)
    {
        if (string.IsNullOrWhiteSpace(remoteUrl)) { return false; }
        var url = remoteUrl.Trim();

        if (url.Contains("github.com", StringComparison.OrdinalIgnoreCase)) { return true; }

        if (string.IsNullOrWhiteSpace(extraHostsEnv)) { return false; }

        foreach (var host in extraHostsEnv.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            if (url.Contains(host, StringComparison.OrdinalIgnoreCase)) { return true; }
        }
        return false;
    }
}
