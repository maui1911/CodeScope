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
}
