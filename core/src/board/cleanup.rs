//! Cleanup (ADR-0027): once the pull request of a dispatch is merged or its issue is closed,
//! CodeScope removes its worktree and branch, as long as nothing there is its own.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::ItemState;
use super::pull_request::{PullRequest, PullRequestState};
use crate::git::{is_dirty, list_worktrees, remove_worktree, run_git};
use crate::path_canon::paths_match;

const FOLDER_TRIES: u32 = 20;
const FOLDER_RETRY: Duration = Duration::from_millis(500);

/// What ended the work of a dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Its pull request was merged with this head.
    Merged(String),
    /// Its issue or pull request was closed without that.
    Closed,
}

/// Why a dispatch's worktree stays after its work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kept {
    /// Uncommitted changes or untracked files.
    Changes,
    /// Commits that neither the merged head nor the default branch holds.
    OwnCommits,
}

/// How the work of a dispatch ended; `None` while it goes on. `item` is its issue or pull
/// request on the board, `own` the pull request it opened or works on, and `read` what the last
/// read of that found (`None` before one).
pub fn ended(
    item: ItemState,
    own: Option<u32>,
    read: Option<Option<&PullRequest>>,
) -> Option<Ended> {
    let closed = (item != ItemState::Open).then_some(Ended::Closed);
    let Some(number) = own else {
        return closed;
    };
    match read? {
        Some(pr) if pr.number == number && pr.state == PullRequestState::Merged => {
            Some(Ended::Merged(pr.head_oid.clone()))
        }
        Some(pr) if pr.state == PullRequestState::Open => None,
        _ => closed,
    }
}

/// What the worktree at `path` holds that `ended` doesn't keep elsewhere; `None` when nothing.
/// `default_branch` is the project's, as a local or remote-tracking branch.
pub fn kept(path: &Path, ended: &Ended, default_branch: &str) -> Result<Option<Kept>> {
    if is_dirty(path)? {
        return Ok(Some(Kept::Changes));
    }
    let tip = rev_parse(path, "HEAD")?;
    let held = match ended {
        Ended::Merged(head) => tip == *head || is_ancestor(path, &tip, head),
        Ended::Closed => default_tips(path, default_branch)?
            .iter()
            .any(|default| is_ancestor(path, &tip, default)),
    };
    Ok((!held).then_some(Kept::OwnCommits))
}

fn rev_parse(path: &Path, rev: &str) -> Result<String> {
    let output = run_git(path, &["rev-parse", "--verify", rev])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_ancestor(path: &Path, commit: &str, of: &str) -> bool {
    run_git(path, &["merge-base", "--is-ancestor", commit, of]).is_ok()
}

fn default_tips(path: &Path, default_branch: &str) -> Result<Vec<String>> {
    let local = format!("refs/heads/{default_branch}");
    let remote = format!("refs/remotes/*/{default_branch}");
    let output = run_git(path, &["for-each-ref", "--format=%(objectname)", &local, &remote])?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().map(String::from).collect())
}

/// Remove the worktree at `path` from `repo`, and its `branch`, once [`kept`] finds nothing;
/// otherwise remove nothing and say what it found. A folder that is still in use, which Windows
/// won't delete, gets 10 s to come free.
pub fn clean_up(
    repo: &Path,
    path: &Path,
    branch: &str,
    ended: &Ended,
    default_branch: &str,
) -> Result<Option<Kept>> {
    if let Some(found) = kept(path, ended, default_branch)? {
        return Ok(Some(found));
    }
    let removed = remove_worktree(repo, path, false);
    let listed = || -> Result<bool> {
        let wanted = path.to_string_lossy();
        Ok(list_worktrees(repo)?.iter().any(|w| paths_match(&w.path, &wanted)))
    };
    if let Err(err) = removed
        && listed()?
    {
        return Err(err);
    }
    if branch != default_branch
        && let Err(err) = run_git(repo, &["branch", "-D", branch])
    {
        eprintln!("warning: removed the worktree of {branch} but kept the branch: {err:#}");
    }
    remove_folder(path)?;
    Ok(None)
}

fn remove_folder(path: &Path) -> Result<()> {
    let mut tries = 0;
    while path.exists() {
        match std::fs::remove_dir_all(path) {
            Err(err) if tries + 1 >= FOLDER_TRIES => {
                let folder = path.display();
                return Err(anyhow!("git let go of {folder}, but the folder stays: {err}"));
            }
            Err(_) => {
                tries += 1;
                std::thread::sleep(FOLDER_RETRY);
            }
            Ok(()) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::board::pull_request::{Checks, Health};
    use crate::process::no_window_command;

    fn pr(number: u32, state: PullRequestState, head: &str) -> PullRequest {
        PullRequest {
            id: format!("PR_{number}"),
            number,
            url: format!("https://github.com/maui1911/CodeScope/pull/{number}"),
            state,
            is_draft: false,
            checks: Checks::Passing,
            head_oid: head.into(),
            health: Health::default(),
            comments: Vec::new(),
        }
    }

    #[test]
    fn a_dispatch_ends_with_the_merge_of_its_own_pull_request_or_a_close() {
        use ItemState::{Closed, Merged, Open};
        use PullRequestState::{Merged as PrMerged, Open as PrOpen};
        let merged = pr(454, PrMerged, "c9");
        assert_eq!(ended(Open, None, None), None, "research, item still open");
        assert_eq!(ended(Closed, None, None), Some(Ended::Closed), "closed after research");
        assert_eq!(ended(Closed, Some(454), None), None, "its pull request isn't read yet");
        assert_eq!(
            ended(Open, Some(454), Some(Some(&merged))),
            Some(Ended::Merged("c9".into())),
            "merged into another branch leaves the issue open"
        );
        assert_eq!(ended(Merged, Some(454), Some(Some(&merged))), Some(Ended::Merged("c9".into())));
        let older = pr(301, PrMerged, "c1");
        assert_eq!(ended(Open, Some(454), Some(Some(&older))), None, "an older pull request");
        assert_eq!(ended(Closed, Some(454), Some(Some(&older))), Some(Ended::Closed));
        let open = pr(454, PrOpen, "c9");
        assert_eq!(ended(Closed, Some(454), Some(Some(&open))), None, "its pull request is open");
        assert_eq!(ended(Closed, Some(454), Some(None)), Some(Ended::Closed), "closed unmerged");
        assert_eq!(ended(Open, Some(454), Some(None)), None);
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = no_window_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    struct Repos {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        path: PathBuf,
    }

    fn dispatched() -> Option<Repos> {
        no_window_command("git").arg("--version").output().ok()?;
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(&upstream, &["config", "user.email", "test@example.invalid"]);
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["commit", "--allow-empty", "-m", "one", "-q"]);
        git(dir.path(), &["clone", "-q", "upstream", "repo"]);
        let repo = dir.path().join("repo");
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        let path = dir.path().join("repo.worktrees").join("456-clean-up");
        let path_arg = path.to_string_lossy().to_string();
        git(&repo, &["worktree", "add", "-q", "-b", "456-clean-up", &path_arg, "origin/main"]);
        let exclude = repo.join(".git").join("info").join("exclude");
        std::fs::write(&exclude, "/.codescope/\n").unwrap();
        std::fs::create_dir_all(path.join(".codescope")).unwrap();
        std::fs::write(path.join(".codescope").join("dispatch.md"), "# Task\n").unwrap();
        Some(Repos { _dir: dir, repo, path })
    }

    fn commit(path: &Path, message: &str) -> String {
        git(path, &["commit", "--allow-empty", "-q", "-m", message]);
        git(path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn a_worktree_keeps_what_the_merge_does_not_hold() {
        let Some(repos) = dispatched() else { return };
        let path = &repos.path;
        let head = commit(path, "the pull request");
        let merged = Ended::Merged(head.clone());
        assert_eq!(kept(path, &merged, "main").unwrap(), None, "the handover files are excluded");
        std::fs::write(path.join("notes.txt"), "mine").unwrap();
        assert_eq!(kept(path, &merged, "main").unwrap(), Some(Kept::Changes));
        std::fs::remove_file(path.join("notes.txt")).unwrap();
        commit(path, "after the merge");
        assert_eq!(kept(path, &merged, "main").unwrap(), Some(Kept::OwnCommits));
        git(path, &["reset", "-q", "--hard", &head]);
        commit(path, "pushed from elsewhere");
        let later = Ended::Merged(git(path, &["rev-parse", "HEAD"]));
        git(path, &["reset", "-q", "--hard", &head]);
        assert_eq!(kept(path, &later, "main").unwrap(), None, "behind the merged head");
        let unknown = Ended::Merged("0123456789012345678901234567890123456789".into());
        assert_eq!(kept(path, &unknown, "main").unwrap(), Some(Kept::OwnCommits));
    }

    #[test]
    fn a_closed_dispatch_keeps_commits_the_default_branch_does_not_hold() {
        let Some(repos) = dispatched() else { return };
        let path = &repos.path;
        assert_eq!(kept(path, &Ended::Closed, "main").unwrap(), None, "research only");
        commit(path, "work");
        assert_eq!(kept(path, &Ended::Closed, "main").unwrap(), Some(Kept::OwnCommits));
    }

    #[test]
    fn cleaning_up_removes_the_worktree_and_its_branch_or_nothing() {
        let Some(repos) = dispatched() else { return };
        let (repo, path) = (&repos.repo, &repos.path);
        let head = commit(path, "the pull request");
        let merged = Ended::Merged(head);
        std::fs::write(path.join("notes.txt"), "mine").unwrap();
        let found = clean_up(repo, path, "456-clean-up", &merged, "main").unwrap();
        assert_eq!(found, Some(Kept::Changes));
        assert!(path.exists());
        std::fs::remove_file(path.join("notes.txt")).unwrap();
        assert_eq!(clean_up(repo, path, "456-clean-up", &merged, "main").unwrap(), None);
        assert!(!path.exists());
        assert_eq!(git(repo, &["branch", "--list", "456-clean-up"]), "");
        assert_eq!(git(repo, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_still_in_use_goes_once_it_is_free() {
        let Some(repos) = dispatched() else { return };
        let (repo, path) = (&repos.repo, &repos.path);
        let merged = Ended::Merged(commit(path, "the pull request"));
        let mut shell = no_window_command("cmd")
            .args(["/c", "ping", "-n", "3", "127.0.0.1"])
            .current_dir(path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert_eq!(clean_up(repo, path, "456-clean-up", &merged, "main").unwrap(), None);
        shell.wait().unwrap();
        assert!(!path.exists());
        assert_eq!(git(repo, &["branch", "--list", "456-clean-up"]), "");
    }

    #[test]
    fn a_kept_worktree_says_why_on_its_row() {
        use crate::board::ItemKind;
        use crate::board::dispatch::{Round, WorktreeDispatch};
        let mut row = WorktreeDispatch {
            board_url: "https://github.com/users/maui1911/projects/3".into(),
            item_id: "PVTI_1".into(),
            content_id: "I_456".into(),
            kind: ItemKind::Issue,
            repository: "maui1911/CodeScope".into(),
            number: 456,
            title: "Clean up".into(),
            url: "https://github.com/maui1911/CodeScope/issues/456".into(),
            round: Round::Implementation,
            trigger_at: "2026-11-27T09:00:00Z".into(),
            findings: None,
            pull_request: None,
            review: None,
            fork: None,
            repair: None,
            forwarded: None,
            kept: None,
        };
        assert!(!serde_json::to_string(&row).unwrap().contains("kept"));
        row.kept = Some(Kept::OwnCommits);
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.ends_with(r#""kept":"ownCommits"}"#), "{json}");
        assert_eq!(serde_json::from_str::<WorktreeDispatch>(&json).unwrap(), row);
    }
}
