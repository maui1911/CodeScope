//! Crash-safe replacement of CodeScope's on-disk state files
//! (`projects.json`, `settings.json`, `layout.json`, `window.json`).
//!
//! A plain `std::fs::write` truncates the target and then writes it,
//! so a crash, kill or power loss in between leaves an empty or
//! partial file — and an empty `projects.json` loads as "no projects"
//! (#391). [`write_atomic`] instead writes a sibling temp file
//! (`<name>.tmp`, same directory so the rename never crosses a
//! volume), syncs it, and renames it over the target. Rename replaces
//! an existing file on every platform we ship (`MoveFileEx` with
//! replace-existing on Windows), so a reader sees either the old
//! file or the new one, never a mix.
//!
//! **Leftover temp files.** The temp name is fixed per target, not
//! per process, so a temp left by a crash is deleted and recreated by
//! the next save — even a read-only one, which would otherwise make
//! every later save of that file fail. Nothing ever reads it. A
//! process-wide lock keeps two threads of this process from sharing
//! that temp file; two *processes* never share a store (the
//! single-instance mutex, and dev builds write to `CodeScope.Dev`).
//!
//! **Symlinks.** The rename replaces the directory entry, so a state
//! file that is a symlink (say, into a dotfiles repo) becomes a
//! regular file on the first save; the link's target is left as it
//! was. That is the price of atomicity — writing through the link
//! would be the in-place write again.
//!
//! **Windows sharing violations.** Replacing a file that another
//! process holds open without `FILE_SHARE_DELETE` (an antivirus scan
//! of the fresh temp file is the usual culprit, an editor sometimes)
//! fails with a sharing violation, or access denied while the file is
//! pending delete. Those holds are short, so the temp-file open and
//! the rename retry with backoff, drawing on one budget per
//! [`write_atomic`] call: 150 ms of sleep at most, however the
//! failures split between the two. Access denied on a *read-only*
//! file is permanent and fails at once rather than stalling every
//! save. On other platforms an open handle never blocks a rename and
//! `EACCES` is permanent (permissions), so nothing is retried there.
//! If the file is still held when the budget runs out, the save fails
//! *safely*: the temp file is removed, the old target stays intact,
//! and the caller gets the error — every caller already logs or
//! toasts a failed save and keeps its in-memory state consistent with
//! disk. We deliberately do not fall back to an in-place write: that
//! is exactly the truncate-then-write window this module exists to
//! close. Rust's own readers (`std::fs::read`) share delete, so
//! CodeScope's loads never block a save.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Sleeps between attempts that hit a transient sharing violation,
/// shared by every step of one [`write_atomic`] call (150 ms total).
static RETRY_DELAYS_MS: [u64; 4] = [10, 20, 40, 80];

/// Serialises writers in this process so two threads saving the same
/// file never interleave inside its shared temp file.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Replace `path` with `contents` atomically: after a crash at any
/// point, `path` holds either its previous content or `contents`.
/// The parent directory must exist.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomic_with(path, |file| file.write_all(contents))
}

/// [`write_atomic`] with the temp file's body written by `fill`, so
/// tests can make the write fail halfway.
fn write_atomic_with(
    path: &Path,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let tmp = temp_path(path)?;
    // A panic while holding the lock leaves no state behind worth
    // protecting (the temp file is rewritten from scratch), so a
    // poisoned lock is still usable.
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut backoff = Backoff::new();
    let result = write_temp(&tmp, fill, &mut backoff)
        .and_then(|()| backoff.retry(path, || fs::rename(&tmp, path)));
    if result.is_err() {
        // Best effort: a temp we can't remove is reclaimed by the
        // next save of the same file.
        let _ = fs::remove_file(&tmp);
    }
    result?;
    sync_parent_dir(path);
    Ok(())
}

/// `<dir>/<name>.tmp` for `<dir>/<name>`.
fn temp_path(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("no file name in {}", path.display()))
    })?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".tmp");
    Ok(path.with_file_name(tmp_name))
}

/// Write the temp file and flush it to disk before it can be renamed
/// into place.
fn write_temp(
    tmp: &Path,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
    backoff: &mut Backoff,
) -> io::Result<()> {
    remove_leftover(tmp);
    let mut file = backoff.retry(tmp, || {
        OpenOptions::new().write(true).create(true).truncate(true).open(tmp)
    })?;
    fill(&mut file)?;
    file.sync_all()
}

/// Delete a temp file an earlier crash left behind. Deleting rather
/// than truncating it also gets past a read-only leftover, which no
/// open-for-write would. Best effort: if it fails, the open reports
/// the real problem.
fn remove_leftover(tmp: &Path) {
    let Ok(meta) = fs::symlink_metadata(tmp) else {
        return; // The usual case: no leftover.
    };
    // Windows refuses to delete a file with the read-only attribute;
    // Unix only needs the directory to be writable.
    #[cfg(windows)]
    if meta.permissions().readonly() {
        let mut perms = meta.permissions();
        // Only clears FILE_ATTRIBUTE_READONLY on Windows, so the lint's
        // "world-writable" concern (a Unix mode) doesn't apply.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(tmp, perms);
    }
    #[cfg(not(windows))]
    let _ = meta;
    let _ = fs::remove_file(tmp);
}

/// The retry budget of one [`write_atomic`] call. Each step draws on
/// the same schedule, so the call as a whole sleeps 150 ms at most.
struct Backoff {
    delays: std::slice::Iter<'static, u64>,
}

impl Backoff {
    fn new() -> Self {
        Self { delays: RETRY_DELAYS_MS.iter() }
    }

    /// Run `op` on `path`, retrying while it fails transiently and the
    /// budget lasts.
    fn retry<T>(&mut self, path: &Path, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        loop {
            match op() {
                Err(err) if is_transient(&err, path) => match self.delays.next() {
                    Some(&ms) => std::thread::sleep(Duration::from_millis(ms)),
                    None => return Err(err),
                },
                other => return other,
            }
        }
    }
}

/// Whether `err` from opening or replacing `path` is another process's
/// short-lived handle: `ERROR_SHARING_VIOLATION` (32),
/// `ERROR_LOCK_VIOLATION` (33), or `ERROR_ACCESS_DENIED` (5) while the
/// file is pending delete. Access denied on a read-only file is
/// permanent.
#[cfg(windows)]
fn is_transient(err: &io::Error, path: &Path) -> bool {
    match err.raw_os_error() {
        Some(32 | 33) => true,
        Some(5) => !fs::metadata(path).is_ok_and(|meta| meta.permissions().readonly()),
        _ => false,
    }
}

/// Elsewhere an open handle never blocks a rename, and `EACCES` is a
/// permission problem no wait fixes.
#[cfg(not(windows))]
fn is_transient(_err: &io::Error, _path: &Path) -> bool {
    false
}

/// Make the rename itself durable. On Unix the new directory entry
/// lives in the parent directory, which needs its own fsync; NTFS
/// journals the rename, and Windows can't open a directory as a file
/// anyway. Best effort: the data is already safe either way.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_of(path: &Path) -> std::path::PathBuf {
        let mut name = path.file_name().unwrap().to_os_string();
        name.push(".tmp");
        path.with_file_name(name)
    }

    #[test]
    fn round_trip_creates_the_file_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, b"{\"a\":1}\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}\n");
        assert!(!temp_of(&path).exists());
    }

    #[test]
    fn replaces_existing_content_wholesale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"a much longer old body than the new one\n").unwrap();
        write_atomic(&path, b"new\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\n");
        assert!(!temp_of(&path).exists());
    }

    #[test]
    fn failed_write_keeps_old_content_and_removes_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        fs::write(&path, b"{\"version\":1,\"projects\":[]}\n").unwrap();
        // A write that dies halfway: some bytes land, then an error,
        // as a full disk or a crash mid-write would leave things.
        let err = write_atomic_with(&path, |file| {
            file.write_all(b"{\"vers")?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        assert_eq!(fs::read(&path).unwrap(), b"{\"version\":1,\"projects\":[]}\n");
        assert!(!temp_of(&path).exists());
    }

    #[test]
    fn leftover_temp_from_a_crash_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        fs::write(&path, b"old\n").unwrap();
        // An earlier process died between writing the temp file and
        // renaming it: a stale, partial temp sits next to the target.
        fs::write(temp_of(&path), b"partial garbage that is longer than new").unwrap();
        write_atomic(&path, b"new\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\n");
        assert!(!temp_of(&path).exists());
    }

    /// A leftover temp that ended up read-only (copied from a
    /// read-only target, restored from a backup) must not block every
    /// later save of its file.
    #[test]
    fn read_only_leftover_temp_does_not_block_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"old\n").unwrap();
        let tmp = temp_of(&path);
        fs::write(&tmp, b"stale").unwrap();
        let mut perms = fs::metadata(&tmp).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&tmp, perms).unwrap();
        write_atomic(&path, b"new\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\n");
        assert!(!tmp.exists());
    }

    /// Open `path` the way a scanner or editor that does not allow
    /// delete-sharing would: replacing it fails until the handle goes.
    #[cfg(windows)]
    fn hold_without_delete_share(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn rename_retries_while_the_target_is_briefly_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        fs::write(&path, b"old\n").unwrap();
        let holder = hold_without_delete_share(&path);
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            drop(holder);
        });
        write_atomic(&path, b"new\n").unwrap();
        release.join().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\n");
        assert!(!temp_of(&path).exists());
    }

    #[cfg(windows)]
    #[test]
    fn target_held_throughout_fails_safely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        fs::write(&path, b"old\n").unwrap();
        let holder = hold_without_delete_share(&path);
        assert!(write_atomic(&path, b"new\n").is_err());
        drop(holder);
        assert_eq!(fs::read(&path).unwrap(), b"old\n");
        assert!(!temp_of(&path).exists());
    }

    /// A read-only target refuses the replace with access denied, which
    /// no wait fixes: fail at once instead of stalling every save.
    #[cfg(windows)]
    #[test]
    fn read_only_target_fails_without_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        fs::write(&path, b"old\n").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).unwrap();
        // Checked directly rather than by timing the save: a wall-clock
        // bound flakes when the test runs alongside the rest of the suite.
        assert!(!is_transient(&io::Error::from_raw_os_error(5), &path));
        assert!(write_atomic(&path, b"new\n").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old\n");
        assert!(!temp_of(&path).exists());
    }

    /// One budget per `write_atomic` call: once the open leg used it
    /// up, the rename leg gets a single attempt, not a fresh 150 ms.
    #[cfg(windows)]
    #[test]
    fn retry_budget_is_shared_across_legs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let mut backoff = Backoff::new();
        let mut calls = 0;
        let mut sharing_violation = || -> io::Result<()> {
            calls += 1;
            Err(io::Error::from_raw_os_error(32))
        };
        assert!(backoff.retry(&path, &mut sharing_violation).is_err());
        assert!(backoff.retry(&path, &mut sharing_violation).is_err());
        assert_eq!(calls, RETRY_DELAYS_MS.len() + 2);
    }
}
