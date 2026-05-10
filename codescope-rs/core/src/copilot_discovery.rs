//! Copilot session adoption — find the session-state directory a
//! freshly launched `copilot` session writes under
//! `~/.copilot/session-state/<sid>/`.
//!
//! Mirrors `CopilotSessionDiscovery` from the C# build but uses
//! polling only (no `notify` crate dep). Same `scan_*` shape as
//! [`crate::claude_discovery`]: pure logic, the caller drives the
//! cadence.
//!
//! # Semantics
//!
//! Adoption fires once per unique session id (a UUID directory name)
//! discovered when:
//!
//! 1. The directory's name parses as a UUID.
//! 2. `created_at` or `last_modified` is at or after `since`.
//! 3. Either `workspace.yaml` records a matching `cwd:` line, or
//!    `events.jsonl`'s first `session.start` event records a matching
//!    `data.context.cwd`.
//!
//! Tracking already-fired ids is the caller's responsibility (mirrors
//! `WatchHandle._fired`). Returns *every* eligible candidate each call
//! so the caller can dedupe by id.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::path_canon::canonicalize_path;

/// Suggested poll interval — 350 ms matches the C#
/// `CopilotSessionDiscovery.PollInterval`.
pub const POLL_INTERVAL_MS: u64 = 350;

/// One adoption candidate found in a session-state-root scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// UUID parsed from the directory name.
    pub session_id: String,
    /// Absolute path to the session directory.
    pub session_dir: PathBuf,
}

/// Scan `session_state_root` for direct-child UUID directories whose
/// `created_at` or `last_modified` is at or after `since`, and whose
/// `workspace.yaml` (or `events.jsonl` fallback) records a `cwd` that
/// canonicalises to the same string as `working_directory`.
///
/// Returns an empty vec when the root doesn't exist, can't be read,
/// or contains no eligible candidates. Callers should treat "empty"
/// as "keep polling".
pub fn scan(
    session_state_root: &Path,
    working_directory: &str,
    since: SystemTime,
) -> Vec<AdoptionCandidate> {
    if !session_state_root.is_dir() {
        return Vec::new();
    }
    let canon_cwd = canonicalize_path(working_directory);
    let entries = match std::fs::read_dir(session_state_root) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_uuid(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let created = meta.created().ok();
        let modified = meta.modified().ok();
        let eligible = matches!(created, Some(t) if t >= since)
            || matches!(modified, Some(t) if t >= since);
        if !eligible {
            continue;
        }
        if !cwd_matches(&path, &canon_cwd) {
            continue;
        }
        out.push(AdoptionCandidate {
            session_id: name.to_owned(),
            session_dir: path,
        });
    }
    out
}

/// Accepts the canonical `8-4-4-4-12` UUID form (case-insensitive).
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let is_dash = matches!(i, 8 | 13 | 18 | 23);
        if is_dash {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn cwd_matches(session_dir: &Path, canon_cwd: &str) -> bool {
    let yaml = session_dir.join("workspace.yaml");
    if let Some(cwd) = read_yaml_cwd(&yaml) {
        return canonicalize_path(&cwd) == canon_cwd;
    }
    let events = session_dir.join("events.jsonl");
    if let Some(cwd) = read_session_start_cwd(&events) {
        return canonicalize_path(&cwd) == canon_cwd;
    }
    false
}

/// Tiny single-line YAML reader: scan for `cwd: <value>` and return the
/// trimmed value. We don't pull in a YAML crate — Copilot's
/// workspace.yaml has a stable two-line layout and a real parser would
/// be overkill for one field.
fn read_yaml_cwd(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("cwd:") else {
            continue;
        };
        let value = rest.trim();
        let value = value
            .trim_start_matches('"')
            .trim_end_matches('"')
            .trim_start_matches('\'')
            .trim_end_matches('\'');
        if value.is_empty() {
            return None;
        }
        return Some(value.to_owned());
    }
    None
}

/// Peek `events.jsonl` for the first `session.start` event and return
/// its `data.context.cwd`. Mirrors C# `CopilotSessionDiscovery.CwdMatchesFromEvents`,
/// which skips malformed/non-`session.start` lines while peeking the
/// first few records (a partial / garbled line landing before the
/// real `session.start` is rare but does happen during a save race
/// and shouldn't poison adoption).
///
/// Bounded to the first [`MAX_HEADER_LINES`] lines so a corrupt or
/// arbitrarily-large file can't turn this peek into a full file
/// scan.
fn read_session_start_cwd(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    const MAX_HEADER_LINES: usize = 5;
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut inspected = 0usize;
    for line in reader.lines() {
        let Ok(line) = line else { return None };
        if line.trim().is_empty() {
            continue;
        }
        inspected += 1;
        if inspected > MAX_HEADER_LINES {
            return None;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            // Skip a partial / garbled line and keep peeking — same
            // posture as the C# `try { ParseLine } catch { continue }`.
            Err(_) => continue,
        };
        let Some(obj) = v.as_object() else { continue };
        if obj.get("type").and_then(serde_json::Value::as_str) != Some("session.start") {
            continue;
        }
        let cwd = obj
            .get("data")
            .and_then(|d| d.get("context"))
            .and_then(|c| c.get("cwd"))
            .and_then(serde_json::Value::as_str)?;
        return Some(cwd.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const SID_A: &str = "11111111-2222-3333-4444-555555555555";

    fn write_workspace_yaml(dir: &Path, cwd: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("workspace.yaml"),
            format!("schemaVersion: 1\ncwd: {cwd}\n"),
        )
        .unwrap();
    }

    #[test]
    fn scan_returns_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let res = scan(
            &tmp.path().join("nope"),
            "/some/dir",
            SystemTime::UNIX_EPOCH,
        );
        assert!(res.is_empty());
    }

    #[test]
    fn scan_finds_uuid_dir_with_matching_cwd() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(SID_A);
        write_workspace_yaml(&session_dir, "C:/dev/x");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1, "got {res:?}");
        assert_eq!(res[0].session_id, SID_A);
    }

    #[test]
    fn scan_skips_non_uuid_dirs() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join("not-a-uuid");
        write_workspace_yaml(&session_dir, "C:/dev/x");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_skips_mismatched_cwd() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(SID_A);
        write_workspace_yaml(&session_dir, "C:/dev/y");

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_falls_back_to_events_jsonl_when_yaml_missing() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(SID_A);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("events.jsonl"),
            br#"{"type":"session.start","timestamp":"2026-04-22T08:00:00Z","data":{"sessionId":"x","selectedModel":"gpt-5","context":{"cwd":"C:/dev/x"}}}
"#,
        )
        .unwrap();

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1, "got {res:?}");
    }

    #[test]
    fn events_jsonl_peek_skips_garbled_first_line() {
        // A partial line landing before the real `session.start`
        // shouldn't poison adoption — mirrors C# `try { ParseLine }
        // catch { continue }` posture.
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(SID_A);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("events.jsonl"),
            br#"{"type":"sessio
{"type":"session.start","timestamp":"2026-04-22T08:00:00Z","data":{"sessionId":"x","selectedModel":"gpt-5","context":{"cwd":"C:/dev/x"}}}
"#,
        )
        .unwrap();

        let res = scan(tmp.path(), "C:/dev/x", SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1, "got {res:?}");
    }

    #[test]
    fn is_uuid_accepts_canonical_form() {
        assert!(is_uuid(SID_A));
        assert!(is_uuid("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"));
    }

    #[test]
    fn is_uuid_rejects_off_lengths() {
        assert!(!is_uuid(""));
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("11111111-2222-3333-4444-55555555555"));
    }
}
