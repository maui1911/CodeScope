namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Bounds the closed-session history surface so long-running CodeScope installs don't
/// accumulate hundreds of soft-closed rows per worktree forever — see issue #33 and the
/// ADR-0015 entry in <c>docs/DECISIONS.md</c>.
///
/// <para>Two complementary cuts are applied (in order, per worktree):</para>
/// <list type="number">
///   <item><b>TTL</b> — sessions whose <c>ClosedAt</c> is older than <see cref="MaxAge"/>
///         are dropped outright. A session you closed three months ago isn't part of
///         "this week's bug" history and is essentially never reopened.</item>
///   <item><b>Cap</b> — if the per-worktree closed count still exceeds
///         <see cref="MaxPerWorktree"/>, the oldest entries beyond the cap are dropped.
///         Most-recent-first ordering preserved so the sidebar disclosure always shows
///         the newest closed sessions.</item>
/// </list>
///
/// <para>Live sessions (<c>ClosedAt is null</c>) are untouched. Pruning happens on
/// <c>LoadAsync</c> (one-time migration of pre-policy state) and after every
/// <c>SoftCloseSessionAsync</c> (so the cap stays enforced as new closes arrive).</para>
/// </summary>
public static class SessionRetentionPolicy
{
    /// <summary>Hard cap on closed-session count per worktree. Oldest beyond this drop.</summary>
    public const int MaxPerWorktree = 100;

    /// <summary>Closed sessions older than this are dropped on the next prune sweep.</summary>
    public static readonly TimeSpan MaxAge = TimeSpan.FromDays(90);
}
