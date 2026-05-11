//! Per-worktree pull-request integration.
//!
//! Shells out to `gh pr list --head <branch> --state open --json
//! number,url,statusCheckRollup --limit 1` and returns a
//! [`PullRequestInfo`] when an open PR is associated with the branch.
//! Mirrors the C# build's `GitHubPullRequestService.GetOpenPrForBranchAsync`
//! — same `gh` invocation shape (`--state open`, `--limit 1`) and the
//! same rollup → [`CiStatus`] reduction.
//!
//! Caching: this module is stateless — callers (the sidebar) own the
//! cache. The C# build polls every 30 s with per-worktree exponential
//! backoff up to 5 minutes (`PullRequestStatusPoller`); the Rust port
//! polls every 60 s on the sidebar and lazy-fetches on right-click to
//! warm the cache on first interaction.

use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(test)]
use serde::Deserialize;
use serde_json::Value;

/// Rollup of the PR's CI checks, derived from `gh`'s
/// `statusCheckRollup`. Mirrors the C# `CiStatus` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiStatus {
    /// No CI configured / empty rollup.
    None,
    /// At least one check is still running (not COMPLETED) and none
    /// have failed.
    Pending,
    /// All checks completed successfully.
    Success,
    /// At least one check ended in FAILURE / CANCELLED / TIMED_OUT /
    /// ACTION_REQUIRED / STARTUP_FAILURE.
    Failure,
}

/// Runtime-only PR metadata. Populated on demand by [`fetch_for_branch`]
/// and cached by the sidebar; not persisted (the PR state is always the
/// upstream's source of truth).
///
/// Mirrors the C# `PullRequestInfo` record. The `state` field the C#
/// build carries is omitted here — every PR returned by
/// `gh pr list --state open` is by definition `OPEN`, so the field would
/// be a constant in this code path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestInfo {
    pub number: u32,
    pub url: String,
    pub branch: String,
    pub ci_status: CiStatus,
}

#[cfg(test)]
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
/// Thin wrapper over [`fetch_for_branch`] that projects out the URL,
/// kept for call sites that only need the URL. New code should prefer
/// [`fetch_for_branch`] for the full info (number, CI status, …).
///
/// Security: rejects branch names starting with `-` so a pathological
/// branch can't be reinterpreted as a `gh pr list` flag.
pub fn detect_pr_url(working_dir: &Path, branch: &str) -> Option<String> {
    fetch_for_branch(working_dir, branch).map(|info| info.url)
}

/// Look up the open pull request for `branch` in the repo at
/// `working_dir`, returning the full [`PullRequestInfo`] (number, url,
/// branch, CI status). Returns `None` when no open PR exists, when
/// `gh` is not on PATH, or when the call fails — the UI treats all
/// failures as "no PR" for now.
///
/// Security: rejects branch names starting with `-` so a pathological
/// branch can't be reinterpreted as a `gh pr list` flag.
pub fn fetch_for_branch(working_dir: &Path, branch: &str) -> Option<PullRequestInfo> {
    if branch.is_empty() || branch.starts_with('-') {
        return None;
    }
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "number,url,statusCheckRollup",
            "--limit",
            "1",
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
    parse_pr_info_json(&stdout, branch)
}

/// Parse the output of `gh pr list --json url --limit 1`. Kept under
/// `#[cfg(test)]` so the original URL-only contract stays locked down
/// by tests while production code uses [`parse_pr_info_json`].
#[cfg(test)]
pub(crate) fn parse_pr_url_json(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let rows: Vec<GhPrRow> = serde_json::from_str(trimmed).ok()?;
    let url = rows.into_iter().next()?.url;
    if url.is_empty() { None } else { Some(url) }
}

/// Parse the output of `gh pr list --json number,url,statusCheckRollup
/// --limit 1`. Empty arrays / missing fields / parse errors collapse
/// to `None`. `branch` is threaded through so the returned
/// [`PullRequestInfo`] carries the branch the lookup ran against — the
/// sidebar's cache uses it to detect branch switches.
pub(crate) fn parse_pr_info_json(stdout: &str, branch: &str) -> Option<PullRequestInfo> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    let arr = value.as_array()?;
    let first = arr.first()?;

    let number = first.get("number")?.as_u64()? as u32;
    let url = first.get("url")?.as_str()?.to_string();
    if url.is_empty() {
        return None;
    }
    let ci_status = first
        .get("statusCheckRollup")
        .and_then(|r| r.as_array())
        .map(|rollup| rollup_ci(rollup))
        .unwrap_or(CiStatus::None);

    Some(PullRequestInfo {
        number,
        url,
        branch: branch.to_string(),
        ci_status,
    })
}

/// Reduce gh's `statusCheckRollup` array to a single [`CiStatus`].
///
///   - any FAILURE / CANCELLED / TIMED_OUT / ACTION_REQUIRED /
///     STARTUP_FAILURE → `Failure`
///   - else any non-COMPLETED status → `Pending`
///   - else (all SUCCESS / COMPLETED) → `Success`
///   - empty rollup → `None`
///
/// Mirrors the C# `GitHubPullRequestService.RollupCi`.
fn rollup_ci(rollup: &[Value]) -> CiStatus {
    if rollup.is_empty() {
        return CiStatus::None;
    }
    let mut has_pending = false;
    for check in rollup {
        let conclusion = check
            .get("conclusion")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let status = check
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if matches!(
            conclusion,
            "FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
        ) {
            return CiStatus::Failure;
        }

        if status != "COMPLETED" {
            has_pending = true;
        }
    }
    if has_pending { CiStatus::Pending } else { CiStatus::Success }
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
        // handling them correctly (vs a manual substring parser, which
        // would stop at the first `"`) is the safer behaviour.
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
    fn fetch_rejects_dash_prefixed_branch() {
        assert!(fetch_for_branch(std::path::Path::new("."), "--state").is_none());
    }

    #[test]
    fn fetch_rejects_empty_branch() {
        assert!(fetch_for_branch(std::path::Path::new("."), "").is_none());
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

    // ── parse_pr_info_json ───────────────────────────────────────

    #[test]
    fn parse_info_typical_with_empty_rollup() {
        let json = r#"[{
            "number": 42,
            "url": "https://github.com/owner/repo/pull/42",
            "statusCheckRollup": []
        }]"#;
        let info = parse_pr_info_json(json, "feat/x").expect("info");
        assert_eq!(info.number, 42);
        assert_eq!(info.url, "https://github.com/owner/repo/pull/42");
        assert_eq!(info.branch, "feat/x");
        assert_eq!(info.ci_status, CiStatus::None);
    }

    #[test]
    fn parse_info_no_rollup_field_defaults_to_none_ci() {
        let json = r#"[{"number":7,"url":"https://example.com/pull/7"}]"#;
        let info = parse_pr_info_json(json, "main").expect("info");
        assert_eq!(info.ci_status, CiStatus::None);
    }

    #[test]
    fn parse_info_with_failing_rollup() {
        let json = r#"[{
            "number": 12,
            "url": "https://github.com/owner/repo/pull/12",
            "statusCheckRollup": [
                {"status":"COMPLETED","conclusion":"SUCCESS"},
                {"status":"COMPLETED","conclusion":"FAILURE"}
            ]
        }]"#;
        let info = parse_pr_info_json(json, "feat/y").expect("info");
        assert_eq!(info.ci_status, CiStatus::Failure);
    }

    #[test]
    fn parse_info_with_pending_rollup() {
        let json = r#"[{
            "number": 99,
            "url": "https://github.com/owner/repo/pull/99",
            "statusCheckRollup": [
                {"status":"COMPLETED","conclusion":"SUCCESS"},
                {"status":"IN_PROGRESS","conclusion":""}
            ]
        }]"#;
        let info = parse_pr_info_json(json, "feat/z").expect("info");
        assert_eq!(info.ci_status, CiStatus::Pending);
    }

    #[test]
    fn parse_info_with_all_success_rollup() {
        let json = r#"[{
            "number": 1,
            "url": "https://github.com/owner/repo/pull/1",
            "statusCheckRollup": [
                {"status":"COMPLETED","conclusion":"SUCCESS"},
                {"status":"COMPLETED","conclusion":"SUCCESS"}
            ]
        }]"#;
        let info = parse_pr_info_json(json, "main").expect("info");
        assert_eq!(info.ci_status, CiStatus::Success);
    }

    #[test]
    fn parse_info_empty_array() {
        assert!(parse_pr_info_json("[]", "main").is_none());
    }

    #[test]
    fn parse_info_missing_url_returns_none() {
        let json = r#"[{"number":7}]"#;
        assert!(parse_pr_info_json(json, "main").is_none());
    }

    #[test]
    fn parse_info_missing_number_returns_none() {
        let json = r#"[{"url":"https://example.com/p/1"}]"#;
        assert!(parse_pr_info_json(json, "main").is_none());
    }

    #[test]
    fn parse_info_garbage_returns_none() {
        assert!(parse_pr_info_json("not json", "main").is_none());
        assert!(parse_pr_info_json("", "main").is_none());
        assert!(parse_pr_info_json("   ", "main").is_none());
    }

    // ── rollup_ci ────────────────────────────────────────────────

    fn check(status: &str, conclusion: &str) -> Value {
        serde_json::json!({"status": status, "conclusion": conclusion})
    }

    #[test]
    fn rollup_empty_is_none() {
        assert_eq!(rollup_ci(&[]), CiStatus::None);
    }

    #[test]
    fn rollup_all_completed_success_is_success() {
        let rollup = vec![
            check("COMPLETED", "SUCCESS"),
            check("COMPLETED", "SUCCESS"),
        ];
        assert_eq!(rollup_ci(&rollup), CiStatus::Success);
    }

    #[test]
    fn rollup_any_failure_short_circuits() {
        let rollup = vec![
            check("COMPLETED", "SUCCESS"),
            check("COMPLETED", "FAILURE"),
            check("IN_PROGRESS", ""),
        ];
        assert_eq!(rollup_ci(&rollup), CiStatus::Failure);
    }

    #[test]
    fn rollup_cancelled_is_failure() {
        let rollup = vec![check("COMPLETED", "CANCELLED")];
        assert_eq!(rollup_ci(&rollup), CiStatus::Failure);
    }

    #[test]
    fn rollup_timed_out_is_failure() {
        let rollup = vec![check("COMPLETED", "TIMED_OUT")];
        assert_eq!(rollup_ci(&rollup), CiStatus::Failure);
    }

    #[test]
    fn rollup_action_required_is_failure() {
        let rollup = vec![check("COMPLETED", "ACTION_REQUIRED")];
        assert_eq!(rollup_ci(&rollup), CiStatus::Failure);
    }

    #[test]
    fn rollup_startup_failure_is_failure() {
        let rollup = vec![check("COMPLETED", "STARTUP_FAILURE")];
        assert_eq!(rollup_ci(&rollup), CiStatus::Failure);
    }

    #[test]
    fn rollup_any_non_completed_is_pending() {
        let rollup = vec![
            check("COMPLETED", "SUCCESS"),
            check("IN_PROGRESS", ""),
        ];
        assert_eq!(rollup_ci(&rollup), CiStatus::Pending);
    }

    #[test]
    fn rollup_queued_is_pending() {
        let rollup = vec![check("QUEUED", "")];
        assert_eq!(rollup_ci(&rollup), CiStatus::Pending);
    }

    #[test]
    fn rollup_neutral_conclusion_treated_as_success() {
        // "NEUTRAL" / "SKIPPED" conclusions are not in the failure
        // set; a COMPLETED check with one of those shouldn't flip
        // pending. Useful for projects that use `gh actions` matrix
        // skips for early-exit jobs.
        let rollup = vec![
            check("COMPLETED", "SUCCESS"),
            check("COMPLETED", "NEUTRAL"),
            check("COMPLETED", "SKIPPED"),
        ];
        assert_eq!(rollup_ci(&rollup), CiStatus::Success);
    }
}
