namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Shells out to the <c>git</c> CLI. All methods are async + cancellable.
/// Phase 1 only needs <see cref="GetVersionAsync"/>. Real git ops arrive in later phases.
/// </summary>
public interface IGitService
{
    /// <summary>
    /// Returns the output of <c>git --version</c>.
    /// Fails if git is not on PATH or returns a nonzero exit code.
    /// </summary>
    Task<Result<string>> GetVersionAsync(CancellationToken ct = default);

    /// <summary>Runs <c>git worktree list --porcelain</c> under <paramref name="repoPath"/> and parses it.</summary>
    Task<Result<IReadOnlyList<Models.Worktree>>> ListWorktreesAsync(string repoPath, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git worktree add &lt;newPath&gt; -b &lt;newBranch&gt; [&lt;baseBranch&gt;]</c> in
    /// <paramref name="repoPath"/>. When <paramref name="baseBranch"/> is null git forks from HEAD.
    /// </summary>
    Task<Result<bool>> AddWorktreeAsync(string repoPath, string newWorktreePath, string newBranch, string? baseBranch = null, CancellationToken ct = default);

    /// <summary>
    /// Lists local and remote branches via
    /// <c>git for-each-ref --format='%(refname:short)|%(objectname:short)|%(committerdate:relative)' refs/heads refs/remotes</c>.
    /// Sorted local-first (current branch float to top), then remote. <c>HEAD</c> symrefs are filtered out.
    /// </summary>
    Task<Result<IReadOnlyList<Models.BranchInfo>>> ListBranchesAsync(string repoPath, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git worktree remove &lt;worktreePath&gt;</c> in <paramref name="repoPath"/>.
    /// <paramref name="force"/> appends <c>--force</c>, which lets git drop the worktree even when it
    /// still has uncommitted changes or untracked files. Windows file locks (processes holding the
    /// directory as cwd) are not bypassed by <c>--force</c> — callers must close those first.
    /// </summary>
    Task<Result<bool>> RemoveWorktreeAsync(string repoPath, string worktreePath, bool force = false, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git worktree move &lt;oldPath&gt; &lt;newPath&gt;</c> in <paramref name="repoPath"/>.
    /// Fails if the worktree is dirty (git's own guard) or if <paramref name="newWorktreePath"/> already exists.
    /// </summary>
    Task<Result<bool>> MoveWorktreeAsync(string repoPath, string oldWorktreePath, string newWorktreePath, CancellationToken ct = default);

    /// <summary>
    /// Returns the current branch (<c>HEAD</c>) of <paramref name="workingDirectory"/>, or
    /// <c>null</c> when the branch cannot be determined — detached HEAD, missing <c>.git</c>,
    /// or <c>git</c> itself returning a non-zero exit. Callers that need to distinguish detached
    /// HEAD from "git failed" should query <see cref="GetStatusAsync"/> instead.
    /// </summary>
    Task<Result<string?>> GetCurrentBranchAsync(string workingDirectory, CancellationToken ct = default);

    /// <summary>Runs <c>git status --porcelain=v2 --branch</c> and returns a parsed status.</summary>
    Task<Result<Models.WorktreeStatus>> GetStatusAsync(string workingDirectory, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git remote get-url &lt;remote&gt;</c>; returns <c>null</c> when the URL cannot be
    /// obtained — either the remote does not exist, or <c>git</c> itself failed (not-a-repo,
    /// missing binary). Lossy by design: the <see cref="PullRequestService"/> treats "no origin"
    /// as "no PR" and skips the lookup for either cause.
    /// </summary>
    Task<Result<string?>> GetRemoteUrlAsync(string workingDirectory, string remote = "origin", CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git diff --no-color HEAD</c> and returns the raw unified patch. Empty string when clean.
    /// Not parsed — callers (like the diff panel) render it directly.
    /// </summary>
    Task<Result<string>> GetDiffAsync(string workingDirectory, CancellationToken ct = default);

    /// <summary>Runs <c>git pull --ff-only</c>; fails cleanly when diverged.</summary>
    Task<Result<string>> PullAsync(string workingDirectory, CancellationToken ct = default);

    /// <summary>Runs <c>git fetch --all --prune</c>; refreshes every remote's tracking refs.</summary>
    Task<Result<string>> FetchAllAsync(string workingDirectory, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git -C &lt;parentDir&gt; clone -- &lt;url&gt; &lt;folderName&gt;</c>. Returns the
    /// absolute path of the resulting working tree on success. Returns a failure message that
    /// includes git's stderr (formatted by ProcessRunner, not raw stderr verbatim)
    /// on auth errors, network errors, "destination already exists", etc. Cancellation kills
    /// the git process tree via ProcessRunner; partially-cloned target directories are NOT
    /// auto-removed by this method (callers can clean up if they want — see
    /// <c>NewProjectDialog</c> for the pattern).
    /// </summary>
    Task<Result<string>> CloneAsync(string url, string parentDir, string folderName, CancellationToken ct = default);

    /// <summary>
    /// Destructively resets <paramref name="workingDirectory"/> to HEAD and removes untracked
    /// files/directories: <c>git reset --hard HEAD</c> followed by <c>git clean -fd</c>.
    /// The caller is responsible for confirming with the user first.
    /// </summary>
    Task<Result<string>> DiscardChangesAsync(string workingDirectory, CancellationToken ct = default);

    /// <summary>
    /// Runs <c>git rebase &lt;baseRef&gt;</c> inside <paramref name="workingDirectory"/>. The
    /// caller picks the base (e.g. "origin/main"); fails on merge conflicts so the caller can
    /// surface a Toast and leave the rebase mid-flight for manual resolution.
    /// </summary>
    Task<Result<string>> RebaseOntoAsync(string workingDirectory, string baseRef, CancellationToken ct = default);
}
