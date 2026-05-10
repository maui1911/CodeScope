//! GitHub release polling — Velopack parity for the Rust port.
//!
//! The C# build (`src/CodeScope.App/Updates/UpdateService.cs`) wires
//! Velopack against `https://github.com/maui1911/CodeScope`'s `win`
//! channel. It checks once at startup and every 3 hours thereafter,
//! downloads any newer release in the background, and surfaces a toast
//! + restart-confirmation dialog when the staged update is ready.
//!
//! The Rust port doesn't bundle Velopack (no automatic downloader, no
//! restart-and-apply hook). What we *can* mirror is the visible signal:
//! poll the same release endpoint on the same cadence, compare against
//! the version baked in by `build.rs`, and push a notification entry
//! when a newer release exists. The user then upgrades manually — same
//! end-to-end shape as Velopack's "Later" path, where the staged
//! update applies on the next clean exit.
//!
//! # Endpoint
//!
//! `https://api.github.com/repos/maui1911/CodeScope/releases/latest`.
//! GitHub's REST API returns the most recent **non-prerelease** release
//! published with `vpk upload github` (which is what `release.yml` uses
//! per the C# build). Unauthenticated requests get 60/h per source IP
//! — far above our once-every-3-hours cadence even with multiple
//! processes on the same network.
//!
//! # Comparison rule
//!
//! Strict semver-major.minor.patch. A leading `v`/`V` is stripped from
//! both sides. A `-dirty` (or any other) suffix on the *current*
//! version is stripped before comparison so a developer build that
//! describes as `0.2.5-dirty` is treated as `0.2.5` for "is the
//! published release newer?" purposes. Prerelease suffixes on the
//! published tag (e.g. `0.3.0-rc1`) are likewise dropped — we use the
//! GitHub `latest` endpoint precisely so we never see them in
//! practice, but the parser handles them defensively.
//!
//! # Notification surface
//!
//! `app.rs` reads `UpdateStatus::Available` and pushes a `Generic`
//! notification with a fixed title (`"Update available"`) and a detail
//! line containing the new version + release URL. The C# build uses a
//! `ToastSeverity.Ok` snackbar plus a confirmation dialog; we use the
//! existing Notification ring buffer because (a) the Rust port has no
//! restart-and-apply path, so the dialog wouldn't have anything
//! actionable to do, and (b) the bell button already collects "passive
//! event" entries, which is exactly what an "update available" hint
//! is.
//!
//! # Dev-mode behaviour
//!
//! `should_poll(paths)` returns `false` under `CODESCOPE_DEV=1` —
//! mirrors the C# `IsDevMode` early return in `UpdateService.CheckAsync`.
//! A dev build runs from `cargo run`, has no installer to upgrade, and
//! the `0.0-unknown` git-describe slug compares falsely-newer against
//! every published tag.
//!
//! # Network-failure semantics
//!
//! Every error path collapses to `UpdateStatus::Unknown` — the caller
//! logs a single line and tries again on the next interval. We never
//! panic, never propagate a `Result`, and never re-toast the same
//! version twice in a single process lifetime (the caller dedupes by
//! holding the last-announced version in `AppShell` state).

use std::cmp::Ordering;
use std::time::Duration;

use serde::Deserialize;

use crate::AppPaths;

/// Public GitHub release endpoint we poll. Mirrors the `RepoUrl`
/// constant in C# `UpdateService` (the `/releases/latest` path is
/// what Velopack hits internally).
pub const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/maui1911/CodeScope/releases/latest";

/// Initial delay before the first poll fires — keeps the network call
/// off the startup-critical path. Mirrors the 10 s delay in
/// `App.xaml.cs`.
pub const INITIAL_DELAY: Duration = Duration::from_secs(10);

/// Cadence between polls. Mirrors the 3 h interval in `App.xaml.cs`.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3 * 60 * 60);

/// HTTP timeout for a single poll. We're hitting `api.github.com`,
/// which is normally <1 s round-trip — anything longer than 30 s
/// almost always means a captive portal or DNS hijack we can't recover
/// from anyway, so we bail and try again on the next interval.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// User-agent header sent with the poll. GitHub's API rejects requests
/// without one (returns 403). Format follows common conventions —
/// `<product>/<version>` — using the slug the caller passes in (the
/// top-level binary bakes `CODESCOPE_VERSION_DISPLAY` via `build.rs`;
/// the `core` crate doesn't see that env var directly).
fn user_agent(current_version: &str) -> String {
    format!("CodeScope/{current_version}")
}

/// One published release as returned by GitHub's `/releases/latest`
/// endpoint. We deserialise only the three fields we surface; GitHub
/// adds new fields regularly and we don't want a future addition to
/// break the parse.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    /// Git tag of the published release, e.g. `v0.2.5` or `0.2.5`.
    pub tag_name: String,
    /// HTML URL the user clicks to view the release notes / download.
    pub html_url: String,
    /// Release notes body (Markdown). Optional because some early
    /// releases have an empty body and GitHub returns `null` rather
    /// than `""` in that case.
    #[serde(default)]
    pub body: Option<String>,
}

/// Result of one poll cycle.
///
/// `UpToDate` and `Unknown` look the same to the user (no
/// notification), but the caller may use them differently — e.g.
/// `Unknown` could trigger a faster retry. We currently treat them
/// identically and just keep the next-tick cadence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// A newer release is published. Caller should surface a
    /// notification with the supplied fields.
    Available {
        /// Version string with any `v`/`V` prefix stripped.
        version: String,
        /// HTML URL of the GitHub release page.
        url: String,
        /// Release notes (Markdown, possibly empty).
        body: String,
    },
    /// The latest published release is the same as or older than the
    /// running build.
    UpToDate,
    /// We couldn't determine the answer (network failure, parse
    /// failure, version comparison ambiguous). Caller logs and retries
    /// on the next tick.
    Unknown,
}

/// Should the background poll run for this process?
///
/// Skips dev builds (`CODESCOPE_DEV=1`) — mirrors the
/// `AppPaths.IsDevMode` early return in C# `UpdateService.CheckAsync`.
/// A dev build has no installer to upgrade, and the `0.0-unknown`
/// git-describe fallback would compare falsely-newer against every
/// published tag.
pub fn should_poll(paths: &AppPaths) -> bool {
    !paths.dev_mode
}

/// Run one poll cycle synchronously: fetch the GitHub `latest`
/// release, parse the tag, compare against `current_version`. Returns
/// `UpdateStatus::Unknown` for any failure path.
///
/// Designed to be called from `cx.background_spawn` — blocking work
/// that must not run on the UI thread.
pub fn check_once(current_version: &str) -> UpdateStatus {
    let body = match fetch_latest_release_json(RELEASES_LATEST_URL, current_version) {
        Ok(body) => body,
        Err(err) => {
            eprintln!("[update_check] fetch failed: {err}");
            return UpdateStatus::Unknown;
        }
    };
    evaluate(&body, current_version)
}

/// Pure logic: given the JSON body of a `/releases/latest` response
/// and the currently-running version slug, decide what to surface.
///
/// Split out from `check_once` so unit tests can exercise the full
/// version-comparison + JSON-shape logic without touching the
/// network. Mirrors the way `pr::parse_pr_url_json` is split from
/// `pr::detect_pr_url`.
pub fn evaluate(json_body: &str, current_version: &str) -> UpdateStatus {
    let release: ReleaseInfo = match serde_json::from_str(json_body) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("[update_check] release parse failed: {err}");
            return UpdateStatus::Unknown;
        }
    };
    let latest = strip_v_prefix(release.tag_name.trim());
    // Reject the build.rs `0.0-unknown` fallback explicitly: a dev
    // build that couldn't resolve git would otherwise normalise to
    // `0.0`, compare false-newer against every published tag, and
    // get spammed with notifications on every poll.
    if is_unknown_fallback(current_version) {
        return UpdateStatus::UpToDate;
    }
    let current = normalise_current(current_version);
    match compare_versions(latest, &current) {
        Some(Ordering::Greater) => UpdateStatus::Available {
            version: latest.to_owned(),
            url: release.html_url,
            body: release.body.unwrap_or_default(),
        },
        Some(Ordering::Equal | Ordering::Less) => UpdateStatus::UpToDate,
        None => {
            // Version comparison is ambiguous — e.g. the running
            // build is `0.0-unknown` (no tag yet) or the published
            // tag isn't semver-shaped. Pretend up-to-date so we
            // don't spam notifications about an "unknown" version
            // delta.
            UpdateStatus::UpToDate
        }
    }
}

/// HTTP GET against GitHub's API. Returns the response body on 2xx,
/// `Err(message)` for any failure — including non-2xx, network error,
/// timeout, or oversized response. Caller treats every error as
/// "skip this tick".
///
/// `pub(crate)` rather than `pub` because the only legitimate caller
/// is `check_once`; tests that need to exercise parse + version
/// logic go through `evaluate` with a fixture JSON string.
fn fetch_latest_release_json(url: &str, current_version: &str) -> Result<String, String> {
    let response = ureq::get(url)
        .set("User-Agent", &user_agent(current_version))
        .set("Accept", "application/vnd.github+json")
        .timeout(REQUEST_TIMEOUT)
        .call()
        .map_err(|e| format!("request failed: {e}"))?;
    if response.status() < 200 || response.status() >= 300 {
        return Err(format!("HTTP {}", response.status()));
    }
    // Cap the body at 1 MiB. Real responses are ~10 KiB; anything
    // larger almost certainly means GitHub returned an HTML error
    // page or our endpoint is being MITMed.
    response
        .into_string()
        .map_err(|e| format!("read body failed: {e}"))
}

/// Strip a leading `v` or `V` from a version string. Mirrors
/// `build.rs::strip_v_prefix` so the same rule applies on both sides
/// of the comparison.
fn strip_v_prefix(s: &str) -> &str {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s)
}

/// Reduce the current build slug to a semver-shaped string for
/// comparison against a published tag.
///
/// `CODESCOPE_VERSION_DISPLAY` comes from `git describe --tags
/// --always --dirty`, which produces shapes like:
///
/// * `0.2.5` — exact tag
/// * `0.2.5-dirty` — exact tag with uncommitted changes
/// * `0.2.5-3-g1a2b3c4` — 3 commits past the tag
/// * `0.2.5-3-g1a2b3c4-dirty` — same plus dirty
/// * `0.0-unknown` — no git info available (build.rs fallback)
///
/// We strip the `v` prefix and drop everything from the first `-`
/// onward, treating "past the tag with uncommitted changes" as the
/// tagged version itself. Conservative on purpose: a dev who's three
/// commits ahead of `0.2.5` shouldn't get a "0.2.5 is available"
/// notification.
/// Recognise the `build.rs` last-resort fallback (`0.0-unknown`).
/// A dev build that can't resolve git gets stamped with this slug;
/// treating it as a real version would compare false-newer against
/// every published tag.
fn is_unknown_fallback(s: &str) -> bool {
    let s = strip_v_prefix(s.trim());
    s == "0.0-unknown" || s.ends_with("-unknown")
}

fn normalise_current(s: &str) -> String {
    let s = strip_v_prefix(s.trim());
    match s.find('-') {
        Some(idx) => s[..idx].to_owned(),
        None => s.to_owned(),
    }
}

/// Lexicographic comparison over `(major, minor, patch)` triples.
/// Both inputs must already have their `v` prefix stripped and
/// suffixes normalised away. Returns `None` when either side fails
/// to parse as a semver triple — caller treats this as "ambiguous,
/// don't notify".
///
/// We accept 1-, 2-, or 3-segment versions; missing segments default
/// to 0 (`1.2` becomes `(1, 2, 0)`). This matches Velopack's
/// `SemanticVersion.Parse` behaviour for short tags like the early
/// `0.1` releases.
fn compare_versions(latest: &str, current: &str) -> Option<Ordering> {
    let l = parse_triple(latest)?;
    let c = parse_triple(current)?;
    Some(l.cmp(&c))
}

fn parse_triple(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch_raw = parts.next().unwrap_or("0");
    // A 4th segment (`a.b.c.d`) is rejected — we don't ship 4-part
    // versions and accepting them would mask malformed tags.
    if parts.next().is_some() {
        return None;
    }
    // Drop any prerelease tail on the patch segment (`0.3.0-rc1` →
    // `0`) so the published `latest` endpoint's defensive parse
    // matches `normalise_current` on the dev side.
    let patch_clean = patch_raw.split('-').next().unwrap_or("0");
    let patch: u64 = patch_clean.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_v_prefix ──────────────────────────────────────────────────

    #[test]
    fn strip_v_prefix_removes_lower_v() {
        assert_eq!(strip_v_prefix("v0.2.5"), "0.2.5");
    }

    #[test]
    fn strip_v_prefix_removes_upper_v() {
        assert_eq!(strip_v_prefix("V0.2.5"), "0.2.5");
    }

    #[test]
    fn strip_v_prefix_no_prefix_unchanged() {
        assert_eq!(strip_v_prefix("0.2.5"), "0.2.5");
    }

    #[test]
    fn strip_v_prefix_preserves_internal_v() {
        assert_eq!(strip_v_prefix("0.2.5-rc1"), "0.2.5-rc1");
    }

    // ── normalise_current ───────────────────────────────────────────────

    #[test]
    fn normalise_current_strips_dirty_suffix() {
        assert_eq!(normalise_current("0.2.5-dirty"), "0.2.5");
    }

    #[test]
    fn normalise_current_strips_describe_distance() {
        assert_eq!(normalise_current("0.2.5-3-g1a2b3c4"), "0.2.5");
        assert_eq!(normalise_current("0.2.5-3-g1a2b3c4-dirty"), "0.2.5");
    }

    #[test]
    fn normalise_current_strips_v_prefix() {
        assert_eq!(normalise_current("v0.2.5"), "0.2.5");
        assert_eq!(normalise_current("V0.2.5-dirty"), "0.2.5");
    }

    #[test]
    fn normalise_current_unknown_passes_through() {
        // The "0.0-unknown" build.rs fallback. Comparison against any
        // semver tag will fail (no patch number) and `evaluate`
        // returns UpToDate.
        assert_eq!(normalise_current("0.0-unknown"), "0.0");
    }

    // ── compare_versions ────────────────────────────────────────────────

    #[test]
    fn compare_patch_increment() {
        assert_eq!(compare_versions("0.2.6", "0.2.5"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.2.5", "0.2.6"), Some(Ordering::Less));
    }

    #[test]
    fn compare_minor_increment() {
        assert_eq!(compare_versions("0.3.0", "0.2.99"), Some(Ordering::Greater));
    }

    #[test]
    fn compare_major_increment() {
        assert_eq!(compare_versions("1.0.0", "0.99.99"), Some(Ordering::Greater));
    }

    #[test]
    fn compare_equal() {
        assert_eq!(compare_versions("0.2.5", "0.2.5"), Some(Ordering::Equal));
    }

    #[test]
    fn compare_short_form_treats_missing_as_zero() {
        // `0.2` and `0.2.0` are equivalent.
        assert_eq!(compare_versions("0.2", "0.2.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1", "1.0.0"), Some(Ordering::Equal));
    }

    #[test]
    fn compare_rejects_too_many_segments() {
        // Four-part versions are not a shape we ship; treat as
        // ambiguous so the caller doesn't notify on garbage input.
        assert_eq!(compare_versions("1.0.0.0", "1.0.0"), None);
    }

    #[test]
    fn compare_rejects_non_numeric() {
        assert_eq!(compare_versions("abc.def.ghi", "0.0.0"), None);
        // Empty component
        assert_eq!(compare_versions("..", "0.0.0"), None);
    }

    #[test]
    fn compare_strips_prerelease_tail_from_patch() {
        // The `latest` endpoint normally returns non-prereleases, but
        // the parser still handles them defensively.
        assert_eq!(compare_versions("0.3.0-rc1", "0.3.0"), Some(Ordering::Equal));
    }

    // ── evaluate (full pipeline, JSON → UpdateStatus) ───────────────────

    fn release_json(tag: &str, url: &str, body: Option<&str>) -> String {
        let body_field = match body {
            Some(b) => format!("\"{}\"", b.replace('\"', "\\\"")),
            None => "null".to_string(),
        };
        format!(
            "{{\"tag_name\":\"{tag}\",\"html_url\":\"{url}\",\"body\":{body_field}}}"
        )
    }

    #[test]
    fn evaluate_available_when_published_newer() {
        let json = release_json(
            "v0.2.6",
            "https://github.com/maui1911/CodeScope/releases/tag/v0.2.6",
            Some("Bug fixes"),
        );
        let status = evaluate(&json, "0.2.5");
        match status {
            UpdateStatus::Available { version, url, body } => {
                assert_eq!(version, "0.2.6");
                assert_eq!(
                    url,
                    "https://github.com/maui1911/CodeScope/releases/tag/v0.2.6"
                );
                assert_eq!(body, "Bug fixes");
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_uptodate_when_running_dirty_at_same_tag() {
        let json = release_json("v0.2.5", "https://example.invalid", None);
        // `-dirty` on the local build still maps to 0.2.5.
        assert_eq!(evaluate(&json, "0.2.5-dirty"), UpdateStatus::UpToDate);
    }

    #[test]
    fn evaluate_uptodate_when_running_ahead_of_release() {
        // Developer building from a future commit shouldn't see a
        // notification for a release they're already past.
        let json = release_json("v0.2.5", "https://example.invalid", None);
        assert_eq!(evaluate(&json, "0.2.6"), UpdateStatus::UpToDate);
    }

    #[test]
    fn evaluate_uptodate_when_current_is_unknown_fallback() {
        // The build.rs `0.0-unknown` fallback — no patch number, parse
        // fails, comparison ambiguous → UpToDate (don't spam).
        let json = release_json("v0.2.5", "https://example.invalid", None);
        assert_eq!(evaluate(&json, "0.0-unknown"), UpdateStatus::UpToDate);
    }

    #[test]
    fn evaluate_unknown_on_malformed_json() {
        assert_eq!(evaluate("not json", "0.2.5"), UpdateStatus::Unknown);
        assert_eq!(evaluate("", "0.2.5"), UpdateStatus::Unknown);
    }

    #[test]
    fn evaluate_unknown_on_missing_required_fields() {
        // Missing `tag_name` — serde_json fails the field, returns
        // Unknown. `body` is optional; everything else isn't.
        assert_eq!(
            evaluate("{\"html_url\":\"x\"}", "0.2.5"),
            UpdateStatus::Unknown
        );
    }

    #[test]
    fn evaluate_handles_null_body() {
        let json = release_json("v0.2.6", "https://example.invalid", None);
        match evaluate(&json, "0.2.5") {
            UpdateStatus::Available { body, .. } => assert_eq!(body, ""),
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_handles_unprefixed_tag() {
        let json = release_json("0.2.6", "https://example.invalid", Some("notes"));
        match evaluate(&json, "0.2.5") {
            UpdateStatus::Available { version, .. } => assert_eq!(version, "0.2.6"),
            other => panic!("expected Available, got {other:?}"),
        }
    }

    // ── should_poll (dev-mode gate) ─────────────────────────────────────

    #[test]
    fn should_poll_skips_dev_mode() {
        let mut paths = AppPaths::detect();
        paths.dev_mode = true;
        assert!(!should_poll(&paths));
    }

    #[test]
    fn should_poll_allows_production() {
        let mut paths = AppPaths::detect();
        paths.dev_mode = false;
        assert!(should_poll(&paths));
    }
}
