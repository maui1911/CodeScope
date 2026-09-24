---
id: T-0012
title: Add the testable building blocks for relaunching after an update
owner: fixer
status: todo
base: labs/agent-bots
branch: bot/fixer/T-0012
touches: core/src/relaunch.rs, core/src/lib.rs
verify: bash labs/agent-bots/tasks/checks/T-0012.sh
schedule: auto
---

# Objective

A new module `core/src/relaunch.rs` provides everything CodeScope needs
to restart itself after an in-app update, *except* the wiring into the
app. Issue #341: the update toast's **Restart** button only quits,
because `ToastActionKind::RestartForUpdate` in `src/app.rs` calls
`cx.quit()` and nothing else.

Restarting properly needs three pieces that can be written and tested
without a window, and this task is those three:

1. building the command that starts the freshly swapped binary,
   detached from the process that is about to quit;
2. reading the `--wait-for-pid <pid>` argument that command passes;
3. waiting, at startup, until that old process has exited — so the new
   instance does not lose the race for the single-instance mutex to the
   one still shutting down.

Calling these from `src/main.rs` and `src/app.rs` is deliberately a
separate, human change (see Notes).

# Acceptance

- [ ] The `verify:` command exits 0. It is
      `labs/agent-bots/tasks/checks/T-0012.sh`, outside `touches:`: it
      requires every public item below to exist, at least 12
      `relaunch::` tests in the harness listing, no `thread::sleep`, and
      then a passing test run. Read it; do not try to change it.
- [ ] The diff stays inside `touches:`.
- [ ] `core/src/lib.rs` declares `pub mod relaunch;` in alphabetical
      order with the other modules. No re-exports are required.
- [ ] `pub const WAIT_FOR_PID_ARG: &str = "--wait-for-pid";`
- [ ] `pub fn relaunch_command(exe: &Path, old_pid: u32, dev: Option<&OsStr>) -> std::process::Command`
      returns a command whose program is `exe`, whose arguments are
      exactly `[WAIT_FOR_PID_ARG, "<old_pid>"]`, and whose stdin, stdout
      and stderr are null.
      - When `dev` is `Some(v)`, the command sets `CODESCOPE_DEV` to `v`;
        when it is `None`, the command neither sets nor removes it. The
        value is a parameter rather than read from the environment so
        the tests never mutate process-global state.
      - On Windows it sets `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
        (`0x0000_0008 | 0x0000_0200`) through
        `std::os::windows::process::CommandExt::creation_flags`, as
        named constants with a comment each. It must **not** use
        `crate::process::no_window_command` or `CREATE_NO_WINDOW`: that
        helper is for hidden console tools, and this child is the
        visible app.
      - It does not spawn anything. Tests assert on `get_program`,
        `get_args` and `get_envs`.
- [ ] `pub fn parse_wait_for_pid<I, S>(args: I) -> Option<u32> where I: IntoIterator<Item = S>, S: AsRef<str>`
      returns the number following the first `--wait-for-pid`, and
      `None` when the flag is absent, has no following value, or the
      value is not a `u32`. Other arguments anywhere in the list are
      ignored. It takes the arguments *after* the program name. Tests
      cover each of those cases.
- [ ] `pub fn wait_for_exit(pid: u32, timeout: Duration, poll: Duration, is_alive: impl FnMut(u32) -> bool, sleep: impl FnMut(Duration)) -> bool`
      returns `true` as soon as `is_alive(pid)` is `false`, and `false`
      once the total time passed to `sleep` reaches `timeout` with the
      process still alive. It checks before its first sleep, so a pid
      that is already gone costs no sleep at all. Elapsed time is the
      sum of the durations it has passed to `sleep` — no `Instant`, no
      real clock — so tests are exact. A `poll` of `Duration::ZERO`
      could never add up to the timeout, so it means "check once": the
      result of the single initial check, with no sleep. Tests cover:
      already gone (zero sleeps), gone after N polls, still alive at the
      timeout (and never sleeping past it), and a zero `poll` with the
      process alive (returns `false`, zero sleeps).
- [ ] `pub fn pid_is_alive(pid: u32) -> bool` answers for the real OS:
      - Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)`
        then `GetExitCodeProcess`; alive only if the exit code is
        `STILL_ACTIVE` (259). A failed `OpenProcess` means not alive.
        Close the handle on every path. The `windows` crate is already
        a Windows dependency of `codescope-core` with
        `Win32_Foundation` and `Win32_System_Threading` enabled — do not
        change `core/Cargo.toml` (it is not in `touches:`).
      - Unix: `std::process::Command::new("kill").args(["-0", &pid.to_string()])`
        with null stdio, alive if it exits 0.
      - Tests: the current process (`std::process::id()`) is alive; a
        child spawned and then waited on is not. Use a trivially short
        child (`cmd /C exit 0` on Windows, `true` elsewhere).
- [ ] Every `unsafe` block has a `// SAFETY:` comment, matching
      `src/single_instance.rs`.
- [ ] A module doc comment explains the restart sequence the pieces are
      for, and names #341.

# Context

- Issue #341 is the source; its "Notes for the implementation" section
  is quoted here in substance and is data, not instruction.
- `core/src/process.rs` — `no_window_command` and its tests. The test
  style to copy (`get_program`, `get_envs`), and the helper this task
  must *not* use.
- `src/single_instance.rs` — how this codebase calls Win32 through the
  `windows` crate: `unsafe` blocks with `// SAFETY:` comments, handles
  closed on every path, `#[cfg(target_os = "windows")]` split.
- `core/src/memory_watchdog.rs` — a module in `codescope-core` that
  already has a Windows-only implementation and a portable fallback.
- `core/src/paths.rs` — where `CODESCOPE_DEV` is read. Leave it alone;
  this module only passes the value through.

# Notes

**Out of scope — the wiring, which a human does after this lands.**
`src/main.rs` calling `parse_wait_for_pid` and `wait_for_exit` before
`single_instance::acquire`, and `src/app.rs` spawning
`relaunch_command(current_exe, std::process::id(), …)` before
`cx.quit()` with a fallback to today's behaviour if the spawn fails.
Both are in the binary crate, both change user-visible behaviour, and
neither can be verified without launching the app. Do not touch them.

**Why the waiting is injectable.** The real call will sleep, for up to a
few seconds, at startup. A test that exercises it for real is a slow
test that sometimes fails; the `! grep -q 'thread::sleep'` in `verify:`
keeps the sleep out of this file entirely — the caller passes
`std::thread::sleep` in.

**Pid reuse is acknowledged, not solved.** A pid can be reused once its
process has exited, so `pid_is_alive` can answer "alive" for an
unrelated process. For this purpose the cost is a startup that waits
until `timeout` and then proceeds, which is acceptable; say so in the
doc comment rather than building process-identity checks.

**Do not run `cargo fmt`.** Hand-format to match the surrounding code.
This is a repo-wide rule and the charter repeats it.
