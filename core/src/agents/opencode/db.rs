//! Shared access to OpenCode's SQLite store, `<data_root>/opencode.db`.
//!
//! OpenCode 1.x keeps every session, message and message part in one
//! WAL-mode SQLite database (it used to write one JSON file per
//! message). Both the adoption scan ([`super::discovery`]) and the
//! per-tab tail ([`super::telemetry`]) read it through a [`DbHandle`],
//! under the same rules (ADR-0025):
//!
//! * **Read-only.** Connections open with `SQLITE_OPEN_READ_ONLY` and a
//!   zero busy timeout. A locked database is "nothing new this tick",
//!   never a wait on the UI thread.
//! * **Probe before query.** `PRAGMA data_version` on the kept
//!   connection changes whenever another connection commits. It reads
//!   the WAL-index header only, no table pages, so a tick with nothing
//!   new costs a few µs. File stats can't do this job: after a
//!   checkpoint SQLite rewrites the WAL from offset 0, so its length
//!   stops moving, and two commits often share one mtime tick.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, OpenFlags};

/// File name of OpenCode's database under its data root.
pub const DB_FILE_NAME: &str = "opencode.db";

/// `<data_root>/opencode.db`.
pub fn db_path(data_root: &Path) -> PathBuf {
    data_root.join(DB_FILE_NAME)
}

/// Identity of the database file between ticks: that it exists, and on
/// Unix which inode it is, so a file deleted and recreated under the
/// same name is reopened instead of read through the old, unlinked
/// inode forever. Windows needs only existence: while a connection holds
/// the file open, deleting or renaming it fails with a sharing
/// violation, so it can't be replaced underneath us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

fn file_id(path: &Path) -> Option<FileId> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileId { dev: meta.dev(), ino: meta.ino() })
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Some(FileId {})
    }
}

/// How long one caller's [`FileId`] is reused by the next. All OpenCode
/// tails poll in the same telemetry tick and all OpenCode tabs scan in
/// the same adoption tick, so within a tick the file is stat'ed once
/// rather than once per tab (a `stat` is ~20 µs on Windows). Only
/// existence and identity ride on it; change detection is
/// `data_version`, per connection, every time.
pub const FILE_ID_REUSE_WINDOW: Duration = Duration::from_millis(50);

/// When each path was last stat'ed, and what that found.
type RecentIds = HashMap<PathBuf, (Instant, Option<FileId>)>;

static RECENT_IDS: LazyLock<Mutex<RecentIds>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// [`file_id`], reusing one read for the same path less than
/// [`file_id_reuse`] ago.
fn shared_file_id(path: &Path) -> Option<FileId> {
    let now = Instant::now();
    let mut recent = RECENT_IDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((at, id)) = recent.get_mut(path) {
        if now.duration_since(*at) >= file_id_reuse() {
            *id = file_id(path);
            *at = now;
        }
        return *id;
    }
    let id = file_id(path);
    recent.insert(path.to_path_buf(), (now, id));
    id
}

#[cfg(not(test))]
fn file_id_reuse() -> Duration {
    FILE_ID_REUSE_WINDOW
}

// Tests create and delete files back to back, so they stat every time
// unless a test opts into the window.
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_FILE_ID_REUSE: std::cell::Cell<Duration> =
        const { std::cell::Cell::new(Duration::ZERO) };
}

#[cfg(test)]
fn file_id_reuse() -> Duration {
    TEST_FILE_ID_REUSE.with(|w| w.get())
}

/// Expire every shared [`FileId`], as the gap between two real ticks does.
#[cfg(test)]
pub(crate) fn forget_shared_file_ids() {
    RECENT_IDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
}

/// Open `path` strictly read-only. No URI parsing, no create, and a
/// zero busy timeout: rusqlite's default would block the caller for up
/// to 5 s while OpenCode holds a lock.
pub fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::ZERO)?;
    Ok(conn)
}

/// `SQLITE_BUSY` / `SQLITE_LOCKED`: OpenCode is mid-write (or mid
/// recovery). Callers retry on the next tick without recording the
/// version they probed, so the retry isn't gated away.
pub fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// How long to wait before retrying an open that failed for a reason
/// other than a lock (unreadable file, not a database).
pub const OPEN_RETRY: Duration = Duration::from_secs(2);

/// What [`DbHandle::probe`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// `opencode.db` doesn't exist.
    Missing,
    /// Nothing committed since the last [`DbHandle::mark_read`].
    Unchanged,
    /// Something was committed; read, then `mark_read` this version.
    Changed(i64),
}

/// One kept read-only connection plus the change detector around it.
///
/// The loop each tick: [`probe`](Self::probe); on
/// [`Probe::Changed`] run the queries through [`conn`](Self::conn) and,
/// once they succeed (or fail for good), [`mark_read`](Self::mark_read)
/// the probed version. A busy read leaves it unmarked, so the next
/// probe says `Changed` again.
#[derive(Debug)]
pub struct DbHandle {
    path: PathBuf,
    conn: Option<Connection>,
    /// Identity of the file `conn` was opened on.
    file: Option<FileId>,
    /// `data_version` the last completed read saw.
    read_version: Option<i64>,
    open_failed_at: Option<Instant>,
    /// Last error printed, so a persistent one is printed once rather
    /// than on every commit OpenCode makes.
    last_logged: Option<String>,
}

impl DbHandle {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            conn: None,
            file: None,
            read_version: None,
            open_failed_at: None,
            last_logged: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Has anything been committed since the last read? One shared
    /// `stat` per tick (existence, and the inode on Unix), opening the
    /// connection if needed, then `PRAGMA data_version`.
    pub fn probe(&mut self) -> rusqlite::Result<Probe> {
        let Some(id) = shared_file_id(&self.path) else {
            self.conn = None;
            self.file = None;
            self.read_version = None;
            return Ok(Probe::Missing);
        };
        if self.file != Some(id) {
            // Replaced (or first sight): whatever we read came from
            // another file.
            self.conn = None;
            self.read_version = None;
        }
        let conn = match self.conn.take() {
            Some(conn) => conn,
            None => {
                if self.open_failed_at.is_some_and(|at| at.elapsed() < OPEN_RETRY) {
                    return Ok(Probe::Unchanged);
                }
                match open_read_only(&self.path) {
                    Ok(conn) => {
                        self.open_failed_at = None;
                        conn
                    }
                    Err(err) => {
                        self.open_failed_at = Some(Instant::now());
                        return Err(err);
                    }
                }
            }
        };
        let version = conn
            .prepare_cached("PRAGMA data_version")
            .and_then(|mut stmt| stmt.query_row([], |r| r.get::<_, i64>(0)));
        self.conn = Some(conn);
        self.file = Some(id);
        let version = version?;
        Ok(if self.read_version == Some(version) {
            Probe::Unchanged
        } else {
            Probe::Changed(version)
        })
    }

    /// The open connection, after a [`Probe::Changed`].
    pub fn conn(&self) -> Option<&Connection> {
        self.conn.as_ref()
    }

    /// Record that everything up to `version` has been read.
    pub fn mark_read(&mut self, version: i64) {
        self.read_version = Some(version);
    }

    /// Print `err` unless it is the same error printed last time.
    pub fn log_error(&mut self, tag: &str, err: &rusqlite::Error) {
        let msg = err.to_string();
        if self.last_logged.as_deref() != Some(msg.as_str()) {
            eprintln!("[{tag}] reading {}: {msg}", self.path.display());
            self.last_logged = Some(msg);
        }
    }
}

/// `true` for the title OpenCode gives a session before it has
/// generated one: `New session - <ISO-8601 ms>` (`Child session - …`
/// for subagents), from `createDefaultTitle` in OpenCode's
/// `session.ts`. Newer OpenCode UIs shorten that to the bare prefix,
/// so the bare labels count too.
pub fn is_placeholder_title(title: &str) -> bool {
    let t = title.trim();
    if t == "New session" || t == "Child session" {
        return true;
    }
    let Some(stamp) = t
        .strip_prefix("New session - ")
        .or_else(|| t.strip_prefix("Child session - "))
    else {
        return false;
    };
    // `Date.toISOString()`: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    const SHAPE: &[u8] = b"dddd-dd-ddTdd:dd:dd.dddZ";
    stamp.len() == SHAPE.len()
        && stamp
            .bytes()
            .zip(SHAPE)
            .all(|(c, &p)| if p == b'd' { c.is_ascii_digit() } else { c == p })
}

/// Fixture databases with OpenCode's schema, for the discovery and
/// telemetry tests.
#[cfg(test)]
pub(crate) mod fixture {
    use std::path::Path;

    use rusqlite::{Connection, params};
    use serde_json::{Value, json};

    /// The subset of OpenCode 1.4's schema the readers touch, column
    /// for column, with the same index on `message`.
    const SCHEMA: &str = "
        CREATE TABLE session (
            id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
            slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
            version text NOT NULL, time_created integer NOT NULL,
            time_updated integer NOT NULL, time_compacting integer,
            time_archived integer, workspace_id text);
        CREATE TABLE message (
            id text PRIMARY KEY, session_id text NOT NULL,
            time_created integer NOT NULL, time_updated integer NOT NULL,
            data text NOT NULL);
        CREATE INDEX message_session_time_created_id_idx
            ON message (session_id, time_created, id);
        CREATE TABLE part (
            id text PRIMARY KEY, message_id text NOT NULL,
            session_id text NOT NULL, time_created integer NOT NULL,
            time_updated integer NOT NULL, data text NOT NULL);
        CREATE INDEX part_message_id_id_idx ON part (message_id, id);
    ";

    /// A WAL-mode `opencode.db` in a temp dir, plus the writer
    /// connection standing in for OpenCode.
    pub struct Fixture {
        pub dir: tempfile::TempDir,
        pub conn: Connection,
    }

    impl Fixture {
        pub fn new() -> Self {
            Self::in_dir(tempfile::tempdir().unwrap())
        }

        /// Create the database in an existing (empty) temp dir.
        pub fn in_dir(dir: tempfile::TempDir) -> Self {
            let conn = Connection::open(super::db_path(dir.path())).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            Self { dir, conn }
        }

        /// The data root, i.e. the directory holding `opencode.db`.
        pub fn root(&self) -> &Path {
            self.dir.path()
        }

        pub fn add_session(
            &self,
            id: &str,
            parent: Option<&str>,
            directory: &str,
            title: &str,
            created_ms: i64,
        ) {
            self.conn
                .execute(
                    "INSERT INTO session (id, project_id, parent_id, slug, directory, title,
                        version, time_created, time_updated)
                     VALUES (?1, 'proj', ?2, 'slug', ?3, ?4, '1.4.7', ?5, ?5)",
                    params![id, parent, directory, title, created_ms],
                )
                .unwrap();
        }

        pub fn touch_session(&self, id: &str, updated_ms: i64) {
            self.conn
                .execute(
                    "UPDATE session SET time_updated = ?2 WHERE id = ?1",
                    params![id, updated_ms],
                )
                .unwrap();
        }

        pub fn set_title(&self, id: &str, title: &str) {
            self.conn
                .execute("UPDATE session SET title = ?2 WHERE id = ?1", params![id, title])
                .unwrap();
        }

        /// Insert or replace a message row; `time_created` comes from
        /// `data.time.created`, as OpenCode writes it.
        pub fn put_message(&self, id: &str, session: &str, data: &Value) {
            let created = data["time"]["created"].as_i64().unwrap();
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO message
                        (id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?3, ?4)",
                    params![id, session, created, data.to_string()],
                )
                .unwrap();
        }

        /// What OpenCode's `/undo` does to the newest messages.
        pub fn delete_message(&self, id: &str) {
            self.conn.execute("DELETE FROM message WHERE id = ?1", [id]).unwrap();
        }

        /// Insert or replace a `tool` part in `status`.
        pub fn put_tool_part(&self, id: &str, message: &str, session: &str, status: &str) {
            let data = json!({"type": "tool", "tool": "bash", "state": {"status": status}});
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO part
                        (id, message_id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, 0, 0, ?4)",
                    params![id, message, session, data.to_string()],
                )
                .unwrap();
        }
    }

    pub fn user(created_ms: i64) -> Value {
        json!({
            "role": "user",
            "time": {"created": created_ms},
            "agent": "build",
            "model": {"providerID": "openai", "modelID": "gpt-5.4"}
        })
    }

    /// An assistant step. `completed` + `finish` describe how it ended;
    /// `None` for one still streaming.
    pub fn assistant(
        created_ms: i64,
        completed: Option<(i64, &str)>,
        tokens: (u64, u64, u64, u64),
        model: &str,
    ) -> Value {
        let (input, output, cache_read, cache_write) = tokens;
        let mut v = json!({
            "role": "assistant",
            "mode": "build",
            "path": {"cwd": "C:/repo/alpha", "root": "/"},
            "cost": 0,
            "tokens": {
                "input": input, "output": output, "reasoning": 0,
                "cache": {"read": cache_read, "write": cache_write}
            },
            "modelID": model,
            "providerID": "anthropic",
            "time": {"created": created_ms}
        });
        if let Some((done_ms, finish)) = completed {
            v["time"]["completed"] = json!(done_ms);
            v["finish"] = json!(finish);
        }
        v
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_titles_are_recognised() {
        assert!(is_placeholder_title("New session - 2026-04-17T09:25:15.466Z"));
        assert!(is_placeholder_title("Child session - 2026-04-17T09:25:15.466Z"));
        assert!(is_placeholder_title("New session"));
        assert!(is_placeholder_title("Child session"));
    }

    #[test]
    fn real_titles_are_not_placeholders() {
        assert!(!is_placeholder_title("Fix the resize bug"));
        assert!(!is_placeholder_title("New session - fix login"));
        assert!(!is_placeholder_title("New session - 2026-04-17T09:25:15Z"));
        assert!(!is_placeholder_title("New session handling in the router"));
    }

    /// Probe, and mark whatever changed as read.
    fn probe_and_mark(handle: &mut DbHandle) -> Probe {
        let probe = handle.probe().unwrap();
        if let Probe::Changed(v) = probe {
            handle.mark_read(v);
        }
        probe
    }

    #[test]
    fn missing_db_probes_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut handle = DbHandle::new(db_path(tmp.path()));
        assert_eq!(handle.probe().unwrap(), Probe::Missing);
        assert!(handle.conn().is_none());
        assert!(!db_path(tmp.path()).exists(), "probing must not create the db");
    }

    #[test]
    fn probe_is_unchanged_until_a_commit() {
        let fx = fixture::Fixture::new();
        let mut handle = DbHandle::new(db_path(fx.root()));
        assert!(matches!(probe_and_mark(&mut handle), Probe::Changed(_)), "first sight");
        assert_eq!(probe_and_mark(&mut handle), Probe::Unchanged);
        assert_eq!(probe_and_mark(&mut handle), Probe::Unchanged);
        fx.add_session("ses_1", None, "C:/repo/alpha", "t", 1);
        assert!(matches!(probe_and_mark(&mut handle), Probe::Changed(_)));
        assert_eq!(probe_and_mark(&mut handle), Probe::Unchanged);
    }

    #[test]
    fn probe_sees_every_commit_even_after_a_checkpoint() {
        // The case file stats miss: after a checkpoint the WAL is
        // rewritten from offset 0 at a constant length, and commits a
        // millisecond apart share an mtime.
        let fx = fixture::Fixture::new();
        fx.conn.pragma_update(None, "wal_autocheckpoint", 1).unwrap();
        let mut handle = DbHandle::new(db_path(fx.root()));
        probe_and_mark(&mut handle);
        for n in 0..300 {
            fx.add_session(&format!("ses_{n}"), None, "C:/repo/alpha", "t", n);
            assert!(matches!(probe_and_mark(&mut handle), Probe::Changed(_)), "commit {n}");
        }
    }

    #[test]
    fn unmarked_change_is_reported_again() {
        // A busy read never calls `mark_read`; the next probe retries.
        let fx = fixture::Fixture::new();
        let mut handle = DbHandle::new(db_path(fx.root()));
        probe_and_mark(&mut handle);
        fx.add_session("ses_1", None, "C:/repo/alpha", "t", 1);
        assert!(matches!(handle.probe().unwrap(), Probe::Changed(_)));
        assert!(matches!(handle.probe().unwrap(), Probe::Changed(_)));
    }

    #[cfg(unix)]
    #[test]
    fn replaced_file_is_reopened() {
        let tmp = tempfile::tempdir().unwrap();
        let path = db_path(tmp.path());
        let first = rusqlite::Connection::open(&path).unwrap();
        first.execute_batch("CREATE TABLE t (v text); INSERT INTO t VALUES ('old');").unwrap();
        let mut handle = DbHandle::new(path.clone());
        probe_and_mark(&mut handle);
        drop(first);

        std::fs::remove_file(&path).unwrap();
        let second = rusqlite::Connection::open(&path).unwrap();
        second.execute_batch("CREATE TABLE t (v text); INSERT INTO t VALUES ('new');").unwrap();
        assert!(matches!(probe_and_mark(&mut handle), Probe::Changed(_)));
        let v: String =
            handle.conn().unwrap().query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "new");
    }

    #[test]
    fn shared_file_id_is_reused_only_inside_the_window() {
        let tmp = tempfile::tempdir().unwrap();
        let path = db_path(tmp.path());
        TEST_FILE_ID_REUSE.with(|w| w.set(Duration::from_secs(60)));
        assert_eq!(shared_file_id(&path), None);
        std::fs::write(&path, b"").unwrap();
        assert_eq!(shared_file_id(&path), None, "inside the window: no stat");

        TEST_FILE_ID_REUSE.with(|w| w.set(Duration::ZERO));
        assert!(shared_file_id(&path).is_some(), "window over: stat again");
    }

    #[test]
    fn read_only_connection_cannot_write() {
        let fx = fixture::Fixture::new();
        let conn = open_read_only(&db_path(fx.root())).unwrap();
        let err = conn
            .execute("DELETE FROM session", [])
            .expect_err("read-only connection must refuse writes");
        assert_eq!(err.sqlite_error_code(), Some(ErrorCode::ReadOnly));
    }

    #[test]
    fn open_read_only_does_not_create_a_missing_db() {
        let tmp = tempfile::tempdir().unwrap();
        let path = db_path(tmp.path());
        assert!(open_read_only(&path).is_err());
        assert!(!path.exists());
    }
}
