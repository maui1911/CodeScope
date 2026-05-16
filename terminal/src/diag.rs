//! Best-effort diagnostic tape for the resize cascade.
//!
//! The cascade `WindowBounds change → AppShell render → group/pane
//! render → TerminalView canvas-layout → maybe_resize (stage) →
//! debounce timer → apply_resize → Backend::resize` has at least four
//! checkpoints where the chain can quietly break. PR #218/#219
//! plugged one (bounds-observer `cx.notify()` racing
//! `WindowInvalidator::invalidate_view`), but the user-reported
//! "release-build resize doesn't repaint until I tab-swap" symptom
//! still reproduces on `cargo run --release` — and *also* reproduces
//! on splitter drag, which never goes through `observe_window_bounds`
//! at all. So the same race lives on at least one more codepath.
//!
//! Static analysis can't pin which checkpoint is the live one; we
//! need a per-launch tape that records every checkpoint's inputs and
//! outputs in order. This module is that tape: a process-global
//! file handle initialised once from the binary's `main()`, used by
//! `TerminalView` to log canvas-layout, `maybe_resize`, and
//! `apply_resize` entries.
//!
//! File path comes in from `main.rs` (via [`set_log_path`]) so dev
//! mode (`CODESCOPE_DEV=1`) lands the log under
//! `%LOCALAPPDATA%\CodeScope.Dev\terminal-resize.log` and prod under
//! `%LOCALAPPDATA%\CodeScope\terminal-resize.log`. Until `set_log_path`
//! is called the loggers are no-ops — keeps the unit-test surface
//! quiet (no temp-file litter) and lets the terminal crate ship with
//! diagnostics that only light up when the host wires them in.
//!
//! Keep this file lightweight: one open file handle, append-only,
//! no rotation. The resize cascade fires a handful of times per user
//! gesture, not at frame rate, so the I/O overhead is negligible. If
//! the bug ever ships as fixed for real we'll either delete this
//! module or gate it behind a feature flag.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::Mutex;

/// Open file handle for the resize tape. `OnceLock<Mutex<...>>` so
/// `set_log_path` can populate the handle exactly once and `log` can
/// take a mutable borrow for the `write_all` without re-opening per
/// call. `None` inside the mutex means either the path was never set
/// or the open failed; both cases short-circuit `log` to a no-op.
static DIAG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// Configure the resize-diagnostic log file path. Idempotent — the
/// first successful call wins; subsequent calls return without
/// touching state. Caller is the binary's `main()` after
/// `AppPaths::detect()` resolves the per-mode state directory.
///
/// **Rotate-then-truncate.** The resize cascade fires a handful of
/// lines per user gesture, and a long-running install would
/// accumulate megabytes of tape over weeks. Triaging "what happened
/// in *this* repro" gets harder, not easier, the longer the file
/// survives — so each launch starts with an empty tape. We rotate
/// the current file to a `.prev.log` sibling before truncating so
/// the previous launch's tape survives one more run; the natural
/// flow is "hit the bug → close the app → relaunch to grab the
/// log" and a plain truncate would wipe the only forensic artifact
/// at exactly the wrong moment.
///
/// I/O errors at every layer are silently dropped — best-effort
/// instrumentation that must not abort startup. A missing current
/// file (first launch) makes `rename` a no-op-Err; a `rename`
/// failure falls through to the truncate-open; if that itself
/// fails the [`log`] calls below stay no-ops for the rest of the
/// process lifetime.
pub fn set_log_path(path: PathBuf) {
    let cell = DIAG_FILE.get_or_init(|| Mutex::new(None));
    let mut slot = cell.lock();
    if slot.is_some() {
        return;
    }
    if let Some(prev) = prev_path_for(&path) {
        let _ = std::fs::rename(&path, &prev);
    }
    if let Ok(file) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
    {
        *slot = Some(file);
    }
}

/// Build the rotated-out filename next to `path`. Strips a single
/// trailing extension and adds `.prev.<ext>` so
/// `terminal-resize.log` rotates to `terminal-resize.prev.log`. A
/// path with no extension or no file name short-circuits to `None`
/// (caller skips the rename); we'd rather drop the rotation than
/// stomp on something we don't understand.
fn prev_path_for(path: &std::path::Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let ext = path.extension()?.to_str()?;
    let parent = path.parent()?;
    Some(parent.join(format!("{stem}.prev.{ext}")))
}

/// Append one timestamped line to the resize tape. No-op when
/// `set_log_path` was never called (unit-test runs, or any host that
/// doesn't want the diagnostics).
///
/// **Durability strategy:** this module keeps a *single long-lived*
/// file handle open across the entire process lifetime and relies on
/// `write_all` having handed the bytes to the kernel before any
/// crash. A process kill after `write_all` returns still leaves the
/// line on disk because the kernel buffer survives userspace
/// teardown. Note this is a *different* strategy from the boot tape
/// in `src/main.rs`, which open-write-closes per call and gets its
/// durability from `close` flushing the kernel buffer — both achieve
/// similar guarantees against a process kill but via different
/// mechanisms, so don't read this comment and assume the boot tape
/// works the same way.
///
/// Format is `<iso8601> <line>\n` so the file can be piped through
/// `grep` / `awk` and copied into a bug report without reformatting.
pub fn log(line: &str) {
    let Some(cell) = DIAG_FILE.get() else { return };
    let mut slot = cell.lock();
    let Some(file) = slot.as_mut() else { return };
    let entry = format!("{} {}\n", codescope_core::now_iso8601(), line);
    let _ = file.write_all(entry.as_bytes());
}
