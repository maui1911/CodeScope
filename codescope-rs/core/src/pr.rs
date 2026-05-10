//! Per-worktree pull-request URL detection.
//!
//! Shells out to `gh pr list --head <branch> --state open --json url
//! --limit 1` and returns the first PR's URL (or `None` if no open PR
//! is associated with the branch). Mirrors the C# build's
//! [`GitHubPullRequestService.GetOpenPrForBranchAsync`] — we keep the
//! `--state open` filter and the `--limit 1` cap so the invocation is
//! identical, but parse the JSON ourselves rather than relying on the
//! local `jq` binary (gh ships its own `--jq` evaluator, but JSON
//! parsing in pure Rust is small enough that we'd rather not depend on
//! external eval semantics for a single field).
//!
//! Caching: this module is intentionally stateless — callers (the
//! sidebar) own the cache. The C# build polls every 30 s with
//! per-worktree exponential backoff up to 5 minutes
//! (`PullRequestStatusPoller`); the Rust port currently lazy-fetches
//! on right-click and caches the result on the sidebar entity. That's
//! a deviation we accept for now — every render path that consumes
//! the URL goes through the cache, and a dedicated background poller
//! is a follow-up task tracked in `docs/HANDOFF.md`.

use std::path::Path;
use std::process::{Command, Stdio};

/// Look up the open pull request URL for `branch` in the repo at
/// `working_dir`. Returns `None` when no open PR is associated, when
/// `gh` is not on PATH, or when the call fails for any reason — UI
/// callers want a binary "show row / hide row" answer and the
/// distinction between "gh missing" and "no PR" is not worth a third
/// state in the menu.
///
/// Security: rejects branch names starting with `-` so a pathological
/// branch can't be reinterpreted as a `gh pr list` flag (e.g.
/// `--help`, `--state`, …) — same defence the `git::rebase_onto`
/// helper applies.
pub fn detect_pr_url(working_dir: &Path, branch: &str) -> Option<String> {
    if branch.is_empty() || branch.starts_with('-') {
        return None;
    }
    let output = Command::new("gh")
        .args([
            "pr", "list", "--head", branch, "--state", "open", "--json", "url", "--limit", "1",
        ])
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pr_url_json(&stdout)
}

/// Parse the output of `gh pr list --json url --limit 1`. The shape
/// is a JSON array — empty when there's no PR, otherwise objects with
/// a `"url"` string field. We parse the first entry by hand to avoid
/// pulling `serde_json` into `core` for one tiny use site.
///
/// Returns the URL string on success, `None` otherwise. Tolerant of
/// trailing whitespace and CRLF — gh on Windows occasionally emits
/// CRLF in piped output.
pub(crate) fn parse_pr_url_json(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return None;
    }
    // We expect `[{"url":"https://github.com/owner/repo/pull/42"}]`.
    // Find the first `"url"` key, then the next `"…"` value after the
    // colon. This is dumb-but-safe: gh's `--json url` emits exactly
    // one field per row, so there's no ambiguity with nested objects.
    let key_idx = trimmed.find("\"url\"")?;
    let after_key = &trimmed[key_idx + "\"url\"".len()..];
    let colon_idx = after_key.find(':')?;
    let after_colon = after_key[colon_idx + 1..].trim_start();
    let rest = after_colon.strip_prefix('"')?;
    let end = rest.find('"')?;
    let url = &rest[..end];
    if url.is_empty() {
        None
    } else {
        Some(url.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_typical_gh_output() {
        let json = "[{\"url\":\"https://github.com/owner/repo/pull/42\"}]";
        assert_eq!(
            parse_pr_url_json(json).as_deref(),
            Some("https://github.com/owner/repo/pull/42")
        );
    }

    #[test]
    fn parse_pretty_printed_output() {
        // gh sometimes pretty-prints when stdout is a tty; in our
        // pipe-captured case it stays compact, but the parser should
        // not assume that.
        let json = "[\n  {\n    \"url\": \"https://github.com/owner/repo/pull/7\"\n  }\n]";
        assert_eq!(
            parse_pr_url_json(json).as_deref(),
            Some("https://github.com/owner/repo/pull/7")
        );
    }

    #[test]
    fn parse_empty_array_returns_none() {
        assert_eq!(parse_pr_url_json("[]"), None);
        assert_eq!(parse_pr_url_json("[]\n"), None);
    }

    #[test]
    fn parse_empty_input_returns_none() {
        assert_eq!(parse_pr_url_json(""), None);
        assert_eq!(parse_pr_url_json("   \n"), None);
    }

    #[test]
    fn parse_missing_url_returns_none() {
        // Defensive: gh is unlikely to emit a row without `url` when
        // we asked for that field, but if it ever does we should hide
        // the row rather than panic.
        assert_eq!(parse_pr_url_json("[{}]"), None);
    }

    #[test]
    fn parse_empty_url_string_returns_none() {
        assert_eq!(parse_pr_url_json("[{\"url\":\"\"}]"), None);
    }

    #[test]
    fn detect_rejects_dash_prefixed_branch() {
        // Security: `--state` must not be reinterpreted as a flag.
        // We can't easily intercept the spawn, but the function
        // returns None up-front without spawning gh.
        let result = detect_pr_url(&PathBuf::from("."), "--state");
        assert!(result.is_none());
    }

    #[test]
    fn detect_rejects_empty_branch() {
        let result = detect_pr_url(&PathBuf::from("."), "");
        assert!(result.is_none());
    }

    #[test]
    fn detect_returns_none_when_gh_missing() {
        // We intentionally don't gate this test on `gh` being absent —
        // if gh *is* installed, the call below still returns None
        // because the temp dir isn't a git repo. Either way the
        // contract holds: "no PR" / "no gh" / "no repo" all collapse
        // to None.
        let temp = std::env::temp_dir().join(format!(
            "codescope_pr_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let result = detect_pr_url(&temp, "no-such-branch-xyz");
        let _ = std::fs::remove_dir_all(&temp);
        assert!(result.is_none());
    }
}
