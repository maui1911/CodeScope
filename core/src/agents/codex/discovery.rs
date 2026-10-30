//! Codex session adoption — find the rollout file an interactive
//! `codex` session writes under
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<thread-id>.jsonl`.
//!
//! Same `scan` shape as [`crate::agents::claude::discovery`]: pure
//! polling, the caller drives the cadence and dedupes by id.
//!
//! # Layout (codex-cli 0.160, `codex-rs/rollout/src/recorder.rs`)
//!
//! * The dated directory and the `<ts>` in the file name are the
//!   *local* date / time the thread started (`OffsetDateTime::now_local`).
//! * The file is created lazily, on the first write after the user's
//!   first prompt — a freshly launched `codex` has no rollout yet.
//! * `codex resume <id>` appends to the existing file, wherever (and
//!   whenever) it was created.
//! * A reverted thread's file appends `_<rollout-id>` to the thread id.
//! * The first line is a `session_meta` record carrying `id`, `cwd`
//!   and `source`. The interactive TUI writes `source: "cli"`; `codex
//!   exec`, the IDE extension / app-server (`"vscode"`, which is also
//!   what the Codex plugin for Claude Code runs as), MCP and sub-agent
//!   threads write something else. Only `cli` threads are adoptable, so
//!   a Claude tab running a Codex review in the same folder is never
//!   mistaken for the Codex tab's session.
//!
//! # Semantics
//!
//! A rollout is a candidate when its `session_meta` says `cli`, its
//! `cwd` canonicalises to the tab's working directory, and it was
//! *created* at or after `since` (the tab's spawn time). Modification
//! alone does not qualify: another tab's long-running session in the
//! same folder is modified constantly, and the one pre-existing file a
//! tab may legitimately own — the thread it resumed — is named
//! explicitly through `known`.
//!
//! # Cost
//!
//! The `sessions` tree accumulates every thread ever run, so the scan
//! never walks it. It looks only at the dated directories around
//! `since` and around now (± one day, which covers any UTC offset),
//! through [`crate::agents::dir_listing`] — a `stat` per directory, a
//! `read_dir` only when one gained a file. Each rollout's
//! `session_meta` (its first line runs to ~20 KiB of base
//! instructions) is cached by path, so it is parsed once per file
//! instead of once per tick.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::agents::dir_listing::list_files;
use crate::path_canon::canonicalize_path;

/// Suggested poll interval — matches the other agents' discovery.
pub const POLL_INTERVAL_MS: u64 = 350;

/// One adoption candidate found in a sessions-root scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// Codex thread id (a UUID, v7 for every thread since 2025).
    pub session_id: String,
    /// Absolute path to the rollout `.jsonl` file.
    pub path: PathBuf,
}

/// Scan `sessions_root` for rollouts of interactive threads started in
/// `working_directory` since `since`, plus the rollout of `known` (the
/// thread the tab already owns — e.g. the one `codex resume <id>`
/// reopened) when it was modified since `since`.
///
/// Returns an empty vec when nothing qualifies; callers treat that as
/// "keep polling".
pub fn scan(
    sessions_root: &Path,
    working_directory: &str,
    since: SystemTime,
    known: Option<&str>,
) -> Vec<AdoptionCandidate> {
    scan_at(sessions_root, working_directory, since, known, SystemTime::now())
}

fn scan_at(
    sessions_root: &Path,
    working_directory: &str,
    since: SystemTime,
    known: Option<&str>,
    now: SystemTime,
) -> Vec<AdoptionCandidate> {
    let canon_cwd = canonicalize_path(working_directory);
    let mut out = Vec::new();
    for dir in day_dirs(sessions_root, &[since, now]) {
        for file in list_files(&dir).iter() {
            let Some(id) = thread_id_from_file_name(&file.name) else { continue };
            if file.born.is_none_or(|t| t < since) {
                continue;
            }
            if session_meta(&file.path).is_some_and(|m| m.adoptable_in(&canon_cwd)) {
                remember_path(id, &file.path);
                out.push(AdoptionCandidate { session_id: id.to_owned(), path: file.path.clone() });
            }
        }
    }
    if let Some(id) = known
        && !out.iter().any(|c| c.session_id == id)
        && let Some(path) = locate_rollout(sessions_root, id)
    {
        let modified = std::fs::metadata(&path)
            .ok()
            .and_then(|m| crate::telemetry::modified_or_none(&m));
        if modified.is_some_and(|t| t >= since)
            && session_meta(&path).is_some_and(|m| m.adoptable_in(&canon_cwd))
        {
            out.push(AdoptionCandidate { session_id: id.to_owned(), path });
        }
    }
    out
}

/// Find the rollout file of thread `id`: the path a scan last saw it
/// at, else the dated directories around the creation time encoded in
/// a v7 thread id. `None` for a v4 id (threads from before 2025) whose
/// file no scan has seen — the tree is never walked.
///
/// A reverted thread has more than one file; the most recently
/// modified wins, since that is the one Codex appends to.
pub fn locate_rollout(sessions_root: &Path, id: &str) -> Option<PathBuf> {
    if let Some(path) = path_cache().lock().ok()?.get(id).cloned()
        && path.starts_with(sessions_root)
        && path.is_file()
    {
        return Some(path);
    }
    let created = uuid_v7_time(id)?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for dir in day_dirs(sessions_root, &[created]) {
        for file in list_files(&dir).iter() {
            if thread_id_from_file_name(&file.name) != Some(id) {
                continue;
            }
            let mtime = std::fs::metadata(&file.path)
                .ok()
                .and_then(|m| crate::telemetry::modified_or_none(&m))
                .unwrap_or(UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
                best = Some((mtime, file.path.clone()));
            }
        }
    }
    let (_, path) = best?;
    remember_path(id, &path);
    Some(path)
}

/// Thread id from a rollout file name:
/// `rollout-2026-10-30T11-36-54-<thread-id>[_<rollout-id>].jsonl`.
/// `None` for anything else — compressed `.jsonl.zst` files included,
/// which Codex only produces for cold rollouts.
pub fn thread_id_from_file_name(name: &str) -> Option<&str> {
    let core = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    // `YYYY-MM-DDThh-mm-ss` is 19 bytes, then a `-` separator.
    if core.get(19..20)? != "-" {
        return None;
    }
    let ids = core.get(20..)?;
    let thread = ids.split_once('_').map_or(ids, |(t, _)| t);
    is_uuid(thread).then_some(thread)
}

/// The facts discovery needs from a rollout's `session_meta` line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionMeta {
    /// `cwd`, canonicalised with [`canonicalize_path`].
    canon_cwd: String,
    /// `source == "cli"` and no non-user `thread_source`.
    interactive: bool,
}

impl SessionMeta {
    fn adoptable_in(&self, canon_cwd: &str) -> bool {
        self.interactive && self.canon_cwd == canon_cwd
    }
}

#[derive(Deserialize)]
struct MetaLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<MetaPayload>,
}

#[derive(Deserialize)]
struct MetaPayload {
    cwd: Option<String>,
    source: Option<serde_json::Value>,
    thread_source: Option<serde_json::Value>,
}

/// Upper bound on the first line we are willing to buffer. Real
/// `session_meta` lines are ~20–25 KiB.
const META_LINE_MAX_BYTES: u64 = 1024 * 1024;

/// Entries kept in each process-wide cache before it is cleared. A
/// day of heavy use produces a few dozen rollouts.
const CACHE_MAX_ENTRIES: usize = 4096;

/// Parsed `session_meta` for `path`, from the cache when this file has
/// been read before. `None` while the first line is still incomplete
/// (not cached, so the next tick retries) and for files that don't
/// open with a `session_meta` record (cached as such).
fn session_meta(path: &Path) -> Option<SessionMeta> {
    let cache = meta_cache();
    if let Some(hit) = cache.lock().ok()?.get(path) {
        return hit.clone();
    }
    let parsed = read_session_meta(path)?;
    if let Ok(mut map) = cache.lock() {
        if map.len() >= CACHE_MAX_ENTRIES {
            map.clear();
        }
        map.insert(path.to_path_buf(), parsed.clone());
    }
    parsed
}

/// `None` = could not decide yet (unreadable, or the line has no
/// newline yet); `Some(None)` = decided: not an adoptable rollout.
fn read_session_meta(path: &Path) -> Option<Option<SessionMeta>> {
    use std::io::{BufRead as _, BufReader, Read as _};
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(META_LINE_MAX_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if !line.ends_with('\n') {
        return None;
    }
    let Ok(parsed) = serde_json::from_str::<MetaLine>(&line) else {
        return Some(None);
    };
    let Some(payload) = parsed.payload.filter(|_| parsed.kind.as_deref() == Some("session_meta"))
    else {
        return Some(None);
    };
    let Some(cwd) = payload.cwd else { return Some(None) };
    let cli = payload.source.as_ref().and_then(serde_json::Value::as_str) == Some("cli");
    let user_thread = payload
        .thread_source
        .as_ref()
        .is_none_or(|s| s.is_null() || s.as_str() == Some("user"));
    Some(Some(SessionMeta {
        canon_cwd: canonicalize_path(&cwd),
        interactive: cli && user_thread,
    }))
}

fn meta_cache() -> &'static Mutex<HashMap<PathBuf, Option<SessionMeta>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<SessionMeta>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn path_cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_path(id: &str, path: &Path) {
    if let Ok(mut map) = path_cache().lock() {
        if map.len() >= CACHE_MAX_ENTRIES {
            map.clear();
        }
        if map.get(id).map(PathBuf::as_path) != Some(path) {
            map.insert(id.to_owned(), path.to_path_buf());
        }
    }
}

/// `sessions_root/YYYY/MM/DD` for the UTC day of each instant and the
/// days either side of it. Codex names directories by *local* date,
/// which is never more than a day away from the UTC one.
fn day_dirs(sessions_root: &Path, instants: &[SystemTime]) -> Vec<PathBuf> {
    let days: BTreeSet<i64> = instants
        .iter()
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| (d.as_secs() / 86_400) as i64)
        .flat_map(|day| [day - 1, day, day + 1])
        .collect();
    days.into_iter()
        .map(|day| {
            let c = crate::time::unix_secs_to_civil(day * 86_400);
            sessions_root
                .join(format!("{:04}", c.year))
                .join(format!("{:02}", c.month))
                .join(format!("{:02}", c.day))
        })
        .collect()
}

/// Creation time encoded in a UUID v7 (its first 48 bits are Unix
/// milliseconds). `None` for any other version.
fn uuid_v7_time(id: &str) -> Option<SystemTime> {
    if !is_uuid(id) || id.as_bytes()[14] != b'7' {
        return None;
    }
    let hex: String = id[..13].chars().filter(|c| *c != '-').collect();
    let ms = u64::from_str_radix(&hex, 16).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_millis(ms))
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    /// A v7 id minted 2026-10-30T10:36:54.534Z.
    pub(crate) const ID_A: &str = "01a18c62-4b86-7fd3-afc0-abb45949f121";
    /// A v7 id minted a few seconds earlier.
    const ID_B: &str = "01a18c5a-3b93-7e73-b7a8-32a4dd2b8b80";
    const CWD: &str = "C:/repo/alpha";

    pub(crate) fn meta_line(id: &str, cwd: &str, source: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-30T10:36:55.704Z","type":"session_meta","payload":{{"session_id":"{id}","id":"{id}","timestamp":"2026-10-30T10:36:54.583Z","cwd":"{}","originator":"codex-tui","cli_version":"0.160.1","source":"{source}","base_instructions":{{"text":"…"}}}}}}"#,
            cwd.replace('\\', "\\\\")
        ) + "\n"
    }

    fn day_dir(root: &Path, t: SystemTime) -> PathBuf {
        day_dirs(root, &[t]).remove(1)
    }

    fn write_rollout(dir: &Path, id: &str, cwd: &str, source: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-30T11-36-54-{id}.jsonl"));
        std::fs::write(&path, meta_line(id, cwd, source)).unwrap();
        path
    }

    fn ids(found: &[AdoptionCandidate]) -> Vec<&str> {
        let mut v: Vec<&str> = found.iter().map(|c| c.session_id.as_str()).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn file_name_yields_thread_id() {
        let name = format!("rollout-2026-10-30T11-36-54-{ID_A}.jsonl");
        assert_eq!(thread_id_from_file_name(&name), Some(ID_A));
        let reverted = format!("rollout-2026-10-30T11-36-54-{ID_A}_{ID_B}.jsonl");
        assert_eq!(thread_id_from_file_name(&reverted), Some(ID_A));
    }

    #[test]
    fn file_name_rejects_other_shapes() {
        for name in [
            format!("rollout-2026-10-30T11-36-54-{ID_A}.jsonl.zst"),
            format!("rollout-2026-10-30T11-36-54-{ID_A}.json"),
            "rollout-2026-10-30T11-36-54-not-a-uuid.jsonl".to_owned(),
            format!("session-{ID_A}.jsonl"),
            String::new(),
        ] {
            assert_eq!(thread_id_from_file_name(&name), None, "{name}");
        }
    }

    #[test]
    fn v7_id_decodes_to_its_creation_time() {
        let t = uuid_v7_time(ID_A).unwrap();
        let secs = t.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_793_356_614);
        // v4 ids carry no time.
        assert_eq!(uuid_v7_time("f1e2d3c4-aaaa-4bbb-8ccc-1234567890ab"), None);
    }

    #[test]
    fn day_dirs_cover_the_day_either_side() {
        let root = Path::new("/home/user/.codex/sessions");
        let t = UNIX_EPOCH + Duration::from_secs(1_793_356_614); // 2026-10-30
        let dirs = day_dirs(root, &[t, t + Duration::from_secs(60)]);
        assert_eq!(
            dirs,
            vec![root.join("2026/10/29"), root.join("2026/10/30"), root.join("2026/10/31")]
        );
    }

    #[test]
    fn scan_finds_interactive_rollout_in_the_working_directory() {
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let since = now - Duration::from_secs(60);
        let path = write_rollout(&day_dir(tmp.path(), now), ID_A, r"C:\repo\alpha", "cli");
        let found = scan_at(tmp.path(), CWD, since, None, now);
        assert_eq!(found, vec![AdoptionCandidate { session_id: ID_A.into(), path }]);
    }

    #[test]
    fn scan_skips_other_working_directories() {
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        write_rollout(&day_dir(tmp.path(), now), ID_A, "C:/repo/beta", "cli");
        assert!(scan_at(tmp.path(), CWD, now - Duration::from_secs(60), None, now).is_empty());
    }

    #[test]
    fn scan_skips_non_interactive_sources() {
        // `codex exec`, the IDE / app-server (the Codex plugin a Claude
        // tab runs reviews through) and sub-agents share the folder.
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let dir = day_dir(tmp.path(), now);
        write_rollout(&dir, ID_A, CWD, "exec");
        write_rollout(&dir, ID_B, CWD, "vscode");
        assert!(scan_at(tmp.path(), CWD, now - Duration::from_secs(60), None, now).is_empty());
    }

    #[test]
    fn scan_skips_rollouts_created_before_spawn() {
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        write_rollout(&day_dir(tmp.path(), now), ID_A, CWD, "cli");
        let since = now + Duration::from_secs(3_600);
        assert!(scan_at(tmp.path(), CWD, since, None, since).is_empty());
    }

    #[test]
    fn scan_ignores_rollouts_outside_the_scanned_days() {
        // A rollout dated a week before the tab spawned is not looked
        // at, however new its file is.
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let week_ago = now - Duration::from_secs(7 * 86_400);
        write_rollout(&day_dir(tmp.path(), week_ago), ID_A, CWD, "cli");
        assert!(scan_at(tmp.path(), CWD, now - Duration::from_secs(60), None, now).is_empty());
    }

    #[test]
    fn scan_waits_for_a_complete_meta_line() {
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let dir = day_dir(tmp.path(), now);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-30T11-36-54-{ID_A}.jsonl"));
        let line = meta_line(ID_A, CWD, "cli");
        std::fs::write(&path, &line[..line.len() / 2]).unwrap();
        let since = now - Duration::from_secs(60);
        assert!(scan_at(tmp.path(), CWD, since, None, now).is_empty());
        std::fs::write(&path, &line).unwrap();
        assert_eq!(ids(&scan_at(tmp.path(), CWD, since, None, now)), vec![ID_A]);
    }

    /// A v7 id minted at `t`.
    fn v7_id(t: SystemTime) -> String {
        let ms = t.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        format!("{:08x}-{:04x}-7abc-8def-0123456789ab", ms >> 16, ms & 0xffff)
    }

    #[test]
    fn scan_returns_known_resumed_rollout_from_its_own_day() {
        // `codex resume <id>` appends to the file in the directory of
        // the day the thread started — found through the id's v7 time.
        let tmp = TempDir::new().unwrap();
        let now = SystemTime::now();
        let created = now - Duration::from_secs(200 * 86_400);
        let id = v7_id(created);
        let path = write_rollout(&day_dir(tmp.path(), created), &id, CWD, "cli");
        let since = now - Duration::from_secs(60);
        assert!(scan_at(tmp.path(), CWD, since, None, now).is_empty());
        let found = scan_at(tmp.path(), CWD, since, Some(&id), now);
        assert_eq!(found, vec![AdoptionCandidate { session_id: id, path }]);
    }

    #[test]
    fn scan_skips_known_rollout_untouched_since_spawn() {
        let tmp = TempDir::new().unwrap();
        let created = uuid_v7_time(ID_A).unwrap();
        write_rollout(&day_dir(tmp.path(), created), ID_A, CWD, "cli");
        let later = SystemTime::now() + Duration::from_secs(3_600);
        assert!(scan_at(tmp.path(), CWD, later, Some(ID_A), later).is_empty());
    }

    #[test]
    fn locate_rollout_prefers_the_newest_file_of_a_reverted_thread() {
        let tmp = TempDir::new().unwrap();
        let dir = day_dir(tmp.path(), uuid_v7_time(ID_B).unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join(format!("rollout-2026-10-30T11-28-06-{ID_B}.jsonl"));
        std::fs::write(&old, "").unwrap();
        let file = std::fs::File::options().write(true).open(&old).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(600)).unwrap();
        let new = dir.join(format!("rollout-2026-10-30T11-28-06-{ID_B}_{ID_A}.jsonl"));
        std::fs::write(&new, "").unwrap();
        assert_eq!(locate_rollout(tmp.path(), ID_B), Some(new));
    }

    #[test]
    fn locate_rollout_without_v7_time_or_cache_is_none() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(locate_rollout(tmp.path(), "f1e2d3c4-aaaa-4bbb-8ccc-1234567890ab"), None);
    }
}
