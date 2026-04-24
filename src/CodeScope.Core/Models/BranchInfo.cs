namespace NoScope.CodeScope.Core.Models;

/// <summary>
/// One row from <c>git for-each-ref refs/heads refs/remotes</c> — surfaced to the
/// "New worktree" dialog so the user can pick which branch to fork from.
/// </summary>
/// <param name="Name">Short refname — e.g. <c>main</c>, <c>feat/csv</c>, <c>origin/main</c>.</param>
/// <param name="IsRemote">True when <see cref="Name"/> starts with a remote prefix.</param>
/// <param name="ShortSha">7-char commit sha the ref points at.</param>
/// <param name="RelativeDate">Committer date relative to now — e.g. <c>2 days ago</c>.</param>
public sealed record BranchInfo(string Name, bool IsRemote, string ShortSha, string RelativeDate);
