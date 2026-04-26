using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Shells out to a forge CLI (gh / tea) to query and create pull requests for a worktree's
/// branch. <see cref="IGitHubPullRequestService"/> and <see cref="IGiteaPullRequestService"/>
/// are marker sub-interfaces so DI can resolve providers by role.
/// </summary>
public interface IPullRequestService
{
    /// <summary>
    /// Returns the currently open PR for <paramref name="branch"/> on the repo at
    /// <paramref name="repoPath"/>, or <c>null</c> when none exists.
    /// Fails only on CLI invocation errors (missing binary, auth, parse).
    /// </summary>
    Task<Result<PullRequestInfo?>> GetOpenPrForBranchAsync(string repoPath, string branch, CancellationToken ct = default);

    /// <summary>
    /// Creates a new PR from <paramref name="branch"/> against the repo at <paramref name="repoPath"/>.
    /// When <paramref name="title"/> is null, the provider auto-fills from commit messages
    /// (<c>gh pr create --fill</c>). Returns the created PR's info (re-fetched after creation).
    /// </summary>
    Task<Result<PullRequestInfo>> CreateForBranchAsync(string repoPath, string branch, string? title, string? body, CancellationToken ct = default);
}
