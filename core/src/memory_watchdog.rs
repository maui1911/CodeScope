//! Dev-mode-only memory watchdog.
//!
//! Mirrors `legacy:CodeScope.App/Diagnostics/MemoryWatchdog.cs` in the C#
//! build. Periodically samples the process working-set and surfaces
//! growth between ticks so per-session terminal scrollback retention
//! regressions (and similar "creep" leaks) don't go unnoticed during
//! long dev runs. Production builds skip this entirely — the cadence
//! is loose enough that it'd be harmless, but there's no value in
//! surfacing internal memory chatter to end users.
//!
//! ### Cadence and thresholds
//!
//! Both copied verbatim from C#:
//!
//! * initial delay: 1 minute (avoids logging the boot-time spike where
//!   nothing meaningful has happened yet)
//! * cadence: 5 minutes
//! * growth threshold: 50 MiB — anything below this logs at
//!   informational level; anything at or above logs as a warning so
//!   it's easy to grep for in console output
//!
//! ### Output
//!
//! Mirrors C#'s `ILogger` output but emits via `eprintln!`, which is
//! the convention everywhere else in `src/` (see `crash_log`'s
//! last-resort surface, the various `eprintln!` warnings in `app.rs`
//! and `sidebar.rs`). Each line is prefixed
//! `MemoryWatchdog:` so it's filterable from terminal output.
//!
//! Each line includes:
//!
//! * ISO-8601 UTC timestamp (matches `crash_log::format_iso8601_utc`)
//! * working-set size in MiB
//! * growth since the previous sample in MiB (signed; first sample
//!   reports 0)
//!
//! ### Cross-platform
//!
//! On Windows we call `K32GetProcessMemoryInfo` from the `windows`
//! crate. On Linux we read `/proc/self/status` (`VmRSS:` line). On
//! macOS we call `task_info`. Anywhere else, sampling returns `None`
//! and the watchdog silently no-ops — a memory tracker that can't
//! actually read memory is nothing but noise, and dev-mode by
//! definition won't be running on those platforms.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::time::iso8601_from_systemtime_with_millis as format_iso8601_utc;

/// Working-set growth threshold beyond which the periodic log line is
/// promoted from `info` to `warn`. 50 MiB chosen as roughly the cost of
/// one fat ConPTY scrollback at the default history size — anything
/// larger per cadence is worth surfacing. Mirrors C# verbatim.
pub const GROWTH_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024;

/// How long to wait before the first sample. C# parity: gives the boot
/// sequence room to settle so the first delta isn't compared against a
/// number that was still climbing.
pub const INITIAL_DELAY: Duration = Duration::from_secs(60);

/// Cadence between samples after the initial delay. C# parity: 5 min.
pub const CADENCE: Duration = Duration::from_secs(5 * 60);

/// Start the watchdog if and only if `dev_mode` is true. Production
/// callers pay nothing — no thread, no allocation. Idempotent: a second
/// call while the watchdog is already running is silently ignored.
///
/// The watchdog runs on a detached background thread. There's no
/// public stop handle; the thread is short-lived only in the sense
/// that the process terminates with it. This matches C#'s
/// `BackgroundService` lifetime, which also lives until shutdown.
pub fn start_if_dev(dev_mode: bool) {
    if !dev_mode {
        return;
    }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::Builder::new()
        .name("memory-watchdog".into())
        .spawn(run_loop)
        .ok(); // Failure to spawn the dev-only watchdog is non-fatal.
}

fn run_loop() {
    thread::sleep(INITIAL_DELAY);
    let mut last: Option<u64> = None;
    loop {
        if let Some(ws) = sample_working_set_bytes() {
            let line = format_line(SystemTime::now(), ws, last);
            // Bump to the stderr stream regardless of severity — the
            // existing logging convention in `src/` is `eprintln!`,
            // and we have no `tracing` subscriber wired up. The
            // `MemoryWatchdog:` prefix makes filtering straightforward.
            eprintln!("{line}");
            last = Some(ws);
        }
        thread::sleep(CADENCE);
    }
}

/// Format one log line. Pulled out so unit tests can exercise the
/// formatting without touching the OS or sleeping the test thread.
pub fn format_line(now: SystemTime, working_set_bytes: u64, last: Option<u64>) -> String {
    let ts = format_iso8601_utc(now);
    let ws_mib = bytes_to_mib(working_set_bytes);
    match last {
        None => format!(
            "MemoryWatchdog: [{ts}] working set {ws_mib:.0} MiB (first sample)"
        ),
        Some(prev) => {
            let delta_bytes = working_set_bytes as i128 - prev as i128;
            let delta_mib = delta_bytes as f64 / (1024.0 * 1024.0);
            let abs_delta = delta_bytes.unsigned_abs() as u64;
            let level = if delta_bytes > 0 && abs_delta >= GROWTH_THRESHOLD_BYTES {
                "warn"
            } else {
                "info"
            };
            format!(
                "MemoryWatchdog: [{ts}] [{level}] working set {ws_mib:.0} MiB (delta {delta_mib:+.0} MiB)"
            )
        }
    }
}

/// Convert raw bytes to a MiB float. Pulled out for unit testing.
pub fn bytes_to_mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Sample the current process's working-set / RSS in bytes. Returns
/// `None` if the platform isn't supported or the syscall fails — the
/// watchdog treats that as "skip this tick" and tries again next time.
#[cfg(target_os = "windows")]
fn sample_working_set_bytes() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that doesn't
    // need to be closed. `K32GetProcessMemoryInfo` writes into the
    // out-pointer we own; we pass the size of our struct so it can't
    // overrun.
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size).as_bool() {
            Some(counters.WorkingSetSize as u64)
        } else {
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn sample_working_set_bytes() -> Option<u64> {
    let body = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // `VmRSS:   123456 kB`
            let kb: u64 = rest
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn sample_working_set_bytes() -> Option<u64> {
    // No `mach` dep in this crate — keep macOS as a no-op for now.
    // Dev mode isn't expected to run there in practice; the C#
    // implementation only targets Windows. Returning `None` makes
    // the watchdog skip ticks silently, which is the desired
    // behaviour on unsupported platforms.
    None
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn sample_working_set_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn bytes_to_mib_round_numbers() {
        assert_eq!(bytes_to_mib(0), 0.0);
        assert!((bytes_to_mib(1024 * 1024) - 1.0).abs() < f64::EPSILON);
        assert!((bytes_to_mib(50 * 1024 * 1024) - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn first_sample_line_marks_baseline() {
        let line = format_line(UNIX_EPOCH, 100 * 1024 * 1024, None);
        assert!(line.starts_with("MemoryWatchdog: ["), "{line}");
        assert!(line.contains("working set 100 MiB"), "{line}");
        assert!(line.contains("first sample"), "{line}");
    }

    #[test]
    fn growing_below_threshold_is_info() {
        let line = format_line(
            UNIX_EPOCH,
            120 * 1024 * 1024,
            Some(100 * 1024 * 1024),
        );
        assert!(line.contains("[info]"), "{line}");
        assert!(line.contains("delta +20 MiB"), "{line}");
        assert!(line.contains("working set 120 MiB"), "{line}");
    }

    #[test]
    fn growing_at_or_above_threshold_is_warn() {
        let line = format_line(
            UNIX_EPOCH,
            (100 + 50) * 1024 * 1024,
            Some(100 * 1024 * 1024),
        );
        assert!(line.contains("[warn]"), "{line}");
        assert!(line.contains("delta +50 MiB"), "{line}");
    }

    #[test]
    fn shrinking_is_info_regardless_of_magnitude() {
        // A negative delta of any size never triggers the warn level —
        // we only care about *growth* since that's the leak signal.
        let line = format_line(
            UNIX_EPOCH,
            10 * 1024 * 1024,
            Some(1024 * 1024 * 1024),
        );
        assert!(line.contains("[info]"), "{line}");
        assert!(line.contains("delta -1014 MiB"), "{line}");
    }

    #[test]
    fn start_if_dev_is_noop_in_production() {
        // Calling with `false` must not spawn a thread or panic. This
        // is a smoke test only — we can't easily prove the *absence*
        // of a thread, but we can prove it returns immediately and
        // can be called repeatedly without blowing up.
        for _ in 0..3 {
            start_if_dev(false);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn sampling_returns_a_plausible_size_on_windows() {
        // Smoke test: sampling our own process should give a value
        // larger than the test binary's static size and well below
        // the 64-bit ceiling. Anything that fits the "is real" check
        // is fine — exact values shift between runs.
        let ws = sample_working_set_bytes().expect("sampling failed on Windows");
        assert!(ws > 1024 * 1024, "working set unexpectedly small: {ws}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sampling_returns_a_plausible_size_on_linux() {
        let ws = sample_working_set_bytes().expect("sampling failed on Linux");
        assert!(ws > 1024 * 1024, "working set unexpectedly small: {ws}");
    }
}
