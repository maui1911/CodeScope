//! Cleanup (ADR-0027): once the pull request of a dispatch is merged or its issue is closed,
//! CodeScope removes its worktree and branch, as long as nothing there is its own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::ItemState;
use super::pull_request::{PullRequest, PullRequestState};
use super::dispatch::{fetch_branch_head, fetch_pull_head, remote_branch_tip, remote_default_branch};
use crate::git::{list_worktrees, remove_worktree, run_git};
use crate::projects::{PathOnDisk, worktree_is_listed};

const FOLDER_TRIES: u32 = 20;
const FOLDER_RETRY: Duration = Duration::from_millis(500);

/// What ended the work of a dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Its pull request `number` was merged with this `head`.
    Merged { number: u32, head: String },
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
            Some(Ended::Merged {
                number,
                head: pr.head_oid.clone(),
            })
        }
        Some(pr) if pr.state == PullRequestState::Open => None,
        _ => closed,
    }
}

/// Where a dispatch works: its worktree of the project, on its branch, with the item's
/// repository as one of the project's remotes.
#[derive(Debug, Clone)]
pub struct Dispatched {
    /// The project's own folder.
    pub repo: PathBuf,
    /// The worktree.
    pub path: PathBuf,
    /// Its local branch.
    pub branch: String,
    /// The project's remote for the item's repository.
    pub remote: String,
    /// The commit CodeScope made the branch at on the remote; set when it did.
    pub linked_at: Option<String>,
}

/// What the worktree or its branch holds that `ended` doesn't keep elsewhere; `None` when
/// nothing. A merged head or default branch that isn't known here is fetched first.
pub fn kept(d: &Dispatched, ended: &Ended) -> Result<Option<Kept>> {
    let worktree = !git_let_go(d)?;
    if worktree && has_changes(&d.path)? {
        return Ok(Some(Kept::Changes));
    }
    let head = worktree.then(|| rev_parse(&d.path, "HEAD")).transpose()?;
    let mut tips: Vec<String> = head.into_iter().chain(branch_tip(d)?).collect();
    tips.dedup();
    for tip in &tips {
        if !held(d, tip, ended)? {
            return Ok(Some(Kept::OwnCommits));
        }
    }
    Ok(None)
}

fn git_let_go(d: &Dispatched) -> Result<bool> {
    if d.path.join(".git").exists() {
        return Ok(false);
    }
    let path = d.path.to_string_lossy();
    let listed = list_worktrees(&d.repo)?;
    Ok(!worktree_is_listed(&listed, &path, Some(&PathOnDisk::probe(&path))))
}

fn has_changes(path: &Path) -> Result<bool> {
    let status = ["status", "--porcelain", "--untracked-files=all", "--ignore-submodules=none"];
    Ok(!run_git(path, &status)?.stdout.is_empty())
}

fn rev_parse(path: &Path, rev: &str) -> Result<String> {
    let output = run_git(path, &["rev-parse", "--verify", rev])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn branch_tip(d: &Dispatched) -> Result<Option<String>> {
    let name = format!("refs/heads/{}", d.branch);
    let output = run_git(&d.repo, &["for-each-ref", "--format=%(refname) %(objectname)", &name])?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().find_map(|line| {
        let (refname, tip) = line.split_once(' ')?;
        (refname == name).then(|| tip.to_string())
    }))
}

fn held(d: &Dispatched, tip: &str, ended: &Ended) -> Result<bool> {
    let repo = d.repo.as_path();
    if let Ended::Merged { number, head } = ended {
        if !has_commit(repo, head) {
            fetch_pull_head(repo, &d.remote, *number)?;
        }
        if tip == head || is_ancestor(repo, tip, head) {
            return Ok(true);
        }
    }
    let known = format!("refs/remotes/{}/HEAD", d.remote);
    if has_commit(repo, &known) && is_ancestor(repo, tip, &known) {
        return Ok(true);
    }
    let default_branch = remote_default_branch(repo, &d.remote)?;
    let fetched = fetch_branch_head(repo, &d.remote, &default_branch)?;
    Ok(is_ancestor(repo, tip, &fetched))
}

fn has_commit(repo: &Path, rev: &str) -> bool {
    run_git(repo, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_ok()
}

fn is_ancestor(repo: &Path, commit: &str, of: &str) -> bool {
    run_git(repo, &["merge-base", "--is-ancestor", commit, of]).is_ok()
}

/// Remove the worktree and its branch once [`kept`] finds nothing; otherwise remove nothing and
/// say what it found. A folder git let go of but Windows still holds gets 10 s to come free.
pub fn clean_up(d: &Dispatched, ended: &Ended) -> Result<Option<Kept>> {
    if let Some(found) = kept(d, ended)? {
        return Ok(Some(found));
    }
    if let Err(err) = remove_worktree(&d.repo, &d.path, false)
        && !git_let_go(d)?
    {
        return Err(err);
    }
    if branch_tip(d)?.is_some()
        && let Err(err) = run_git(&d.repo, &["branch", "-D", &d.branch])
    {
        let branch = &d.branch;
        eprintln!("warning: removed the worktree of {branch} but kept the branch: {err:#}");
    }
    remove_folder(&d.path)?;
    Ok(None)
}

/// Remove the branch CodeScope made on the remote while it is still at [`Dispatched::linked_at`];
/// nothing when it moved, is gone, or wasn't made here.
pub fn remove_linked_branch(d: &Dispatched) -> Result<()> {
    let Some(linked_at) = &d.linked_at else {
        return Ok(());
    };
    if remote_branch_tip(&d.repo, &d.remote, &d.branch)?.as_ref() != Some(linked_at) {
        return Ok(());
    }
    let reference = format!("refs/heads/{}", d.branch);
    let lease = format!("--force-with-lease={reference}:{linked_at}");
    let delete = format!(":{reference}");
    run_git(&d.repo, &["push", "--quiet", &lease, &d.remote, &delete]).map(|_| ())
}

fn remove_folder(path: &Path) -> Result<()> {
    for tries in 1.. {
        let removed = std::fs::remove_dir_all(path);
        if !path.exists() {
            break;
        }
        if tries == FOLDER_TRIES {
            let folder = path.display();
            let why = removed.err().map_or("it came back".to_string(), |err| err.to_string());
            return Err(anyhow!("git let go of {folder}, but the folder stays: {why}"));
        }
        std::thread::sleep(FOLDER_RETRY);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let at_c9 = Some(Ended::Merged {
            number: 454,
            head: "c9".into(),
        });
        assert_eq!(ended(Open, None, None), None, "research, item still open");
        assert_eq!(ended(Closed, None, None), Some(Ended::Closed), "closed after research");
        assert_eq!(ended(Closed, Some(454), None), None, "its pull request isn't read yet");
        assert_eq!(
            ended(Open, Some(454), Some(Some(&merged))),
            at_c9,
            "merged into another branch leaves the issue open"
        );
        assert_eq!(ended(Merged, Some(454), Some(Some(&merged))), at_c9);
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
        dir: tempfile::TempDir,
        upstream: PathBuf,
        dispatched: Dispatched,
    }

    fn dispatched() -> Option<Repos> {
        dispatched_from("main")
    }

    fn dispatched_from(default_branch: &str) -> Option<Repos> {
        no_window_command("git").arg("--version").output().ok()?;
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        let init = format!("init.defaultBranch={default_branch}");
        git(&upstream, &["-c", &init, "init", "-q"]);
        git(&upstream, &["config", "user.email", "test@example.invalid"]);
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["commit", "--allow-empty", "-m", "one", "-q"]);
        git(dir.path(), &["clone", "-q", "upstream", "repo"]);
        let repo = dir.path().join("repo");
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        let path = dir.path().join("repo.worktrees").join("456-clean-up");
        let path_arg = path.to_string_lossy().to_string();
        let base = format!("origin/{default_branch}");
        git(&repo, &["worktree", "add", "-q", "-b", "456-clean-up", &path_arg, &base]);
        let exclude = repo.join(".git").join("info").join("exclude");
        std::fs::write(&exclude, "/.codescope/\n").unwrap();
        std::fs::create_dir_all(path.join(".codescope")).unwrap();
        std::fs::write(path.join(".codescope").join("dispatch.md"), "# Task\n").unwrap();
        let dispatched = Dispatched {
            repo,
            path,
            branch: "456-clean-up".into(),
            remote: "origin".into(),
            linked_at: None,
        };
        Some(Repos {
            dir,
            upstream,
            dispatched,
        })
    }

    fn commit(path: &Path, message: &str) -> String {
        git(path, &["commit", "--allow-empty", "-q", "-m", message]);
        git(path, &["rev-parse", "HEAD"])
    }

    fn merged(head: &str) -> Ended {
        Ended::Merged {
            number: 7,
            head: head.into(),
        }
    }

    fn has_branch(d: &Dispatched) -> bool {
        !git(&d.repo, &["branch", "--list", &d.branch]).is_empty()
    }

    #[test]
    fn a_worktree_keeps_what_the_merge_does_not_hold() {
        let Some(repos) = dispatched() else { return };
        let d = &repos.dispatched;
        let path = &d.path;
        let head = commit(path, "the pull request");
        assert_eq!(kept(d, &merged(&head)).unwrap(), None, "the handover files are excluded");
        std::fs::write(path.join("notes.txt"), "mine").unwrap();
        assert_eq!(kept(d, &merged(&head)).unwrap(), Some(Kept::Changes));
        git(path, &["config", "status.showUntrackedFiles", "no"]);
        assert_eq!(
            kept(d, &merged(&head)).unwrap(),
            Some(Kept::Changes),
            "untracked files that git status is set to hide"
        );
        std::fs::remove_file(path.join("notes.txt")).unwrap();
        commit(path, "after the merge");
        assert_eq!(kept(d, &merged(&head)).unwrap(), Some(Kept::OwnCommits));
        git(path, &["reset", "-q", "--hard", &head]);
        let later = commit(path, "pushed from elsewhere");
        git(path, &["reset", "-q", "--hard", &head]);
        assert_eq!(kept(d, &merged(&later)).unwrap(), None, "behind the merged head");
        let unknown = merged("0123456789012345678901234567890123456789");
        assert!(kept(d, &unknown).is_err(), "a head that can't be fetched");
    }

    #[test]
    fn a_branch_keeps_its_own_commits_wherever_the_worktree_s_head_is() {
        let Some(repos) = dispatched() else { return };
        let d = &repos.dispatched;
        let head = commit(&d.path, "the pull request");
        commit(&d.path, "after the merge");
        git(&d.path, &["switch", "-q", "--detach", &head]);
        assert_eq!(kept(d, &merged(&head)).unwrap(), Some(Kept::OwnCommits));
        assert_eq!(clean_up(d, &merged(&head)).unwrap(), Some(Kept::OwnCommits));
        assert!(has_branch(d));
    }

    #[test]
    fn a_merged_head_made_on_github_is_fetched_before_it_counts() {
        let Some(repos) = dispatched() else { return };
        let d = &repos.dispatched;
        let pushed = commit(&d.path, "the pull request");
        git(&d.path, &["push", "-q", "origin", "HEAD:refs/heads/456-clean-up"]);
        let tree = format!("{pushed}^{{tree}}");
        let update = ["commit-tree", &tree, "-p", &pushed, "-m", "Update branch"];
        let updated = git(&repos.upstream, &update);
        git(&repos.upstream, &["update-ref", "refs/pull/7/head", &updated]);
        assert_eq!(kept(d, &merged(&updated)).unwrap(), None);
    }

    #[test]
    fn a_closed_dispatch_keeps_commits_the_remote_s_default_branch_does_not_hold() {
        let Some(repos) = dispatched_from("trunk") else { return };
        let d = &repos.dispatched;
        assert_eq!(kept(d, &Ended::Closed).unwrap(), None, "research only");
        git(&d.repo, &["remote", "set-head", "origin", "-d"]);
        assert_eq!(kept(d, &Ended::Closed).unwrap(), None, "the remote names its default");
        commit(&d.path, "work");
        assert_eq!(kept(d, &Ended::Closed).unwrap(), Some(Kept::OwnCommits));
        git(&d.path, &["push", "-q", "origin", "HEAD:refs/heads/elsewhere"]);
        git(&repos.upstream, &["merge", "-q", "--ff-only", "elsewhere"]);
        assert_eq!(kept(d, &Ended::Closed).unwrap(), None, "merged by another pull request");
    }

    #[test]
    fn cleaning_up_removes_the_worktree_and_its_branch_or_nothing() {
        let Some(repos) = dispatched() else { return };
        let d = &repos.dispatched;
        let head = commit(&d.path, "the pull request");
        std::fs::write(d.path.join("notes.txt"), "mine").unwrap();
        assert_eq!(clean_up(d, &merged(&head)).unwrap(), Some(Kept::Changes));
        assert!(d.path.exists());
        std::fs::remove_file(d.path.join("notes.txt")).unwrap();
        assert_eq!(clean_up(d, &merged(&head)).unwrap(), None);
        assert!(!d.path.exists());
        assert!(!has_branch(d));
        let listed = git(&d.repo, &["worktree", "list", "--porcelain"]);
        assert_eq!(listed.matches("worktree ").count(), 1);
    }

    fn on_the_remote(repos: &Repos) -> Option<String> {
        let branch = format!("refs/heads/{}", repos.dispatched.branch);
        let found = git(&repos.upstream, &["for-each-ref", "--format=%(objectname)", &branch]);
        (!found.is_empty()).then_some(found)
    }

    #[test]
    fn the_branch_it_made_on_the_remote_goes_while_nothing_was_pushed_to_it() {
        let Some(mut repos) = dispatched() else { return };
        let base = git(&repos.dispatched.path, &["rev-parse", "HEAD"]);
        git(&repos.upstream, &["branch", &repos.dispatched.branch, &base]);
        repos.dispatched.linked_at = Some(base.clone());
        assert_eq!(clean_up(&repos.dispatched, &Ended::Closed).unwrap(), None);
        assert_eq!(on_the_remote(&repos), Some(base), "cleaning up leaves the remote alone");
        remove_linked_branch(&repos.dispatched).unwrap();
        assert_eq!(on_the_remote(&repos), None);
        remove_linked_branch(&repos.dispatched).unwrap();
    }

    #[test]
    fn a_branch_on_the_remote_stays_when_it_moved_or_wasn_t_made_here() {
        let Some(repos) = dispatched() else { return };
        let base = git(&repos.dispatched.path, &["rev-parse", "HEAD"]);
        let branch = repos.dispatched.branch.clone();
        git(&repos.upstream, &["branch", &branch, &base]);
        assert_eq!(clean_up(&repos.dispatched, &Ended::Closed).unwrap(), None);
        remove_linked_branch(&repos.dispatched).unwrap();
        assert_eq!(on_the_remote(&repos), Some(base), "not made by CodeScope");

        let Some(mut repos) = dispatched() else { return };
        let base = git(&repos.dispatched.path, &["rev-parse", "HEAD"]);
        let tree = format!("{base}^{{tree}}");
        let pushed = git(&repos.upstream, &["commit-tree", "-p", &base, "-m", "pushed", &tree]);
        git(&repos.upstream, &["branch", &branch, &pushed]);
        repos.dispatched.linked_at = Some(base);
        assert_eq!(clean_up(&repos.dispatched, &Ended::Closed).unwrap(), None);
        remove_linked_branch(&repos.dispatched).unwrap();
        assert_eq!(on_the_remote(&repos), Some(pushed), "someone pushed to it");
    }

    fn link_folder(target: &Path, link: &Path) {
        #[cfg(windows)]
        {
            let made = no_window_command("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(made.success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[test]
    fn a_worktree_git_refuses_to_remove_stays_whatever_path_leads_to_it() {
        let Some(repos) = dispatched() else { return };
        let real = &repos.dispatched;
        let head = commit(&real.path, "the pull request");
        git(&real.repo, &["worktree", "lock", &real.path.to_string_lossy()]);
        let link = repos.dir.path().join("linked.worktrees");
        link_folder(&repos.dir.path().join("repo.worktrees"), &link);
        let d = Dispatched {
            path: link.join("456-clean-up"),
            ..real.clone()
        };
        assert!(clean_up(&d, &merged(&head)).is_err());
        assert!(real.path.join(".git").exists());
        assert!(has_branch(real));
    }

    #[test]
    fn a_folder_git_let_go_of_goes_on_a_later_try() {
        let Some(repos) = dispatched() else { return };
        let d = &repos.dispatched;
        let head = commit(&d.path, "the pull request");
        std::fs::remove_file(d.path.join(".git")).unwrap();
        git(&d.repo, &["worktree", "prune"]);
        assert_eq!(kept(d, &merged(&head)).unwrap(), None);
        assert_eq!(clean_up(d, &merged(&head)).unwrap(), None);
        assert!(!d.path.exists());
        assert!(!has_branch(d));
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_still_in_use_goes_once_it_is_free() {
        let Some(repos) = dispatched() else { return };
        let d = &repos.dispatched;
        let head = commit(&d.path, "the pull request");
        let mut shell = no_window_command("cmd")
            .args(["/c", "ping", "-n", "3", "127.0.0.1"])
            .current_dir(&d.path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert_eq!(clean_up(d, &merged(&head)).unwrap(), None);
        shell.wait().unwrap();
        assert!(!d.path.exists());
        assert!(!has_branch(d));
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
            linked_at: None,
        };
        assert!(!serde_json::to_string(&row).unwrap().contains("kept"));
        row.kept = Some(Kept::OwnCommits);
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.ends_with(r#""kept":"ownCommits"}"#), "{json}");
        assert_eq!(serde_json::from_str::<WorktreeDispatch>(&json).unwrap(), row);
    }
}
