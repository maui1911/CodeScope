using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Shells out to the <c>gh</c> CLI to query GitHub PRs for a worktree's branch.
/// Phase 5 starts with a single read path; mutations (create/merge) arrive later.
/// </summary>
public interface IPullRequestService
{
    /// <summary>
    /// Returns the currently open PR for <paramref name="branch"/> on the repo at
    /// <paramref name="repoPath"/>, or <c>null</c> when none exists.
    /// Fails only on gh invocation errors (gh missing, auth, parse).
    /// </summary>
    Task<Result<PullRequestInfo?>> GetOpenPrForBranchAsync(string repoPath, string branch, CancellationToken ct = default);

    /// <summary>
    /// Creates a new PR from <paramref name="branch"/> against the repo at <paramref name="repoPath"/>.
    /// When <paramref name="title"/> is null, the provider auto-fills from commit messages
    /// (<c>gh pr create --fill</c>). Returns the created PR's info (re-fetched after creation).
    /// </summary>
    Task<Result<PullRequestInfo>> CreateForBranchAsync(string repoPath, string branch, string? title, string? body, CancellationToken ct = default);
}

/// <summary>
/// Marker interface for the GitHub-backed provider (<c>gh</c> CLI). Exists so
/// <see cref="PullRequestService"/> can depend on the provider role rather than the
/// concrete class; also lets tests swap a stub via DI.
/// </summary>
public interface IGitHubPullRequestService : IPullRequestService { }

/// <summary>
/// Marker interface for the Gitea-backed provider (<c>tea</c> CLI). Role sibling of
/// <see cref="IGitHubPullRequestService"/>.
/// </summary>
public interface IGiteaPullRequestService : IPullRequestService { }
