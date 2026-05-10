//! Crash log writer + panic hook.
//!
//! Mirrors `App.xaml.cs::LogFatal` in the C# build, which appends a
//! single line per fatal event to `%LOCALAPPDATA%\CodeScope\crash.log`
//! (and `CodeScope.Dev\crash.log` in dev mode):
//!
//! ```text
//! [<DateTime.Now:O>] <source>: <exception>
//! ```
//!
//! C# wires it into three fault sources:
//!   * `AppDomain.CurrentDomain.UnhandledException` (managed)
//!   * `Application.DispatcherUnhandledException` (UI thread)
//!   * `TaskScheduler.UnobservedTaskException` (background tasks)
//!
//! Rust has neither dispatcher nor task-scheduler hooks, but
//! [`std::panic::set_hook`] catches every Rust panic — which is the
//! analogue of an unhandled managed exception. We register one process-
//! wide hook in [`install_panic_hook`] that writes a multi-line record
//! per panic so the post-mortem is more useful than the C# single-line
//! shape (file is still append-only and human-readable).
//!
//! ### Format
//!
//! ```text
//! [<UTC ISO-8601>] panic: <message>
//!   version : <CODESCOPE_VERSION_DISPLAY>
//!   target  : <os>/<arch>
//!   thread  : <thread name or "<unnamed>">
//!   location: <file>:<line>:<col>     (or "<unknown>" if missing)
//!   backtrace:
//!     <captured backtrace, each line indented>
//! ```
//!
//! Records are separated by a blank line. Append-only — the file grows
//! over time but each entry is self-contained, which matches the C#
//! behaviour. Rotation isn't implemented in C# either, so we don't add
//! it here (parity rule: don't invent flows).
//!
//! ### Dev-mode separation
//!
//! Path resolution goes through [`AppPaths`], which already routes
//! `CODESCOPE_DEV=1` to the `CodeScope.Dev` sibling folder. No extra
//! logic here — every consumer of `state_dir` gets dev-mode for free.

use std::backtrace::Backtrace;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::AppPaths;

/// Filename written under [`AppPaths::state_dir`]. Mirrors C# verbatim
/// so a side-by-side install ends up overwriting/appending the same
/// file as the previous build would have.
pub const CRASH_LOG_FILENAME: &str = "crash.log";

/// Resolve `<state_dir>/crash.log`. The state dir is dev-aware via
/// [`AppPaths`].
pub fn crash_log_path(paths: &AppPaths) -> PathBuf {
    paths.state_dir.join(CRASH_LOG_FILENAME)
}

/// Render a single crash record. Pulled out of [`install_panic_hook`]
/// so it can be unit-tested without poking the process-global panic
/// hook (which is set-once and shared across the whole test binary).
pub fn format_record(
    now: SystemTime,
    version: &str,
    thread_name: Option<&str>,
    payload: &str,
    location: Option<(&str, u32, u32)>,
    backtrace: &str,
) -> String {
    let mut out = String::with_capacity(512);
    out.push('[');
    out.push_str(&format_iso8601_utc(now));
    out.push_str("] panic: ");
    out.push_str(payload);
    out.push('\n');
    out.push_str("  version : ");
    out.push_str(version);
    out.push('\n');
    out.push_str("  target  : ");
    out.push_str(std::env::consts::OS);
    out.push('/');
    out.push_str(std::env::consts::ARCH);
    out.push('\n');
    out.push_str("  thread  : ");
    out.push_str(thread_name.unwrap_or("<unnamed>"));
    out.push('\n');
    out.push_str("  location: ");
    match location {
        Some((file, line, col)) => {
            out.push_str(file);
            out.push(':');
            out.push_str(&line.to_string());
            out.push(':');
            out.push_str(&col.to_string());
        }
        None => out.push_str("<unknown>"),
    }
    out.push('\n');
    out.push_str("  backtrace:\n");
    if backtrace.is_empty() {
        out.push_str("    <unavailable>\n");
    } else {
        for line in backtrace.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    // Blank separator between records so a tail of multiple panics is
    // visually scannable.
    out.push('\n');
    out
}

/// Append `record` to the crash log at `path`. Creates parent dirs as
/// needed. Errors are intentionally swallowed at the call site (a
/// panic hook that itself fails should not abort the process further);
/// returning `io::Result` here lets tests assert success.
pub fn append_record(path: &Path, record: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(record.as_bytes())?;
    Ok(())
}

/// Install the process-wide panic hook. Idempotent — calling twice is
/// a no-op (the second call is silently ignored). The hook chains the
/// previous hook so `RUST_BACKTRACE=1` console output still works.
///
/// `version` is typically the binary's `env!("CODESCOPE_VERSION_DISPLAY")`.
/// We take it as a parameter because `core` is a workspace library and
/// has no `build.rs` of its own — the env var is baked into the binary
/// crate, not here.
pub fn install_panic_hook(paths: AppPaths, version: String) {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = panic_payload_string(info);
        let location = info.location().map(|l| (l.file(), l.line(), l.column()));
        let thread = std::thread::current();
        let thread_name = thread.name();
        // `Backtrace::force_capture` always gives us frames regardless
        // of `RUST_BACKTRACE`. A crash log without a stack trace is
        // not very useful and the cost is paid only on panic.
        let bt = Backtrace::force_capture().to_string();
        let record = format_record(
            SystemTime::now(),
            &version,
            thread_name,
            &payload,
            location,
            &bt,
        );
        let path = crash_log_path(&paths);
        if let Err(err) = append_record(&path, &record) {
            // Last-resort surface — stderr only, nothing else available.
            eprintln!(
                "[crash_log] failed to write {}: {}",
                path.display(),
                err
            );
        }
        // Chain so the default printer (or the user's custom hook) still runs.
        previous(info);
    }));
}

fn panic_payload_string(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Minimal ISO-8601 UTC formatter (`YYYY-MM-DDTHH:MM:SS.fffZ`). Avoids
/// pulling `chrono`/`time` into `core` for a single timestamp render.
/// Algorithm: civil-from-days, per Howard Hinnant's date library.
fn format_iso8601_utc(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // civil_from_days: shift epoch so era-day-of-era arithmetic stays
    // non-negative. Reference:
    // https://howardhinnant.github.io/date_algorithms.html#civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, m, d, hour, minute, second, millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn iso8601_known_epoch() {
        // 2024-01-15T12:34:56.789Z = 1705321996.789 since epoch.
        let t = UNIX_EPOCH + Duration::from_millis(1_705_322_096_789);
        assert_eq!(format_iso8601_utc(t), "2024-01-15T12:34:56.789Z");
    }

    #[test]
    fn iso8601_unix_zero() {
        assert_eq!(format_iso8601_utc(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn record_contains_required_fields() {
        let now = UNIX_EPOCH + Duration::from_millis(1_705_322_096_789);
        let rec = format_record(
            now,
            "1.2.3-abc",
            Some("worker-7"),
            "boom",
            Some(("src/main.rs", 42, 9)),
            "frame 0: foo\nframe 1: bar",
        );
        assert!(rec.contains("[2024-01-15T12:34:56.789Z] panic: boom"), "{rec}");
        assert!(rec.contains("version : 1.2.3-abc"), "{rec}");
        assert!(rec.contains("thread  : worker-7"), "{rec}");
        assert!(rec.contains("location: src/main.rs:42:9"), "{rec}");
        assert!(rec.contains("    frame 0: foo"), "{rec}");
        assert!(rec.contains("    frame 1: bar"), "{rec}");
        // Trailing blank line separator.
        assert!(rec.ends_with("\n\n"), "{rec}");
    }

    #[test]
    fn record_handles_missing_location_and_thread() {
        let rec = format_record(UNIX_EPOCH, "0.0", None, "boom", None, "");
        assert!(rec.contains("thread  : <unnamed>"), "{rec}");
        assert!(rec.contains("location: <unknown>"), "{rec}");
        assert!(rec.contains("    <unavailable>"), "{rec}");
    }

    #[test]
    fn append_creates_file_and_grows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("crash.log");
        append_record(&path, "first\n").unwrap();
        append_record(&path, "second\n").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "first\nsecond\n");
    }

    #[test]
    fn path_honours_dev_split() {
        let dir = tempfile::tempdir().unwrap();
        let prod = crate::paths::rooted_for_tests(false, dir.path());
        let dev = crate::paths::rooted_for_tests(true, dir.path());

        let prod_path = crash_log_path(&prod);
        let dev_path = crash_log_path(&dev);

        assert!(prod_path.ends_with("CodeScope/crash.log") || prod_path.ends_with("CodeScope\\crash.log"));
        assert!(dev_path.ends_with("CodeScope.Dev/crash.log") || dev_path.ends_with("CodeScope.Dev\\crash.log"));
        assert_ne!(prod_path, dev_path);
    }
}
