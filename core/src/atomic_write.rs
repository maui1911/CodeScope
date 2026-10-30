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
//! per process, so a temp left by a crash is simply truncated and
//! reused by the next save. Nothing ever reads it. A process-wide
//! lock keeps two threads of this process from sharing that temp
//! file; two *processes* never share a store (the single-instance
//! mutex, and dev builds write to `CodeScope.Dev`).
//!
//! **Windows sharing violations.** Replacing a file that another
//! process holds open without `FILE_SHARE_DELETE` (an antivirus scan
//! of the fresh temp file is the usual culprit, an editor sometimes)
//! fails with a sharing violation or access denied. Those holds are
//! short, so the open and the rename retry a few times with backoff
//! (150 ms in total). If the file is still held after that, the save
//! fails *safely*: the temp file is removed, the old target stays
//! intact, and the caller gets the error — every caller already logs
//! or toasts a failed save and keeps its in-memory state consistent
//! with disk. We deliberately do not fall back to an in-place write:
//! that is exactly the truncate-then-write window this module exists
//! to close. Rust's own readers (`std::fs::read`) share delete, so
//! CodeScope's loads never block a save.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Sleeps between attempts of an open/rename that hit a transient
/// sharing violation. One more attempt follows the last sleep.
const RETRY_DELAYS_MS: [u64; 4] = [10, 20, 40, 80];

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
    let result = write_temp(&tmp, fill).and_then(|()| retry_transient(|| fs::rename(&tmp, path)));
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

/// Write the temp file (truncating any leftover) and flush it to disk
/// before it can be renamed into place.
fn write_temp(tmp: &Path, fill: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let mut file = retry_transient(|| {
        OpenOptions::new().write(true).create(true).truncate(true).open(tmp)
    })?;
    fill(&mut file)?;
    file.sync_all()
}

/// Run `op`, retrying with backoff while it fails with an error that a
/// short-lived handle in another process explains.
fn retry_transient<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    for delay in RETRY_DELAYS_MS {
        match op() {
            Err(err) if is_transient(&err) => std::thread::sleep(Duration::from_millis(delay)),
            other => return other,
        }
    }
    op()
}

/// Access denied, or on Windows `ERROR_SHARING_VIOLATION` (32) /
/// `ERROR_LOCK_VIOLATION` (33): another process has the file open in
/// a way that blocks replacing it.
fn is_transient(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33)))
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
}
