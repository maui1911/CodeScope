//! Subprocess-spawning helpers shared by every background poller and
//! one-shot CLI invocation we make (`git`, `gh`, …).
//!
//! ## Why this exists
//!
//! Once we set `windows_subsystem = "windows"` on the release binary
//! (PR #184), every `Command::new("git").output()` we run popped a
//! fresh `conhost.exe` console window because the parent (now GUI
//! subsystem) no longer owns a console for the child to inherit.
//! Cargo-debug builds were unaffected — the dev binary is console
//! subsystem and the child borrows the parent's console.
//!
//! The five-second git status / dirty-state pollers plus the 60-second
//! `gh pr list` poll were spawning a flicker of empty conhost windows
//! every tick, sometimes piling up faster than the OS could dismiss
//! them. The fix is to set `CREATE_NO_WINDOW` on every CreateProcess
//! call we make to a console-subsystem child.
//!
//! See:
//!   - <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>
//!   - <https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.creation_flags>
//!
//! ## Usage
//!
//! Replace `Command::new("git")` with [`no_window_command("git")`]
//! everywhere we spawn a console binary for our own machine-readable
//! consumption (`.output()` / `.status()`). For user-launched GUI
//! apps (Explorer, `wt`, `open`, `xdg-open`) the flag is unnecessary —
//! those binaries are already GUI subsystem on Windows or don't
//! exist on Windows at all.

use std::ffi::OsStr;
use std::process::Command;

/// Windows `CREATE_NO_WINDOW` process-creation flag. Suppresses the
/// transient `conhost.exe` window a GUI-subsystem parent would
/// otherwise allocate when spawning a console-subsystem child.
///
/// Value mirrors the constant in `winbase.h` (0x0800_0000). We pull
/// the literal directly to avoid taking a dependency on the `windows`
/// or `winapi` crates just for one symbol.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a `Command` for a console-subsystem child binary with the
/// transient-console window suppressed on Windows. No-op on every
/// other host so callers can use the same helper unconditionally.
///
/// Call this for `git`, `gh`, or any other tool we shell out to for
/// machine-readable output — i.e. anything where the user must never
/// see a flashing console window.
///
/// Also sets `GIT_OPTIONAL_LOCKS=0` (issue #294): our status/diff
/// pollers run `git status` about once a second across all open
/// projects, and by default every one of those refreshes the index —
/// taking `index.lock` — as a side effect. Agents committing in the
/// same worktree lose that race and their commits fail. The env var
/// skips only *optional* lock-taking (the opportunistic index
/// refresh); mandatory locks for real writes (`commit`, `worktree
/// add`, …) are unaffected. Set here at the shared choke point so
/// every current and future git spawn inherits it; `gh` ignores it.
pub fn no_window_command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper must not change behaviour on non-Windows hosts —
    /// the returned `Command` should be byte-equivalent to a plain
    /// `Command::new`. We can only assert the program name; the
    /// `creation_flags` value is Windows-private state we can't read
    /// back through the public API.
    #[test]
    fn returns_command_for_program() {
        let cmd = no_window_command("git");
        assert_eq!(cmd.get_program(), "git");
    }

    /// Polled `git status` must never take `index.lock` (issue #294)
    /// — the env var that guarantees it has to be present on every
    /// spawned command.
    #[test]
    fn disables_optional_git_locks() {
        let cmd = no_window_command("git");
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "GIT_OPTIONAL_LOCKS" && v == Some(OsStr::new("0")))
        );
    }
}
