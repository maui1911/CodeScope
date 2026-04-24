namespace NoScope.CodeScope.Core.Models;

/// <summary>Rollup of the PR's CI checks, derived from <c>gh</c>'s <c>statusCheckRollup</c>.</summary>
public enum CiStatus
{
    None,
    Pending,
    Success,
    Failure,
}

/// <summary>
/// Runtime-only PR metadata. Populated on demand by <see cref="Services.IPullRequestService"/>,
/// not persisted (PR state is always the upstream's source of truth).
/// </summary>
public sealed record PullRequestInfo
{
    public required int Number { get; init; }
    public required string State { get; init; }
    public required string Url { get; init; }
    public required CiStatus CiStatus { get; init; }
}
