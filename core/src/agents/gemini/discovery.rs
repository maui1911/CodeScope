//! Gemini CLI session adoption — find the chat file a `gemini` session
//! writes under `<gemini-home>/tmp/<project>/chats/`.
//!
//! Same `scan` shape as [`crate::agents::claude::discovery`]: pure
//! polling, the caller drives the cadence and dedupes by id.
//!
//! # Layout (confirmed on gemini-cli 0.30 and the 0.67 sources)
//!
//! * `<gemini-home>` is `$GEMINI_CLI_HOME/.gemini`, else `~/.gemini`.
//! * `<project>` is a short slug of the project folder's name
//!   (`codescope-public`, `repo-1`, …). The mapping lives in
//!   `<gemini-home>/projects.json` (`{"projects": {"<path>": "<slug>"}}`,
//!   paths lower-cased on Windows), and each slug directory carries a
//!   `.project_root` file holding the path it belongs to. Releases
//!   before the slugs used a SHA-256 of the path instead, and migrate
//!   those directories on startup; they are not looked for here.
//! * Chats are `chats/session-<YYYY-MM-DDThh-mm>-<first 8 of id>.json`
//!   (≤ 0.3x: one JSON document, rewritten on every change, created
//!   with the first message) or `.jsonl` (later releases: an
//!   append-only log, created at startup). Sub-agent chats live one
//!   level deeper and are never candidates.
//! * The full session id is the file's `sessionId` — the first key of
//!   the JSON document, or of the first JSONL line.
//! * `gemini --resume <id>` keeps writing the same `.json` file;
//!   later releases continue a `.json` chat in a new `.jsonl` next to
//!   it, with the same id.
//!
//! # Semantics
//!
//! A chat is a candidate when it was *created* at or after `since` in
//! the working directory's project. The one older chat a tab may own —
//! the session it resumed — is named through `known` and qualifies
//! when it was modified since `since`.
//!
//! # Cost
//!
//! The project directory is resolved once per working directory and
//! cached; the chats directory is listed through
//! [`crate::agents::dir_listing`] (a `stat`, a `read_dir` only when a
//! chat was added); each chat's head is read once and cached by path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::agents::dir_listing::list_files;
use crate::path_canon::canonicalize_path;

/// Suggested poll interval — matches the other agents' discovery.
pub const POLL_INTERVAL_MS: u64 = 350;

/// How long an unresolved working directory waits before its project
/// directory is looked for again. `gemini` creates it on startup.
const RESOLVE_RETRY: Duration = Duration::from_secs(2);

/// Entries kept in each process-wide cache before it is cleared.
const CACHE_MAX_ENTRIES: usize = 4096;

/// Bytes read from a chat's head to find its `sessionId`.
const HEAD_MAX_BYTES: u64 = 16 * 1024;

/// One adoption candidate found in a chats-directory scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// The chat's full `sessionId`.
    pub session_id: String,
    /// Absolute path to the chat file.
    pub path: PathBuf,
}

/// `$GEMINI_CLI_HOME/.gemini`, else `<home>/.gemini`.
pub fn default_gemini_home() -> Option<PathBuf> {
    let home = std::env::var_os("GEMINI_CLI_HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".gemini"))
}

/// Scan the working directory's project for chats created since
/// `since`, plus the chat of `known` (the session the tab already owns)
/// when it was modified since `since`.
pub fn scan(
    gemini_home: &Path,
    working_directory: &str,
    since: SystemTime,
    known: Option<&str>,
) -> Vec<AdoptionCandidate> {
    let Some(chats) = chats_dir(gemini_home, working_directory) else {
        return Vec::new();
    };
    let mut out: Vec<(SystemTime, AdoptionCandidate)> = Vec::new();
    for file in list_files(&chats).iter() {
        if !is_chat_file_name(&file.name) {
            continue;
        }
        let born = file.born.is_some_and(|t| t >= since);
        let maybe_known = known.is_some_and(|k| short_id_matches(&file.name, k));
        if !born && !maybe_known {
            continue;
        }
        let Some(head) = chat_head(&file.path).filter(|h| !h.subagent) else { continue };
        let mtime = std::fs::metadata(&file.path)
            .ok()
            .and_then(|m| crate::telemetry::modified_or_none(&m))
            .unwrap_or(UNIX_EPOCH);
        if !born && (known != Some(head.session_id.as_str()) || mtime < since) {
            continue;
        }
        // A resumed `.json` chat continued in a `.jsonl`: one candidate,
        // the file being written.
        match out.iter_mut().find(|(_, c)| c.session_id == head.session_id) {
            Some((t, c)) if mtime > *t => {
                *t = mtime;
                c.path = file.path.clone();
            }
            Some(_) => {}
            None => out.push((
                mtime,
                AdoptionCandidate { session_id: head.session_id, path: file.path.clone() },
            )),
        }
    }
    out.into_iter().map(|(_, c)| c).collect()
}

/// The chat file of session `id` in the working directory's project:
/// the most recently modified one, when a `.json` chat was continued in
/// a `.jsonl`.
pub fn locate_chat(gemini_home: &Path, working_directory: &str, id: &str) -> Option<PathBuf> {
    let chats = chats_dir(gemini_home, working_directory)?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for file in list_files(&chats).iter() {
        if !is_chat_file_name(&file.name) || !short_id_matches(&file.name, id) {
            continue;
        }
        if chat_head(&file.path).is_none_or(|h| h.session_id != id) {
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
    best.map(|(_, p)| p)
}

/// `session-….json` / `session-….jsonl` — not the `.tmp-<pid>` and
/// `.unreadable-<ts>` siblings Gemini leaves while rewriting.
pub fn is_chat_file_name(name: &str) -> bool {
    name.starts_with("session-") && (name.ends_with(".json") || name.ends_with(".jsonl"))
}

/// Whether a chat file name ends in the first eight characters of `id`.
fn short_id_matches(name: &str, id: &str) -> bool {
    let stem = name.strip_suffix(".jsonl").or_else(|| name.strip_suffix(".json"));
    match (stem, id.get(..8)) {
        (Some(stem), Some(short)) => stem.ends_with(&format!("-{short}")),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Project directory
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ProjectCache {
    found: HashMap<String, PathBuf>,
    retry_at: HashMap<String, Instant>,
}

fn project_cache() -> &'static Mutex<ProjectCache> {
    static CACHE: OnceLock<Mutex<ProjectCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ProjectCache::default()))
}

/// `<gemini_home>/tmp/<project>/chats` for `working_directory`.
pub fn chats_dir(gemini_home: &Path, working_directory: &str) -> Option<PathBuf> {
    let canon = canonicalize_path(working_directory);
    if canon.is_empty() {
        return None;
    }
    let key = format!("{}|{canon}", gemini_home.display());
    let now = Instant::now();
    let mut cache = project_cache().lock().ok()?;
    if let Some(dir) = cache.found.get(&key) {
        return Some(dir.join("chats"));
    }
    if cache.retry_at.get(&key).is_some_and(|t| now < *t) {
        return None;
    }
    match resolve_project_dir(gemini_home, &canon) {
        Some(dir) => {
            if cache.found.len() >= CACHE_MAX_ENTRIES {
                cache.found.clear();
            }
            cache.retry_at.remove(&key);
            cache.found.insert(key, dir.clone());
            Some(dir.join("chats"))
        }
        None => {
            if cache.retry_at.len() >= CACHE_MAX_ENTRIES {
                cache.retry_at.clear();
            }
            cache.retry_at.insert(key, now + RESOLVE_RETRY);
            None
        }
    }
}

#[derive(Deserialize)]
struct ProjectsFile {
    #[serde(default)]
    projects: HashMap<String, String>,
}

/// Find the slug directory that belongs to `canon_cwd`: through
/// `projects.json` first, then by reading each slug's `.project_root`.
fn resolve_project_dir(gemini_home: &Path, canon_cwd: &str) -> Option<PathBuf> {
    let tmp = gemini_home.join("tmp");
    if let Ok(text) = std::fs::read_to_string(gemini_home.join("projects.json"))
        && let Ok(file) = serde_json::from_str::<ProjectsFile>(&text)
        && let Some(slug) = file
            .projects
            .iter()
            .find(|(path, _)| canonicalize_path(path) == canon_cwd)
            .map(|(_, slug)| slug)
        && !slug.is_empty()
        && !slug.contains(['/', '\\'])
        && tmp.join(slug).is_dir()
    {
        return Some(tmp.join(slug));
    }
    std::fs::read_dir(&tmp).ok()?.flatten().map(|e| e.path()).find(|dir| {
        std::fs::read_to_string(dir.join(".project_root"))
            .is_ok_and(|owner| canonicalize_path(owner.trim()) == canon_cwd)
    })
}

// ---------------------------------------------------------------------------
// Chat heads
// ---------------------------------------------------------------------------

/// What discovery needs from a chat file's head.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatHead {
    session_id: String,
    subagent: bool,
}

fn head_cache() -> &'static Mutex<HashMap<PathBuf, ChatHead>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ChatHead>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The chat's head, read once per file. `None` (not cached) while the
/// file is still too short to tell.
fn chat_head(path: &Path) -> Option<ChatHead> {
    if let Some(hit) = head_cache().lock().ok()?.get(path) {
        return Some(hit.clone());
    }
    let head = read_head(path)?;
    if let Ok(mut map) = head_cache().lock() {
        if map.len() >= CACHE_MAX_ENTRIES {
            map.clear();
        }
        map.insert(path.to_path_buf(), head.clone());
    }
    Some(head)
}

#[derive(Deserialize)]
struct HeadLine {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    kind: Option<String>,
}

fn read_head(path: &Path) -> Option<ChatHead> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(HEAD_MAX_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if path.extension().is_some_and(|e| e == "jsonl") {
        let line = text.split_once('\n')?.0;
        let head: HeadLine = serde_json::from_str(line).ok()?;
        return Some(ChatHead {
            session_id: head.session_id?,
            subagent: head.kind.as_deref() == Some("subagent"),
        });
    }
    Some(ChatHead { session_id: json_string_field(&text, "sessionId")?, subagent: false })
}

/// The value of the first `"key": "<value>"` in `text` — enough for
/// the leading `sessionId` of a pretty-printed chat document without
/// parsing the (possibly many-megabyte) rest of it.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let after_key = &text[text.find(&format!("\"{key}\""))? + key.len() + 2..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let value = after_colon.strip_prefix('"')?;
    let end = value.find('"')?;
    let value = &value[..end];
    (!value.is_empty() && !value.contains('\\')).then(|| value.to_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;

    pub(crate) const SID: &str = "d1c96a3f-b64d-4144-a3e6-09670879c25e";
    const SID_B: &str = "77f17e64-1551-458d-9659-92bc2524a302";

    /// A gemini home with `cwd` registered under `slug`.
    pub(crate) fn home_with_project(cwd: &str, slug: &str) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join(".gemini");
        let chats = home.join("tmp").join(slug).join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(home.join("tmp").join(slug).join(".project_root"), cwd.to_lowercase())
            .unwrap();
        let projects = serde_json::json!({ "projects": { cwd.to_lowercase(): slug } });
        std::fs::write(home.join("projects.json"), projects.to_string()).unwrap();
        (tmp, chats)
    }

    pub(crate) fn legacy_doc(id: &str) -> String {
        format!(
            "{{\n  \"sessionId\": \"{id}\",\n  \"projectHash\": \"6e31\",\n  \"startTime\": \"2026-02-27T12:05:52.098Z\",\n  \"lastUpdated\": \"2026-02-27T12:05:52.098Z\",\n  \"messages\": []\n}}"
        )
    }

    pub(crate) fn jsonl_head(id: &str, kind: &str) -> String {
        format!(
            r#"{{"sessionId":"{id}","projectHash":"6e31","startTime":"2026-10-30T10:00:00.000Z","lastUpdated":"2026-10-30T10:00:00.000Z","kind":"{kind}"}}"#
        ) + "\n"
    }

    fn chat_name(id: &str, ext: &str) -> String {
        format!("session-2026-10-30T10-00-{}.{ext}", &id[..8])
    }

    fn set_mtime(path: &Path, t: SystemTime) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn chat_file_names() {
        assert!(is_chat_file_name("session-2026-02-27T12-05-d1c96a3f.json"));
        assert!(is_chat_file_name("session-2026-10-30T10-00-1-d1c96a3f.jsonl"));
        assert!(!is_chat_file_name("session-2026-10-30T10-00-d1c96a3f.jsonl.tmp-123"));
        assert!(!is_chat_file_name("logs.json"));
        assert!(short_id_matches("session-2026-02-27T12-05-d1c96a3f.json", SID));
        assert!(!short_id_matches("session-2026-02-27T12-05-77f17e64.json", SID));
    }

    #[test]
    fn session_id_from_a_pretty_printed_document() {
        assert_eq!(json_string_field(&legacy_doc(SID), "sessionId").as_deref(), Some(SID));
        assert_eq!(json_string_field("{\"sessionId\": 3}", "sessionId"), None);
    }

    #[test]
    fn project_resolves_through_projects_json() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        assert_eq!(chats_dir(home, r"C:\Repo\Alpha"), Some(chats.clone()));
    }

    #[test]
    fn project_resolves_through_project_root_marker() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        std::fs::remove_file(home.join("projects.json")).unwrap();
        assert_eq!(chats_dir(home, "c:/repo/alpha"), Some(chats.clone()));
    }

    #[test]
    fn unknown_project_is_none() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        assert_eq!(chats_dir(home, "C:/repo/beta"), None);
    }

    #[test]
    fn scan_finds_new_chats_of_both_formats() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        let since = SystemTime::now() - Duration::from_secs(60);
        let legacy = chats.join(chat_name(SID, "json"));
        std::fs::write(&legacy, legacy_doc(SID)).unwrap();
        let log = chats.join(chat_name(SID_B, "jsonl"));
        std::fs::write(&log, jsonl_head(SID_B, "main")).unwrap();
        let mut found = scan(home, "C:/repo/alpha", since, None);
        found.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(
            found,
            vec![
                AdoptionCandidate { session_id: SID_B.into(), path: log },
                AdoptionCandidate { session_id: SID.into(), path: legacy },
            ]
        );
    }

    #[test]
    fn scan_skips_other_projects() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        std::fs::write(chats.join(chat_name(SID, "json")), legacy_doc(SID)).unwrap();
        let since = SystemTime::now() - Duration::from_secs(60);
        assert!(scan(home, "C:/repo/beta", since, None).is_empty());
    }

    #[test]
    fn scan_skips_chats_created_before_spawn_and_subagents() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        std::fs::write(chats.join(chat_name(SID, "json")), legacy_doc(SID)).unwrap();
        let later = SystemTime::now() + Duration::from_secs(3_600);
        assert!(scan(home, "C:/repo/alpha", later, None).is_empty());
        std::fs::write(chats.join(chat_name(SID_B, "jsonl")), jsonl_head(SID_B, "subagent"))
            .unwrap();
        let since = SystemTime::now() - Duration::from_secs(60);
        crate::agents::dir_listing::forget(&chats);
        let found = scan(home, "C:/repo/alpha", since, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, SID);
    }

    #[test]
    fn scan_returns_known_resumed_chat_once_it_is_written() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        let path = chats.join(chat_name(SID, "json"));
        std::fs::write(&path, legacy_doc(SID)).unwrap();
        let since = SystemTime::now() + Duration::from_secs(60);
        // Created before the tab, untouched since: nothing.
        assert!(scan(home, "C:/repo/alpha", since, Some(SID)).is_empty());
        // `gemini --resume` rewrote it.
        set_mtime(&path, since + Duration::from_secs(1));
        let found = scan(home, "C:/repo/alpha", since, Some(SID));
        assert_eq!(found, vec![AdoptionCandidate { session_id: SID.into(), path }]);
        // Another session's old chat is not adopted through the id.
        assert!(scan(home, "C:/repo/alpha", since, Some(SID_B)).is_empty());
    }

    #[test]
    fn continued_legacy_chat_yields_the_jsonl() {
        let (_tmp, chats) = home_with_project("C:/repo/alpha", "alpha");
        let home = chats.parent().unwrap().parent().unwrap().parent().unwrap();
        let legacy = chats.join(chat_name(SID, "json"));
        std::fs::write(&legacy, legacy_doc(SID)).unwrap();
        set_mtime(&legacy, SystemTime::now() - Duration::from_secs(600));
        let log = chats.join(chat_name(SID, "jsonl"));
        std::fs::write(&log, jsonl_head(SID, "main")).unwrap();
        let since = SystemTime::now() - Duration::from_secs(60);
        let found = scan(home, "C:/repo/alpha", since, Some(SID));
        assert_eq!(found, vec![AdoptionCandidate { session_id: SID.into(), path: log.clone() }]);
        assert_eq!(locate_chat(home, "C:/repo/alpha", SID), Some(log));
    }
}
