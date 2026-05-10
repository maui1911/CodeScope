//! Claude Code session adoption — find the JSONL transcript a freshly
//! launched `claude` pwsh session writes inside
//! `~/.claude/projects/<encoded-cwd>/`.
//!
//! Mirrors `ClaudeSessionDiscovery` from the C# build but uses polling
//! only (no `notify` crate dep). The caller drives the cadence — the
//! C# build polls every 350 ms next to its FSWatcher; we keep the same
//! interval as a single source of truth.
//!
//! # Semantics
//!
//! Adoption fires once per unique `.jsonl` path discovered with
//! `created_at >= since` *or* `last_modified >= since`. Tracking
//! already-fired paths is the caller's responsibility (the C# build
//! does this inside `WatchHandle._fired`); we return *every* eligible
//! file each call so the caller can dedupe by id and keep the watch
//! alive across `/clear` rotations.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::claude_telemetry::encode_cwd;

/// Suggested poll interval for adoption watches — 350 ms matches the
/// C# `ClaudeSessionDiscovery.PollInterval`.
pub const POLL_INTERVAL_MS: u64 = 350;

/// One adoption candidate found in a projects-root scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    /// UUID parsed from the `.jsonl` filename (without extension).
    pub session_id: String,
    /// Absolute path to the `.jsonl` file.
    pub path: PathBuf,
}

/// Scan `~/.claude/projects/<encoded_cwd(working_directory)>/` for
/// `.jsonl` files whose `created_at` or `last_modified` timestamps are
/// at or after `since`. Returns every match — the caller dedupes by
/// path.
///
/// Returns an empty vec when the directory doesn't exist, can't be
/// read, or contains no eligible candidates. Callers should treat
/// "empty" as "keep polling".
///
/// Filenames must parse as UUIDs (Claude Code v2.x uses 8-4-4-4-12 hex
/// digits) — anything else is silently skipped. Mirrors C#
/// `WatchHandle.IsValidSessionId`.
pub fn scan(
    projects_root: &Path,
    working_directory: &str,
    since: SystemTime,
) -> Vec<AdoptionCandidate> {
    let dir = projects_root.join(encode_cwd(working_directory));
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_session_id(stem) {
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
        out.push(AdoptionCandidate {
            session_id: stem.to_owned(),
            path,
        });
    }
    out
}

/// Does an auto-typed command launch a Claude Code session?
///
/// Mirrors the `agentId == "claude"` branch in C#
/// `MainViewModel.RegisterAgentTelemetry`. We don't carry an
/// `agentId` field on tabs — instead we match the literal command
/// `claude` modulo leading whitespace and trailing args / flags.
/// `claude --resume <id>` and `claude --new` both qualify; plain
/// `pwsh` (`None`) and other agents (`pi`, `opencode`, `copilot`,
/// `gemini`, …) do not.
pub fn is_claude_auto_type(auto_type: Option<&str>) -> bool {
    let Some(s) = auto_type else { return false };
    let s = s.trim_start();
    let first = s.split(|c: char| c.is_whitespace()).next().unwrap_or("");
    first.eq_ignore_ascii_case("claude")
}

/// Recognise a Claude Code session id — the `.jsonl` filename without
/// extension. The C# build uses `Guid.TryParseExact(id, "D")` which
/// accepts the `8-4-4-4-12` lowercase-hex form. We mirror that exactly:
/// 36 chars, ASCII hex digits with hyphens at 8/13/18/23.
pub fn is_session_id(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let is_hyphen = matches!(i, 8 | 13 | 18 | 23);
        if is_hyphen {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    const SID_A: &str = "11111111-2222-3333-4444-555555555555";
    const SID_B: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    // --- is_claude_auto_type ---

    #[test]
    fn auto_type_none_is_not_claude() {
        assert!(!is_claude_auto_type(None));
    }

    #[test]
    fn auto_type_plain_claude_is_claude() {
        assert!(is_claude_auto_type(Some("claude")));
    }

    #[test]
    fn auto_type_claude_with_args_is_claude() {
        assert!(is_claude_auto_type(Some("claude --resume abc-123")));
        assert!(is_claude_auto_type(Some("claude --new")));
    }

    #[test]
    fn auto_type_leading_whitespace_is_claude() {
        assert!(is_claude_auto_type(Some("   claude")));
        assert!(is_claude_auto_type(Some("\tclaude --new")));
    }

    #[test]
    fn auto_type_case_insensitive() {
        assert!(is_claude_auto_type(Some("Claude")));
        assert!(is_claude_auto_type(Some("CLAUDE")));
    }

    #[test]
    fn auto_type_other_agents_are_not_claude() {
        assert!(!is_claude_auto_type(Some("pi")));
        assert!(!is_claude_auto_type(Some("opencode")));
        assert!(!is_claude_auto_type(Some("copilot")));
        assert!(!is_claude_auto_type(Some("gemini")));
        assert!(!is_claude_auto_type(Some("pwsh")));
    }

    #[test]
    fn auto_type_empty_string_is_not_claude() {
        assert!(!is_claude_auto_type(Some("")));
        assert!(!is_claude_auto_type(Some("   ")));
    }

    #[test]
    fn auto_type_substring_is_not_match() {
        // "claudeflare" is not the agent — only `claude` itself.
        assert!(!is_claude_auto_type(Some("claudeflare")));
        assert!(!is_claude_auto_type(Some("notclaude")));
    }

    // --- is_session_id ---

    #[test]
    fn is_session_id_accepts_lowercase_uuid() {
        assert!(is_session_id(SID_A));
        assert!(is_session_id(SID_B));
    }

    #[test]
    fn is_session_id_accepts_uppercase_hex() {
        assert!(is_session_id("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"));
    }

    #[test]
    fn is_session_id_rejects_wrong_length() {
        assert!(!is_session_id("11111111-2222-3333-4444-55555555555")); // 35
        assert!(!is_session_id("11111111-2222-3333-4444-5555555555555")); // 37
    }

    #[test]
    fn is_session_id_rejects_misplaced_hyphens() {
        assert!(!is_session_id("111111112-2223-3334-4445-55555555555"));
    }

    #[test]
    fn is_session_id_rejects_non_hex() {
        assert!(!is_session_id("zzzzzzzz-2222-3333-4444-555555555555"));
    }

    #[test]
    fn is_session_id_rejects_empty_and_arbitrary() {
        assert!(!is_session_id(""));
        assert!(!is_session_id("not-a-uuid"));
        assert!(!is_session_id("backup"));
    }

    // --- scan ---

    fn working_dir_layout(tmp: &TempDir, working_directory: &str) -> PathBuf {
        let dir = tmp.path().join(encode_cwd(working_directory));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_jsonl(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}.jsonl"));
        std::fs::write(&path, b"{\"type\":\"user\"}\n").unwrap();
        path
    }

    #[test]
    fn scan_returns_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let res = scan(tmp.path(), "/no/such/dir", SystemTime::UNIX_EPOCH);
        assert!(res.is_empty());
    }

    #[test]
    fn scan_finds_jsonl_after_since() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        let _path = write_jsonl(&dir, SID_A);

        // since = a long time ago, file definitely qualifies.
        let res = scan(tmp.path(), wd, SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1, "expected one candidate, got {res:?}");
        assert_eq!(res[0].session_id, SID_A);
    }

    #[test]
    fn scan_skips_non_jsonl_files() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        std::fs::write(dir.join("not-a-transcript.txt"), b"hi").unwrap();
        std::fs::write(dir.join(format!("{SID_A}.json")), b"hi").unwrap();
        let res = scan(tmp.path(), wd, SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_skips_filenames_that_arent_uuids() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        write_jsonl(&dir, "backup");
        write_jsonl(&dir, "not-a-real-id");
        let res = scan(tmp.path(), wd, SystemTime::UNIX_EPOCH);
        assert!(res.is_empty(), "got {res:?}");
    }

    #[test]
    fn scan_returns_all_eligible_files() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        write_jsonl(&dir, SID_A);
        write_jsonl(&dir, SID_B);

        let res = scan(tmp.path(), wd, SystemTime::UNIX_EPOCH);
        let mut ids: Vec<_> = res.iter().map(|c| c.session_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec![SID_A.to_owned(), SID_B.to_owned()]);
    }

    #[test]
    fn scan_filters_files_modified_before_since() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        write_jsonl(&dir, SID_A);

        // since = far in the future — nothing qualifies.
        let future = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365 * 50);
        let res = scan(tmp.path(), wd, future);
        assert!(res.is_empty(), "got {res:?}");
    }
}
