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
//!
//! Transcripts written by *headless* `claude` processes spawned through
//! the Agent SDK are skipped — see [`TranscriptOrigin`] and issue #336.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agents::claude::telemetry::encode_cwd;

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
///
/// Transcripts whose head identifies them as SDK-spawned
/// ([`TranscriptOrigin::Sdk`]) are skipped too — they live in the same
/// directory as the interactive session's transcript and would
/// otherwise win the "newest file" race (issue #336).
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
        let modified = crate::telemetry::modified_or_none(&meta);
        let eligible = matches!(created, Some(t) if t >= since)
            || matches!(modified, Some(t) if t >= since);
        if !eligible {
            continue;
        }
        if transcript_origin(&path) == TranscriptOrigin::Sdk {
            continue;
        }
        out.push(AdoptionCandidate {
            session_id: stem.to_owned(),
            path,
        });
    }
    out
}

/// Who wrote a transcript, as far as its head reveals.
///
/// Claude Code stamps every `user` / `assistant` / `attachment` line
/// with an `entrypoint` field: `"cli"` and `"claude-desktop"` for the
/// interactive sessions we host in a tab, `"sdk-py"` / `"sdk-cli"` /
/// `"sdk-ts"` for headless processes driven through the Agent SDK.
/// Those SDK runs land in the same `~/.claude/projects/<encoded-cwd>/`
/// directory as the interactive session — the `security-guidance`
/// plugin, for instance, runs a background Opus review through
/// `claude_agent_sdk.query(...)` on every `Stop` / `git commit` /
/// `git push` hook, minting a fresh transcript each time. Without
/// this check every such run looked like a `/clear` rotation: the tab
/// adopted the review's uuid, persisted it, and the next resume opened
/// "Review this change for security vulnerabilities." on Opus 4.7
/// instead of the user's conversation (issue #336).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptOrigin {
    /// First `entrypoint`-bearing line is not an SDK entrypoint.
    Interactive,
    /// First `entrypoint`-bearing line starts with `sdk`.
    Sdk,
    /// No `entrypoint`-bearing line within the inspected head — a
    /// brand-new interactive transcript (which opens with `mode` /
    /// `file-history-snapshot` lines), an older CLI without the field,
    /// or an unreadable file. Treated as adoptable so existing
    /// behaviour is unchanged for anything we can't classify.
    Unknown,
}

/// Lines to inspect before giving up on classification. Interactive
/// transcripts carry the field on their first `user` line, which sits
/// behind a handful of bookkeeping lines; SDK transcripts put it on
/// line 3 (two `queue-operation` lines first). 64 leaves room for
/// `attachment` bursts and future preamble without reading a
/// multi-megabyte file on every 350 ms poll.
const ORIGIN_PEEK_LINES: usize = 64;

/// Bytes to inspect before giving up. `BufRead::lines` buffers a whole
/// line, so without a byte ceiling a corrupt or hostile transcript with
/// no newline in it would be read into memory in full on every poll.
/// The decisive line is the *first* `user` turn, and for SDK reviews
/// that line carries the entire diff under review — the largest one in
/// the reporting store is ~380 KiB — so the cap sits an order of
/// magnitude above that. A transcript whose decisive line straddles
/// the cap classifies as `Unknown` (torn JSON), i.e. adoptable.
const ORIGIN_PEEK_BYTES: u64 = 4 * 1024 * 1024;

/// Classify a transcript by the first `entrypoint` field within its
/// head. See [`TranscriptOrigin`] for what each variant means.
pub fn transcript_origin(path: &Path) -> TranscriptOrigin {
    use std::io::{BufRead, BufReader, Read};
    let Ok(file) = std::fs::File::open(path) else {
        return TranscriptOrigin::Unknown;
    };
    let reader = BufReader::new(file.take(ORIGIN_PEEK_BYTES));
    for line in reader.lines().take(ORIGIN_PEEK_LINES) {
        // A read error (or invalid UTF-8) ends the peek; whatever we
        // saw before it wasn't decisive, so fall through to Unknown.
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            // A torn line mid-write is the common case here — the
            // next line may still be decisive.
            continue;
        };
        let Some(entrypoint) = v.get("entrypoint").and_then(serde_json::Value::as_str) else {
            continue;
        };
        return if entrypoint.starts_with("sdk") {
            TranscriptOrigin::Sdk
        } else {
            TranscriptOrigin::Interactive
        };
    }
    TranscriptOrigin::Unknown
}

/// Recognise a Claude Code session id — the `.jsonl` filename without
/// extension. The C# build uses `Guid.TryParseExact(id, "D")` which
/// accepts the `8-4-4-4-12` UUID form *case-insensitively* (`D`
/// allows either case for the hex digits). We mirror that: 36 chars,
/// ASCII hex digits with hyphens at indices 8 / 13 / 18 / 23.
/// Practical Claude Code transcripts always emit lowercase, but the
/// validator stays permissive so a manually-renamed file or future
/// CLI revision keeps working.
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

    // --- transcript_origin ---

    /// Head of a real interactive transcript (Claude Code 2.1.245):
    /// bookkeeping lines first, `entrypoint` on the first `user` line.
    const INTERACTIVE_HEAD: &str = concat!(
        "{\"type\":\"mode\",\"mode\":\"default\"}\n",
        "{\"type\":\"permission-mode\",\"mode\":\"default\"}\n",
        "{\"type\":\"file-history-snapshot\",\"snapshot\":{}}\n",
        "{\"type\":\"user\",\"entrypoint\":\"cli\",\"promptSource\":\"typed\",",
        "\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
    );

    /// Head of a transcript minted by the security-guidance plugin's
    /// background review through the Python Agent SDK.
    const SDK_HEAD: &str = concat!(
        "{\"type\":\"queue-operation\",\"operation\":\"enqueue\"}\n",
        "{\"type\":\"queue-operation\",\"operation\":\"dequeue\"}\n",
        "{\"type\":\"user\",\"entrypoint\":\"sdk-py\",\"promptSource\":\"sdk\",",
        "\"message\":{\"role\":\"user\",\"content\":",
        "\"Review this change for security vulnerabilities.\"}}\n",
    );

    fn write_transcript(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(format!("{name}.jsonl"));
        std::fs::write(&path, body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn origin_of_interactive_transcript_is_interactive() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(tmp.path(), SID_A, INTERACTIVE_HEAD);
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Interactive);
    }

    #[test]
    fn origin_of_claude_desktop_transcript_is_interactive() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            SID_A,
            "{\"type\":\"user\",\"entrypoint\":\"claude-desktop\"}\n",
        );
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Interactive);
    }

    #[test]
    fn origin_of_sdk_transcript_is_sdk() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(tmp.path(), SID_A, SDK_HEAD);
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Sdk);
    }

    #[test]
    fn origin_recognises_every_sdk_flavour() {
        let tmp = TempDir::new().unwrap();
        for ep in ["sdk-py", "sdk-cli", "sdk-ts"] {
            let path = write_transcript(
                tmp.path(),
                SID_A,
                &format!("{{\"type\":\"user\",\"entrypoint\":\"{ep}\"}}\n"),
            );
            assert_eq!(transcript_origin(&path), TranscriptOrigin::Sdk, "{ep}");
        }
    }

    #[test]
    fn origin_is_unknown_without_entrypoint_line() {
        let tmp = TempDir::new().unwrap();
        // Only the bookkeeping preamble so far — the user hasn't typed.
        let path = write_transcript(
            tmp.path(),
            SID_A,
            "{\"type\":\"mode\",\"mode\":\"default\"}\n{\"type\":\"file-history-snapshot\"}\n",
        );
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Unknown);
        // Empty file and missing file behave the same.
        let empty = write_transcript(tmp.path(), SID_B, "");
        assert_eq!(transcript_origin(&empty), TranscriptOrigin::Unknown);
        assert_eq!(
            transcript_origin(&tmp.path().join("missing.jsonl")),
            TranscriptOrigin::Unknown
        );
    }

    #[test]
    fn origin_skips_torn_lines_and_keeps_reading() {
        let tmp = TempDir::new().unwrap();
        let body = format!("{{\"type\":\"mode\",\"mo\n{SDK_HEAD}");
        let path = write_transcript(tmp.path(), SID_A, &body);
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Sdk);
    }

    #[test]
    fn origin_gives_up_after_peek_window() {
        let tmp = TempDir::new().unwrap();
        let mut body = "{\"type\":\"attachment\"}\n".repeat(ORIGIN_PEEK_LINES);
        body.push_str(SDK_HEAD);
        let path = write_transcript(tmp.path(), SID_A, &body);
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Unknown);
    }

    #[test]
    fn origin_gives_up_after_byte_cap() {
        let tmp = TempDir::new().unwrap();
        // One newline-free line past the cap, followed by a decisive
        // SDK head we must never reach. Buffered `lines()` would
        // otherwise slurp the whole thing.
        let mut body = "{\"type\":\"mode\",\"pad\":\"".to_owned();
        body.push_str(&"x".repeat(ORIGIN_PEEK_BYTES as usize + 1));
        body.push_str("\"}\n");
        body.push_str(SDK_HEAD);
        let path = write_transcript(tmp.path(), SID_A, &body);
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Unknown);
    }

    #[test]
    fn origin_classifies_large_sdk_first_turn_under_cap() {
        let tmp = TempDir::new().unwrap();
        // A review of a big diff: ~1 MiB user line, well inside the
        // cap, with `entrypoint` after the payload as Claude writes it.
        let body = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}},\"entrypoint\":\"sdk-py\"}}\n",
            "d".repeat(1024 * 1024)
        );
        let path = write_transcript(tmp.path(), SID_A, &body);
        assert_eq!(transcript_origin(&path), TranscriptOrigin::Sdk);
    }

    #[test]
    fn scan_skips_sdk_transcripts() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        write_transcript(&dir, SID_A, INTERACTIVE_HEAD);
        write_transcript(&dir, SID_B, SDK_HEAD);

        let res = scan(tmp.path(), wd, SystemTime::UNIX_EPOCH);
        let ids: Vec<_> = res.iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(ids, vec![SID_A], "SDK transcript must not be a candidate");
    }

    #[test]
    fn scan_still_returns_unclassifiable_transcripts() {
        let tmp = TempDir::new().unwrap();
        let wd = "/some/project";
        let dir = working_dir_layout(&tmp, wd);
        // The existing `write_jsonl` helper writes a bare `user` line
        // with no `entrypoint` — the pre-#336 shape every other scan
        // test relies on. It must keep adopting.
        write_jsonl(&dir, SID_A);
        let res = scan(tmp.path(), wd, SystemTime::UNIX_EPOCH);
        assert_eq!(res.len(), 1);
    }
}
