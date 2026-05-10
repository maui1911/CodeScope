//! Per-worktree pull-request URL detection.
//!
//! Shells out to `gh pr list --head <branch> --state open --json url
//! --limit 1` and returns the first PR's URL (or `None` if no open PR
//! is associated with the branch). Mirrors the C# build's
//! [`GitHubPullRequestService.GetOpenPrForBranchAsync`] — same `gh`
//! invocation shape (`--state open`, `--limit 1`), parsed with
//! `serde_json` rather than relying on a local `jq` (gh's bundled
//! `--jq` would also work, but routing the result through the same
//! parser the rest of `core` uses keeps error handling consistent).
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

use serde::Deserialize;

#[derive(Deserialize)]
struct GhPrRow {
    url: String,
}

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
/// is a JSON array of `{ "url": "<string>" }`; we deserialise the
/// first row and return its URL. Empty arrays / missing url / parse
/// errors all collapse to `None` so the caller can render "no row"
/// without branching on the failure mode.
pub(crate) fn parse_pr_url_json(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let rows: Vec<GhPrRow> = serde_json::from_str(trimmed).ok()?;
    let url = rows.into_iter().next()?.url;
    if url.is_empty() { None } else { Some(url) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_invalid_json_returns_none() {
        // gh shouldn't ever emit garbage, but a half-printed buffer
        // (e.g. SIGPIPE on the stream) shouldn't panic the UI.
        assert_eq!(parse_pr_url_json("not json"), None);
        assert_eq!(parse_pr_url_json("[{"), None);
    }

    #[test]
    fn parse_url_with_escaped_chars() {
        // Real PRs don't have escapes in the URL, but `serde_json`
        // handling them correctly (vs the previous manual substring
        // parser, which would have stopped at the first `"`) is the
        // safer behaviour to lock in.
        let json = "[{\"url\":\"https://example.com/a\\u0026b\"}]";
        assert_eq!(
            parse_pr_url_json(json).as_deref(),
            Some("https://example.com/a&b")
        );
    }

    #[test]
    fn detect_rejects_dash_prefixed_branch() {
        // Security: `--state` must not be reinterpreted as a flag.
        // We can't easily intercept the spawn, but the function
        // returns None up-front without spawning gh.
        let result = detect_pr_url(std::path::Path::new("."), "--state");
        assert!(result.is_none());
    }

    #[test]
    fn detect_rejects_empty_branch() {
        let result = detect_pr_url(std::path::Path::new("."), "");
        assert!(result.is_none());
    }

    #[test]
    fn detect_returns_none_when_gh_missing_or_no_repo() {
        // Use `tempfile::tempdir()` so the directory cleans up even
        // if the assertion below fires unexpectedly. We don't gate
        // the test on `gh` being absent — if gh *is* installed, the
        // call still returns None because the temp dir isn't a git
        // repo. Either way the contract holds: "no PR" / "no gh" /
        // "no repo" all collapse to None.
        let temp = tempfile::tempdir().expect("tempdir");
        let result = detect_pr_url(temp.path(), "no-such-branch-xyz");
        assert!(result.is_none());
    }
}
