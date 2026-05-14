//! OpenCode session adoption — find the message directory a freshly
//! launched `opencode` session writes under
//! `~/.local/share/opencode/project/<slug>/storage/message/<sid>/`.
//!
//! Mirrors `OpenCodeSessionDiscovery` from the C# build but uses
//! polling only (no `notify` crate dep). Same `scan_*` shape as
//! [`crate::agents::claude::discovery`]: pure logic, the caller drives the
//! cadence.
//!
//! # Semantics
//!
//! Adoption fires once per unique session id discovered when:
//!
//! 1. Some `msg_*.json` file exists under
//!    `<root>/.../message/<sid>/`.
//! 2. The file's `created_at` or `last_modified` is at or after
//!    `since`.
//! 3. The first parseable message's `cwd` (canonicalised) matches the
//!    tab's working directory.
//!
//! Tracking already-fired ids is the caller's responsibility (mirrors
//! `WatchHandle._firedSessions`). Returns *every* eligible candidate
//! each call so the caller can dedupe by id.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agents::opencode::telemetry::parse_content;
use crate::path_canon::canonicalize_path;

/// Suggested poll interval — 400 ms matches the C#
/// `OpenCodeSessionDiscovery.PollInterval`.
pub const POLL_INTERVAL_MS: u64 = 400;

/// Exclusive bound on recursion depth when walking the data root:
/// `walk` is invoked at depth 0 for the root and the guard rejects
/// calls at `depth >= MAX_RECURSION_DEPTH`, so the deepest directory
/// ever read from is `MAX_RECURSION_DEPTH - 1` levels below the root.
/// The OpenCode layout is
/// `project/<slug>/storage/message/<sid>/<msg>.json` — 5 levels of
/// nesting below the root — so 6 covers it cleanly while still
/// terminating on a stray symlink loop.
const MAX_RECURSION_DEPTH: usize = 6;

/// One adoption candidate found in a data-root scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// OpenCode session id — the directory name immediately under
    /// `message/`. Not necessarily a UUID; OpenCode mints its own
    /// session ids.
    pub session_id: String,
    /// Absolute path to the first matching `msg_*.json` we found.
    pub message_path: PathBuf,
}

/// Recursively scan `data_root` for OpenCode message files whose
/// `created_at` or `last_modified` timestamps are at or after `since`,
/// and whose first parseable message records a `cwd` that
/// canonicalises to the same string as `working_directory`.
///
/// Returns an empty vec when the root doesn't exist, can't be read,
/// or contains no eligible candidates. Callers should treat "empty"
/// as "keep polling".
pub fn scan(
    data_root: &Path,
    working_directory: &str,
    since: SystemTime,
) -> Vec<AdoptionCandidate> {
    if !data_root.is_dir() {
        return Vec::new();
    }
    let canon_cwd = canonicalize_path(working_directory);
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    walk(data_root, since, &canon_cwd, 0, &mut seen, &mut out);
    out
}

fn walk(
    dir: &Path,
    since: SystemTime,
    canon_cwd: &str,
    depth: usize,
    seen: &mut std::collections::HashSet<String>,
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
            walk(&path, since, canon_cwd, depth + 1, seen, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_message_file(name) {
            continue;
        }
        // Path layout: <root>/.../message/<sid>/msg_*.json
        let Some(parent) = path.parent() else { continue };
        let Some(grandparent) = parent.parent() else { continue };
        let parent_name = grandparent.file_name().and_then(|s| s.to_str());
        if parent_name.map(|n| !n.eq_ignore_ascii_case("message")).unwrap_or(true) {
            continue;
        }
        let Some(sid) = parent.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if sid.is_empty() {
            continue;
        }
        if seen.contains(sid) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let created = meta.created().ok();
        let modified = crate::telemetry::modified_or_none(&meta);
        let eligible = matches!(created, Some(t) if t >= since)
            || matches!(modified, Some(t) if t >= since);
        if !eligible {
            continue;
        }
        if !cwd_matches(&path, canon_cwd) {
            continue;
        }
        seen.insert(sid.to_owned());
        out.push(AdoptionCandidate {
            session_id: sid.to_owned(),
            message_path: path,
        });
    }
}

fn is_message_file(name: &str) -> bool {
    if !name.starts_with("msg_") {
        return false;
    }
    // Suffix-match `.json` case-insensitively without allocating a
    // new lowercase `String` per directory entry — the recursive walk
    // can hit large data roots, so the per-entry allocation is hot.
    const EXT: &[u8] = b".json";
    let bytes = name.as_bytes();
    if bytes.len() <= EXT.len() {
        return false;
    }
    let tail = &bytes[bytes.len() - EXT.len()..];
    tail.eq_ignore_ascii_case(EXT)
}

fn cwd_matches(path: &Path, canon_cwd: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(entry) = parse_content(&content) else {
        return false;
    };
    let Some(cwd) = entry.cwd else {
        return false;
    };
    canonicalize_path(&cwd) == canon_cwd
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_msg(path: &Path, cwd: &str) {
        // Mirrors the assistant-message shape `parse_content` reads
        // `metadata.assistant.path.cwd` from. The discovery scan only
        // matches messages whose first parseable file records a cwd,
        // so we always write an assistant message here.
        let content = format!(
            r#"{{
                "id":"msg-1","role":"assistant",
                "metadata":{{
                    "sessionID":"x",
                    "time":{{"created":1700000000000}},
                    "assistant":{{
                        "modelID":"m","providerID":"p",
                        "path":{{"cwd":"{cwd}","root":"{cwd}"}},
                        "tokens":{{"input":0,"output":0,"reasoning":0,"cache":{{"read":0,"write":0}}}},
                        "cost":0
                    }}
                }},
                "parts":[{{"type":"text","text":"ok"}}]
            }}"#
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content.as_bytes()).unwrap();
    }

    #[test]
    fn scan_returns_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let res = scan(
            &tmp.path().join("nonexistent"),
            "/some/dir",
            SystemTime::UNIX_EPOCH,
        );
        assert!(res.is_empty());
    }

    #[test]
    fn scan_finds_candidate_under_message_dir() {
        let tmp = TempDir::new().unwrap();
        let msg_path = tmp
            .path()
            .join("project/slug/storage/message/sid-123/msg_001.json");
        write_msg(&msg_path, "C:/dev/x");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1, "got {res:?}");
        assert_eq!(res[0].session_id, "sid-123");
    }

    #[test]
    fn scan_skips_files_outside_message_dir() {
        let tmp = TempDir::new().unwrap();
        // Wrong grandparent — not "message".
        let msg_path = tmp
            .path()
            .join("project/slug/storage/foo/sid-123/msg_001.json");
        write_msg(&msg_path, "C:/dev/x");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_skips_mismatched_cwd() {
        let tmp = TempDir::new().unwrap();
        let msg_path = tmp
            .path()
            .join("project/slug/storage/message/sid-123/msg_001.json");
        write_msg(&msg_path, "C:/dev/y");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_dedupes_by_session_id() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp
            .path()
            .join("project/slug/storage/message/sid-123");
        std::fs::create_dir_all(&dir).unwrap();
        write_msg(&dir.join("msg_001.json"), "C:/dev/x");
        write_msg(&dir.join("msg_002.json"), "C:/dev/x");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].session_id, "sid-123");
    }
}
