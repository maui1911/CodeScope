//! Shared access to OpenCode's SQLite store, `<data_root>/opencode.db`.
//!
//! OpenCode 1.x keeps every session, message and message part in one
//! WAL-mode SQLite database (it used to write one JSON file per
//! message). Both the adoption scan ([`super::discovery`]) and the
//! per-tab tail ([`super::telemetry`]) read it under the same rules
//! (ADR-0025):
//!
//! * **Read-only.** Connections open with `SQLITE_OPEN_READ_ONLY` and a
//!   zero busy timeout. A locked database is "nothing new this tick",
//!   never a wait on the UI thread.
//! * **Stat before query.** [`DbStamp`] is the mtime and length of
//!   `opencode.db` and `opencode.db-wal`. Every commit OpenCode makes
//!   lands in the WAL, and a checkpoint rewrites the main file, so a
//!   stamp that hasn't moved means there is nothing new to read.
//!   [`shared_stamp`] lets every tab in one tick share a single stat.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{Connection, ErrorCode, OpenFlags};

/// File name of OpenCode's database under its data root.
pub const DB_FILE_NAME: &str = "opencode.db";

/// `<data_root>/opencode.db`.
pub fn db_path(data_root: &Path) -> PathBuf {
    data_root.join(DB_FILE_NAME)
}

/// What a `stat` of the database and its WAL says, compared tick to
/// tick to decide whether a query is worth running. The length is
/// carried next to the mtime so two commits inside one mtime tick
/// still differ: each one grows the WAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbStamp {
    db: Option<(Option<SystemTime>, u64)>,
    wal: Option<(Option<SystemTime>, u64)>,
}

impl DbStamp {
    /// Stat `db_path` and `<db_path>-wal`. Two `metadata` calls, no
    /// file is opened for reading.
    pub fn read(db_path: &Path) -> Self {
        let mut wal = db_path.as_os_str().to_owned();
        wal.push("-wal");
        Self {
            db: file_stamp(db_path),
            wal: file_stamp(Path::new(&wal)),
        }
    }

    /// `false` when `opencode.db` itself is missing (OpenCode never
    /// ran, or an older version that predates the database).
    pub fn db_exists(&self) -> bool {
        self.db.is_some()
    }
}

fn file_stamp(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok(), meta.len()))
}

/// How long one caller's stamp is reused by the next. All OpenCode
/// tails poll in the same telemetry tick and all OpenCode tabs scan in
/// the same adoption tick, so within a tick the database is stat'ed
/// once rather than once per tab (a `stat` is ~20 µs on Windows). A
/// commit landing inside the window is picked up one poll later.
pub const STAMP_REUSE_WINDOW: Duration = Duration::from_millis(50);

static RECENT_STAMPS: LazyLock<Mutex<HashMap<PathBuf, (Instant, DbStamp)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// [`DbStamp::read`], reusing a stamp of the same file read less than
/// [`stamp_reuse`] ago.
pub fn shared_stamp(db_path: &Path) -> DbStamp {
    let now = Instant::now();
    let mut recent = RECENT_STAMPS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((at, stamp)) = recent.get_mut(db_path) {
        if now.duration_since(*at) < stamp_reuse() {
            return *stamp;
        }
        *stamp = DbStamp::read(db_path);
        *at = now;
        return *stamp;
    }
    let stamp = DbStamp::read(db_path);
    recent.insert(db_path.to_path_buf(), (now, stamp));
    stamp
}

#[cfg(not(test))]
fn stamp_reuse() -> Duration {
    STAMP_REUSE_WINDOW
}

// Tests write and poll back to back, so they stat every time unless a
// test opts into the window.
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_STAMP_REUSE: std::cell::Cell<Duration> =
        const { std::cell::Cell::new(Duration::ZERO) };
}

#[cfg(test)]
fn stamp_reuse() -> Duration {
    TEST_STAMP_REUSE.with(|w| w.get())
}

/// Expire every shared stamp, as the gap between two real ticks does.
#[cfg(test)]
pub(crate) fn forget_shared_stamps() {
    RECENT_STAMPS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
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
/// stamp, so the retry isn't gated away.
pub fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
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

    #[test]
    fn stamp_of_missing_db_reports_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let stamp = DbStamp::read(&db_path(tmp.path()));
        assert!(!stamp.db_exists());
        assert_eq!(stamp, DbStamp::read(&db_path(tmp.path())));
    }

    #[test]
    fn stamp_moves_on_every_commit() {
        let fx = fixture::Fixture::new();
        let path = db_path(fx.root());
        let before = DbStamp::read(&path);
        assert!(before.db_exists());
        assert_eq!(before, DbStamp::read(&path), "no write, no change");
        fx.add_session("ses_1", None, "C:/repo/alpha", "t", 1);
        assert_ne!(before, DbStamp::read(&path));
    }

    #[test]
    fn shared_stamp_is_reused_only_inside_the_window() {
        let fx = fixture::Fixture::new();
        let path = db_path(fx.root());
        TEST_STAMP_REUSE.with(|w| w.set(Duration::from_secs(60)));
        let first = shared_stamp(&path);
        fx.add_session("ses_1", None, "C:/repo/alpha", "t", 1);
        assert_eq!(shared_stamp(&path), first, "inside the window: no stat");

        TEST_STAMP_REUSE.with(|w| w.set(Duration::ZERO));
        let fresh = shared_stamp(&path);
        assert_ne!(fresh, first, "window over: stat again");
        assert_eq!(fresh, DbStamp::read(&path));
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
