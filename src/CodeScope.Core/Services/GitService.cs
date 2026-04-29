using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class GitService : IGitService
{
    private readonly ILogger<GitService> _logger;
    private readonly string _gitExecutable;

    public GitService(ILogger<GitService> logger, string gitExecutable = "git")
    {
        _logger = logger;
        _gitExecutable = gitExecutable;
    }

    public Task<Result<string>> GetVersionAsync(CancellationToken ct = default)
        => RunAsync(cwd: null, "--version", ct);

    public async Task<Result<IReadOnlyList<Worktree>>> ListWorktreesAsync(string repoPath, CancellationToken ct = default)
    {
        var output = await RunAsync(repoPath, "worktree list --porcelain", ct).ConfigureAwait(false);
        if (output.IsFailure)
        {
            return Result<IReadOnlyList<Worktree>>.Fail(output.Error);
        }

        return Result<IReadOnlyList<Worktree>>.Ok(ParsePorcelain(output.Value));
    }

    public async Task<Result<bool>> AddWorktreeAsync(string repoPath, string newWorktreePath, string newBranch, string? baseBranch = null, CancellationToken ct = default)
    {
        var baseArg = string.IsNullOrWhiteSpace(baseBranch) ? string.Empty : $" {baseBranch}";
        var result = await RunAsync(repoPath,
            $"worktree add \"{newWorktreePath}\" -b {newBranch}{baseArg}",
            ct).ConfigureAwait(false);
        return result.IsSuccess ? Result<bool>.Ok(true) : Result<bool>.Fail(result.Error);
    }

    public async Task<Result<IReadOnlyList<BranchInfo>>> ListBranchesAsync(string repoPath, CancellationToken ct = default)
    {
        var local = await ForEachRefAsync(repoPath, isRemote: false, "refs/heads", ct).ConfigureAwait(false);
        if (local.IsFailure) { return Result<IReadOnlyList<BranchInfo>>.Fail(local.Error); }

        var remote = await ForEachRefAsync(repoPath, isRemote: true, "refs/remotes", ct).ConfigureAwait(false);
        if (remote.IsFailure) { return Result<IReadOnlyList<BranchInfo>>.Fail(remote.Error); }

        var all = new List<BranchInfo>(local.Value.Count + remote.Value.Count);
        all.AddRange(local.Value);
        all.AddRange(remote.Value);
        return Result<IReadOnlyList<BranchInfo>>.Ok(all);
    }

    private async Task<Result<IReadOnlyList<BranchInfo>>> ForEachRefAsync(string repoPath, bool isRemote, string prefix, CancellationToken ct)
    {
        // '|' is not a valid refname character, so it's safe as a field separator.
        var output = await RunAsync(repoPath,
            $"for-each-ref --format=\"%(refname:short)|%(objectname:short)|%(committerdate:relative)\" {prefix}",
            ct).ConfigureAwait(false);
        if (output.IsFailure) { return Result<IReadOnlyList<BranchInfo>>.Fail(output.Error); }

        var list = new List<BranchInfo>();
        foreach (var raw in output.Value.Split('\n', StringSplitOptions.RemoveEmptyEntries))
        {
            var line = raw.Trim('\r', ' ', '\t', '"');
            if (line.Length == 0) { continue; }
            var parts = line.Split('|');
            if (parts.Length < 3) { continue; }

            var name = parts[0].Trim();
            // Skip symbolic 'origin/HEAD' rows.
            if (name.EndsWith("/HEAD", StringComparison.Ordinal)) { continue; }

            list.Add(new BranchInfo(name, isRemote, parts[1].Trim(), parts[2].Trim()));
        }
        list.Sort((a, b) => StringComparer.OrdinalIgnoreCase.Compare(a.Name, b.Name));
        return Result<IReadOnlyList<BranchInfo>>.Ok(list);
    }

    public async Task<Result<bool>> RemoveWorktreeAsync(string repoPath, string worktreePath, bool force = false, CancellationToken ct = default)
    {
        var args = force
            ? $"worktree remove --force \"{worktreePath}\""
            : $"worktree remove \"{worktreePath}\"";
        var result = await RunAsync(repoPath, args, ct).ConfigureAwait(false);
        return result.IsSuccess ? Result<bool>.Ok(true) : Result<bool>.Fail(result.Error);
    }

    public async Task<Result<bool>> MoveWorktreeAsync(string repoPath, string oldWorktreePath, string newWorktreePath, CancellationToken ct = default)
    {
        var result = await RunAsync(repoPath,
            $"worktree move \"{oldWorktreePath}\" \"{newWorktreePath}\"",
            ct).ConfigureAwait(false);
        return result.IsSuccess ? Result<bool>.Ok(true) : Result<bool>.Fail(result.Error);
    }

    public async Task<Result<WorktreeStatus>> GetStatusAsync(string workingDirectory, CancellationToken ct = default)
    {
        var output = await RunAsync(workingDirectory, "status --porcelain=v2 --branch", ct).ConfigureAwait(false);
        if (output.IsFailure)
        {
            return Result<WorktreeStatus>.Fail(output.Error);
        }

        var status = ParseStatusV2(output.Value);

        // When dirty, piggy-back a numstat pass so the status bar can show "+N −N" per spec
        // callout #3. `git diff --numstat HEAD` covers staged + unstaged modifications; untracked
        // new files aren't counted here (they have no HEAD baseline). Cheap, single extra call.
        if (status.IsDirty)
        {
            var numstat = await RunAsync(workingDirectory, "diff --numstat HEAD", ct).ConfigureAwait(false);
            if (numstat.IsSuccess)
            {
                var (added, removed, files) = ParseNumstat(numstat.Value);
                status = status with { Added = added, Removed = removed, ChangedFiles = files };
            }
        }

        return Result<WorktreeStatus>.Ok(status);
    }

    /// <summary>
    /// Parses <c>git diff --numstat HEAD</c>: one line per file,
    ///   <c>&lt;added&gt;\t&lt;removed&gt;\t&lt;path&gt;</c>.
    /// Binary files emit <c>"-\t-\tpath"</c> and are counted only toward file count.
    /// </summary>
    internal static (int Added, int Removed, int ChangedFiles) ParseNumstat(string output)
    {
        var added = 0;
        var removed = 0;
        var files = 0;
        foreach (var rawLine in output.Split('\n'))
        {
            var line = rawLine.TrimEnd('\r');
            if (line.Length == 0) { continue; }
            var parts = line.Split('\t', 3);
            if (parts.Length < 3) { continue; }
            files++;
            if (int.TryParse(parts[0], out var a)) { added += a; }
            if (int.TryParse(parts[1], out var r)) { removed += r; }
        }
        return (added, removed, files);
    }

    /// <summary>
    /// Parses <c>git status --porcelain=v2 --branch</c>:
    ///   # branch.oid &lt;sha&gt;
    ///   # branch.head &lt;name|(detached)&gt;
    ///   # branch.upstream &lt;remote/branch&gt;   (optional)
    ///   # branch.ab +&lt;ahead&gt; -&lt;behind&gt;       (optional, only with upstream)
    ///   1 …   / 2 …   / u …  / ? path           (one per changed/untracked entry)
    /// </summary>
    internal static WorktreeStatus ParseStatusV2(string output)
    {
        string? branch = null;
        var ahead = 0;
        var behind = 0;
        var isDirty = false;

        foreach (var rawLine in output.Split('\n'))
        {
            var line = rawLine.TrimEnd('\r');
            if (line.Length == 0) { continue; }

            if (line.StartsWith("# branch.head ", StringComparison.Ordinal))
            {
                var name = line["# branch.head ".Length..];
                branch = string.Equals(name, "(detached)", StringComparison.Ordinal) ? null : name;
            }
            else if (line.StartsWith("# branch.ab ", StringComparison.Ordinal))
            {
                var tokens = line["# branch.ab ".Length..].Split(' ');
                foreach (var t in tokens)
                {
                    if (t.Length < 2) { continue; }
                    _ = int.TryParse(t[1..], out var n);
                    if (t[0] == '+') { ahead = n; }
                    else if (t[0] == '-') { behind = n; }
                }
            }
            else if (!line.StartsWith("#", StringComparison.Ordinal))
            {
                // Any non-header line means at least one change: '1 ', '2 ', 'u ', '? '.
                isDirty = true;
            }
        }

        return new WorktreeStatus
        {
            Branch = branch,
            IsDirty = isDirty,
            Ahead = ahead,
            Behind = behind,
        };
    }

    public Task<Result<string>> GetDiffAsync(string workingDirectory, CancellationToken ct = default)
        => RunAsync(workingDirectory, "diff --no-color HEAD", ct);

    public Task<Result<string>> PullAsync(string workingDirectory, CancellationToken ct = default)
        => RunAsync(workingDirectory, "pull --ff-only", ct);

    public Task<Result<string>> FetchAllAsync(string workingDirectory, CancellationToken ct = default)
        => RunAsync(workingDirectory, "fetch --all --prune", ct);

    public async Task<Result<string>> CloneAsync(string url, string parentDir, string folderName, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(url))
        {
            return Result<string>.Fail("URL is empty");
        }
        if (string.IsNullOrWhiteSpace(parentDir))
        {
            return Result<string>.Fail("Parent directory is empty");
        }
        if (string.IsNullOrWhiteSpace(folderName))
        {
            return Result<string>.Fail("Folder name is empty");
        }

        if (!Directory.Exists(parentDir))
        {
            return Result<string>.Fail($"Parent directory does not exist: {parentDir}");
        }

        var target = Path.Combine(parentDir, folderName);
        if (Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any())
        {
            return Result<string>.Fail($"Destination already exists and is not empty: {target}");
        }

        // Quote both arguments and use `--` so URLs/folder names that start with a dash
        // can't be parsed as flags.
        var args = $"-C \"{parentDir}\" clone -- \"{url}\" \"{folderName}\"";
        var result = await RunAsync(cwd: null, args, ct).ConfigureAwait(false);
        return result.IsSuccess
            ? Result<string>.Ok(target)
            : Result<string>.Fail(result.Error);
    }

    public async Task<Result<string>> DiscardChangesAsync(string workingDirectory, CancellationToken ct = default)
    {
        var reset = await RunAsync(workingDirectory, "reset --hard HEAD", ct).ConfigureAwait(false);
        if (reset.IsFailure) { return reset; }
        return await RunAsync(workingDirectory, "clean -fd", ct).ConfigureAwait(false);
    }

    public Task<Result<string>> RebaseOntoAsync(string workingDirectory, string baseRef, CancellationToken ct = default)
        => RunAsync(workingDirectory, $"rebase {baseRef}", ct);

    public async Task<Result<string?>> GetRemoteUrlAsync(string workingDirectory, string remote = "origin", CancellationToken ct = default)
    {
        var result = await RunAsync(workingDirectory, $"remote get-url {remote}", ct).ConfigureAwait(false);
        if (result.IsFailure)
        {
            // Non-zero from `git remote get-url` for a missing remote is expected; map to null.
            return Result<string?>.Ok(null);
        }
        return Result<string?>.Ok(string.IsNullOrWhiteSpace(result.Value) ? null : result.Value);
    }

    public async Task<Result<string?>> GetCurrentBranchAsync(string workingDirectory, CancellationToken ct = default)
    {
        // `symbolic-ref --short --quiet HEAD` prints the branch name on a normal HEAD and exits
        // with code 1 on detached HEAD (or any other "HEAD doesn't resolve to a ref") without
        // writing anything to stderr. A non-zero exit is also what you get from git if the
        // directory isn't a repo or git isn't on PATH — we collapse all of those to `null`
        // here, matching the docstring on IGitService.GetCurrentBranchAsync.
        var result = await RunAsync(workingDirectory, "symbolic-ref --short --quiet HEAD", ct).ConfigureAwait(false);
        if (result.IsFailure)
        {
            return Result<string?>.Ok(null);
        }
        return Result<string?>.Ok(string.IsNullOrWhiteSpace(result.Value) ? null : result.Value);
    }

    /// <summary>
    /// Parses <c>git worktree list --porcelain</c> output into <see cref="Worktree"/> records.
    /// Format is a sequence of record blocks separated by blank lines, each with:
    ///   worktree &lt;path&gt;
    ///   HEAD &lt;sha&gt;
    ///   branch refs/heads/&lt;name&gt;   (or "detached")
    /// The first record is always the primary worktree.
    /// </summary>
    internal static IReadOnlyList<Worktree> ParsePorcelain(string porcelain)
    {
        var list = new List<Worktree>();
        string? path = null;
        string? branch = null;
        var isFirst = true;

        foreach (var rawLine in porcelain.Split('\n'))
        {
            var line = rawLine.TrimEnd('\r');
            if (string.IsNullOrWhiteSpace(line))
            {
                if (path is not null)
                {
                    list.Add(new Worktree
                    {
                        Id = isFirst ? "primary" : Path.GetFileName(path.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)),
                        Path = path,
                        Branch = branch,
                        IsPrimary = isFirst,
                    });
                    isFirst = false;
                }
                path = null;
                branch = null;
                continue;
            }

            if (line.StartsWith("worktree ", StringComparison.Ordinal))
            {
                path = line[9..];
            }
            else if (line.StartsWith("branch refs/heads/", StringComparison.Ordinal))
            {
                branch = line[18..];
            }
            else if (line.Equals("detached", StringComparison.Ordinal))
            {
                branch = null;
            }
        }

        // Final record (no trailing blank line).
        if (path is not null)
        {
            list.Add(new Worktree
            {
                Id = isFirst ? "primary" : Path.GetFileName(path.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)),
                Path = path,
                Branch = branch,
                IsPrimary = isFirst,
            });
        }

        return list;
    }

    private Task<Result<string>> RunAsync(string? cwd, string args, CancellationToken ct)
        => ProcessRunner.RunAsync(_gitExecutable, cwd, args, toolLabel: "git", _logger, ct);
}
