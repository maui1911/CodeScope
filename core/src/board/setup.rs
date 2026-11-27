//! The setup command (ADR-0027): a dispatch's new worktree runs the repository's own setup
//! script, as the remote's default branch has it, before its session gets the task.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use super::dispatch::{fetch_branch_head, remote_default_branch};
use crate::git::run_git;
use crate::process::no_window_command;

/// The setup script this OS runs, relative to the repository.
#[cfg(windows)]
pub const SETUP_SCRIPT: &str = ".codescope/setup.ps1";
/// The setup script this OS runs, relative to the repository.
#[cfg(not(windows))]
pub const SETUP_SCRIPT: &str = ".codescope/setup.sh";
/// Where the setup's output goes, relative to the worktree.
pub const SETUP_LOG: &str = ".codescope/setup.log";

const SETUP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SETUP_POLL: Duration = Duration::from_millis(200);
const TAIL_LINES: usize = 2;

/// Run the setup script of `remote`'s default branch in the new `worktree` of `repo`, with its
/// output in [`SETUP_LOG`]; nothing when that branch has none. An error says why it failed.
pub fn run_setup(repo: &Path, worktree: &Path, remote: &str) -> Result<()> {
    run_setup_within(repo, worktree, remote, SETUP_TIMEOUT)
}

fn run_setup_within(repo: &Path, worktree: &Path, remote: &str, limit: Duration) -> Result<()> {
    let default_branch = remote_default_branch(repo, remote)?;
    let base = fetch_branch_head(repo, remote, &default_branch)?;
    let Some(script) = script_at(repo, &base)? else {
        return Ok(());
    };
    let copy = script_copy(worktree)?;
    std::fs::write(&copy, script).with_context(|| format!("write {}", copy.display()))?;
    let ran = run_script(repo, worktree, &copy, limit);
    let _ = std::fs::remove_file(&copy);
    match ran? {
        Ran::Exited(status) if status.success() => Ok(()),
        Ran::Exited(status) => Err(failed(worktree, &exit_text(status))),
        Ran::Stopped => Err(failed(worktree, &format!("was stopped after {}", duration(limit)))),
    }
}

fn script_at(repo: &Path, base: &str) -> Result<Option<Vec<u8>>> {
    let listed = run_git(repo, &["ls-tree", base, "--", SETUP_SCRIPT])?;
    let listing = String::from_utf8_lossy(&listed.stdout);
    let mut fields = listing.split_whitespace().skip(1);
    let (Some("blob"), Some(blob)) = (fields.next(), fields.next()) else {
        return Ok(None);
    };
    Ok(Some(run_git(repo, &["cat-file", "blob", blob])?.stdout))
}

fn script_copy(worktree: &Path) -> Result<PathBuf> {
    let output = run_git(worktree, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let name = Path::new(SETUP_SCRIPT).file_name().unwrap_or_default().to_string_lossy();
    Ok(git_dir.join(format!("codescope-{name}")))
}

enum Ran {
    Exited(ExitStatus),
    Stopped,
}

fn run_script(repo: &Path, worktree: &Path, script: &Path, limit: Duration) -> Result<Ran> {
    let log = worktree.join(SETUP_LOG);
    if let Some(folder) = log.parent() {
        std::fs::create_dir_all(folder).with_context(|| format!("create {}", folder.display()))?;
    }
    let output = File::create(&log).with_context(|| format!("create {}", log.display()))?;
    let mut child = spawn_script(script, |command| {
        command
            .current_dir(worktree)
            .env("CODESCOPE_PROJECT", repo)
            .env("CODESCOPE_WORKTREE", worktree)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output.try_clone()?);
        Ok(())
    })?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ran::Exited(status));
        }
        if started.elapsed() >= limit {
            stop_tree(&mut child);
            return Ok(Ran::Stopped);
        }
        std::thread::sleep(SETUP_POLL);
    }
}

#[cfg(windows)]
fn spawn_script(script: &Path, prepare: impl Fn(&mut Command) -> Result<()>) -> Result<Child> {
    let options = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"];
    for program in ["pwsh", "powershell"] {
        let mut command = no_window_command(program);
        command.args(options).arg(script);
        prepare(&mut command)?;
        match command.spawn() {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            spawned => return spawned.with_context(|| format!("run {SETUP_SCRIPT} with {program}")),
        }
    }
    Err(anyhow!("{SETUP_SCRIPT} needs PowerShell, and neither pwsh nor powershell is on PATH"))
}

#[cfg(not(windows))]
fn spawn_script(script: &Path, prepare: impl Fn(&mut Command) -> Result<()>) -> Result<Child> {
    use std::os::unix::fs::PermissionsExt;
    let text = std::fs::read(script).with_context(|| format!("read {}", script.display()))?;
    let mut command = if text.starts_with(b"#!") {
        let executable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(script, executable)
            .with_context(|| format!("make {} executable", script.display()))?;
        no_window_command(script)
    } else {
        let mut command = no_window_command("sh");
        command.arg(script);
        command
    };
    prepare(&mut command)?;
    command.spawn().with_context(|| format!("run {SETUP_SCRIPT}"))
}

fn stop_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = no_window_command("taskkill").args(["/T", "/F", "/PID", &pid]).output();
    }
    #[cfg(not(windows))]
    {
        let table = crate::process_tree::process_table();
        for pid in crate::process_tree::descendants(child.id(), &table) {
            let _ = no_window_command("kill").args(["-9", &pid.to_string()]).output();
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn exit_text(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "was stopped by a signal".to_string(),
        |code| format!("exited with {code}"),
    )
}

fn duration(limit: Duration) -> String {
    match limit.as_secs() {
        seconds if seconds >= 60 => format!("{} minutes", seconds / 60),
        seconds => format!("{seconds} s"),
    }
}

fn failed(worktree: &Path, what: &str) -> anyhow::Error {
    let log = std::fs::read(worktree.join(SETUP_LOG)).unwrap_or_default();
    let log = String::from_utf8_lossy(&log);
    let lines: Vec<&str> = log.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join(" · ");
    let tail = if tail.is_empty() { String::new() } else { format!(": {tail}") };
    anyhow!("{SETUP_SCRIPT} {what}{tail} (see {SETUP_LOG})")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;
    use crate::board::dispatch::exclude_handover_files;
    use crate::process::no_window_command;

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
        upstream: PathBuf,
        repo: PathBuf,
        worktree: PathBuf,
    }

    fn repos() -> Option<Repos> {
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
        let worktree = dir.path().join("repo.worktrees").join("462-setup");
        let path = worktree.to_string_lossy().to_string();
        git(&repo, &["worktree", "add", "-q", "-b", "462-setup", &path, "origin/main"]);
        Some(Repos {
            _dir: dir,
            upstream,
            repo,
            worktree,
        })
    }

    fn write_setup(folder: &Path, powershell: &str, shell: &str) {
        let scripts = folder.join(".codescope");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("setup.ps1"), powershell).unwrap();
        std::fs::write(scripts.join("setup.sh"), shell).unwrap();
        git(folder, &["add", ".codescope"]);
        git(folder, &["commit", "-q", "-m", "setup"]);
    }

    fn ran(repos: &Repos, file: &str) -> Option<String> {
        std::fs::read_to_string(repos.worktree.join(file)).ok()
    }

    fn writes(file: &str) -> (String, String) {
        let project = "$env:CODESCOPE_PROJECT";
        let powershell = format!("Set-Content -NoNewline -Path {file} -Value {project}\n");
        let shell = format!("printf '%s' \"$CODESCOPE_PROJECT\" > {file}\n");
        (powershell, shell)
    }

    #[test]
    fn a_new_worktree_runs_the_default_branch_s_setup_in_itself() {
        let Some(repos) = repos() else { return };
        let (powershell, shell) = writes("ran.txt");
        write_setup(&repos.upstream, &powershell, &shell);
        exclude_handover_files(&repos.worktree).unwrap();
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        let project = repos.repo.to_string_lossy();
        assert_eq!(ran(&repos, "ran.txt").as_deref(), Some(project.as_ref()));
        assert!(repos.worktree.join(SETUP_LOG).exists());
        let status = git(&repos.worktree, &["status", "--porcelain", "--untracked-files=all"]);
        assert_eq!(status, "?? ran.txt", "the log stays out of git");
        let git_dir = PathBuf::from(git(&repos.worktree, &["rev-parse", "--absolute-git-dir"]));
        let left: Vec<_> = std::fs::read_dir(git_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains("setup"))
            .collect();
        assert!(left.is_empty(), "its copy of the script is gone");
    }

    #[test]
    fn the_setup_comes_from_the_default_branch_not_the_item_s_branch() {
        let Some(repos) = repos() else { return };
        let (powershell, shell) = writes("ran.txt");
        write_setup(&repos.upstream, &powershell, &shell);
        let (powershell, shell) = writes("branch.txt");
        write_setup(&repos.worktree, &powershell, &shell);
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert!(ran(&repos, "ran.txt").is_some());
        assert!(ran(&repos, "branch.txt").is_none());
    }

    #[test]
    fn without_a_setup_script_nothing_runs() {
        let Some(repos) = repos() else { return };
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert!(!repos.worktree.join(SETUP_LOG).exists());
    }

    #[test]
    fn a_failed_setup_says_why_with_the_end_of_its_log() {
        let Some(repos) = repos() else { return };
        let powershell = "Write-Output 'installing'\nWrite-Output 'no node here'\nexit 3\n";
        let shell = "echo installing\necho 'no node here'\nexit 3\n";
        write_setup(&repos.upstream, powershell, shell);
        let err = run_setup(&repos.repo, &repos.worktree, "origin").unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("exited with 3"), "{message}");
        assert!(message.contains("installing · no node here"), "{message}");
        assert!(message.contains(SETUP_LOG), "{message}");
        let (powershell, shell) = writes("ran.txt");
        write_setup(&repos.upstream, &powershell, &shell);
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert!(ran(&repos, "ran.txt").is_some(), "the fixed setup runs on the next try");
    }

    #[test]
    fn a_setup_that_hangs_is_stopped() {
        let Some(repos) = repos() else { return };
        write_setup(&repos.upstream, "Start-Sleep -Seconds 60\n", "sleep 60\n");
        let started = Instant::now();
        let limit = Duration::from_secs(3);
        let err = run_setup_within(&repos.repo, &repos.worktree, "origin", limit).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(30));
        let message = format!("{err:#}");
        assert!(message.contains("was stopped after 3 s"), "{message}");
    }
}
