//! Pi session adoption — find the JSONL transcript a freshly launched
//! `pi` session writes under `~/.pi/agent/sessions/--<encoded-cwd>--/`.
//!
//! Mirrors `PiSessionDiscovery` from the C# build but uses polling
//! only (no `notify` crate dep). Same `scan_*` shape as
//! [`crate::claude_discovery`]: pure logic, the caller drives the
//! cadence.
//!
//! # Semantics
//!
//! Adoption fires once per unique `.jsonl` path discovered with
//! `created_at >= since` *or* `last_modified >= since`, whose first
//! parseable line is a `session` header whose `cwd` field
//! (after canonicalisation) matches the tab's working directory.
//! Tracking already-fired ids is the caller's responsibility (mirrors
//! `WatchHandle._fired`).
//!
//! Returns *every* eligible candidate each call so the caller can dedupe
//! by id and keep the watch alive across CLI invocations (each `pi`
//! invocation lands in its own session id).
//!
//! Pi stores transcripts inside subdirectories of the sessions root,
//! so we walk recursively (bounded by [`MAX_RECURSION_DEPTH`] to avoid
//! pathological symlink loops).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::path_canon::canonicalize_path;
use crate::pi_telemetry;

/// Suggested poll interval — 350 ms matches the C#
/// `PiSessionDiscovery.PollInterval`.
pub const POLL_INTERVAL_MS: u64 = 350;

/// Exclusive bound on recursion depth when walking the sessions
/// root: `walk` is invoked at depth 0 for the root and the guard
/// rejects calls at `depth >= MAX_RECURSION_DEPTH`, so the deepest
/// directory ever read from is `MAX_RECURSION_DEPTH - 1` levels below
/// the root. Pi typically only nests one level
/// (`<root>/--<cwd>--/<file>.jsonl`) so 4 (i.e. up to 3 levels of
/// nesting below the root) is plenty while still terminating on a
/// stray symlink loop.
const MAX_RECURSION_DEPTH: usize = 4;

/// One adoption candidate found in a sessions-root scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// UUID parsed from the `_<sid>.jsonl` filename suffix.
    pub session_id: String,
    /// Absolute path to the `.jsonl` file.
    pub path: PathBuf,
}

/// Recursively scan `sessions_root` for `.jsonl` transcripts whose
/// `created_at` or `last_modified` timestamps are at or after `since`,
/// and whose `session` header line records a `cwd` that canonicalises
/// to the same string as `working_directory`.
///
/// Returns an empty vec when the root doesn't exist, can't be read,
/// or contains no eligible candidates. Callers should treat "empty"
/// as "keep polling".
pub fn scan(
    sessions_root: &Path,
    working_directory: &str,
    since: SystemTime,
) -> Vec<AdoptionCandidate> {
    if !sessions_root.exists() {
        return Vec::new();
    }
    let canon_cwd = canonicalize_path(working_directory);
    let mut out = Vec::new();
    walk(sessions_root, since, &canon_cwd, 0, &mut out);
    out
}

fn walk(
    dir: &Path,
    since: SystemTime,
    canon_cwd: &str,
    depth: usize,
    out: &mut Vec<AdoptionCandidate>,
) {
    if depth >= MAX_RECURSION_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            walk(&path, since, canon_cwd, depth + 1, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(sid) = pi_telemetry::extract_session_id_from_file_name(name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let created = meta.created().ok();
        let modified = meta.modified().ok();
        let eligible = matches!(created, Some(t) if t >= since)
            || matches!(modified, Some(t) if t >= since);
        if !eligible {
            continue;
        }
        if !header_cwd_matches(&path, canon_cwd) {
            continue;
        }
        out.push(AdoptionCandidate {
            session_id: sid,
            path,
        });
    }
}

/// Peek the first non-empty JSONL line and return `true` when it's a
/// `session` header whose `cwd` (canonicalised) matches `canon_cwd`.
/// Mirrors C# `PiSessionDiscovery.WatchHandle.HeaderMatches`.
fn header_cwd_matches(path: &Path, canon_cwd: &str) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else { return false };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let obj = match v.as_object() {
            Some(o) => o,
            None => return false,
        };
        if obj.get("type").and_then(serde_json::Value::as_str) != Some("session") {
            return false;
        }
        let Some(cwd) = obj.get("cwd").and_then(serde_json::Value::as_str) else {
            return false;
        };
        return canonicalize_path(cwd) == canon_cwd;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const SID_A: &str = "11111111-2222-3333-4444-555555555555";

    fn write_session_jsonl(path: &Path, sid: &str, cwd: &str) {
        let line = format!(
            r#"{{"type":"session","id":"{sid}","cwd":"{cwd}","timestamp":"2026-04-22T08:00:00Z"}}"#
        );
        std::fs::write(path, line.as_bytes()).unwrap();
    }

    #[test]
    fn scan_returns_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let res = scan(
            &tmp.path().join("does-not-exist"),
            "/some/dir",
            SystemTime::UNIX_EPOCH,
        );
        assert!(res.is_empty());
    }

    #[test]
    fn scan_finds_jsonl_with_matching_cwd() {
        let tmp = TempDir::new().unwrap();
        let cwd_dir = tmp.path().join("--c-some-project--");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        let path = cwd_dir.join(format!("2026-04-22T08-00-00-000Z_{SID_A}.jsonl"));
        write_session_jsonl(&path, SID_A, "C:/some/project");

        let res = scan(tmp.path(), "C:/some/project", SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1, "got {res:?}");
        assert_eq!(res[0].session_id, SID_A);
    }

    #[test]
    fn scan_skips_jsonl_with_mismatched_cwd() {
        let tmp = TempDir::new().unwrap();
        let cwd_dir = tmp.path().join("--c-some-project--");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        let path = cwd_dir.join(format!("2026-04-22T08-00-00-000Z_{SID_A}.jsonl"));
        write_session_jsonl(&path, SID_A, "C:/different/project");

        let res = scan(tmp.path(), "C:/some/project", SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_skips_files_modified_before_since() {
        let tmp = TempDir::new().unwrap();
        let cwd_dir = tmp.path().join("--c-some-project--");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        let path = cwd_dir.join(format!("2026-04-22T08-00-00-000Z_{SID_A}.jsonl"));
        write_session_jsonl(&path, SID_A, "C:/some/project");

        let future = SystemTime::now() + std::time::Duration::from_secs(60 * 60 * 24 * 365 * 50);
        let res = scan(tmp.path(), "C:/some/project", future);
        assert!(res.is_empty(), "got {res:?}");
    }
}
