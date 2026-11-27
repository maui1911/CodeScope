//! The setup command (ADR-0027): a dispatch's new worktree runs the repository's own setup
//! script, as the remote's default branch has it, before its session gets the task.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::dispatch::{fetch_branch_head, linked_part, remote_default_branch};
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

#[cfg(windows)]
const SETUP_COPY: &str = ".codescope/.setup.ps1";
#[cfg(not(windows))]
const SETUP_COPY: &str = ".codescope/.setup.sh";
const SETUP_DONE: &str = "codescope-setup-done";
const SETUP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SETUP_POLL: Duration = Duration::from_millis(200);
const TAIL_LINES: usize = 2;
const TAIL_LINE_CHARS: usize = 200;

static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Run the setup script of `remote`'s default branch in the new `worktree` of `repo`, with its
/// output in [`SETUP_LOG`]; nothing when that branch has none or this worktree already ran that
/// same script. An error says why it failed.
pub fn run_setup(repo: &Path, worktree: &Path, remote: &str) -> Result<()> {
    run_setup_within(repo, worktree, remote, SETUP_TIMEOUT)
}

/// Stop every setup still running, with all it started; for when CodeScope quits.
pub fn stop_running_setups() {
    let running = std::mem::take(&mut *RUNNING.lock().unwrap_or_else(PoisonError::into_inner));
    for pid in running {
        stop_tree_of(pid);
    }
}

fn run_setup_within(repo: &Path, worktree: &Path, remote: &str, limit: Duration) -> Result<()> {
    let default_branch = remote_default_branch(repo, remote)?;
    let base = fetch_branch_head(repo, remote, &default_branch)?;
    let Some(blob) = script_at(repo, &base)? else {
        return Ok(());
    };
    let done = git_dir(worktree)?.join(SETUP_DONE);
    if std::fs::read_to_string(&done).is_ok_and(|ran| ran.trim() == blob) {
        return Ok(());
    }
    let script = run_git(repo, &["cat-file", "blob", &blob])?.stdout;
    let (copy, mut file) = create_fresh(worktree, SETUP_COPY)?;
    let written = file
        .write_all(&script)
        .with_context(|| format!("write {}", copy.display()));
    drop(file);
    let ran = written.and_then(|()| run_script(repo, worktree, &copy, limit));
    let _ = std::fs::remove_file(&copy);
    match ran? {
        Ran::Exited(status) if status.success() => {
            let _ = std::fs::write(&done, &blob);
            Ok(())
        }
        Ran::Exited(status) => Err(failed(worktree, &exit_text(status))),
        Ran::Stopped => Err(failed(worktree, &format!("was stopped after {}", duration(limit)))),
    }
}

fn script_at(repo: &Path, base: &str) -> Result<Option<String>> {
    let listed = run_git(repo, &["ls-tree", base, "--", SETUP_SCRIPT])?;
    let listing = String::from_utf8_lossy(&listed.stdout);
    let mut fields = listing.split_whitespace();
    let (Some(mode), Some(blob)) = (fields.next(), fields.nth(1)) else {
        return Ok(None);
    };
    match mode {
        "100644" | "100755" => Ok(Some(blob.to_string())),
        "120000" => bail!("{SETUP_SCRIPT} on the default branch is a link; make it the script"),
        _ => bail!("{SETUP_SCRIPT} on the default branch isn't a file"),
    }
}

fn git_dir(worktree: &Path) -> Result<PathBuf> {
    let output = run_git(worktree, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
}

fn create_fresh(worktree: &Path, relative: &str) -> Result<(PathBuf, File)> {
    if let Some(part) = linked_part(worktree, relative) {
        bail!("{} is a link", part.display());
    }
    let path = worktree.join(relative);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).with_context(|| format!("create {}", folder.display()))?;
    }
    match std::fs::remove_file(&path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(err).with_context(|| format!("remove {}", path.display()));
        }
        _ => {}
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    Ok((path, file))
}

enum Ran {
    Exited(ExitStatus),
    Stopped,
}

struct Running(u32);

impl Running {
    fn new(pid: u32) -> Self {
        RUNNING.lock().unwrap_or_else(PoisonError::into_inner).push(pid);
        Self(pid)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        running.retain(|pid| *pid != self.0);
    }
}

fn run_script(repo: &Path, worktree: &Path, script: &Path, limit: Duration) -> Result<Ran> {
    let (_, output) = create_fresh(worktree, SETUP_LOG)?;
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
    drop(output);
    let _running = Running::new(child.id());
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
    let run = "$ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
               if ($PSStyle) { $PSStyle.OutputRendering = 'PlainText' }; \
               & $env:CODESCOPE_SETUP_SCRIPT; \
               if (Test-Path variable:LASTEXITCODE) { exit $LASTEXITCODE }";
    let options = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"];
    for program in ["pwsh", "powershell"] {
        let mut command = no_window_command(program);
        command.args(options).arg(run).env("CODESCOPE_SETUP_SCRIPT", script);
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
    use std::os::unix::process::CommandExt;
    let text = std::fs::read(script).with_context(|| format!("read {}", script.display()))?;
    let shebang = text.starts_with(b"#!");
    let mut command = if shebang {
        let executable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(script, executable)
            .with_context(|| format!("make {} executable", script.display()))?;
        no_window_command(script)
    } else {
        let mut command = no_window_command("sh");
        command.arg("-e").arg(script);
        command
    };
    command.process_group(0);
    prepare(&mut command)?;
    command.spawn().map_err(|err| {
        if shebang && err.kind() == std::io::ErrorKind::NotFound {
            let first = text.split(|byte| *byte == b'\n').next().unwrap_or_default();
            let first = String::from_utf8_lossy(first);
            anyhow!(
                "{SETUP_SCRIPT} starts with {first:?}, a program that isn't there (or the file \
                 has Windows line endings)"
            )
        } else {
            anyhow::Error::new(err).context(format!("run {SETUP_SCRIPT}"))
        }
    })
}

fn stop_tree(child: &mut Child) {
    stop_tree_of(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn stop_tree_of(pid: u32) {
    let pid = pid.to_string();
    let _ = no_window_command("taskkill").args(["/T", "/F", "/PID", &pid]).output();
}

#[cfg(not(windows))]
fn stop_tree_of(pid: u32) {
    let group = format!("-{pid}");
    let _ = no_window_command("kill").args(["-s", "KILL", "--", &group]).output();
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
    let log = match linked_part(worktree, SETUP_LOG) {
        Some(_) => Vec::new(),
        None => std::fs::read(worktree.join(SETUP_LOG)).unwrap_or_default(),
    };
    let log = String::from_utf8_lossy(&log);
    let lines: Vec<String> = log
        .lines()
        .map(|line| plain(line).trim().to_string())
        .filter(|line| !line.is_empty())
        .map(|line| shortened(&line))
        .collect();
    let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join(" · ");
    let tail = if tail.is_empty() { String::new() } else { format!(": {tail}") };
    anyhow!("{SETUP_SCRIPT} {what}{tail} (see {SETUP_LOG})")
}

fn plain(line: &str) -> String {
    let mut kept = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            kept.push(c);
        } else if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    kept
}

fn shortened(line: &str) -> String {
    match line.char_indices().nth(TAIL_LINE_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
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
        repos_in("462-setup")
    }

    fn repos_in(folder: &str) -> Option<Repos> {
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
        let worktree = dir.path().join("repo.worktrees").join(folder);
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

    fn link_folder(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(made.status.success(), "{made:?}");
        }
    }

    fn lines_in(repos: &Repos, file: &str) -> usize {
        ran(repos, file).map_or(0, |text| text.lines().count())
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
        let left: Vec<_> = std::fs::read_dir(repos.worktree.join(".codescope"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["setup.log"], "its copy of the script is gone");
    }

    #[test]
    fn the_script_finds_the_worktree_next_to_itself() {
        let Some(repos) = repos() else { return };
        let powershell = "Set-Content -Path (Join-Path (Split-Path $PSScriptRoot) 'ran.txt') \
                          -Value x\n";
        let shell = "cd \"$(dirname \"$0\")/..\" && echo x > ran.txt\n";
        write_setup(&repos.upstream, powershell, shell);
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert!(ran(&repos, "ran.txt").is_some());
    }

    #[test]
    fn a_worktree_path_with_quotes_runs_only_the_script() {
        let Some(repos) = repos_in("462-x’; Set-Content hacked.txt 1; ’'") else {
            return;
        };
        let (powershell, shell) = writes("ran.txt");
        write_setup(&repos.upstream, &powershell, &shell);
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert!(ran(&repos, "ran.txt").is_some());
        assert!(ran(&repos, "hacked.txt").is_none());
    }

    #[test]
    fn a_linked_codescope_folder_is_refused_and_nothing_lands_outside() {
        let Some(repos) = repos() else { return };
        let (powershell, shell) = writes("ran.txt");
        write_setup(&repos.upstream, &powershell, &shell);
        let outside = repos.repo.with_file_name("outside");
        std::fs::create_dir_all(&outside).unwrap();
        link_folder(&outside, &repos.worktree.join(".codescope"));
        let err = run_setup(&repos.repo, &repos.worktree, "origin").unwrap_err();
        assert!(format!("{err:#}").contains("is a link"), "{err:#}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert!(ran(&repos, "ran.txt").is_none());
    }

    #[test]
    fn a_linked_setup_script_on_the_default_branch_is_refused() {
        let Some(repos) = repos() else { return };
        std::fs::write(repos.upstream.join("link-text"), "../scripts/setup").unwrap();
        let blob = git(&repos.upstream, &["hash-object", "-w", "link-text"]);
        for name in [".codescope/setup.ps1", ".codescope/setup.sh"] {
            let entry = format!("120000,{blob},{name}");
            git(&repos.upstream, &["update-index", "--add", "--cacheinfo", &entry]);
        }
        git(&repos.upstream, &["commit", "-q", "-m", "linked setup"]);
        let err = run_setup(&repos.repo, &repos.worktree, "origin").unwrap_err();
        assert!(format!("{err:#}").contains("is a link"), "{err:#}");
        assert!(!repos.worktree.join(SETUP_LOG).exists());
    }

    #[test]
    fn a_failing_command_fails_the_setup_without_an_exit() {
        let Some(repos) = repos() else { return };
        let powershell = "Get-Item no-such-file\nSet-Content -Path ran.txt -Value x\n";
        let shell = "ls no-such-file\necho x > ran.txt\n";
        write_setup(&repos.upstream, powershell, shell);
        let err = run_setup(&repos.repo, &repos.worktree, "origin").unwrap_err();
        assert!(ran(&repos, "ran.txt").is_none(), "it stops at the failing command");
        let message = format!("{err:#}");
        assert!(message.contains("no-such-file") && !message.contains('\u{1b}'), "{message:?}");
    }

    #[test]
    fn a_failing_program_fails_the_setup_without_an_exit() {
        let Some(repos) = repos() else { return };
        let fails = "git rev-parse --verify --quiet no-such-ref";
        let powershell = format!("{fails}\nSet-Content -Path ran.txt -Value x\n");
        let shell = format!("{fails}\necho x > ran.txt\n");
        write_setup(&repos.upstream, &powershell, &shell);
        assert!(run_setup(&repos.repo, &repos.worktree, "origin").is_err());
    }

    #[test]
    fn a_setup_that_succeeded_runs_again_only_for_a_changed_script() {
        let Some(repos) = repos() else { return };
        let once = ("Add-Content -Path count.txt -Value one\n", "echo one >> count.txt\n");
        write_setup(&repos.upstream, once.0, once.1);
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert_eq!(lines_in(&repos, "count.txt"), 1);
        let changed = ("Add-Content -Path count.txt -Value two\n", "echo two >> count.txt\n");
        write_setup(&repos.upstream, changed.0, changed.1);
        run_setup(&repos.repo, &repos.worktree, "origin").unwrap();
        assert_eq!(lines_in(&repos, "count.txt"), 2);
    }

    #[test]
    fn a_long_log_line_is_cut_short_in_the_error() {
        let Some(repos) = repos() else { return };
        let powershell = "Write-Output ('x' * 5000)\nexit 1\n";
        let shell = "awk 'BEGIN { while (n++ < 5000) printf \"x\"; print \"\" }'\nexit 1\n";
        write_setup(&repos.upstream, powershell, shell);
        let err = run_setup(&repos.repo, &repos.worktree, "origin").unwrap_err();
        let message = format!("{err:#}");
        assert!(message.chars().count() < 400, "{} chars", message.chars().count());
        assert!(message.contains("x…"), "{message}");
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
        assert!(run_setup(&repos.repo, &repos.worktree, "origin").is_err(), "it runs again");
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
