//! Cached directory listings for discovery scans that run every
//! ~350 ms per tab over directories that rarely change.
//!
//! Creating, deleting or renaming an entry bumps a directory's mtime;
//! appending to a file inside it does not (checked on NTFS; POSIX
//! filesystems behave the same). So a listing stays valid for as long
//! as the directory's mtime does, and refreshing it costs one `stat`
//! instead of a `read_dir`. Callers that scan the same directory within
//! [`STAT_REUSE`] of each other — several tabs in one discovery tick —
//! share that `stat` too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a directory's `stat` is trusted before it is repeated. New
/// files show up in scans at most this late.
pub(crate) const STAT_REUSE: Duration = Duration::from_millis(1000);

/// A listing whose directory changed this recently is re-read even if
/// its mtime looks unchanged: two entries created within one mtime tick
/// would otherwise hide the second.
const SETTLE: Duration = Duration::from_secs(2);

/// Directories kept before the cache is cleared.
const MAX_DIRS: usize = 256;

/// One file directly inside a listed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedFile {
    pub name: String,
    pub path: PathBuf,
    /// Creation time, or the mtime where the platform has none. Never
    /// changes for a given file, which is what makes it cacheable.
    pub born: Option<SystemTime>,
}

struct Entry {
    checked: Instant,
    mtime: Option<SystemTime>,
    files: Arc<Vec<ListedFile>>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The regular files directly inside `dir`; empty when it is missing.
pub(crate) fn list_files(dir: &Path) -> Arc<Vec<ListedFile>> {
    list_files_at(dir, Instant::now(), SystemTime::now())
}

fn list_files_at(dir: &Path, now: Instant, wall: SystemTime) -> Arc<Vec<ListedFile>> {
    let Ok(mut map) = cache().lock() else { return Arc::new(read(dir)) };
    if let Some(e) = map.get(dir)
        && now.saturating_duration_since(e.checked) < STAT_REUSE
    {
        return e.files.clone();
    }
    let mtime = std::fs::metadata(dir).ok().and_then(|m| m.modified().ok());
    if let Some(e) = map.get_mut(dir) {
        let settled = mtime.is_some_and(|t| wall.duration_since(t).is_ok_and(|age| age >= SETTLE));
        if e.mtime == mtime && (settled || mtime.is_none()) {
            e.checked = now;
            return e.files.clone();
        }
    }
    let files = Arc::new(if mtime.is_some() { read(dir) } else { Vec::new() });
    if map.len() >= MAX_DIRS {
        map.clear();
    }
    map.insert(dir.to_path_buf(), Entry { checked: now, mtime, files: files.clone() });
    files
}

/// Drop `dir`'s cached listing, so a test that changes a directory
/// right after listing it needn't wait out [`STAT_REUSE`].
#[cfg(test)]
pub(crate) fn forget(dir: &Path) {
    if let Ok(mut map) = cache().lock() {
        map.remove(dir);
    }
}

fn read(dir: &Path) -> Vec<ListedFile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok();
            let born = meta
                .as_ref()
                .and_then(|m| m.created().ok().or_else(|| m.modified().ok()));
            Some(ListedFile { name, path: e.path(), born })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn names(files: &[ListedFile]) -> Vec<&str> {
        let mut v: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn lists_files_but_not_subdirectories() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.jsonl"), "").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        assert_eq!(names(&list_files(tmp.path())), vec!["a.jsonl"]);
    }

    #[test]
    fn missing_directory_lists_nothing_and_appears_later() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("later");
        let t0 = Instant::now();
        assert!(list_files_at(&dir, t0, SystemTime::now()).is_empty());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.jsonl"), "").unwrap();
        let files = list_files_at(&dir, t0 + STAT_REUSE, SystemTime::now());
        assert_eq!(names(&files), vec!["a.jsonl"]);
    }

    #[test]
    fn stat_is_reused_within_the_window_then_new_files_show() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.jsonl"), "").unwrap();
        let t0 = Instant::now();
        assert_eq!(list_files_at(tmp.path(), t0, SystemTime::now()).len(), 1);
        std::fs::write(tmp.path().join("b.jsonl"), "").unwrap();
        let soon = t0 + STAT_REUSE / 2;
        assert_eq!(list_files_at(tmp.path(), soon, SystemTime::now()).len(), 1);
        let later = t0 + STAT_REUSE;
        assert_eq!(list_files_at(tmp.path(), later, SystemTime::now()).len(), 2);
    }

    #[test]
    fn settled_unchanged_directory_is_not_reread() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.jsonl"), "").unwrap();
        let t0 = Instant::now();
        let first = list_files_at(tmp.path(), t0, SystemTime::now());
        // Pretend the listing is old news: the directory last changed
        // well before the settle window.
        let wall = SystemTime::now() + SETTLE * 2;
        let second = list_files_at(tmp.path(), t0 + STAT_REUSE, wall);
        assert!(Arc::ptr_eq(&first, &second), "served from cache");
    }
}
