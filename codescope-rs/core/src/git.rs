//! Thin wrapper around `git worktree` and friends.
//!
//! Shells out to the system `git` binary — see CLAUDE.md, the
//! technology decisions list explicitly chooses shelling-out over
//! libgit2 bindings. Process spawning makes this module the only
//! impure thing in `core`, but parsing porcelain output stays here
//! (away from gpui) so it can be unit-tested without the renderer.
//!
//! The C# build's [`GitService`] has a much wider surface
//! (`for-each-ref`, branch listing, status). We start narrow: the
//! Rust port only needs the primitives that drive the sidebar's
//! per-session worktrees. Branch listing lands the day the new-
//! session dialog needs it.
//!
//! Errors carry the trimmed stderr verbatim when `git` exits non-
//! zero, so a UI dialog can surface "fatal: 'foo' is already checked
//! out at '...'" without us having to guess the cause. We don't
//! truncate — the failure modes in scope here (`worktree add/remove/
//! list`, `for-each-ref`) emit short messages, and clipping mid-
//! sentence is worse UX than a slightly long line.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, anyhow};

/// One row from `git for-each-ref refs/heads refs/remotes`. Surfaced
/// to the "New worktree" dialog so the user can pick a base branch.
/// Mirrors `NoScope.CodeScope.Core.Models.BranchInfo` from the C# build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// Short refname — e.g. `main`, `feat/csv`, `origin/main`.
    pub name: String,
    /// `true` when `name` lives under `refs/remotes/...`.
    pub is_remote: bool,
    /// 7-char commit sha the ref points at.
    pub short_sha: String,
    /// Committer date relative to now — e.g. `2 days ago`. Verbatim
    /// from `%(committerdate:relative)` so the UI doesn't have to
    /// parse and re-format.
    pub relative_date: String,
}

/// One row from `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// Absolute path to the worktree root.
    pub path: String,
    /// SHA the worktree currently points at (`HEAD <sha>` line). The
    /// porcelain emits this for every worktree, so it's never empty
    /// in practice — we keep it `String` (not `Option`) on purpose.
    pub head: String,
    /// Branch name without the `refs/heads/` prefix. `None` for a
    /// detached HEAD or a bare repo.
    pub branch: Option<String>,
    /// Whether this is the primary working tree (the one the repo
    /// was initialised at). Porcelain output emits the primary
    /// first; we tag the first stanza accordingly.
    pub is_primary: bool,
    /// `true` if the worktree is currently locked. Locked worktrees
    /// can't be removed without `--force` and we surface the flag
    /// so the UI can warn before destructive ops.
    pub locked: bool,
}

/// `git worktree list --porcelain` for `repo`. The repo is *any*
/// path inside the working tree — git resolves it to the right
/// `.git` directory.
pub fn list_worktrees(repo: &Path) -> Result<Vec<WorktreeInfo>> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_worktree_porcelain(&stdout))
}

/// `git worktree add <path> -b <branch> [<base>]`. Creates the
/// branch as part of the same call so we don't race.
pub fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<()> {
    let path_str = path.to_string_lossy();
    let mut args: Vec<&str> = vec!["worktree", "add", &path_str, "-b", branch];
    if let Some(b) = base {
        args.push(b);
    }
    run_git(repo, &args).map(|_| ())
}

/// `git for-each-ref` for both local heads and remotes — the union
/// drives the base-branch picker in the "New worktree" dialog. Mirrors
/// the C# `GitService.ListBranchesAsync`: format
/// `<short_ref>|<short_sha>|<relative_date>`, `origin/HEAD` symbolic
/// refs are dropped, and the final list is sorted alphabetically by
/// `name` (case-insensitive) so locals and remotes interleave under a
/// stable ordering — the dialog handles LOCAL/REMOTE grouping itself.
pub fn list_branches(repo: &Path) -> Result<Vec<BranchInfo>> {
    let mut all = Vec::new();
    all.extend(for_each_ref(repo, false, "refs/heads")?);
    all.extend(for_each_ref(repo, true, "refs/remotes")?);
    all.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(all)
}

fn for_each_ref(repo: &Path, is_remote: bool, prefix: &str) -> Result<Vec<BranchInfo>> {
    // `|` is not a valid refname character, so it's safe as a field
    // separator (matches the C# build's choice for the same reason).
    let format = "--format=%(refname:short)|%(objectname:short)|%(committerdate:relative)";
    let output = run_git(repo, &["for-each-ref", format, prefix])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rows = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim_matches(|c: char| c == '\r' || c == ' ' || c == '\t' || c == '"');
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() < 3 {
            continue;
        }
        let name = parts[0].trim();
        // Skip the `<remote>/HEAD` symbolic row — it's not a real
        // branch and showing it in a base-branch picker is just
        // noise. On Windows git the same row sometimes shortens to
        // `<remote>` (HEAD elided) so we also drop bare-remote rows
        // (a real remote branch always has at least one `/` after
        // the remote name).
        if name.ends_with("/HEAD") || (is_remote && !name.contains('/')) {
            continue;
        }
        rows.push(BranchInfo {
            name: name.to_string(),
            is_remote,
            short_sha: parts[1].trim().to_string(),
            relative_date: parts[2].trim().to_string(),
        });
    }
    Ok(rows)
}

/// `git pull --ff-only`. Refuses to merge — if the upstream has
/// diverged the user gets the failure instead of a surprise merge
/// commit. Mirrors the C# build's `PullCommand` (Ctrl+Shift+F).
pub fn pull_ff_only(repo: &Path) -> Result<()> {
    run_git(repo, &["pull", "--ff-only"]).map(|_| ())
}

/// Drop every uncommitted change in the worktree: `git reset --hard
/// HEAD` resets tracked files, then `git clean -fd` removes untracked
/// files and directories. Destructive — callers must confirm with
/// the user first. Mirrors the C# build's `DiscardChangesCommand`.
pub fn discard_all_changes(repo: &Path) -> Result<()> {
    run_git(repo, &["reset", "--hard", "HEAD"])?;
    run_git(repo, &["clean", "-fd"])?;
    Ok(())
}

/// `git status --porcelain` — empty stdout means a clean worktree
/// (no staged, unstaged, or untracked changes), anything else
/// means dirty. Cheap enough to poll a couple of times per second
/// per worktree without blowing the I/O budget. Mirrors C#'s
/// `WorktreePoller.IsDirtyAsync`.
pub fn is_dirty(repo: &Path) -> Result<bool> {
    let output = run_git(repo, &["status", "--porcelain"])?;
    Ok(!output.stdout.is_empty())
}

/// `git fetch --all --prune`. Updates every remote and prunes the
/// **remote-tracking** refs (`refs/remotes/<remote>/*`) whose
/// upstream branches have been deleted. Local branches and tags
/// are left untouched — only the remote-tracking shadows go. Runs
/// at project scope (the primary worktree root); fetched refs are
/// shared across all worktrees of the project. Mirrors the C#
/// build's `FetchAllCommand`.
pub fn fetch_all_prune(repo: &Path) -> Result<()> {
    run_git(repo, &["fetch", "--all", "--prune"]).map(|_| ())
}

/// `git rebase <base_ref>` against the worktree at `repo`. Mirrors
/// the C# `GitService.RebaseOntoAsync` — the bare `git rebase` form
/// rather than `--onto`, because the C# build uses the simpler
/// "replay current branch on top of `<base_ref>`" semantics for the
/// "Rebase onto origin/<default>" sidebar action.
///
/// Returns the trimmed stdout on success (commit summary lines that
/// can surface in a toast). Failures usually mean conflicts the user
/// has to resolve manually in the worktree — the caller surfaces the
/// stderr verbatim because that's where git puts the conflict
/// breadcrumbs.
///
/// `base_ref` must not start with `-` — that would be interpreted
/// as a `git rebase` flag (e.g. `--abort`, `--continue`) and could
/// destroy in-progress rebase state. The ref ultimately comes from
/// `projects.json::default_branch`, which the user *can* edit, so we
/// reject up-front rather than trust the caller. Returns an `Err`
/// describing the rejection without spawning git.
pub fn rebase_onto(repo: &Path, base_ref: &str) -> Result<String> {
    if base_ref.starts_with('-') {
        return Err(anyhow!(
            "refusing to rebase onto a ref that starts with '-': {base_ref:?} \
             would be parsed as a git option, not a revision"
        ));
    }
    let output = run_git(repo, &["rebase", base_ref])?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}

/// `git config --get remote.origin.url`. Returns the trimmed URL
/// string, or `Ok(None)` when the remote isn't configured. Other
/// failure modes (not a git repo, unreadable config, …) bubble up
/// as `Err` so callers can distinguish "no origin" from "something
/// is wrong" — collapsing both into `Ok(None)` would mis-report
/// genuine errors as "no remote" and silently hide them.
///
/// Exit-code conventions: `git config --get` returns 1 specifically
/// for "key not found"; anything else is a real error.
pub fn remote_origin_url(repo: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| "spawn git config")?;
    match output.status.code() {
        Some(0) => {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if url.is_empty() { Ok(None) } else { Ok(Some(url)) }
        }
        // 1 = "the key was not found"; treat as "no origin".
        Some(1) => Ok(None),
        // Anything else (128 = not in a git directory, 129 = bad args,
        // …) is a real error worth surfacing.
        Some(code) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(anyhow!("git config --get remote.origin.url exited {code}: {stderr}"))
        }
        None => Err(anyhow!(
            "git config --get remote.origin.url terminated by signal"
        )),
    }
}

/// Convert a git remote URL (HTTPS or SSH) into a browser URL.
/// Returns `None` for shapes we don't recognise so the caller can
/// fall back to "open the URL as-is" or hide the action.
///
/// Handles:
/// - `https://github.com/owner/repo(.git)` → `https://github.com/owner/repo`
/// - `git@github.com:owner/repo(.git)` → `https://github.com/owner/repo`
/// - `ssh://git@github.com/owner/repo(.git)` → `https://github.com/owner/repo`
///
/// Mirrors the C# `GitService.NormaliseRemoteUrl` heuristic.
pub fn remote_url_to_browser(url: &str) -> Option<String> {
    let url = url.trim();
    let stripped = url.strip_suffix(".git").unwrap_or(url);
    if stripped.starts_with("https://") || stripped.starts_with("http://") {
        return Some(stripped.to_string());
    }
    if let Some(rest) = stripped.strip_prefix("git@") {
        // `host:owner/repo` → `https://host/owner/repo`
        let (host, path) = rest.split_once(':')?;
        return Some(format!("https://{host}/{path}"));
    }
    if let Some(rest) = stripped.strip_prefix("ssh://git@") {
        return Some(format!("https://{rest}"));
    }
    if let Some(rest) = stripped.strip_prefix("ssh://") {
        return Some(format!("https://{rest}"));
    }
    None
}

/// `git worktree remove <path>`. Set `force = true` to bypass dirty-
/// tree / locked checks. The C# build uses force=false in normal
/// flows and lets the user retry with force after seeing the error.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let path_str = path.to_string_lossy();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_str);
    run_git(repo, &args).map(|_| ())
}

/// Compute the short right-aligned status slug shown on a sidebar
/// worktree row.  Mirrors C# `WorktreeViewModel.StatusLabel`; the
/// `busy` (active agent) and `ci!` (failing PR CI) cases the C#
/// build supports aren't included here yet — the Rust port has no
/// per-tab session model and no PR integration. Returns:
///
/// * `"chg"` — working tree is dirty (numstat or untracked-only),
/// * `"↑N"` / `"↓N"` / `"↑N ↓N"` — clean but out of sync,
/// * `"idle"` — clean and in sync with upstream,
/// * `""`     — no upstream and not dirty (the C# build still
///              shows "idle"; we hide the label so a brand-new
///              standalone branch doesn't claim sync state).
pub fn worktree_status_label(status: &GitStatus) -> String {
    if status.added > 0 || status.removed > 0 || status.has_changes {
        return "chg".into();
    }
    if status.has_upstream {
        match (status.ahead, status.behind) {
            (0, 0) => "idle".into(),
            (a, 0) => format!("\u{2191}{a}"),
            (0, b) => format!("\u{2193}{b}"),
            (a, b) => format!("\u{2191}{a} \u{2193}{b}"),
        }
    } else {
        String::new()
    }
}

/// Snapshot of a worktree's git state at a point in time.
/// Produced by [`git_status`] and cached by the sidebar poller.
/// Mirrors the data the C# build's `WorktreePoller` gathers per
/// tick: branch name, line-level diff against HEAD, and
/// ahead/behind vs the upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// Short branch name — `"main"`, `"feat/csv"`, etc.
    /// Detached-HEAD worktrees get the 7-char short SHA instead
    /// (e.g. `"abc1234"`), matching the C# build's fallback.
    pub branch: String,
    /// Lines added relative to HEAD across all modified files
    /// (sum of the first column from `git diff --numstat HEAD`).
    pub added: u32,
    /// Lines removed relative to HEAD (second column, same source).
    pub removed: u32,
    /// `true` when the worktree is dirty but the numstat diff is
    /// empty — i.e. only untracked files are present. Lets the UI
    /// show a fallback "changes" label without a misleading `+0 -0`.
    pub has_changes: bool,
    /// Commits in HEAD not yet pushed to the upstream.
    pub ahead: u32,
    /// Commits on the upstream not yet merged locally.
    pub behind: u32,
    /// `false` when no upstream branch is configured — the
    /// ahead/behind segment should be hidden rather than showing
    /// `0 / 0`.
    pub has_upstream: bool,
}

/// Collect the three git status signals for a single worktree at
/// `path`. Returns `None` when the path is not a git repo (or
/// `git` is not on PATH); partial failures *within* the three
/// follow-up queries degrade gracefully (branch falls back to SHA;
/// numstat skips; ahead/behind hides).
///
/// Shells out to `git` four times *sequentially*: an early
/// `rev-parse --is-inside-work-tree` probe gates the rest, then
/// `symbolic-ref` (or `rev-parse --short HEAD` fallback for
/// detached HEAD), `diff --numstat HEAD`, and
/// `rev-list --left-right --count HEAD...@{u}`. Mirrors the
/// per-tick work the C# `WorktreePoller` does per worktree.
/// Typical total under 30 ms on local repos.
pub fn git_status(path: &str) -> Option<GitStatus> {
    let repo = std::path::Path::new(path);

    // ── 0. Early repo probe ────────────────────────────────────────
    // `rev-parse --is-inside-work-tree` exits 0 / prints "true" only
    // inside an actual worktree. Anything else (path doesn't exist,
    // not a git repo, git missing) → bail with `None` so callers can
    // distinguish "no data yet" from "valid repo but quiet".
    let probe = run_git(repo, &["rev-parse", "--is-inside-work-tree"]).ok()?;
    if !probe.status.success() {
        return None;
    }
    let probe_stdout = String::from_utf8_lossy(&probe.stdout);
    if probe_stdout.trim() != "true" {
        return None;
    }

    // ── 1. Current branch ──────────────────────────────────────────
    let branch = current_branch(repo);

    // ── 2. numstat diff against HEAD ───────────────────────────────
    let (added, removed, has_changes) = numstat_summary(repo);

    // ── 3. Ahead / behind upstream ─────────────────────────────────
    let (ahead, behind, has_upstream) = ahead_behind(repo);

    Some(GitStatus { branch, added, removed, has_changes, ahead, behind, has_upstream })
}

/// `git symbolic-ref --short HEAD` → short branch name.
/// Falls back to the 7-char short SHA on detached HEAD.
/// Returns `"?"` only if both git calls fail (not a repo, etc.).
fn current_branch(repo: &Path) -> String {
    // Happy path: attached HEAD.
    if let Ok(out) = run_git(repo, &["symbolic-ref", "--short", "HEAD"]) {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    // Detached HEAD: fall back to short SHA.
    if let Ok(out) = run_git(repo, &["rev-parse", "--short", "HEAD"]) {
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !sha.is_empty() {
            return sha;
        }
    }
    "?".to_string()
}

/// `git diff --numstat HEAD` → `(added, removed, has_changes)`.
///
/// `has_changes` is set when the worktree is dirty but the numstat
/// sum is zero — meaning only untracked files exist (git diff does
/// not count those, but git status --porcelain does).
fn numstat_summary(repo: &Path) -> (u32, u32, bool) {
    // Run numstat. On a clean repo this prints nothing; on a repo
    // with only untracked files it also prints nothing — we
    // distinguish the two cases below with is_dirty.
    let numstat_out = run_git(repo, &["diff", "--numstat", "HEAD"])
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let (added, removed) = parse_numstat(&numstat_out);

    // If numstat came back empty, check the dirty flag to see if
    // only untracked files are present.
    let has_changes = if added == 0 && removed == 0 {
        is_dirty(repo).unwrap_or(false)
    } else {
        true
    };

    (added, removed, has_changes)
}

/// Parse the text output of `git diff --numstat HEAD` into
/// `(total_added, total_removed)`.
///
/// Each line has the form `<added>\t<removed>\t<path>`.
/// Binary files emit `-\t-\t<path>` — we skip those rows since
/// there are no meaningful line counts.
///
/// Kept `pub(crate)` so the unit tests in this file can call it
/// directly without spawning a real `git` process.
pub(crate) fn parse_numstat(stdout: &str) -> (u32, u32) {
    let mut total_added: u32 = 0;
    let mut total_removed: u32 = 0;
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let added_str = parts.next().unwrap_or("");
        let removed_str = parts.next().unwrap_or("");
        // Binary files → "-" — skip.
        if added_str == "-" || removed_str == "-" {
            continue;
        }
        if let (Ok(a), Ok(r)) = (added_str.parse::<u32>(), removed_str.parse::<u32>()) {
            total_added = total_added.saturating_add(a);
            total_removed = total_removed.saturating_add(r);
        }
    }
    (total_added, total_removed)
}

/// `git rev-list --left-right --count HEAD...@{u}` →
/// `(ahead, behind, has_upstream)`.
///
/// Returns `(0, 0, false)` when no upstream is configured (exit
/// code 128 with "no upstream" in stderr). Any other error also
/// returns `false` for `has_upstream` so the UI hides the segment
/// rather than showing stale / wrong data.
fn ahead_behind(repo: &Path) -> (u32, u32, bool) {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{u}"])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return (0, 0, false),
    };

    if !output.status.success() {
        // Git returns 128 with "fatal: no upstream configured" when
        // there's no tracking branch. Treat all non-zero exits the
        // same: no upstream.
        return (0, 0, false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (ahead, behind) = parse_ahead_behind(stdout.trim());
    (ahead, behind, true)
}

/// Parse the single-line output of
/// `git rev-list --left-right --count HEAD...@{u}`:
/// `"<ahead>\t<behind>"`.
///
/// Kept `pub(crate)` for unit tests.
pub(crate) fn parse_ahead_behind(line: &str) -> (u32, u32) {
    let line = line.trim_end_matches('\r').trim();
    let mut parts = line.splitn(2, '\t');
    let ahead = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
    let behind = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
    (ahead, behind)
}

/// Run `git <args...>` in `cwd`. Returns the captured `Output` on
/// success; bubbles up stderr (trimmed) on non-zero exit.
fn run_git(cwd: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output.status.code().unwrap_or(-1);
        return Err(anyhow!("git {} exited {}: {}", args.join(" "), code, stderr));
    }
    Ok(output)
}

/// Parse the porcelain output from `git worktree list --porcelain`.
/// Format (per `git-worktree(1)`):
///
/// ```text
/// worktree /path/to/wt
/// HEAD <sha>
/// branch refs/heads/<name>   ← OR `bare` OR `detached`
/// [locked [reason]]
///
/// worktree /path/to/wt2
/// ...
/// ```
///
/// Stanzas are separated by blank lines. The first stanza is the
/// primary working tree.
pub fn parse_worktree_porcelain(stdout: &str) -> Vec<WorktreeInfo> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut head: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut locked = false;

    let flush = |path: &mut Option<String>,
                     head: &mut Option<String>,
                     branch: &mut Option<String>,
                     locked: &mut bool,
                     out: &mut Vec<WorktreeInfo>| {
        if let (Some(p), Some(h)) = (path.take(), head.take()) {
            let is_primary = out.is_empty();
            out.push(WorktreeInfo {
                path: p,
                head: h,
                branch: branch.take(),
                is_primary,
                locked: std::mem::replace(locked, false),
            });
        } else {
            // Reset if we hit a malformed stanza.
            path.take();
            head.take();
            branch.take();
            *locked = false;
        }
    };

    for raw in stdout.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            flush(&mut path, &mut head, &mut branch, &mut locked, &mut out);
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            head = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.trim_start_matches("refs/heads/").to_string());
        } else if line == "bare" || line == "detached" {
            // No branch — leave `branch` as None.
        } else if line == "locked" || line.starts_with("locked ") {
            locked = true;
        }
        // Unknown lines are ignored — porcelain v1 is documented to
        // be append-only, so a future field can show up here without
        // breaking us.
    }
    // Trailing stanza without a blank line.
    flush(&mut path, &mut head, &mut branch, &mut locked, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "worktree /home/me/repo\n\
HEAD abc1234\n\
branch refs/heads/main\n\
\n\
worktree /home/me/repo.worktrees/feat-x\n\
HEAD def5678\n\
branch refs/heads/feat-x\n\
locked\n\
\n\
worktree /home/me/repo.worktrees/detached\n\
HEAD 999aaaa\n\
detached\n";

    #[test]
    fn parses_primary_branch_and_detached() {
        let wts = parse_worktree_porcelain(SAMPLE);
        assert_eq!(wts.len(), 3);

        assert_eq!(wts[0].path, "/home/me/repo");
        assert_eq!(wts[0].head, "abc1234");
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(wts[0].is_primary);
        assert!(!wts[0].locked);

        assert_eq!(wts[1].branch.as_deref(), Some("feat-x"));
        assert!(!wts[1].is_primary);
        assert!(wts[1].locked);

        assert_eq!(wts[2].branch, None);
        assert!(!wts[2].is_primary);
    }

    #[test]
    fn empty_input_yields_no_rows() {
        assert!(parse_worktree_porcelain("").is_empty());
    }

    #[test]
    fn remote_url_browser_https_passthrough() {
        assert_eq!(
            remote_url_to_browser("https://github.com/foo/bar"),
            Some("https://github.com/foo/bar".into())
        );
        assert_eq!(
            remote_url_to_browser("https://github.com/foo/bar.git"),
            Some("https://github.com/foo/bar".into())
        );
    }

    #[test]
    fn remote_url_browser_ssh_short_form() {
        assert_eq!(
            remote_url_to_browser("git@github.com:foo/bar.git"),
            Some("https://github.com/foo/bar".into())
        );
        assert_eq!(
            remote_url_to_browser("git@gitlab.com:group/sub/repo.git"),
            Some("https://gitlab.com/group/sub/repo".into())
        );
    }

    #[test]
    fn remote_url_browser_ssh_long_form() {
        assert_eq!(
            remote_url_to_browser("ssh://git@github.com/foo/bar.git"),
            Some("https://github.com/foo/bar".into())
        );
    }

    #[test]
    fn remote_url_browser_unknown_returns_none() {
        assert!(remote_url_to_browser("file:///tmp/repo").is_none());
        assert!(remote_url_to_browser("/local/path").is_none());
    }

    #[test]
    fn malformed_stanza_is_dropped() {
        // Missing HEAD line → stanza dropped, but next valid stanza
        // still parses.
        let stdout = "worktree /broken\n\
\n\
worktree /good\n\
HEAD abc\n\
branch refs/heads/main\n";
        let wts = parse_worktree_porcelain(stdout);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].path, "/good");
        // First *successfully-parsed* stanza is primary.
        assert!(wts[0].is_primary);
    }

    #[test]
    fn unknown_lines_are_ignored() {
        let stdout = "worktree /repo\n\
HEAD abc\n\
branch refs/heads/main\n\
some-future-field foo bar\n";
        let wts = parse_worktree_porcelain(stdout);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].path, "/repo");
    }

    // ─── Integration tests against a real `git` binary ──────────────
    //
    // These exercise `add_worktree` / `list_worktrees` / `remove_worktree`
    // end-to-end against a freshly-initialised temp repo. Skipped (with
    // an `eprintln!` trace) when `git` isn't on PATH so a CI image
    // without git in scope still goes green — but in normal dev the
    // CLAUDE.md technology-decisions list makes git a hard dependency,
    // so these run on every local invocation.

    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    /// Initialise a fresh repo *inside a per-test tempdir* and produce
    /// one commit so HEAD points at a real ref. Both the repo and its
    /// `repo.worktrees/` sibling live under the same `TempDir`, so
    /// drop = full cleanup and parallel test runs can't collide on
    /// shared paths in the OS tempdir root. Uses `-c
    /// init.defaultBranch=main` so `worktree add -b feat <path>`
    /// doesn't trip on Git 2.28+'s "no default branch configured"
    /// warning.
    ///
    /// Returns the tempdir guard (drop = cleanup), the absolute repo
    /// path, and the absolute worktrees-root path (already created so
    /// `git worktree add` doesn't have to).
    fn init_repo() -> Option<(TempDir, std::path::PathBuf, std::path::PathBuf)> {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping: `git` not on PATH");
            return None;
        }
        let dir = tempfile::tempdir().ok()?;
        let repo = dir.path().join("repo");
        let worktrees_root = dir.path().join("repo.worktrees");
        std::fs::create_dir_all(&repo).ok()?;
        std::fs::create_dir_all(&worktrees_root).ok()?;
        run(&repo, &["-c", "init.defaultBranch=main", "init", "-q"]);
        // Identity is required for `commit` even with `--allow-empty`.
        run(&repo, &["config", "user.email", "test@example.invalid"]);
        run(&repo, &["config", "user.name", "Test"]);
        run(&repo, &["commit", "--allow-empty", "-m", "init", "-q"]);
        Some((dir, repo, worktrees_root))
    }

    fn run(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    #[test]
    fn rebase_onto_replays_commits_on_default_branch() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        // Branch off the seed commit, add one local commit, then move
        // `main` forward by one commit so the topology actually
        // requires a rebase. `rebase_onto(repo, "main")` should
        // replay `feat/x`'s commit on top of the new `main`.
        run(&repo, &["checkout", "-b", "feat/x", "-q"]);
        std::fs::write(repo.join("a.txt"), "feat\n").unwrap();
        run(&repo, &["add", "a.txt"]);
        run(&repo, &["commit", "-m", "feat", "-q"]);
        run(&repo, &["checkout", "main", "-q"]);
        run(&repo, &["commit", "--allow-empty", "-m", "main forward", "-q"]);
        run(&repo, &["checkout", "feat/x", "-q"]);

        rebase_onto(&repo, "main").expect("rebase succeeds in clean topology");

        // After the rebase `feat/x` should descend from the new main
        // tip — confirm by checking `git log main..feat/x` lists the
        // single feat commit.
        let output = Command::new("git")
            .args(["log", "--oneline", "main..feat/x"])
            .current_dir(&repo)
            .output()
            .expect("spawn git log");
        let log = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            log.lines().count(),
            1,
            "feat/x should be one commit ahead of main after rebase, got: {log:?}"
        );
    }

    #[test]
    fn rebase_onto_rejects_dash_prefixed_ref() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        let err = rebase_onto(&repo, "--abort").expect_err("must reject");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("starts with '-'"),
            "error should explain why; got: {msg}"
        );
    }

    #[test]
    fn add_worktree_then_list_includes_new_branch() {
        let Some((_guard, repo, wts_root)) = init_repo() else { return };
        let wt_path = wts_root.join("feat-x");

        add_worktree(&repo, &wt_path, "feat/x", None).expect("add");

        let wts = list_worktrees(&repo).expect("list");
        assert_eq!(wts.len(), 2, "primary + feat-x = 2");
        assert!(wts[0].is_primary);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));

        let feat = wts.iter().find(|w| w.branch.as_deref() == Some("feat/x"));
        assert!(feat.is_some(), "feat/x worktree should be listed");
        let feat = feat.unwrap();
        assert!(!feat.is_primary);
        // git canonicalises the path; just check that the leaf matches
        // — full-path comparison would fight Windows short-name vs
        // long-name, junctioned drives, and macOS `/private/var` vs
        // `/var` symlinks for tempdirs.
        assert!(feat.path.ends_with("feat-x"), "path: {}", feat.path);
    }

    #[test]
    fn remove_worktree_drops_it_from_list() {
        let Some((_guard, repo, wts_root)) = init_repo() else { return };
        let wt_path = wts_root.join("feat-y");
        add_worktree(&repo, &wt_path, "feat/y", None).expect("add");
        assert_eq!(list_worktrees(&repo).expect("list").len(), 2);

        remove_worktree(&repo, &wt_path, false).expect("remove");

        let wts = list_worktrees(&repo).expect("list after remove");
        assert_eq!(wts.len(), 1, "primary alone after remove");
        assert!(wts[0].is_primary);
    }

    #[test]
    fn list_branches_returns_local_and_remote_sorted() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        // Create a couple of locals and a fake remote so we can verify
        // both groups come back. `git update-ref` lets us mint a
        // `refs/remotes/origin/foo` without spinning up an actual
        // remote — the seed commit's SHA is fine to point it at.
        run(&repo, &["branch", "feat/x"]);
        run(&repo, &["branch", "feat/y"]);
        run(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run(&repo, &["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
        // Symbolic origin/HEAD — must be dropped from the result.
        run(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);

        let branches = list_branches(&repo).expect("list");
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        // Sorted alphabetically (case-insensitive). `origin/HEAD` is
        // gone. Locals + remotes interleave under one ordering.
        assert_eq!(
            names,
            vec!["feat/x", "feat/y", "main", "origin/feat/x", "origin/main"],
            "branches: {names:?}"
        );
        // Local vs remote flag matches the prefix.
        for b in &branches {
            assert_eq!(b.is_remote, b.name.starts_with("origin/"), "{b:?}");
            // Every row must have a non-empty short sha — without it
            // the dialog footer can't render `<sha> · <date>`.
            assert!(!b.short_sha.is_empty(), "{b:?}");
        }
    }

    #[test]
    fn list_branches_in_repo_with_only_main_returns_one_row() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        let branches = list_branches(&repo).expect("list");
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, "main");
        assert!(!branches[0].is_remote);
    }

    #[test]
    fn remote_origin_url_returns_none_when_unset() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        // `init_repo` doesn't configure a remote — this should be the
        // canonical "no origin" case.
        let result = remote_origin_url(&repo).expect("call ok");
        assert_eq!(result, None);
    }

    #[test]
    fn remote_origin_url_returns_configured_value() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        run(
            &repo,
            &[
                "config",
                "remote.origin.url",
                "git@github.com:foo/bar.git",
            ],
        );
        let result = remote_origin_url(&repo).expect("call ok");
        assert_eq!(result.as_deref(), Some("git@github.com:foo/bar.git"));
    }

    // Note: there's no integration test for the error path (e.g.
    // a corrupt config file) because reproducing one reliably
    // across hosts is awkward — `git config --get` outside a repo
    // can succeed with `Ok(None)` if global config is reachable but
    // doesn't have the key. The exit-code branching in the source
    // is small enough that the doc + visual review serves better
    // than a flaky integration test would.

    #[test]
    fn is_dirty_false_on_freshly_initialised_repo() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        assert!(!is_dirty(&repo).expect("call ok"));
    }

    #[test]
    fn is_dirty_true_after_creating_an_untracked_file() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        std::fs::write(repo.join("dirty.txt"), b"hello").expect("write");
        assert!(is_dirty(&repo).expect("call ok"));
    }

    #[test]
    fn is_dirty_true_after_modifying_tracked_file() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        // Commit a file first so it's tracked, then modify it.
        std::fs::write(repo.join("README"), b"v1").expect("write");
        run(&repo, &["add", "README"]);
        run(&repo, &["commit", "-m", "add README", "-q"]);
        assert!(!is_dirty(&repo).expect("clean after commit"));
        std::fs::write(repo.join("README"), b"v2").expect("write");
        assert!(is_dirty(&repo).expect("dirty after modify"));
    }

    #[test]
    fn discard_all_changes_resets_tracked_and_drops_untracked() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        // Track a file then modify it; create an untracked file too.
        std::fs::write(repo.join("tracked.txt"), b"v1").expect("write");
        run(&repo, &["add", "tracked.txt"]);
        run(&repo, &["commit", "-m", "add tracked", "-q"]);
        std::fs::write(repo.join("tracked.txt"), b"v2").expect("write");
        std::fs::write(repo.join("untracked.txt"), b"junk").expect("write");
        assert!(is_dirty(&repo).expect("dirty before discard"));

        discard_all_changes(&repo).expect("discard");

        assert!(!is_dirty(&repo).expect("clean after discard"));
        // tracked.txt back to v1
        let body = std::fs::read_to_string(repo.join("tracked.txt")).expect("read");
        assert_eq!(body, "v1");
        // untracked.txt gone
        assert!(!repo.join("untracked.txt").exists());
    }

    #[test]
    fn add_worktree_existing_branch_returns_stderr_error() {
        let Some((_guard, repo, wts_root)) = init_repo() else { return };
        let wt1 = wts_root.join("dup-1");
        let wt2 = wts_root.join("dup-2");
        add_worktree(&repo, &wt1, "feat/dup", None).expect("first add");

        let err = add_worktree(&repo, &wt2, "feat/dup", None)
            .expect_err("second add of same branch must fail");
        let msg = format!("{err:#}");
        // The message should contain git's actual stderr — without it
        // the UI dialog can't tell the user *why* the call failed.
        // We just check for substring matches that hold across git
        // versions ("already exists" / "fatal:") instead of pinning
        // to one phrasing.
        assert!(
            msg.contains("already exists") || msg.contains("fatal:"),
            "expected stderr to bubble up; got: {msg}"
        );
    }

    // ─── Unit tests for GitStatus parser helpers ─────────────────────
    //
    // These exercise the pure parsing logic against known output strings,
    // with no git process spawned. Mirrors the C# build's `GitServiceTests`
    // pattern of testing the parser in isolation.

    #[test]
    fn parse_numstat_empty_returns_zeroes() {
        assert_eq!(parse_numstat(""), (0, 0));
    }

    #[test]
    fn parse_numstat_single_file() {
        // Typical numstat line: added<TAB>removed<TAB>path
        assert_eq!(parse_numstat("3\t1\tsrc/foo.rs\n"), (3, 1));
    }

    #[test]
    fn parse_numstat_multiple_files_sums_correctly() {
        let input = "10\t2\tsrc/a.rs\n5\t3\tsrc/b.rs\n1\t0\tsrc/c.rs\n";
        assert_eq!(parse_numstat(input), (16, 5));
    }

    #[test]
    fn parse_numstat_skips_binary_files() {
        // Binary files emit "-" for both columns.
        let input = "-\t-\tassets/image.png\n4\t1\tsrc/main.rs\n";
        assert_eq!(parse_numstat(input), (4, 1));
    }

    #[test]
    fn parse_numstat_windows_crlf_lines() {
        // git on Windows can emit CRLF in stdout even with piped output.
        let input = "7\t2\tsrc/a.rs\r\n3\t1\tsrc/b.rs\r\n";
        assert_eq!(parse_numstat(input), (10, 3));
    }

    #[test]
    fn parse_numstat_ignores_malformed_lines() {
        // A line with no tabs should be silently skipped.
        let input = "no-tabs-here\n2\t1\tsrc/valid.rs\n";
        assert_eq!(parse_numstat(input), (2, 1));
    }

    #[test]
    fn parse_ahead_behind_zero_zero() {
        assert_eq!(parse_ahead_behind("0\t0"), (0, 0));
    }

    #[test]
    fn parse_ahead_behind_typical() {
        assert_eq!(parse_ahead_behind("3\t1"), (3, 1));
    }

    #[test]
    fn parse_ahead_behind_only_ahead() {
        assert_eq!(parse_ahead_behind("2\t0"), (2, 0));
    }

    #[test]
    fn parse_ahead_behind_only_behind() {
        assert_eq!(parse_ahead_behind("0\t4"), (0, 4));
    }

    #[test]
    fn parse_ahead_behind_crlf() {
        assert_eq!(parse_ahead_behind("1\t2\r"), (1, 2));
    }

    #[test]
    fn parse_ahead_behind_empty_falls_back_to_zero() {
        assert_eq!(parse_ahead_behind(""), (0, 0));
    }

    // ─── Integration tests for git_status against a real repo ────────

    #[test]
    fn git_status_clean_repo_returns_main_no_changes() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        let path_str = repo.to_string_lossy().to_string();
        let status = git_status(&path_str).expect("should return Some on a valid repo");

        assert_eq!(status.branch, "main", "branch");
        assert_eq!(status.added, 0, "added");
        assert_eq!(status.removed, 0, "removed");
        assert!(!status.has_changes, "has_changes on clean repo");
        // No upstream configured in init_repo → has_upstream = false.
        assert!(!status.has_upstream, "has_upstream without upstream");
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn git_status_untracked_only_sets_has_changes_flag() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        std::fs::write(repo.join("new.txt"), b"hello").expect("write");

        let path_str = repo.to_string_lossy().to_string();
        let status = git_status(&path_str).expect("Some");

        // numstat sees nothing (untracked), but dirty check fills the flag.
        assert_eq!(status.added, 0);
        assert_eq!(status.removed, 0);
        assert!(status.has_changes, "untracked file must set has_changes");
    }

    #[test]
    fn git_status_tracked_modification_shows_numstat() {
        let Some((_guard, repo, _wts)) = init_repo() else { return };
        // Commit a tracked file so we can measure a diff against HEAD.
        std::fs::write(repo.join("file.txt"), b"line1\nline2\n").expect("write");
        run(&repo, &["add", "file.txt"]);
        run(&repo, &["commit", "-m", "add file", "-q"]);

        // Overwrite with different content: 1 line added, 1 removed.
        std::fs::write(repo.join("file.txt"), b"line1\nnew_line\n").expect("write");

        let path_str = repo.to_string_lossy().to_string();
        let status = git_status(&path_str).expect("Some");

        // numstat diffs against HEAD — expect at least some activity.
        assert!(
            status.added > 0 || status.removed > 0,
            "expected non-zero diff; got +{} -{}", status.added, status.removed
        );
        assert!(status.has_changes, "has_changes with tracked modification");
    }

    #[test]
    fn git_status_non_repo_returns_none() {
        // Create a temp directory that's NOT a git repo. `git_status`
        // must return `None` so callers can distinguish "no data" from
        // "valid repo but no signal yet".
        let temp = std::env::temp_dir()
            .join(format!("codescope_git_status_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp).expect("create temp dir");

        // Sanity-check: temp is fresh and contains no .git directory.
        assert!(
            !temp.join(".git").exists(),
            "test precondition: temp dir must not be a git repo"
        );

        let result = git_status(temp.to_str().expect("temp path is utf-8"));

        // Cleanup before assertion so a failing assert still removes
        // the directory.
        let _ = std::fs::remove_dir_all(&temp);

        assert!(
            result.is_none(),
            "non-repo path should return None, got {:?}",
            result
        );
    }

    #[test]
    fn git_status_missing_path_returns_none() {
        // A path that doesn't exist at all. `rev-parse` exits non-zero
        // → `git_status` returns None.
        let result = git_status("C:\\does_not_exist_xyz_codescope_test_42");
        assert!(
            result.is_none(),
            "missing path should return None, got {:?}",
            result
        );
    }

    // --- worktree_status_label ---

    fn status(
        added: u32,
        removed: u32,
        has_changes: bool,
        ahead: u32,
        behind: u32,
        has_upstream: bool,
    ) -> GitStatus {
        GitStatus {
            branch: "main".into(),
            added,
            removed,
            has_changes,
            ahead,
            behind,
            has_upstream,
        }
    }

    #[test]
    fn status_label_clean_in_sync_is_idle() {
        let s = status(0, 0, false, 0, 0, true);
        assert_eq!(worktree_status_label(&s), "idle");
    }

    #[test]
    fn status_label_dirty_with_numstat_is_chg() {
        let s = status(3, 1, false, 0, 0, true);
        assert_eq!(worktree_status_label(&s), "chg");
    }

    #[test]
    fn status_label_untracked_only_is_chg() {
        let s = status(0, 0, true, 0, 0, true);
        assert_eq!(worktree_status_label(&s), "chg");
    }

    #[test]
    fn status_label_ahead_only() {
        let s = status(0, 0, false, 3, 0, true);
        assert_eq!(worktree_status_label(&s), "\u{2191}3");
    }

    #[test]
    fn status_label_behind_only() {
        let s = status(0, 0, false, 0, 5, true);
        assert_eq!(worktree_status_label(&s), "\u{2193}5");
    }

    #[test]
    fn status_label_both_ahead_and_behind() {
        let s = status(0, 0, false, 2, 7, true);
        assert_eq!(worktree_status_label(&s), "\u{2191}2 \u{2193}7");
    }

    #[test]
    fn status_label_no_upstream_is_blank_when_clean() {
        let s = status(0, 0, false, 0, 0, false);
        assert_eq!(worktree_status_label(&s), "");
    }

    #[test]
    fn status_label_dirty_wins_over_no_upstream() {
        let s = status(1, 0, false, 0, 0, false);
        assert_eq!(worktree_status_label(&s), "chg");
    }
}
