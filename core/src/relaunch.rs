//! Building blocks for relaunching CodeScope after an in-app update.
//!
//! Issue #341: the update toast's **Restart** button only quits. A real
//! restart is a hand-off between two processes:
//!
//! 1. The old instance, after the updater has swapped the binary on
//!    disk, spawns the new binary with [`relaunch_command`] — detached,
//!    null stdio, and `--wait-for-pid <its own pid>` — and then quits.
//! 2. The new instance reads that argument with [`parse_wait_for_pid`]
//!    before it tries to take the single-instance lock.
//! 3. It then blocks in [`wait_for_exit`] (polling [`pid_is_alive`])
//!    until the old instance is gone, so it does not lose the race for
//!    the single-instance mutex to a process that is still shutting
//!    down and exit as "already running".
//!
//! This module only provides the pieces; calling them from the binary
//! crate's `main` and the toast action is a separate change. The wait
//! takes its clock as parameters — the caller passes the standard
//! library's thread sleep — so the tests here are exact and never sleep.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Command-line flag the relaunched instance receives, followed by the
/// pid of the instance that spawned it.
pub const WAIT_FOR_PID_ARG: &str = "--wait-for-pid";

/// Env var that selects dev mode; see `paths::AppPaths::detect`.
const DEV_ENV_VAR: &str = "CODESCOPE_DEV";

/// Windows `DETACHED_PROCESS`: the child does not inherit the parent's
/// console, so it survives the parent quitting (`winbase.h`).
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Windows `CREATE_NEW_PROCESS_GROUP`: the child is not in the parent's
/// process group, so a Ctrl+C / Ctrl+Break aimed at the parent does not
/// reach it (`winbase.h`).
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Windows `CREATE_BREAKAWAY_FROM_JOB`: the child is not assigned to the
/// parent's job (`winbase.h`). CodeScope puts itself in a
/// `KILL_ON_JOB_CLOSE` job (`codescope_terminal::process_group`), so a
/// child that stays in it is killed the moment the old instance exits.
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Build (but do not spawn) the command that starts `exe` as the next
/// CodeScope instance: arguments `[WAIT_FOR_PID_ARG, old_pid]`, null
/// stdio, detached from the current process on Windows.
///
/// `dev` is the `CODESCOPE_DEV` value to pass on, if any. `None` leaves
/// the variable alone (the child inherits whatever the environment has).
/// It is a parameter rather than read here so tests never touch
/// process-global environment state.
///
/// Deliberately not `process::no_window_command`: that helper hides the
/// console of tools we shell out to, and this child is the visible app.
pub fn relaunch_command(exe: &Path, old_pid: u32, dev: Option<&OsStr>) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg(WAIT_FOR_PID_ARG)
        .arg(old_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(value) = dev {
        cmd.env(DEV_ENV_VAR, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
        );
    }
    cmd
}

/// Drop `CREATE_BREAKAWAY_FROM_JOB` from a [`relaunch_command`]. Spawning
/// fails with access denied when a job CodeScope runs in (one it was
/// launched inside, not its own) does not allow breakaway; retrying
/// without the flag is the fallback, and the child then survives only if
/// no enclosing job kills it on close.
#[cfg(windows)]
pub fn without_job_breakaway(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Read the pid following the first [`WAIT_FOR_PID_ARG`] in `args` (the
/// arguments *after* the program name). `None` when the flag is absent,
/// has no following value, or that value is not a `u32`. Every other
/// argument is ignored.
pub fn parse_wait_for_pid<I, S>(args: I) -> Option<u32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    args.by_ref().find(|arg| arg.as_ref() == WAIT_FOR_PID_ARG)?;
    args.next()?.as_ref().parse().ok()
}

/// Wait until `is_alive(pid)` reports `false`, polling every `poll`.
///
/// Returns `true` as soon as the process is gone — checked before the
/// first sleep, so an already-exited pid costs nothing — and `false`
/// once the durations passed to `sleep` add up to `timeout` with the
/// process still alive. The last sleep is shortened so the total never
/// exceeds `timeout`. Elapsed time is only ever that sum; there is no
/// real clock, which keeps the behaviour exact under test.
///
/// A `poll` of [`Duration::ZERO`] could never reach the timeout, so it
/// means "check once": the initial check's result, with no sleep.
///
/// Pid reuse is not handled: once the old process has exited its pid
/// may be given to an unrelated process, which `is_alive` would then
/// report as alive. The cost is a startup that waits out `timeout` and
/// proceeds anyway, which is acceptable here.
pub fn wait_for_exit(
    pid: u32,
    timeout: Duration,
    poll: Duration,
    mut is_alive: impl FnMut(u32) -> bool,
    mut sleep: impl FnMut(Duration),
) -> bool {
    if !is_alive(pid) {
        return true;
    }
    if poll.is_zero() {
        return false;
    }
    let mut elapsed = Duration::ZERO;
    while elapsed < timeout {
        let step = poll.min(timeout - elapsed);
        sleep(step);
        elapsed += step;
        if !is_alive(pid) {
            return true;
        }
    }
    false
}

/// Whether a process with `pid` is currently running.
///
/// Subject to pid reuse (see [`wait_for_exit`]): a recycled pid answers
/// for whichever process holds it now.
#[cfg(target_os = "windows")]
pub fn pid_is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: plain value arguments; the call returns a handle we own
    // or an error, and no pointers are dereferenced.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        // No such process, or no access to it — either way we cannot
        // wait on it, so treat it as gone.
        return false;
    };
    let mut code = 0u32;
    // SAFETY: `handle` is the valid handle opened just above, and `code`
    // is a live local the call writes a single `u32` into.
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) };
    // SAFETY: `handle` came from `OpenProcess` and is closed exactly
    // once, here, before either return below.
    unsafe {
        let _ = CloseHandle(handle);
    }
    queried.is_ok() && code == STILL_ACTIVE.0 as u32
}

/// Whether a process with `pid` is currently running.
///
/// Subject to pid reuse (see [`wait_for_exit`]): a recycled pid answers
/// for whichever process holds it now.
#[cfg(not(target_os = "windows"))]
pub fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn dev_env(cmd: &Command) -> Option<Option<&OsStr>> {
        cmd.get_envs().find(|(k, _)| *k == DEV_ENV_VAR).map(|(_, v)| v)
    }

    #[test]
    fn relaunch_command_runs_the_given_exe() {
        let exe = Path::new("some/dir/codescope.exe");
        let cmd = relaunch_command(exe, 42, None);
        assert_eq!(cmd.get_program(), exe.as_os_str());
    }

    #[test]
    fn relaunch_command_args_are_exactly_flag_and_pid() {
        let cmd = relaunch_command(Path::new("codescope"), 4711, None);
        let args: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(args, [OsStr::new(WAIT_FOR_PID_ARG), OsStr::new("4711")]);
    }

    #[test]
    fn relaunch_command_sets_dev_when_given() {
        let cmd = relaunch_command(Path::new("codescope"), 1, Some(OsStr::new("1")));
        assert_eq!(dev_env(&cmd), Some(Some(OsStr::new("1"))));
    }

    #[test]
    fn relaunch_command_leaves_dev_untouched_when_none() {
        let cmd = relaunch_command(Path::new("codescope"), 1, None);
        assert_eq!(dev_env(&cmd), None);
    }

    /// What the old instance writes, the new one must read back.
    #[test]
    fn relaunch_command_args_round_trip_through_parse() {
        let cmd = relaunch_command(Path::new("codescope"), 9001, None);
        let args: Vec<String> =
            cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(parse_wait_for_pid(args), Some(9001));
    }

    #[test]
    fn parse_reads_pid_after_flag() {
        assert_eq!(parse_wait_for_pid(["--wait-for-pid", "1234"]), Some(1234));
    }

    #[test]
    fn parse_ignores_other_args_anywhere() {
        let args = ["--verbose", "x", "--wait-for-pid", "77", "--other", "5"];
        assert_eq!(parse_wait_for_pid(args), Some(77));
    }

    #[test]
    fn parse_uses_first_flag() {
        let args = ["--wait-for-pid", "1", "--wait-for-pid", "2"];
        assert_eq!(parse_wait_for_pid(args), Some(1));
    }

    #[test]
    fn parse_returns_none_when_flag_absent() {
        assert_eq!(parse_wait_for_pid(["--verbose", "1234"]), None);
        assert_eq!(parse_wait_for_pid(Vec::<String>::new()), None);
    }

    #[test]
    fn parse_returns_none_when_flag_has_no_value() {
        assert_eq!(parse_wait_for_pid(["--other", "--wait-for-pid"]), None);
    }

    #[test]
    fn parse_returns_none_when_value_is_not_u32() {
        assert_eq!(parse_wait_for_pid(["--wait-for-pid", "abc"]), None);
        assert_eq!(parse_wait_for_pid(["--wait-for-pid", "-5"]), None);
        assert_eq!(parse_wait_for_pid(["--wait-for-pid", "4294967296"]), None);
        assert_eq!(parse_wait_for_pid(["--wait-for-pid", ""]), None);
    }

    #[test]
    fn wait_returns_immediately_when_already_gone() {
        let sleeps = RefCell::new(Vec::new());
        let gone = wait_for_exit(
            7,
            Duration::from_secs(5),
            Duration::from_millis(100),
            |_| false,
            |d| sleeps.borrow_mut().push(d),
        );
        assert!(gone);
        assert!(sleeps.borrow().is_empty());
    }

    #[test]
    fn wait_returns_once_gone_after_n_polls() {
        let checks = RefCell::new(0u32);
        let sleeps = RefCell::new(Vec::new());
        let gone = wait_for_exit(
            7,
            Duration::from_secs(5),
            Duration::from_millis(100),
            |pid| {
                assert_eq!(pid, 7);
                *checks.borrow_mut() += 1;
                // Alive for the initial check and the first two polls.
                *checks.borrow() <= 3
            },
            |d| sleeps.borrow_mut().push(d),
        );
        assert!(gone);
        assert_eq!(*checks.borrow(), 4);
        assert_eq!(*sleeps.borrow(), vec![Duration::from_millis(100); 3]);
    }

    #[test]
    fn wait_gives_up_at_timeout_without_sleeping_past_it() {
        let sleeps = RefCell::new(Vec::new());
        let gone = wait_for_exit(
            7,
            Duration::from_millis(250),
            Duration::from_millis(100),
            |_| true,
            |d| sleeps.borrow_mut().push(d),
        );
        assert!(!gone);
        let total: Duration = sleeps.borrow().iter().sum();
        assert_eq!(total, Duration::from_millis(250));
        assert_eq!(
            *sleeps.borrow(),
            [
                Duration::from_millis(100),
                Duration::from_millis(100),
                Duration::from_millis(50),
            ]
        );
    }

    #[test]
    fn wait_with_zero_poll_checks_once() {
        let checks = RefCell::new(0u32);
        let sleeps = RefCell::new(Vec::new());
        let gone = wait_for_exit(
            7,
            Duration::from_secs(5),
            Duration::ZERO,
            |_| {
                *checks.borrow_mut() += 1;
                true
            },
            |d| sleeps.borrow_mut().push(d),
        );
        assert!(!gone);
        assert_eq!(*checks.borrow(), 1);
        assert!(sleeps.borrow().is_empty());
    }

    #[test]
    fn wait_with_zero_timeout_checks_once() {
        let sleeps = RefCell::new(Vec::new());
        let gone = wait_for_exit(
            7,
            Duration::ZERO,
            Duration::from_millis(100),
            |_| true,
            |d| sleeps.borrow_mut().push(d),
        );
        assert!(!gone);
        assert!(sleeps.borrow().is_empty());
    }

    #[test]
    fn current_process_is_alive() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn exited_child_is_not_alive() {
        #[cfg(target_os = "windows")]
        let mut child = Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap();
        #[cfg(not(target_os = "windows"))]
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_is_alive(pid));
    }
}
