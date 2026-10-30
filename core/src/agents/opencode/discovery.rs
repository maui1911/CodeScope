//! OpenCode session adoption — find the session a freshly launched
//! `opencode` writes into `~/.local/share/opencode/opencode.db`.
//!
//! Same `scan` shape as [`crate::agents::claude::discovery`]: pure
//! logic, the caller drives the cadence.
//!
//! # Semantics
//!
//! A `session` row is a candidate when:
//!
//! 1. it is a top-level session (`parent_id IS NULL`) — subagent
//!    sessions belong to their parent's tab;
//! 2. its `time_updated` is at or after `since`;
//! 3. its `directory` canonicalises to the tab's working directory.
//!
//! Tracking already-fired ids is the caller's responsibility. Returns
//! *every* eligible candidate each call so the caller can dedupe by id.
//!
//! # Cost
//!
//! The adoption tick calls [`scan`] for every OpenCode tab every
//! ~350 ms. The session list is cached per database path behind a
//! [`DbStamp`]: a scan whose stamp hasn't moved costs a filter over the
//! cached rows, plus two `stat`s shared by every tab scanned in the same
//! tick ([`db::shared_stamp`]). Only a moved stamp re-runs the query,
//! once, for every tab sharing the database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use crate::agents::opencode::db::{self, DbStamp};
use crate::path_canon::canonicalize_path;

/// Suggested poll interval — 400 ms matches the C#
/// `OpenCodeSessionDiscovery.PollInterval`.
pub const POLL_INTERVAL_MS: u64 = 400;

/// One adoption candidate found in a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// OpenCode session id (`ses_…`).
    pub session_id: String,
    /// The session's `time_updated`: when OpenCode last wrote to it.
    /// There is no per-session file to stat, so this is the recency
    /// signal the adoption tick compares candidates by.
    pub updated_at: SystemTime,
}

/// One top-level session, as cached between scans.
#[derive(Debug, Clone)]
struct SessionRow {
    id: String,
    canon_dir: String,
    updated_ms: i64,
}

/// Cached session list for one database.
#[derive(Default)]
struct DbCache {
    /// Stamp the rows were read at; `None` until the first read.
    stamp: Option<DbStamp>,
    conn: Option<Connection>,
    rows: Vec<SessionRow>,
    /// Queries run against this database — lets the tests prove a
    /// quiet scan never touches SQLite.
    queries: u64,
}

/// Process-wide cache, keyed by database path. In the app there is one
/// entry; tests key theirs by temp dir.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, DbCache>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Scan `<data_root>/opencode.db` for top-level sessions in
/// `working_directory` updated at or after `since`.
///
/// Returns an empty vec when the database doesn't exist, is locked, or
/// holds no eligible session. Callers should treat "empty" as "keep
/// polling".
pub fn scan(
    data_root: &Path,
    working_directory: &str,
    since: SystemTime,
) -> Vec<AdoptionCandidate> {
    let path = db::db_path(data_root);
    let stamp = db::shared_stamp(&path);
    let mut caches = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache = caches.entry(path.clone()).or_default();
    refresh(cache, &path, stamp);

    let canon_cwd = canonicalize_path(working_directory);
    let since_ms = since
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    cache
        .rows
        .iter()
        .filter(|r| r.updated_ms >= since_ms && r.canon_dir == canon_cwd)
        .map(|r| AdoptionCandidate {
            session_id: r.id.clone(),
            updated_at: SystemTime::UNIX_EPOCH
                + Duration::from_millis(u64::try_from(r.updated_ms).unwrap_or(0)),
        })
        .collect()
}

/// Re-read the session list when `stamp` moved since the last read.
fn refresh(cache: &mut DbCache, path: &Path, stamp: DbStamp) {
    if cache.stamp == Some(stamp) {
        return;
    }
    if !stamp.db_exists() {
        // Deleted (or never created): drop the handle to the old file.
        cache.conn = None;
        cache.rows.clear();
        cache.stamp = Some(stamp);
        return;
    }
    cache.queries += 1;
    match read_sessions(cache, path) {
        Ok(rows) => {
            cache.rows = rows;
            cache.stamp = Some(stamp);
        }
        // Locked mid-write: keep the old rows and leave the stamp
        // unrecorded so the next tick tries again.
        Err(err) if db::is_busy(&err) => {}
        Err(err) => {
            // Not an OpenCode schema we know, or a broken file. Record
            // the stamp so this doesn't re-run every tick, and reopen
            // once the file changes.
            eprintln!("[opencode_discovery] reading {}: {err}", path.display());
            cache.conn = None;
            cache.rows.clear();
            cache.stamp = Some(stamp);
        }
    }
}

fn read_sessions(cache: &mut DbCache, path: &Path) -> rusqlite::Result<Vec<SessionRow>> {
    let conn = match cache.conn.take() {
        Some(conn) => conn,
        None => db::open_read_only(path)?,
    };
    let rows = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, directory, time_updated FROM session WHERE parent_id IS NULL",
        )?;
        stmt.query_map([], |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                canon_dir: canonicalize_path(&row.get::<_, String>(1)?),
                updated_ms: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    cache.conn = Some(conn);
    Ok(rows)
}

#[cfg(test)]
fn query_count(data_root: &Path) -> u64 {
    let caches = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    caches.get(&db::db_path(data_root)).map_or(0, |c| c.queries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::opencode::db::fixture::Fixture;

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    fn ids(found: &[AdoptionCandidate]) -> Vec<&str> {
        let mut ids: Vec<&str> = found.iter().map(|c| c.session_id.as_str()).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn scan_returns_empty_when_db_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan(tmp.path(), "C:/repo/alpha", SystemTime::UNIX_EPOCH).is_empty());
        assert!(scan(&tmp.path().join("nonexistent"), "/x", SystemTime::UNIX_EPOCH).is_empty());
    }

    #[test]
    fn scan_finds_session_by_cwd_with_its_update_time() {
        let fx = Fixture::new();
        fx.add_session("ses_a", None, "C:/repo/alpha", "t", 1_000);
        fx.touch_session("ses_a", 5_000);
        let found = scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH);
        assert_eq!(
            found,
            vec![AdoptionCandidate { session_id: "ses_a".into(), updated_at: at(5_000) }]
        );
    }

    #[test]
    fn scan_matches_across_path_spellings() {
        // OpenCode records the native Windows spelling; the tab's cwd
        // may be any of the forms `canonicalize_path` folds together.
        let fx = Fixture::new();
        fx.add_session("ses_a", None, r"C:\repo\Alpha\", "t", 1_000);
        for cwd in ["C:/repo/alpha", "c:\\repo\\alpha", "/c/repo/alpha"] {
            assert_eq!(ids(&scan(fx.root(), cwd, SystemTime::UNIX_EPOCH)), ["ses_a"], "{cwd}");
        }
    }

    #[test]
    fn scan_skips_other_directories() {
        let fx = Fixture::new();
        fx.add_session("ses_a", None, "C:/repo/beta", "t", 1_000);
        assert!(scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH).is_empty());
    }

    #[test]
    fn scan_skips_subagent_sessions() {
        let fx = Fixture::new();
        fx.add_session("ses_parent", None, "C:/repo/alpha", "t", 1_000);
        fx.add_session("ses_child", Some("ses_parent"), "C:/repo/alpha", "t", 2_000);
        assert_eq!(
            ids(&scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH)),
            ["ses_parent"]
        );
    }

    #[test]
    fn scan_skips_sessions_untouched_since_spawn() {
        let fx = Fixture::new();
        fx.add_session("ses_old", None, "C:/repo/alpha", "t", 1_000);
        fx.add_session("ses_new", None, "C:/repo/alpha", "t", 9_000);
        assert_eq!(ids(&scan(fx.root(), "C:/repo/alpha", at(5_000))), ["ses_new"]);
        // Exactly at `since` still counts.
        assert_eq!(ids(&scan(fx.root(), "C:/repo/alpha", at(9_000))), ["ses_new"]);

        // A resumed session becomes eligible once OpenCode writes to it.
        fx.touch_session("ses_old", 6_000);
        assert_eq!(
            ids(&scan(fx.root(), "C:/repo/alpha", at(5_000))),
            ["ses_new", "ses_old"]
        );
    }

    #[test]
    fn quiet_scans_do_not_query() {
        let fx = Fixture::new();
        fx.add_session("ses_a", None, "C:/repo/alpha", "t", 1_000);
        scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH);
        assert_eq!(query_count(fx.root()), 1);

        // Nothing moved: any number of scans, from any tab, are served
        // from the cache.
        for cwd in ["C:/repo/alpha", "C:/repo/beta", "C:/repo/alpha"] {
            scan(fx.root(), cwd, SystemTime::UNIX_EPOCH);
        }
        assert_eq!(query_count(fx.root()), 1);

        // A commit moves the WAL: exactly one re-read, and it sees the
        // new row.
        fx.add_session("ses_b", None, "C:/repo/alpha", "t", 2_000);
        assert_eq!(
            ids(&scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH)),
            ["ses_a", "ses_b"]
        );
        scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH);
        assert_eq!(query_count(fx.root()), 2);
    }

    #[test]
    fn missing_db_is_not_queried_every_tick() {
        let tmp = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            scan(tmp.path(), "C:/repo/alpha", SystemTime::UNIX_EPOCH);
        }
        assert_eq!(query_count(tmp.path()), 0);
    }

    #[test]
    fn scan_never_writes_to_the_db() {
        let fx = Fixture::new();
        fx.add_session("ses_a", None, "C:/repo/alpha", "t", 1_000);
        let path = db::db_path(fx.root());
        let before = std::fs::read(&path).unwrap();
        let stamp = DbStamp::read(&path);
        scan(fx.root(), "C:/repo/alpha", SystemTime::UNIX_EPOCH);
        assert_eq!(DbStamp::read(&path), stamp);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
