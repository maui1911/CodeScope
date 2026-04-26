namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Marker interface for the GitHub-backed provider (<c>gh</c> CLI). Exists so
/// <see cref="PullRequestService"/> can depend on the provider role rather than the
/// concrete class; also lets tests swap a stub via DI.
/// </summary>
public interface IGitHubPullRequestService : IPullRequestService { }
